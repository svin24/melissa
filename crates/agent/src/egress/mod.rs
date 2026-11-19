//! Deny-by-default network egress for sandboxed agents.
//!
//! A sandboxed agent runs in its own network namespace (`bwrap --unshare-net`) with only
//! loopback. Inside, `melissa-netbridge` listens on loopback ports and forwards every TCP
//! connection over a Unix socket that is bound into the sandbox; the queen side of each
//! socket is served here:
//!
//! - `egress.sock`: an HTTP `CONNECT` proxy (no TLS interception). A tunnel is opened only
//!   to hosts on the backend's allowlist (default port 443), resolved by the queen, and only
//!   to public addresses (no loopback, LAN, link-local, CGNAT/tailnet, multicast; checked
//!   after resolution, so DNS rebinding cannot reach the LAN). Every decision is audited
//!   (`egress.allow` / `egress.deny` / `egress.error`, host and port, never payload).
//! - `mcp.sock`: Melissa's MCP endpoint, and nothing else: only `POST /mcp/<token>` for the
//!   session tokens given to this sandbox.
//!
//! Each sandbox gets its own socket directory ([`EgressLease`]), so the socket an agent
//! connects through identifies it; there is nothing to spoof. Dropping the lease closes the
//! sockets and every open tunnel.

mod addr;
mod mcp;
mod relay;
mod rules;

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UnixListener, UnixStream},
    sync::Semaphore,
    task::JoinSet,
};

pub use addr::is_public;
pub use rules::{
    allowed, head_end, normalize_host, parse_request, BadRequest, BadRule, HostRule, ProxyRequest,
    DEFAULT_PORT,
};

use crate::{mcp::McpServer, sandbox::Bridge, AuditLog, AuditRecord, TaskId};

/// Socket file names inside a lease directory (and inside the sandbox).
pub const EGRESS_SOCKET: &str = "egress.sock";
pub const MCP_SOCKET: &str = "mcp.sock";
/// Name of the bridge binary, installed next to `melissa-queen`.
pub const NETBRIDGE: &str = "melissa-netbridge";
/// Loopback port of the proxy inside the sandbox (the MCP port is the queen's own).
const PROXY_PORT: u16 = 3128;

/// How a sandboxed agent reaches the network (`network = "..."` in `[backend.sandbox]`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkMode {
    /// Own network namespace; only the allowlisted hosts through Melissa's proxy, and
    /// Melissa's MCP endpoint. The default.
    #[default]
    Proxy,
    /// Like `proxy`, but every public host is allowed and logged (to find out what a CLI
    /// needs). Private addresses stay refused.
    Learn,
    /// Own network namespace with no egress at all; Melissa's MCP endpoint only.
    None,
    /// The host's network, unfiltered (the old behaviour; explicit opt-out).
    Host,
}

impl NetworkMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proxy => "proxy",
            Self::Learn => "learn",
            Self::None => "none",
            Self::Host => "host",
        }
    }
}

/// Built-in allowlist for an agent CLI, chosen by the command's file name. Found
/// empirically with `network = "learn"`; see docs/agent-skills/agent-egress.md for what
/// each CLI contacts and what is deliberately left out (telemetry, auto-update).
pub fn default_allow(command: &str) -> Vec<String> {
    let name = Path::new(command)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let hosts: &[&str] = match name {
        // Claude Code: the Messages API (also its own metrics/event endpoints) and the OAuth
        // token endpoint (`platform.claude.com/v1/oauth/token`). Denied: Datadog log intake.
        "claude" => &["api.anthropic.com", "platform.claude.com"],
        // Grok CLI: the subscription chat proxy, the xAI API, and the login issuer
        // (`auth.x.ai`, token refresh). Denied: grok.com, Mixpanel analytics.
        "grok" => &["cli-chat-proxy.grok.com", "api.x.ai", "auth.x.ai"],
        // OpenCode: its hosted gateway (the free default models) and model catalogue.
        "opencode" => &["opencode.ai", "models.opencode.ai"],
        _ => &[],
    };
    hosts.iter().map(|h| (*h).to_string()).collect()
}

/// Limits per sandbox and per connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressLimits {
    /// Open tunnels at once, per sandbox.
    pub max_connections: usize,
    /// Time to send the request head.
    pub head_timeout: Duration,
    /// Time to connect to the upstream host (all its addresses together).
    pub connect_timeout: Duration,
    /// A tunnel with no bytes in either direction for this long is closed.
    pub idle_timeout: Duration,
    /// Bytes per tunnel (both directions together); the tunnel is closed beyond.
    pub max_bytes: u64,
}

impl Default for EgressLimits {
    fn default() -> Self {
        Self {
            max_connections: 64,
            head_timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(600),
            max_bytes: 256 * 1024 * 1024,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EgressError {
    #[error("egress proxy i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("`{NETBRIDGE}` was not found next to the queen binary, in /usr/bin or on PATH")]
    NoNetbridge,
}

#[derive(Clone)]
struct ProxyInner {
    dir: PathBuf,
    audit: Option<AuditLog>,
    mcp: Option<(McpServer, SocketAddr)>,
    netbridge: Option<PathBuf>,
    limits: EgressLimits,
    /// Fixed name → address mappings that bypass DNS and the public-address check.
    overrides: HashMap<String, Vec<IpAddr>>,
}

/// The queen side of every sandbox's network. Cheap to clone.
#[derive(Clone)]
pub struct EgressProxy {
    inner: Arc<ProxyInner>,
}

/// What one sandbox may reach.
#[derive(Debug, Clone)]
pub struct EgressRequest {
    pub backend: String,
    pub task: TaskId,
    /// `Proxy`, `Learn` or `None` (`Host` needs no lease).
    pub mode: NetworkMode,
    pub allow: Vec<HostRule>,
    /// MCP session tokens this sandbox may use (empty: no MCP socket).
    pub mcp_tokens: Vec<String>,
}

/// One sandbox's sockets. Dropping it closes them and every tunnel through them.
pub struct EgressLease {
    dir: PathBuf,
    bridge: Option<Bridge>,
    _tasks: JoinSet<()>,
}

impl EgressLease {
    /// The bridge to run inside the sandbox (`None`: nothing to bridge, no network at all).
    pub fn bridge(&self) -> Option<&Bridge> {
        self.bridge.as_ref()
    }
}

impl Drop for EgressLease {
    fn drop(&mut self) {
        // The JoinSet aborts the accept loops (and with them every tunnel) when dropped.
        if let Err(e) = std::fs::remove_dir_all(&self.dir) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(dir = %self.dir.display(), "cannot remove egress sockets: {e}");
            }
        }
    }
}

impl std::fmt::Debug for EgressProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EgressProxy")
            .field("dir", &self.inner.dir)
            .field("netbridge", &self.inner.netbridge)
            .finish_non_exhaustive()
    }
}

impl EgressProxy {
    /// Sockets live in per-sandbox directories under `dir` (created 0700; leftovers from
    /// an earlier run are removed). Unix socket paths are limited to 107 bytes, so keep
    /// `dir` short (e.g. `$XDG_RUNTIME_DIR/melissa/egress`).
    pub fn new(dir: PathBuf, audit: Option<AuditLog>) -> std::io::Result<Self> {
        // `<dir>/<16 hex>/egress.sock` must fit in sockaddr_un (108 bytes with the NUL).
        let longest = dir.as_os_str().len() + 1 + 16 + 1 + EGRESS_SOCKET.len();
        if longest > 107 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "egress socket directory {} is too long for Unix socket paths \
                     ({longest} > 107 bytes); use a shorter XDG_RUNTIME_DIR",
                    dir.display()
                ),
            ));
        }
        create_private_dir(&dir)?;
        remove_stale_leases(&dir);
        Ok(Self {
            inner: Arc::new(ProxyInner {
                dir,
                audit,
                mcp: None,
                netbridge: find_netbridge(),
                limits: EgressLimits::default(),
                overrides: HashMap::new(),
            }),
        })
    }

    fn inner_mut(&mut self) -> &mut ProxyInner {
        // Builders run before the proxy is shared; if it already is, this copies it.
        Arc::make_mut(&mut self.inner)
    }

    /// Serve Melissa's MCP endpoint into sandboxes, at `addr` inside them (the queen's own
    /// listen address, so MCP URLs are the same inside and outside).
    pub fn with_mcp(mut self, server: McpServer, addr: SocketAddr) -> Self {
        self.inner_mut().mcp = Some((server, addr));
        self
    }

    /// Use this `melissa-netbridge` binary.
    pub fn with_netbridge(mut self, path: PathBuf) -> Self {
        self.inner_mut().netbridge = Some(path);
        self
    }

    pub fn with_limits(mut self, limits: EgressLimits) -> Self {
        self.inner_mut().limits = limits;
        self
    }

    /// Resolve `host` to `addrs` without DNS and without the public-address check. For
    /// tests (a local server under an allowlisted name) and deliberate local services.
    pub fn with_resolver_override(mut self, host: &str, addrs: Vec<IpAddr>) -> Self {
        if let Some(host) = normalize_host(host) {
            self.inner_mut().overrides.insert(host, addrs);
        }
        self
    }

    pub fn netbridge(&self) -> Option<&Path> {
        self.inner.netbridge.as_deref()
    }

    /// Create the sockets for one sandbox and start serving them.
    pub fn open(&self, req: EgressRequest) -> Result<EgressLease, EgressError> {
        let dir = self.inner.dir.join(random_hex(8)?);
        create_private_dir(&dir)?;
        let mut lease = EgressLease {
            dir: dir.clone(),
            bridge: None,
            _tasks: JoinSet::new(),
        };
        let mut ports = Vec::new();
        let mut proxy = None;
        let ctx = Arc::new(LeaseCtx::new(self.inner.clone(), req));

        let mcp = match (&self.inner.mcp, ctx.req.mcp_tokens.is_empty()) {
            (Some((server, addr)), false) => Some((server.clone(), *addr)),
            _ => None,
        };
        if matches!(ctx.req.mode, NetworkMode::Proxy | NetworkMode::Learn) {
            let mcp_port = mcp.as_ref().map(|(_, a)| a.port());
            let port = if mcp_port == Some(PROXY_PORT) {
                PROXY_PORT + 1
            } else {
                PROXY_PORT
            };
            let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
            let listener = UnixListener::bind(dir.join(EGRESS_SOCKET))?;
            lease._tasks.spawn(serve_proxy(listener, ctx.clone()));
            ports.push((addr, EGRESS_SOCKET.to_string()));
            proxy = Some(addr);
        }
        if let Some((server, addr)) = mcp {
            let listener = UnixListener::bind(dir.join(MCP_SOCKET))?;
            lease
                ._tasks
                .spawn(mcp::serve(listener, server, ctx.req.mcp_tokens.clone()));
            ports.push((addr, MCP_SOCKET.to_string()));
        }
        if !ports.is_empty() {
            let netbridge = self
                .inner
                .netbridge
                .clone()
                .ok_or(EgressError::NoNetbridge)?;
            let runtime = runtime_dirs(&netbridge);
            lease.bridge = Some(Bridge {
                netbridge,
                socket_dir: dir,
                ports,
                proxy,
                runtime,
            });
        }
        Ok(lease)
    }
}

/// Find `melissa-netbridge`: next to the running binary, then `/usr/bin`, then `PATH`.
pub fn find_netbridge() -> Option<PathBuf> {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(NETBRIDGE)));
    beside
        .into_iter()
        .chain(std::iter::once(Path::new("/usr/bin").join(NETBRIDGE)))
        .find(|p| p.is_file())
        .or_else(|| crate::cli::resolve_program(NETBRIDGE))
}

/// Remove lease directories left behind by a queen that died. A directory whose sockets
/// still accept connections belongs to a running queen (sharing the runtime dir) and stays.
fn remove_stale_leases(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for path in entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        let live = [EGRESS_SOCKET, MCP_SOCKET]
            .iter()
            .any(|s| std::os::unix::net::UnixStream::connect(path.join(s)).is_ok());
        if !live {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// Directories a binary needs besides `/usr`: `/nix/store` if its ELF interpreter lives
/// there (a Nix build). Image builds use `/usr/lib64` and need nothing.
fn runtime_dirs(binary: &Path) -> Vec<PathBuf> {
    if needs_nix_store(binary) {
        vec!["/nix/store".into()]
    } else {
        Vec::new()
    }
}

/// True if `binary` is an ELF file whose interpreter lives in `/nix/store`, or a script
/// whose `#!` line points there (Nix wrappers).
pub(crate) fn needs_nix_store(binary: &Path) -> bool {
    elf_interpreter(binary)
        .or_else(|| shebang(binary))
        .is_some_and(|interp| interp.starts_with("/nix/store/"))
}

/// The interpreter named on a script's `#!` line.
fn shebang(path: &Path) -> Option<PathBuf> {
    use std::io::Read;
    let mut head = [0u8; 256];
    let n = std::fs::File::open(path).ok()?.read(&mut head).ok()?;
    let line = head.get(..n)?.strip_prefix(b"#!")?;
    let line = line.split(|b| *b == b'\n').next()?;
    let interp = std::str::from_utf8(line).ok()?.split_whitespace().next()?;
    Some(PathBuf::from(interp))
}

/// The `PT_INTERP` path of a 64-bit little-endian ELF file, if any.
fn elf_interpreter(path: &Path) -> Option<PathBuf> {
    use std::io::{Read, Seek, SeekFrom};
    const PT_INTERP: u32 = 3;
    let mut f = std::fs::File::open(path).ok()?;
    let mut hdr = [0u8; 64];
    f.read_exact(&mut hdr).ok()?;
    // Magic, ELFCLASS64, ELFDATA2LSB.
    if hdr[..4] != *b"\x7fELF" || hdr[4] != 2 || hdr[5] != 1 {
        return None;
    }
    let u16_at = |b: &[u8], o: usize| Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?));
    let u32_at = |b: &[u8], o: usize| Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?));
    let u64_at = |b: &[u8], o: usize| Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?));
    let phoff = u64_at(&hdr, 0x20)?;
    let phentsize = u64::from(u16_at(&hdr, 0x36)?);
    let phnum = u16_at(&hdr, 0x38)?;
    let mut ph = vec![0u8; usize::try_from(phentsize).ok()?.max(0x28)];
    for i in 0..u64::from(phnum) {
        f.seek(SeekFrom::Start(
            phoff.checked_add(i.checked_mul(phentsize)?)?,
        ))
        .ok()?;
        f.read_exact(&mut ph).ok()?;
        if u32_at(&ph, 0)? == PT_INTERP {
            let (off, len) = (u64_at(&ph, 0x08)?, u64_at(&ph, 0x20)?);
            if len > 4096 {
                return None;
            }
            let mut buf = vec![0u8; usize::try_from(len).ok()?];
            f.seek(SeekFrom::Start(off)).ok()?;
            f.read_exact(&mut buf).ok()?;
            let s = String::from_utf8(buf).ok()?;
            return Some(PathBuf::from(s.trim_end_matches('\0')));
        }
    }
    None
}

/// Repeated identical denials within this window are counted, not written one by one.
const DENY_REPEAT_WINDOW: Duration = Duration::from_secs(60);

struct LeaseCtx {
    proxy: Arc<ProxyInner>,
    slots: Arc<Semaphore>,
    req: EgressRequest,
    /// Per denial detail: when it was last written, and how many repeats since.
    denials: std::sync::Mutex<HashMap<String, (std::time::Instant, u64)>>,
}

impl LeaseCtx {
    fn new(proxy: Arc<ProxyInner>, req: EgressRequest) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(proxy.limits.max_connections)),
            proxy,
            req,
            denials: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Audit a denial. CLIs retry denied telemetry hosts many times a minute; identical
    /// denials within [`DENY_REPEAT_WINDOW`] are counted and reported with the next record
    /// for the same detail (or when the sandbox ends), so every attempt is accounted for.
    fn deny(&self, detail: String) {
        let now = std::time::Instant::now();
        let repeats = {
            let mut seen = self.denials.lock().unwrap_or_else(|p| p.into_inner());
            match seen.get_mut(&detail) {
                Some((last, n)) if now.duration_since(*last) < DENY_REPEAT_WINDOW => {
                    *n += 1;
                    return;
                }
                Some((last, n)) => {
                    *last = now;
                    std::mem::take(n)
                }
                None => {
                    seen.insert(detail.clone(), (now, 0));
                    0
                }
            }
        };
        let detail = match repeats {
            0 => detail,
            n => format!("{detail} (+{n} repeats)"),
        };
        self.audit("egress.deny", detail);
    }

    fn audit(&self, action: &str, detail: String) {
        let level_deny = action != "egress.allow";
        if level_deny {
            tracing::info!(backend = %self.req.backend, task = self.req.task, "{action} {detail}");
        } else {
            tracing::debug!(backend = %self.req.backend, task = self.req.task, "{action} {detail}");
        }
        if let Some(log) = &self.proxy.audit {
            let record =
                AuditRecord::now(action, self.req.task, &self.req.backend, detail).by("agent");
            if let Err(e) = log.append(&record) {
                tracing::error!(path = %log.path().display(), "audit log write failed: {e}");
            }
        }
    }
}

impl Drop for LeaseCtx {
    // The sandbox is gone: report the denials that were only counted.
    fn drop(&mut self) {
        let seen = std::mem::take(self.denials.get_mut().unwrap_or_else(|p| p.into_inner()));
        let mut pending: Vec<_> = seen.into_iter().filter(|(_, (_, n))| *n > 0).collect();
        pending.sort();
        for (detail, (_, n)) in pending {
            self.audit("egress.deny", format!("{detail} (+{n} repeats)"));
        }
    }
}

async fn serve_proxy(listener: UnixListener, ctx: Arc<LeaseCtx>) {
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let ctx = ctx.clone();
                    conns.spawn(async move { handle_proxy(stream, ctx).await });
                }
                Err(e) => {
                    tracing::warn!("egress proxy accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
            // Reap finished connections so the set does not grow without bound.
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
        }
    }
}

/// Outcome of a request, before any bytes are relayed.
enum Decision {
    Tunnel(TcpStream, String),
    Refuse(&'static str, String),
}

async fn handle_proxy(mut stream: UnixStream, ctx: Arc<LeaseCtx>) {
    let limits = &ctx.proxy.limits;
    let (head, rest) = match tokio::time::timeout(limits.head_timeout, read_head(&mut stream)).await
    {
        Ok(Ok(parts)) => parts,
        Ok(Err(_)) | Err(_) => {
            let _ = respond(&mut stream, "400 Bad Request", "bad request").await;
            return;
        }
    };
    let (host, port) = match parse_request(&head) {
        Ok(ProxyRequest::Connect { host, port }) => (host, port),
        Ok(ProxyRequest::Other { method, target }) => {
            let target: String = target.chars().take(200).collect();
            ctx.deny(format!(
                "{method} {target}: only CONNECT (HTTPS) is proxied"
            ));
            let _ = respond(&mut stream, "403 Forbidden", "only CONNECT is allowed").await;
            return;
        }
        Err(_) => {
            let _ = respond(&mut stream, "400 Bad Request", "bad request").await;
            return;
        }
    };
    let Ok(_slot) = ctx.slots.clone().try_acquire_owned() else {
        let max = limits.max_connections;
        ctx.deny(format!(
            "{host}:{port}: too many open connections (max {max})"
        ));
        let _ = respond(&mut stream, "429 Too Many Requests", "too many connections").await;
        return;
    };
    match decide(&ctx, &host, port).await {
        Decision::Refuse(status, reason) => {
            ctx.deny(format!("{host}:{port}: {reason}"));
            let _ = respond(&mut stream, status, &reason).await;
        }
        Decision::Tunnel(mut upstream, detail) => {
            ctx.audit("egress.allow", format!("{host}:{port} -> {detail}"));
            if stream
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .await
                .is_err()
            {
                return;
            }
            if !rest.is_empty() && upstream.write_all(&rest).await.is_err() {
                return;
            }
            let (up, down) =
                relay::relay(stream, upstream, limits.idle_timeout, limits.max_bytes).await;
            tracing::debug!(backend = %ctx.req.backend, "egress {host}:{port} closed: {up} B up, {down} B down");
        }
    }
}

async fn decide(ctx: &LeaseCtx, host: &str, port: u16) -> Decision {
    let in_list = allowed(&ctx.req.allow, host, port);
    let learn = ctx.req.mode == NetworkMode::Learn;
    if !in_list && !learn {
        return Decision::Refuse("403 Forbidden", "not in the allowlist".into());
    }
    let note = match (learn, in_list) {
        (true, true) => " (learn: in the allowlist)",
        (true, false) => " (learn: NOT in the allowlist)",
        _ => "",
    };
    let addrs: Vec<SocketAddr> = match ctx.proxy.overrides.get(host) {
        Some(ips) => ips.iter().map(|ip| SocketAddr::new(*ip, port)).collect(),
        None => {
            let resolved = match tokio::time::timeout(
                ctx.proxy.limits.connect_timeout,
                tokio::net::lookup_host((host, port)),
            )
            .await
            {
                Ok(Ok(addrs)) => addrs.collect::<Vec<_>>(),
                Ok(Err(e)) => return Decision::Refuse("502 Bad Gateway", format!("dns: {e}")),
                Err(_) => return Decision::Refuse("504 Gateway Timeout", "dns timeout".into()),
            };
            let public: Vec<SocketAddr> = resolved
                .iter()
                .copied()
                .filter(|a| is_public(a.ip()))
                .collect();
            if public.is_empty() {
                let shown: Vec<String> = resolved.iter().map(|a| a.ip().to_string()).collect();
                return Decision::Refuse(
                    "403 Forbidden",
                    format!(
                        "resolves only to non-public addresses ({})",
                        shown.join(", ")
                    ),
                );
            }
            public
        }
    };
    let connect = async {
        let mut last = None;
        for addr in &addrs {
            match TcpStream::connect(addr).await {
                Ok(s) => return Ok((s, *addr)),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| std::io::Error::other("no addresses")))
    };
    match tokio::time::timeout(ctx.proxy.limits.connect_timeout, connect).await {
        Ok(Ok((stream, addr))) => {
            let _ = stream.set_nodelay(true);
            Decision::Tunnel(stream, format!("{addr}{note}"))
        }
        Ok(Err(e)) => {
            ctx.audit(
                "egress.error",
                format!("{host}:{port}: connect failed: {e}"),
            );
            Decision::Refuse("502 Bad Gateway", "connect failed".into())
        }
        Err(_) => {
            ctx.audit("egress.error", format!("{host}:{port}: connect timed out"));
            Decision::Refuse("504 Gateway Timeout", "connect timed out".into())
        }
    }
}

/// Read up to the end of the request head (at most 8 KiB). Returns the head and any bytes
/// the client already sent after it.
async fn read_head(stream: &mut UnixStream) -> std::io::Result<(Vec<u8>, Vec<u8>)> {
    const MAX_HEAD: usize = 8 * 1024;
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = head_end(&buf) {
            let rest = buf.split_off(end);
            return Ok((buf, rest));
        }
        if buf.len() > MAX_HEAD {
            return Err(std::io::Error::other("request head too large"));
        }
    }
}

async fn respond(stream: &mut UnixStream, status: &str, reason: &str) -> std::io::Result<()> {
    let body = format!("melissa egress: {reason}\n");
    let msg = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(msg.as_bytes()).await?;
    stream.shutdown().await
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

fn random_hex(bytes: usize) -> std::io::Result<String> {
    use std::io::Read;
    let mut buf = vec![0u8; bytes];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests;
