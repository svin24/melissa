//! Proxy and MCP socket behaviour, talking to the lease's Unix sockets directly.

use std::net::Ipv4Addr;

use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, UnixStream},
};

use super::*;
use crate::tools::{DeviceInfo, TaskContext, Tool, ToolRegistry};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("melissa-egress-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A TCP server on 127.0.0.1 that answers every connection with `hello` and echoes
/// one read.
async fn echo_server() -> SocketAddr {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                s.write_all(b"hello").await.unwrap();
                let mut buf = [0u8; 64];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let _ = s.write_all(&buf[..n]).await;
            });
        }
    });
    addr
}

fn audit_lines(log: &AuditLog) -> Vec<Value> {
    std::fs::read_to_string(log.path())
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Send a request head on a fresh connection to the lease's proxy socket; return the
/// status line and the stream.
async fn connect(dir: &Path, head: &str) -> (String, UnixStream) {
    let mut s = UnixStream::connect(dir.join(EGRESS_SOCKET)).await.unwrap();
    s.write_all(head.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while head_end(&buf).is_none() {
        if s.read(&mut byte).await.unwrap() == 0 {
            break;
        }
        buf.push(byte[0]);
    }
    let text = String::from_utf8(buf).unwrap();
    (text.lines().next().unwrap_or_default().to_string(), s)
}

fn request(mode: NetworkMode, allow: &[&str], tokens: Vec<String>) -> EgressRequest {
    EgressRequest {
        backend: "test".into(),
        task: 7,
        mode,
        allow: allow.iter().map(|r| HostRule::parse(r).unwrap()).collect(),
        mcp_tokens: tokens,
    }
}

#[tokio::test]
async fn allowlisted_hosts_tunnel_and_everything_is_audited() {
    let root = scratch("proxy");
    let audit = AuditLog::new(root.join("audit.jsonl"));
    let server = echo_server().await;
    let proxy = EgressProxy::new(root.join("sockets"), Some(audit.clone()))
        .unwrap()
        .with_netbridge("/usr/bin/true".into())
        .with_resolver_override("api.test.example", vec![server.ip()]);
    let port = server.port();
    let lease = proxy
        .open(request(
            NetworkMode::Proxy,
            &[
                &format!("api.test.example:{port}"),
                &format!("localhost:{port}"),
            ],
            vec![],
        ))
        .unwrap();
    let bridge = lease.bridge().unwrap().clone();
    assert_eq!(bridge.proxy, Some("127.0.0.1:3128".parse().unwrap()));
    assert_eq!(bridge.ports.len(), 1, "no MCP tokens, no MCP socket");
    let dir = bridge.socket_dir.clone();

    // Allowed: tunnel established, bytes flow both ways.
    let (status, mut s) = connect(
        &dir,
        &format!("CONNECT API.test.example:{port} HTTP/1.1\r\nHost: x\r\n\r\n"),
    )
    .await;
    assert_eq!(status, "HTTP/1.1 200 Connection established");
    let mut hello = [0u8; 5];
    s.read_exact(&mut hello).await.unwrap();
    assert_eq!(&hello, b"hello");
    s.write_all(b"ping").await.unwrap();
    let mut pong = [0u8; 4];
    s.read_exact(&mut pong).await.unwrap();
    assert_eq!(&pong, b"ping");
    drop(s);

    // Denied: not on the list, wrong port, plain HTTP, and a listed name that resolves
    // to loopback (DNS answers are checked, not just names).
    for (head, want) in [
        ("CONNECT example.com:443 HTTP/1.1\r\n\r\n", "403"),
        ("CONNECT api.test.example:443 HTTP/1.1\r\n\r\n", "403"),
        ("GET http://api.test.example/ HTTP/1.1\r\n\r\n", "403"),
        (&format!("CONNECT localhost:{port} HTTP/1.1\r\n\r\n"), "403"),
        ("CONNECT nonsense HTTP/1.1\r\n\r\n", "400"),
    ] {
        let (status, _) = connect(&dir, head).await;
        assert!(
            status.starts_with(&format!("HTTP/1.1 {want}")),
            "{head:?}: {status}"
        );
    }

    let lines = audit_lines(&audit);
    let actions: Vec<(&str, &str)> = lines
        .iter()
        .map(|l| (l["action"].as_str().unwrap(), l["detail"].as_str().unwrap()))
        .collect();
    assert!(
        actions[0].0 == "egress.allow"
            && actions[0]
                .1
                .starts_with(&format!("api.test.example:{port} -> 127.0.0.1:{port}")),
        "{actions:?}"
    );
    assert!(actions.contains(&("egress.deny", "example.com:443: not in the allowlist")));
    assert!(actions.contains(&("egress.deny", "api.test.example:443: not in the allowlist")));
    assert!(actions
        .iter()
        .any(|(a, d)| *a == "egress.deny" && d.starts_with("GET http://api.test.example/")));
    assert!(actions
        .iter()
        .any(|(a, d)| *a == "egress.deny" && d.contains("non-public addresses")));
    for l in &lines {
        assert_eq!(l["actor"], "agent");
        assert_eq!(l["task"], 7);
        assert_eq!(l["backend"], "test");
    }

    // Dropping the lease removes the sockets.
    drop(lease);
    assert!(!dir.exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn repeated_denials_are_counted_not_repeated() {
    let root = scratch("repeat");
    let audit = AuditLog::new(root.join("audit.jsonl"));
    let proxy = EgressProxy::new(root.join("sockets"), Some(audit.clone()))
        .unwrap()
        .with_netbridge("/usr/bin/true".into());
    let lease = proxy
        .open(request(NetworkMode::Proxy, &[], vec![]))
        .unwrap();
    let dir = lease.bridge().unwrap().socket_dir.clone();
    for _ in 0..5 {
        let (status, _) = connect(&dir, "CONNECT telemetry.example:443 HTTP/1.1\r\n\r\n").await;
        assert!(status.contains("403"));
    }
    let (status, _) = connect(&dir, "CONNECT other.example:443 HTTP/1.1\r\n\r\n").await;
    assert!(status.contains("403"));
    let details = |log: &AuditLog| -> Vec<String> {
        audit_lines(log)
            .iter()
            .map(|l| l["detail"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        details(&audit),
        [
            "telemetry.example:443: not in the allowlist",
            "other.example:443: not in the allowlist"
        ]
    );
    // When the sandbox ends, the counted repeats are written.
    drop(lease);
    for _ in 0..50 {
        if details(&audit).len() == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        details(&audit)[2],
        "telemetry.example:443: not in the allowlist (+4 repeats)"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn learn_mode_allows_unlisted_hosts_but_not_private_ones() {
    let root = scratch("learn");
    let audit = AuditLog::new(root.join("audit.jsonl"));
    let server = echo_server().await;
    let port = server.port();
    let proxy = EgressProxy::new(root.join("sockets"), Some(audit.clone()))
        .unwrap()
        .with_netbridge("/usr/bin/true".into())
        .with_resolver_override("anything.test.example", vec![server.ip()]);
    let lease = proxy
        .open(request(NetworkMode::Learn, &[], vec![]))
        .unwrap();
    let dir = lease.bridge().unwrap().socket_dir.clone();
    let (status, _) = connect(
        &dir,
        &format!("CONNECT anything.test.example:{port} HTTP/1.1\r\n\r\n"),
    )
    .await;
    assert!(status.contains("200"), "{status}");
    let (status, _) = connect(&dir, &format!("CONNECT localhost:{port} HTTP/1.1\r\n\r\n")).await;
    assert!(status.contains("403"), "{status}");
    let lines = audit_lines(&audit);
    assert!(lines[0]["detail"]
        .as_str()
        .unwrap()
        .ends_with("(learn: NOT in the allowlist)"));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn connection_limit_per_sandbox() {
    let root = scratch("limit");
    let proxy = EgressProxy::new(root.join("sockets"), None)
        .unwrap()
        .with_netbridge("/usr/bin/true".into())
        .with_limits(EgressLimits {
            max_connections: 1,
            ..EgressLimits::default()
        });
    // The first tunnel holds the only slot while it is open.
    let server = echo_server().await;
    let proxy = proxy.with_resolver_override("one.test.example", vec![server.ip()]);
    let rule = format!("one.test.example:{}", server.port());
    let lease = proxy
        .open(request(NetworkMode::Proxy, &[&rule], vec![]))
        .unwrap();
    let dir = lease.bridge().unwrap().socket_dir.clone();
    let head = format!("CONNECT {rule} HTTP/1.1\r\n\r\n");
    let (status, _held) = connect(&dir, &head).await;
    assert!(status.contains("200"), "{status}");
    let (status, _) = connect(&dir, &head).await;
    assert!(status.contains("429"), "{status}");
    let _ = std::fs::remove_dir_all(&root);
}

async fn http(dir: &Path, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut s = UnixStream::connect(dir.join(MCP_SOCKET)).await.unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:8080\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let status = out[9..12].parse().unwrap();
    let body = out.split("\r\n\r\n").nth(1).unwrap_or_default().to_string();
    (status, body)
}

#[tokio::test]
async fn mcp_socket_serves_only_its_own_tokens() {
    let root = scratch("mcp");
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(DeviceInfo::new())];
    let server = McpServer::new(ToolRegistry::new(tools, None));
    let mine = server
        .open_session(7, TaskContext::default(), None)
        .unwrap();
    let other = server
        .open_session(8, TaskContext::default(), None)
        .unwrap();
    let proxy = EgressProxy::new(root.join("sockets"), None)
        .unwrap()
        .with_netbridge("/usr/bin/true".into())
        .with_mcp(server.clone(), "127.0.0.1:8080".parse().unwrap());
    // `none`: no egress socket, only MCP.
    let lease = proxy
        .open(request(NetworkMode::None, &[], vec![mine.clone()]))
        .unwrap();
    let bridge = lease.bridge().unwrap().clone();
    assert_eq!(bridge.proxy, None);
    assert_eq!(
        bridge.ports,
        [("127.0.0.1:8080".parse().unwrap(), MCP_SOCKET.to_string())]
    );
    assert!(!bridge.socket_dir.join(EGRESS_SOCKET).exists());
    let dir = bridge.socket_dir;

    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string();
    let (status, body) = http(&dir, "POST", &format!("/mcp/{mine}"), &list).await;
    assert_eq!(status, 200);
    let reply: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(reply["result"]["tools"][0]["name"], "device_info");
    // Another task's token, the rest of the lead agent's API, other methods: refused.
    assert_eq!(
        http(&dir, "POST", &format!("/mcp/{other}"), &list).await.0,
        404
    );
    assert_eq!(http(&dir, "POST", "/api/lead/messages", "{}").await.0, 404);
    assert_eq!(
        http(&dir, "POST", "/api/confirmations/1/approve", "")
            .await
            .0,
        404
    );
    assert_eq!(http(&dir, "GET", &format!("/mcp/{mine}"), "").await.0, 405);
    assert_eq!(
        http(&dir, "POST", &format!("/mcp/{mine}"), "nope").await.0,
        400
    );
    // A closed session is gone here too.
    server.close_session(&mine);
    assert_eq!(
        http(&dir, "POST", &format!("/mcp/{mine}"), &list).await.0,
        404
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn proxy_port_moves_aside_for_the_mcp_port_and_needs_a_bridge() {
    let root = scratch("ports");
    let server = McpServer::new(ToolRegistry::new(Vec::new(), None));
    let proxy = EgressProxy::new(root.join("sockets"), None)
        .unwrap()
        .with_netbridge("/usr/bin/true".into())
        .with_mcp(server, "127.0.0.1:3128".parse().unwrap());
    let lease = proxy
        .open(request(NetworkMode::Proxy, &[], vec!["t".into()]))
        .unwrap();
    assert_eq!(
        lease.bridge().unwrap().proxy,
        Some("127.0.0.1:3129".parse().unwrap())
    );
    // `none` without MCP tokens needs no bridge at all.
    let lease = proxy.open(request(NetworkMode::None, &[], vec![])).unwrap();
    assert!(lease.bridge().is_none());

    let mut no_bridge = EgressProxy::new(root.join("sockets2"), None).unwrap();
    Arc::make_mut(&mut no_bridge.inner).netbridge = None;
    assert!(matches!(
        no_bridge.open(request(NetworkMode::Proxy, &[], vec![])),
        Err(EgressError::NoNetbridge)
    ));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn startup_keeps_live_leases_and_rejects_long_paths() {
    let root = scratch("stale");
    let sockets = root.join("s");
    let first = EgressProxy::new(sockets.clone(), None)
        .unwrap()
        .with_netbridge("/usr/bin/true".into());
    let live = first
        .open(request(NetworkMode::Proxy, &[], vec![]))
        .unwrap();
    let live_dir = live.bridge().unwrap().socket_dir.clone();
    let stale = sockets.join("0123456789abcdef");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join(EGRESS_SOCKET), "").unwrap();
    // A second lead agent on the same directory removes only the dead lease.
    let _second = EgressProxy::new(sockets.clone(), None).unwrap();
    assert!(live_dir.exists());
    assert!(!stale.exists());

    let long = root.join("x".repeat(100));
    let err = EgressProxy::new(long, None).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    drop(live);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn elf_interpreter_is_read_from_the_binary() {
    // /usr/bin/true is a dynamically linked ELF on Fedora; its loader is under /usr or /lib64.
    // (NixOS and the Nix build sandbox have no /usr/bin/true.)
    if Path::new("/usr/bin/true").is_file() {
        let interp = elf_interpreter(Path::new("/usr/bin/true")).unwrap();
        assert!(interp.to_string_lossy().contains("ld-linux"), "{interp:?}");
        assert!(runtime_dirs(Path::new("/usr/bin/true")).is_empty());
    }
    assert_eq!(elf_interpreter(Path::new("/etc/hostname")), None);
    // This test binary: from the Nix dev shell its loader is in /nix/store.
    let me = std::env::current_exe().unwrap();
    let dirs = runtime_dirs(&me);
    let nix = elf_interpreter(&me).is_some_and(|i| i.starts_with("/nix/store"));
    assert_eq!(dirs.is_empty(), !nix);
}

#[test]
fn scripts_from_the_nix_store_need_it() {
    let dir = std::env::temp_dir().join(format!("melissa-shebang-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let nix = dir.join("nix");
    std::fs::write(&nix, "#!/nix/store/abc-bash/bin/bash -e\nexec x\n").unwrap();
    let env = dir.join("env");
    std::fs::write(&env, "#! /usr/bin/env node\n").unwrap();
    assert!(needs_nix_store(&nix));
    assert_eq!(shebang(&env), Some(PathBuf::from("/usr/bin/env")));
    assert!(!needs_nix_store(&env));
    assert!(!needs_nix_store(&dir.join("missing")));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn default_allowlists_parse() {
    for cli in ["claude", "grok", "opencode", "/x/y/claude"] {
        let list = default_allow(cli);
        assert!(!list.is_empty(), "{cli}");
        for rule in list {
            HostRule::parse(&rule).unwrap();
        }
    }
    assert!(default_allow("sh").is_empty());
}

/// A destructive tool, so every call waits in the confirmation inbox.
struct NeedsApproval;
impl Tool for NeedsApproval {
    fn spec(&self) -> crate::tools::ToolSpec {
        crate::tools::ToolSpec {
            name: "remove_everything",
            description: "test",
            labels: crate::tools::Labels {
                effect: crate::tools::Effect::Destructive,
                ..crate::tools::Labels::READ_ONLY
            },
            params: json!({"type": "object"}),
        }
    }
    fn call<'a>(
        &'a self,
        _args: Value,
    ) -> crate::BoxFuture<'a, Result<Value, crate::tools::ToolError>> {
        Box::pin(async { Ok(json!({"removed": true})) })
    }
}

/// An agent's MCP client gives up on a call that waits for a confirmation (Claude Code's
/// HTTP client does so after 300 s) and closes the connection: the confirmation must leave
/// the inbox, or a later approval would run an action nobody is waiting for.
#[tokio::test]
async fn mcp_call_abandoned_by_the_client_withdraws_its_confirmation() {
    let root = scratch("mcp-abandon");
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(NeedsApproval)];
    let server = McpServer::new(ToolRegistry::new(tools, None));
    let token = server
        .open_session(7, TaskContext::default(), None)
        .unwrap();
    let proxy = EgressProxy::new(root.join("sockets"), None)
        .unwrap()
        .with_netbridge("/usr/bin/true".into())
        .with_mcp(server.clone(), "127.0.0.1:8080".parse().unwrap());
    let lease = proxy
        .open(request(NetworkMode::None, &[], vec![token.clone()]))
        .unwrap();
    let dir = lease.bridge().unwrap().socket_dir.clone();
    let inbox = server.registry().confirmations().clone();

    for half_close in [false, true] {
        let call = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "remove_everything", "arguments": {}}})
        .to_string();
        let mut s = UnixStream::connect(dir.join(MCP_SOCKET)).await.unwrap();
        let req = format!(
            "POST /mcp/{token} HTTP/1.1\r\nHost: 127.0.0.1:8080\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{call}",
            call.len()
        );
        s.write_all(req.as_bytes()).await.unwrap();
        for _ in 0..200 {
            if !inbox.pending().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(inbox.pending().len(), 1, "the call waits for a human");
        // The bridge passes a client's close on as a half-close (copy_bidirectional); a
        // killed bridge closes the socket completely.
        if half_close {
            s.shutdown().await.unwrap();
        } else {
            drop(s);
        }
        for _ in 0..200 {
            if inbox.pending().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            inbox.pending().is_empty(),
            "half_close={half_close}: confirmation still pending after the client left"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}
