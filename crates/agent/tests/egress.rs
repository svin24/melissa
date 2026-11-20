//! Deny-by-default egress, end to end: a sandboxed shell script (bwrap, own network
//! namespace, `melissa-netbridge` inside) tries to reach an allowlisted test host, a
//! denied host, the host's loopback and the LAN directly, DNS (only a placeholder answer),
//! and Melissa's MCP endpoint.
//! The "allowlisted host" is a local test server under a made-up name, mapped through the
//! proxy's resolver override, so the test needs no internet. Skips if bwrap or curl is
//! missing or bwrap cannot create namespaces.

// Test-only file: panicking on setup failure is the intended behaviour.
#![allow(clippy::unwrap_used)]

use std::{
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
};

use melissa_agent::{
    egress::EgressProxy,
    mcp::McpServer,
    sandbox,
    tools::{DeviceInfo, TaskContext, Tool, ToolRegistry},
    AuditLog, Backend, CliBackend, CliSpec, EventSink, McpEndpoint, NetworkMode, RunRequest,
    SandboxSpec,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("melissa-egress-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A host program: `/usr/bin/<name>` (Fedora) or the first `<name>` on `PATH` (NixOS).
fn host_program(name: &str) -> Option<PathBuf> {
    let usr = Path::new("/usr/bin").join(name);
    if usr.is_file() {
        return Some(usr);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// Can bwrap create the sandbox's namespaces here? Probes with the host's root bound
/// read-only, so the check does not depend on the distribution's layout.
fn namespaces_work(bwrap: &Path, sh: &Path) -> bool {
    std::process::Command::new(bwrap)
        .args(["--unshare-all", "--ro-bind", "/", "/", "--"])
        .arg(sh)
        .args(["-c", "true"])
        .status()
        .is_ok_and(|s| s.success())
}

/// `sh` and `curl` on this host, as strings for scripts and specs.
fn sh_and_curl() -> (String, String) {
    let path = |n| host_program(n).unwrap().to_str().unwrap().to_string();
    (path("sh"), path("curl"))
}

/// `melissa-netbridge`: `MELISSA_NETBRIDGE` when set (test binaries run outside their
/// build tree, e.g. in the NixOS VM test), else the one cargo built.
fn netbridge() -> PathBuf {
    std::env::var_os("MELISSA_NETBRIDGE")
        .map(PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_melissa-netbridge").into())
}

fn tools_present() -> bool {
    let Some(bwrap) = sandbox::find_bwrap() else {
        eprintln!("SKIP: bwrap not found");
        return false;
    };
    let (Some(sh), Some(_)) = (host_program("sh"), host_program("curl")) else {
        eprintln!("SKIP: sh or curl not found");
        return false;
    };
    let ok = namespaces_work(&bwrap, &sh);
    if !ok {
        eprintln!("SKIP: bwrap cannot create namespaces here");
    }
    ok
}

/// A tiny HTTP server on the host's loopback answering every request with `body`.
async fn http_server(body: &'static str) -> SocketAddr {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf).await;
                let reply = format!(
                    "HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(reply.as_bytes()).await;
            });
        }
    });
    addr
}

#[tokio::test]
async fn sandboxed_agent_reaches_only_allowlisted_hosts_and_mcp() {
    if !tools_present() {
        return;
    }
    let root = scratch("e2e");
    let home = root.join("home");
    let workdir = root.join("work");
    std::fs::create_dir_all(&home).unwrap();
    let audit = AuditLog::new(root.join("audit.jsonl"));
    let server = http_server("allowed-body").await;
    let port = server.port();

    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(DeviceInfo::new())];
    let mcp = McpServer::new(ToolRegistry::new(tools, None));
    let token = mcp.open_session(42, TaskContext::default(), None).unwrap();
    // The MCP address inside the sandbox; nothing listens there on the host.
    let mcp_addr: SocketAddr = "127.0.0.1:18080".parse().unwrap();

    let proxy = EgressProxy::new(root.join("sockets"), Some(audit.clone()))
        .unwrap()
        .with_netbridge(netbridge())
        .with_mcp(mcp.clone(), mcp_addr)
        .with_resolver_override("allowed.test.example", vec![server.ip()])
        .with_resolver_override("denied.test.example", vec![server.ip()]);

    let (sh, curl) = sh_and_curl();
    let curl = format!("{curl} -sS --max-time 5");
    let script = format!(
        r#"
        echo "proxy=$HTTPS_PROXY"
        # Allowed: through the proxy (-p: CONNECT even for http URLs).
        echo "allowed=$({curl} -p http://allowed.test.example:{port}/ 2>&1)"
        # Denied by the allowlist (resolvable to the same server, so only policy stops it).
        {curl} -p http://denied.test.example:{port}/ >/dev/null 2>&1 && echo LEAK-denied-host
        {curl} https://example.com/ >/dev/null 2>&1 && echo LEAK-internet
        # Around the proxy: the host's loopback, a LAN address, DNS.
        {curl} --noproxy '*' http://127.0.0.1:{port}/ >/dev/null 2>&1 && echo LEAK-host-loopback
        {curl} --noproxy '*' --connect-timeout 2 http://192.0.2.1/ >/dev/null 2>&1 && echo LEAK-lan
        # DNS answers only the placeholder (nothing is forwarded), and it leads nowhere.
        # (`getent hosts`, not `ahosts`: AI_ADDRCONFIG finds no address but loopback here.)
        dns=$(getent hosts example.com 2>/dev/null | head -n1 | cut -d' ' -f1)
        [ -n "$dns" ] && [ "$dns" != "192.88.99.53" ] && echo LEAK-dns
        echo "dns=$dns"
        {curl} --noproxy '*' --connect-timeout 2 http://example.com/ >/dev/null 2>&1 && echo LEAK-placeholder
        # Melissa's tools through the bridge.
        echo "mcp=$({curl} -X POST -H 'Content-Type: application/json' \
            -d '{{"jsonrpc":"2.0","id":1,"method":"tools/list"}}' \
            http://{mcp_addr}/mcp/$1)"
        echo "api=$({curl} -o /dev/null -w '%{{http_code}}' -X POST http://{mcp_addr}/api/lead/messages)"
        echo done
        "#
    );
    let spec = CliSpec::new("sh", &sh, &["-c", &script, "melissa", "{prompt}"])
        .with_sandbox(SandboxSpec::default())
        .with_network(NetworkMode::Proxy)
        .with_allow(vec![format!("allowed.test.example:{port}")]);
    let backend = CliBackend::new(spec, workdir.clone())
        .with_home(home)
        .with_egress(proxy);
    assert!(backend.info().available, "{:?}", backend.info());
    assert!(backend.info().detail.contains("net: proxy"));

    let (sink, mut rx) = EventSink::channel();
    let req = RunRequest {
        prompt: token.clone(),
        workdir: Some(workdir),
        task: Some(42),
        mcp: Some(McpEndpoint {
            url: format!("http://{mcp_addr}/mcp/{token}"),
            token: token.clone(),
        }),
        ..RunRequest::default()
    };
    let result = backend.run(&req, sink).await;
    let mut lines = Vec::new();
    while let Ok(line) = rx.try_recv() {
        lines.push(line.text);
    }
    assert!(result.is_ok(), "{result:?} {lines:?}");
    assert!(lines.contains(&"done".to_string()), "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("LEAK")), "{lines:?}");
    assert!(
        lines.contains(&"proxy=http://127.0.0.1:3128".to_string()),
        "{lines:?}"
    );
    // The bridge handed its DNS socket over and the lead agent answers on port 53.
    assert!(lines.contains(&"dns=192.88.99.53".to_string()), "{lines:?}");
    assert!(
        lines.contains(&"allowed=allowed-body".to_string()),
        "{lines:?}"
    );
    let mcp_line = lines.iter().find(|l| l.starts_with("mcp=")).unwrap();
    assert!(mcp_line.contains("device_info"), "{mcp_line}");
    assert!(lines.contains(&"api=404".to_string()), "{lines:?}");

    let audit_text = std::fs::read_to_string(audit.path()).unwrap();
    let records: Vec<serde_json::Value> = audit_text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let has = |action: &str, prefix: &str| {
        records.iter().any(|r| {
            r["action"] == action
                && r["task"] == 42
                && r["backend"] == "sh"
                && r["detail"].as_str().unwrap().starts_with(prefix)
        })
    };
    assert!(
        has(
            "egress.allow",
            &format!("allowed.test.example:{port} -> 127.0.0.1")
        ),
        "{audit_text}"
    );
    assert!(
        has(
            "egress.deny",
            &format!("denied.test.example:{port}: not in the allowlist")
        ),
        "{audit_text}"
    );
    assert!(
        has("egress.deny", "example.com:443: not in the allowlist"),
        "{audit_text}"
    );
    // Direct attempts never reach the proxy, so they cannot appear in its audit.
    assert_eq!(records.len(), 3, "{audit_text}");
    // The lease is gone with the run: no sockets left behind.
    assert_eq!(std::fs::read_dir(root.join("sockets")).unwrap().count(), 0);
    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn proxy_mode_without_the_egress_proxy_fails_closed() {
    if !tools_present() {
        return;
    }
    let root = scratch("closed");
    let (sh, curl) = sh_and_curl();
    let spec = CliSpec::new("sh", &sh, &["-c", "echo ran", "melissa", "{prompt}"])
        .with_sandbox(SandboxSpec::default());
    let backend = CliBackend::new(spec, root.join("work")).with_home(root.clone());
    // Not available, and running anyway is refused rather than falling back to the host.
    assert!(!backend.info().available);
    let (sink, _rx) = EventSink::channel();
    let result = backend.run(&RunRequest::prompt("x"), sink).await;
    assert!(result.is_err(), "{result:?}");

    // `none`: runs, but with no network at all.
    let script = format!(
        "{curl} -sS --max-time 3 --noproxy '*' http://1.1.1.1/ >/dev/null 2>&1 && echo LEAK; echo ok"
    );
    let spec = CliSpec::new("sh", &sh, &["-c", &script, "melissa", "{prompt}"])
        .with_sandbox(SandboxSpec::default())
        .with_network(NetworkMode::None);
    let backend = CliBackend::new(spec, root.join("work")).with_home(root.clone());
    assert!(backend.info().available);
    let (sink, mut rx) = EventSink::channel();
    backend.run(&RunRequest::prompt("x"), sink).await.unwrap();
    let mut lines = Vec::new();
    while let Ok(line) = rx.try_recv() {
        lines.push(line.text);
    }
    assert_eq!(lines, ["ok"]);
    std::fs::remove_dir_all(&root).unwrap();
}
