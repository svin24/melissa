//! Proxy and MCP socket behaviour, talking to the lease's Unix sockets directly.

use std::net::Ipv4Addr;

use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, UnixStream},
};

use super::*;
use crate::test_support::fresh_path;
use crate::tools::{DeviceInfo, TaskContext, Tool, ToolRegistry};

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
        deny: Vec::new(),
        mcp_tokens: tokens,
    }
}

#[tokio::test]
async fn allowlisted_hosts_tunnel_and_everything_is_audited() {
    let root = fresh_path("proxy");
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
    let root = fresh_path("repeat");
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
    let root = fresh_path("learn");
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
    let root = fresh_path("limit");
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
    let root = fresh_path("mcp");
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
    let root = fresh_path("ports");
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
    let root = fresh_path("stale");
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
    let dir = crate::test_support::fresh_path("shebang");
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
fn default_allow_and_deny_lists_parse() {
    // The deny lists (telemetry the CLIs would otherwise send) by the program's file name.
    assert_eq!(default_deny("/opt/x/grok").len(), 2);
    assert!(allowed(
        &default_deny("claude"),
        "http-intake.logs.us5.datadoghq.com",
        443
    ));
    assert!(default_deny("custom-cli").is_empty());
    for cli in [
        "claude",
        "grok",
        "opencode",
        "gemini",
        "codex-acp",
        "muse",
        "/x/y/claude",
    ] {
        let list = default_allow(cli);
        assert!(!list.is_empty(), "{cli}");
        for rule in list {
            HostRule::parse(&rule).unwrap();
        }
    }
    assert!(default_allow("sh").is_empty());
}

#[test]
fn new_clis_reach_their_apis_but_not_their_telemetry_or_updates() {
    let allowed = |cli: &str, host: &str| {
        default_allow(cli)
            .iter()
            .any(|r| HostRule::parse(r).unwrap().matches(host, 443))
    };
    let denied = |cli: &str, host: &str| default_deny(cli).iter().any(|r| r.matches(host, 443));
    assert!(allowed("gemini", "generativelanguage.googleapis.com"));
    assert!(allowed(
        "/home/m/.local/bin/gemini",
        "cloudcode-pa.googleapis.com"
    ));
    assert!(!allowed("gemini", "play.googleapis.com"));
    assert!(denied("gemini", "play.googleapis.com"));
    assert!(denied("gemini", "registry.npmjs.org"));
    assert!(allowed("codex-acp", "api.openai.com"));
    assert!(allowed("codex-acp", "chatgpt.com"));
    assert!(!allowed("codex-acp", "ab.chatgpt.com"));
    assert!(denied("codex-acp", "ab.chatgpt.com"));
    assert!(denied("codex-acp", "api.github.com"));
    assert!(denied("codex-acp", "files.openai.com"));
    assert!(allowed("muse", "api.meta.ai"));
    assert!(allowed("muse", "auth.meta.com"));
    assert!(denied("muse", "lookaside.facebook.com"));
    // Each list is the program's own: Codex's hosts are not Gemini's.
    assert!(!allowed("gemini", "api.openai.com"));
    assert!(!allowed("codex", "api.openai.com"));
}

/// An agent's MCP client gives up on a call that waits for a confirmation (Claude Code's
/// HTTP client does so after 300 s) and closes the connection: the confirmation must leave
/// the inbox, or a later approval would run an action nobody is waiting for.
#[tokio::test]
async fn mcp_call_abandoned_by_the_client_withdraws_its_confirmation() {
    let root = fresh_path("mcp-abandon");
    // A destructive tool, so every call waits in the confirmation inbox.
    let tools: Vec<Arc<dyn Tool>> = vec![crate::test_support::TestTool::new("remove_everything")
        .effect(crate::tools::Effect::Destructive)
        .reply(|_| json!({"removed": true}))
        .arc()];
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

/// Send a request head; return the whole response (the proxy closes after a refusal).
async fn refusal(dir: &Path, head: &str) -> String {
    let (status, mut s) = connect(dir, head).await;
    let mut body = String::new();
    s.read_to_string(&mut body).await.unwrap();
    format!("{status}\n{body}")
}

/// A proxy with web reading on and a lease for `tokens`.
fn web_proxy(root: &Path, audit: &AuditLog, web: WebRead) -> EgressProxy {
    EgressProxy::new(root.join("sockets"), Some(audit.clone()))
        .unwrap()
        .with_netbridge("/usr/bin/true".into())
        .with_web_read(web)
}

#[tokio::test]
async fn web_reads_reach_any_public_host_audited_capped_and_tainting() {
    let root = fresh_path("web");
    let audit = AuditLog::new(root.join("audit.jsonl"));
    let server = echo_server().await;
    let port = server.port();
    let mcp = McpServer::new(ToolRegistry::new(Vec::new(), None));
    let token = mcp.open_session(7, TaskContext::default(), None).unwrap();
    let web = WebRead {
        https_ports: vec![port],
        max_upload: 100,
        ..WebRead::on()
    };
    // The override stands in for a public address (the echo server is on loopback).
    let proxy = web_proxy(&root, &audit, web)
        .with_mcp(mcp.clone(), "127.0.0.1:8080".parse().unwrap())
        .with_resolver_override("docs.test.example", vec![server.ip()]);
    let lease = proxy
        .open(request(NetworkMode::Proxy, &[], vec![token.clone()]))
        .unwrap();
    let dir = lease.bridge().unwrap().socket_dir.clone();
    assert!(!mcp.session_context(&token).unwrap().tainted);

    let (status, mut s) = connect(
        &dir,
        &format!("CONNECT docs.test.example:{port} HTTP/1.1\r\n\r\n"),
    )
    .await;
    assert_eq!(status, "HTTP/1.1 200 Connection established");
    let mut hello = [0u8; 5];
    s.read_exact(&mut hello).await.unwrap();
    // Reading taints the task at once, naming the host.
    assert!(mcp.session_context(&token).unwrap().tainted);
    assert_eq!(
        mcp.taint_source(&token),
        Some(crate::mcp::TaintSource {
            tool: Some("web".into()),
            subject: Some("docs.test.example".into()),
        })
    );
    // An upload beyond the cap closes the tunnel; nothing of it is echoed.
    s.write_all(&[b'x'; 300]).await.unwrap();
    let mut rest = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut rest)).await;
    assert!(rest.is_empty(), "{rest:?}");

    // Wait for the close record.
    let mut lines = Vec::new();
    for _ in 0..100 {
        lines = audit_lines(&audit);
        if lines.iter().any(|l| l["action"] == "egress.web") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let find = |action: &str| {
        lines
            .iter()
            .find(|l| l["action"] == action)
            .map(|l| l["detail"].as_str().unwrap().to_string())
            .unwrap_or_default()
    };
    assert_eq!(
        find("egress.allow"),
        format!("docs.test.example:{port} -> 127.0.0.1:{port} (web read)")
    );
    let closed = find("egress.web");
    assert!(
        closed.starts_with(&format!(
            "CONNECT docs.test.example:{port}: 0 B up, 5 B down"
        )),
        "{closed}"
    );
    assert!(
        closed.ends_with("closed: upload limit of 100 bytes reached (web reading is read-only)"),
        "{closed}"
    );
    drop(lease);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn web_reads_refuse_private_addresses_local_names_and_other_ports() {
    let root = fresh_path("ssrf");
    let audit = AuditLog::new(root.join("audit.jsonl"));
    let mut proxy = web_proxy(&root, &audit, WebRead::on());
    // Names whose DNS answers point inside: LAN, loopback (also hidden in IPv6), cloud
    // metadata, the tailnet, k3s pods and services, the sandbox's own DNS placeholder.
    let inside: &[(&str, &str)] = &[
        ("lan.test.example", "10.0.0.1"),
        ("loop.test.example", "127.0.0.1"),
        ("mapped.test.example", "::ffff:127.0.0.1"),
        ("nat64.test.example", "64:ff9b::a00:1"),
        ("meta.test.example", "169.254.169.254"),
        ("tail.test.example", "100.100.100.100"),
        ("tailv6.test.example", "fd7a:115c:a1e0::1"),
        ("pod.test.example", "10.42.0.5"),
        ("svc.test.example", "10.43.0.1"),
        ("hole.test.example", "192.88.99.53"),
    ];
    for (name, ip) in inside {
        proxy = proxy.with_resolver_answer(name, vec![ip.parse().unwrap()]);
    }
    // Rebinding: every answer is private, and only checked answers are ever connected to.
    proxy = proxy.with_resolver_answer(
        "rebind.test.example",
        vec!["192.168.1.1".parse().unwrap(), "::1".parse().unwrap()],
    );
    let lease = proxy
        .open(request(NetworkMode::Proxy, &[], vec![]))
        .unwrap();
    let dir = lease.bridge().unwrap().socket_dir.clone();

    for (name, ip) in inside
        .iter()
        .chain(&[("rebind.test.example", "192.168.1.1")])
    {
        let r = refusal(&dir, &format!("CONNECT {name}:443 HTTP/1.1\r\n\r\n")).await;
        assert!(r.starts_with("HTTP/1.1 403"), "{name}: {r}");
        assert!(
            r.contains(&format!(
                "blocked by Melissa's network policy: {name}:443: resolves only to non-public addresses ({ip}"
            )),
            "{name}: {r}"
        );
    }
    // Address literals are judged the same way.
    for target in [
        "127.0.0.1:443",
        "[::1]:443",
        "[::ffff:10.0.0.1]:443",
        "169.254.169.254:443",
    ] {
        let r = refusal(&dir, &format!("CONNECT {target} HTTP/1.1\r\n\r\n")).await;
        assert!(
            r.starts_with("HTTP/1.1 403") && r.contains("non-public addresses"),
            "{target}: {r}"
        );
    }
    // Local names: refused before any lookup.
    for name in ["localhost", "printer", "nas.local", "router.lan"] {
        let r = refusal(&dir, &format!("CONNECT {name}:443 HTTP/1.1\r\n\r\n")).await;
        assert!(
            r.contains(&format!(
                "blocked by Melissa's network policy: {name}:443: a local name; web reading reaches public hosts only"
            )),
            "{name}: {r}"
        );
    }
    // Other ports (SMTP, SSH, ...) are not web reading.
    for port in [25, 22, 8443] {
        let r = refusal(
            &dir,
            &format!("CONNECT example.com:{port} HTTP/1.1\r\n\r\n"),
        )
        .await;
        assert!(
            r.contains(&format!(
                "example.com:{port}: port {port} is not open for web reading (HTTPS: 443, HTTP: 80)"
            )),
            "{port}: {r}"
        );
    }
    drop(lease);
    let _ = std::fs::remove_dir_all(&root);
}

/// An HTTP server on loopback that records the request head and answers with `body`.
async fn http_origin(body: &'static str) -> (SocketAddr, tokio::sync::mpsc::Receiver<String>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    tokio::spawn(async move {
        loop {
            let (mut s, _) = listener.accept().await.unwrap();
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                while head_end(&buf).is_none() {
                    let n = s.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let _ = tx.send(String::from_utf8_lossy(&buf).into_owned()).await;
                let answer = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(answer.as_bytes()).await;
            });
        }
    });
    (addr, rx)
}

#[tokio::test]
async fn plain_http_is_forwarded_read_only() {
    let root = fresh_path("http");
    let audit = AuditLog::new(root.join("audit.jsonl"));
    let (origin, mut heads) = http_origin("<title>Hi</title>").await;
    let port = origin.port();
    let web = WebRead {
        http_ports: vec![port],
        ..WebRead::on()
    };
    let proxy = web_proxy(&root, &audit, web)
        .with_resolver_override("site.test.example", vec![origin.ip()]);
    let lease = proxy
        .open(request(NetworkMode::Proxy, &[], vec![]))
        .unwrap();
    let dir = lease.bridge().unwrap().socket_dir.clone();

    let r = refusal(
        &dir,
        &format!(
            "GET http://site.test.example:{port}/page?q=1 HTTP/1.1\r\nHost: site.test.example\r\nProxy-Connection: keep-alive\r\nAccept: */*\r\n\r\n"
        ),
    )
    .await;
    assert!(r.starts_with("HTTP/1.1 200 OK"), "{r}");
    assert!(r.ends_with("<title>Hi</title>"), "{r}");
    let head = heads.recv().await.unwrap();
    assert_eq!(
        head,
        format!(
            "GET /page?q=1 HTTP/1.1\r\nHost: site.test.example:{port}\r\nConnection: close\r\nAccept: */*\r\n\r\n"
        )
    );

    for (req, want) in [
        (
            format!("POST http://site.test.example:{port}/ HTTP/1.1\r\nContent-Length: 2\r\n\r\nhi"),
            "only GET and HEAD are allowed (web reading is read-only), not POST",
        ),
        (
            format!("GET http://site.test.example:{port}/ HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"),
            "requests with a body are not allowed",
        ),
        (
            "GET http://site.test.example:8080/ HTTP/1.1\r\n\r\n".to_string(),
            "port 8080 is not open for web reading",
        ),
        (
            format!("GET http://10.1.2.3:{port}/ HTTP/1.1\r\n\r\n"),
            "resolves only to non-public addresses (10.1.2.3)",
        ),
    ] {
        let r = refusal(&dir, &req).await;
        assert!(r.starts_with("HTTP/1.1 403"), "{req:?}: {r}");
        assert!(r.contains("blocked by Melissa's network policy: "), "{r}");
        assert!(r.contains(want), "{req:?}: {r}");
    }

    let mut web = String::new();
    for _ in 0..100 {
        if let Some(l) = audit_lines(&audit)
            .into_iter()
            .find(|l| l["action"] == "egress.web")
        {
            web = l["detail"].as_str().unwrap().to_string();
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        web.starts_with(&format!("GET http://site.test.example:{port}/page?q=1: ")),
        "{web}"
    );
    drop(lease);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn background_hosts_and_web_reading_off() {
    let root = fresh_path("weboff");
    let audit = AuditLog::new(root.join("audit.jsonl"));
    // On: the CLI's own background hosts stay denied (no lookup, no taint).
    let proxy = web_proxy(&root, &audit, WebRead::on());
    let mut req = request(NetworkMode::Proxy, &[], vec![]);
    req.deny = default_deny("/usr/bin/grok");
    let lease = proxy.open(req).unwrap();
    let dir = lease.bridge().unwrap().socket_dir.clone();
    let r = refusal(&dir, "CONNECT api.mixpanel.com:443 HTTP/1.1\r\n\r\n").await;
    assert!(
        r.contains(
            "api.mixpanel.com:443: not in the allowlist (background traffic of the agent program)"
        ),
        "{r}"
    );
    drop(lease);

    // Off: only the allowlist, and plain HTTP is refused with a hint.
    let proxy = web_proxy(&root, &audit, WebRead::default());
    let lease = proxy
        .open(request(NetworkMode::Proxy, &[], vec![]))
        .unwrap();
    let dir = lease.bridge().unwrap().socket_dir.clone();
    let r = refusal(&dir, "CONNECT example.com:443 HTTP/1.1\r\n\r\n").await;
    assert!(
        r.contains("blocked by Melissa's network policy: example.com:443: not in the allowlist"),
        "{r}"
    );
    let r = refusal(&dir, "GET http://example.com/ HTTP/1.1\r\n\r\n").await;
    assert!(r.contains("web reading is off"), "{r}");
    drop(lease);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn proxied_sandboxes_get_a_dns_handoff_and_resolv_conf() {
    let root = fresh_path("dnsfiles");
    let proxy = EgressProxy::new(root.join("sockets"), None)
        .unwrap()
        .with_netbridge("/usr/bin/true".into());
    let lease = proxy
        .open(request(NetworkMode::Proxy, &[], vec![]))
        .unwrap();
    let bridge = lease.bridge().unwrap().clone();
    assert_eq!(bridge.dns.as_deref(), Some(DNS_SOCKET));
    assert!(bridge.socket_dir.join(DNS_SOCKET).exists());
    assert_eq!(
        std::fs::read_to_string(bridge.socket_dir.join(RESOLV_CONF)).unwrap(),
        "nameserver 127.0.0.1\noptions attempts:2 timeout:2\n"
    );
    drop(lease);
    // `none`: no proxy, no DNS.
    let mcp = McpServer::new(ToolRegistry::new(Vec::new(), None));
    let token = mcp.open_session(7, TaskContext::default(), None).unwrap();
    let proxy = proxy.with_mcp(mcp, "127.0.0.1:8080".parse().unwrap());
    let lease = proxy
        .open(request(NetworkMode::None, &[], vec![token]))
        .unwrap();
    assert_eq!(lease.bridge().unwrap().dns, None);
    drop(lease);
    let _ = std::fs::remove_dir_all(&root);
}
