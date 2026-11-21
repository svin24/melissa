//! Tailscale, built into the container image and off by default (owner, 2026-11-21;
//! DECISIONS.md). Settings → Tailscale and the agents' `tailscale_status` / `enable_tailscale`.
//!
//! - **Detect first.** install.sh tells the container whether the host already runs Tailscale
//!   ([`HOST_ENV`]). If it does, Melissa leaves Tailscale to the host ([`Management::Host`]) and
//!   the page only says so. Installs without the built-in binaries (Nix/host) and installs from
//!   before the check say the same ([`Management::Unavailable`]).
//! - **Otherwise, one click.** Enable writes `tailscale.toml` and starts
//!   `melissa-tailscaled.service` (root, inside the privileged container: `/dev/net/tun`,
//!   interface `tailscale0`, state on the `/var` volume, no logs sent to Tailscale). The unit
//!   checks the setting itself at every start (`melissa-server tailscale enabled`), so it
//!   comes back after a restart only while enabled. Disable logs the node out and stops it.
//! - **Sign-in.** The server, Tailscale's *operator* (set by the unit), runs `tailscale up`
//!   and reads the login URL from `tailscale status --json`. Whoever opens that URL can attach
//!   this device to their tailnet, so it is treated like the setup password: it goes from the
//!   server to the owner's browser only ([`TailscaleControl::start_login`], called by the web
//!   UI alone), never to an agent, a transcript or a log ([`LoginUrl`] has no `Display` and a
//!   redacted `Debug`; the CLI's own output goes nowhere). Audited `tailscale.login_started`.
//! - **Apps.** Tailnet traffic arrives on `tailscale0` inside the container: the app access
//!   filter lets it reach the app ports under "Tailnet" and wider, never under "This device",
//!   and drops everything else it sends to the container (`app_access::ruleset`). App links add
//!   the node's MagicDNS name and address ([`TailscaleControl::own_addresses`]).
//! - **Network calls:** only while enabled, tailscaled talks to Tailscale's coordination server
//!   and DERP relays (PLAN.md section 4, owner-approved).

use std::{
    io::Read,
    net::{Ipv4Addr, Ipv6Addr},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use melissa_agent::{AuditLog, AuditRecord, BoxFuture};
use serde::{Deserialize, Serialize};

use crate::{write_private, SettingsError};

pub mod tool;

#[cfg(test)]
mod tests;

/// The setting, next to `app-access.toml` (`/etc/melissa` in the container).
pub const FILE_NAME: &str = "tailscale.toml";
/// Melissa's own tailscaled (container image only).
pub const UNIT: &str = "melissa-tailscaled.service";
/// The Tailscale CLI shipped in the image.
pub const CLI: &str = "/usr/bin/tailscale";
/// tailscaled's socket (its default path, so `podman exec melissa tailscale status` works).
pub const SOCKET: &str = "/run/tailscale/tailscaled.sock";
/// Set by install.sh in the Quadlet: `yes` when the host runs Tailscale, `no` when not.
pub const HOST_ENV: &str = "MELISSA_HOST_TAILSCALE";
/// Set by install.sh in the Quadlet: the node's name on the tailnet (`melissa-<host>`).
pub const NODE_NAME_ENV: &str = "MELISSA_TAILSCALE_NAME";
/// The interface tailnet traffic arrives on inside the container.
pub const INTERFACE: &str = "tailscale0";
/// The Unix user the server runs as: tailscaled's operator (may run `up` and `logout`).
pub const OPERATOR: &str = "melissa";
/// Largest setting file read (it holds one line).
const MAX_SETTING_BYTES: u64 = 4096;
/// How long a state read for app links is reused.
const CACHE_FOR: Duration = Duration::from_secs(10);

/// Who runs Tailscale for this device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Management {
    /// Melissa's own tailscaled in the container (off until the owner enables it).
    Melissa,
    /// The host runs Tailscale (install.sh saw it): Melissa keeps using the host's.
    Host,
    /// No built-in Tailscale here (Nix/host install), or an install from before install.sh
    /// checked the host.
    Unavailable,
}

impl Management {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Melissa => "melissa",
            Self::Host => "host",
            Self::Unavailable => "unavailable",
        }
    }
}

/// [`HOST_ENV`] as install.sh writes it: `Some(true)` the host runs Tailscale, `Some(false)` it
/// doesn't, `None` unknown (unset, empty or anything else).
pub fn parse_host_flag(value: Option<&str>) -> Option<bool> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "yes" | "1" | "true" => Some(true),
        "no" | "0" | "false" => Some(false),
        _ => None,
    }
}

/// Who manages Tailscale: Melissa only when the image ships it and install.sh checked that
/// the host doesn't run its own.
pub fn management(built_in: bool, host_flag: Option<&str>) -> Management {
    match (built_in, parse_host_flag(host_flag)) {
        (_, Some(true)) => Management::Host,
        (true, Some(false)) => Management::Melissa,
        _ => Management::Unavailable,
    }
}

/// Does this image ship Melissa's own Tailscale (the CLI and the unit)?
pub fn built_in() -> bool {
    Path::new(CLI).exists() && Path::new("/usr/lib/systemd/system").join(UNIT).exists()
}

/// The node's name on the tailnet from [`NODE_NAME_ENV`]: lower case letters, digits and
/// dashes, at most 63, no leading or trailing dash. Empty or unusable: `melissa`.
pub fn node_name(raw: Option<&str>) -> String {
    let mut name = String::new();
    for c in raw.unwrap_or("").trim().chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            name.push(c);
        } else if !name.is_empty() && !name.ends_with('-') {
            name.push('-');
        }
    }
    name.truncate(63);
    let name = name.trim_matches('-').to_string();
    if name.is_empty() {
        "melissa".into()
    } else {
        name
    }
}

// --- the setting file --------------------------------------------------------------------

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingFile {
    #[serde(default)]
    enabled: bool,
}

/// Is Melissa's Tailscale enabled? A missing file is off. Errors never quote the file (root
/// reads a file the server's user writes): not a regular file (a symlink is refused), too
/// big, or not a valid setting.
pub fn read_setting(path: &Path) -> Result<bool, String> {
    let shown = path.display();
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("cannot read {shown}: {}", e.kind())),
    };
    if !meta.file_type().is_file() {
        return Err(format!("{shown} is not a regular file"));
    }
    let mut text = String::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(MAX_SETTING_BYTES + 1).read_to_string(&mut text))
        .map_err(|e| format!("cannot read {shown}: {}", e.kind()))?;
    if text.len() as u64 > MAX_SETTING_BYTES {
        return Err(format!("{shown} is too big"));
    }
    toml::from_str::<SettingFile>(&text)
        .map(|f| f.enabled)
        .map_err(|_| format!("{shown} is not a valid setting"))
}

fn write_setting(path: &Path, enabled: bool) -> Result<(), SettingsError> {
    let text = format!(
        "# Melissa's own Tailscale (Settings → Tailscale), written by Melissa.\nenabled = {enabled}\n"
    );
    write_private(path, text.as_bytes()).map_err(|source| SettingsError::Write {
        path: path.display().to_string(),
        source,
    })
}

/// For the unit's `ExecCondition` (root): start tailscaled only when the setting says so and
/// the host doesn't run its own. An unreadable setting is off.
pub fn should_run(setting: &Path, host_flag: Option<&str>) -> Result<bool, String> {
    if parse_host_flag(host_flag) == Some(true) {
        return Ok(false);
    }
    read_setting(setting)
}

// --- `tailscale status --json` -------------------------------------------------------------

/// tailscaled's backend state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    NoState,
    NeedsLogin,
    NeedsMachineAuth,
    Stopped,
    Starting,
    Running,
    Other(String),
}

impl Backend {
    fn parse(s: &str) -> Self {
        match s {
            "NoState" => Self::NoState,
            "NeedsLogin" => Self::NeedsLogin,
            "NeedsMachineAuth" => Self::NeedsMachineAuth,
            "Stopped" => Self::Stopped,
            "Starting" => Self::Starting,
            "Running" => Self::Running,
            other => Self::Other(other.to_string()),
        }
    }
}

/// A Tailscale sign-in URL. Whoever opens it can attach this device to their tailnet: it has
/// no `Display`, its `Debug` is redacted, and it is never serialized. Only `https` URLs on
/// tailscale.com are accepted.
#[derive(Clone, PartialEq, Eq)]
pub struct LoginUrl(String);

impl LoginUrl {
    pub fn parse(raw: &str) -> Option<Self> {
        let rest = raw.trim().strip_prefix("https://")?;
        let host = rest.split(['/', '?', '#']).next()?;
        let ok_host = host == "login.tailscale.com" || host.ends_with(".tailscale.com");
        let clean = !raw
            .trim()
            .chars()
            .any(|c| c.is_whitespace() || c.is_control());
        (ok_host && clean && rest.len() > host.len()).then(|| Self(raw.trim().to_string()))
    }

    /// For the owner's browser only (the web UI's sign-in button).
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for LoginUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LoginUrl(..)")
    }
}

/// This device as a node on the tailnet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Its host name on the tailnet, e.g. `melissa-box`.
    pub name: String,
    /// Its MagicDNS name (`melissa-box.<tailnet>.ts.net`, no trailing dot), when MagicDNS is
    /// on for the tailnet.
    pub dns_name: Option<String>,
    pub ipv4: Option<Ipv4Addr>,
    pub ipv6: Option<Ipv6Addr>,
}

/// What tailscaled says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Daemon {
    pub backend: Backend,
    pub login_url: Option<LoginUrl>,
    /// Set while running.
    pub node: Option<Node>,
    pub health: Vec<String>,
}

#[derive(Deserialize)]
struct RawStatus {
    #[serde(rename = "BackendState", default)]
    backend_state: String,
    #[serde(rename = "AuthURL", default)]
    auth_url: String,
    #[serde(rename = "Self", default)]
    me: Option<RawPeer>,
    #[serde(rename = "Health", default)]
    health: Option<Vec<String>>,
    #[serde(rename = "CurrentTailnet", default)]
    tailnet: Option<RawTailnet>,
}

#[derive(Deserialize)]
struct RawPeer {
    #[serde(rename = "HostName", default)]
    host_name: String,
    #[serde(rename = "DNSName", default)]
    dns_name: String,
    #[serde(rename = "TailscaleIPs", default)]
    ips: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct RawTailnet {
    #[serde(rename = "MagicDNSEnabled", default)]
    magic_dns: bool,
}

/// Parse `tailscale status --json`. Errors never quote the input (it may hold the login URL).
pub fn parse_status(json: &str) -> Result<Daemon, String> {
    let raw: RawStatus = serde_json::from_str(json)
        .map_err(|e| format!("unexpected `tailscale status` output (line {})", e.line()))?;
    let backend = Backend::parse(&raw.backend_state);
    let login_url = match raw.auth_url.trim() {
        "" => None,
        url => Some(LoginUrl::parse(url).ok_or("tailscaled gave an unexpected sign-in address")?),
    };
    let magic_dns = raw.tailnet.as_ref().is_some_and(|t| t.magic_dns);
    let node = raw.me.filter(|_| backend == Backend::Running).map(|me| {
        let ips = me.ips.unwrap_or_default();
        let dns = me
            .dns_name
            .trim()
            .trim_end_matches('.')
            .to_ascii_lowercase();
        Node {
            name: me.host_name,
            dns_name: (magic_dns && dns.ends_with(".ts.net")).then_some(dns),
            ipv4: ips.iter().find_map(|ip| ip.parse::<Ipv4Addr>().ok()),
            ipv6: ips.iter().find_map(|ip| ip.parse::<Ipv6Addr>().ok()),
        }
    });
    Ok(Daemon {
        backend,
        login_url,
        node,
        health: raw.health.unwrap_or_default(),
    })
}

// --- the state the page and the tools show -----------------------------------------------

/// Settings → Tailscale, one of: Not managed by Melissa | Off | Starting | Sign in needed |
/// Waiting for approval | Connected | Error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    NotManaged,
    Off,
    Starting,
    SignInNeeded,
    /// Signed in; the tailnet's admin must approve the device.
    NeedsApproval,
    Connected(Node),
    /// One line for the owner (no raw error text).
    Error(String),
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotManaged => "not_managed",
            Self::Off => "off",
            Self::Starting => "starting",
            Self::SignInNeeded => "sign_in_needed",
            Self::NeedsApproval => "needs_approval",
            Self::Connected(_) => "connected",
            Self::Error(_) => "error",
        }
    }

    /// The state in words, for the page and the agents.
    pub fn sentence(&self) -> String {
        match self {
            Self::NotManaged => "Tailscale is not managed by Melissa on this system.".into(),
            Self::Off => "Off.".into(),
            Self::Starting => "Starting.".into(),
            Self::SignInNeeded => "Sign in needed.".into(),
            Self::NeedsApproval => "Waiting for approval in your Tailscale admin console.".into(),
            Self::Connected(n) => {
                let mut s = format!("Connected · {}", n.name);
                if let Some(ip) = n.ipv4 {
                    s.push_str(&format!(" · {ip}"));
                }
                s
            }
            Self::Error(e) => e.clone(),
        }
    }
}

/// The state for an enabled daemon from what `tailscale status --json` gave (or why not).
pub fn state_from(daemon: Result<Daemon, String>) -> State {
    let Ok(d) = daemon else {
        return State::Error("Tailscale isn't running.".into());
    };
    match d.backend {
        Backend::NoState | Backend::Starting => State::Starting,
        Backend::NeedsLogin | Backend::Stopped => State::SignInNeeded,
        Backend::NeedsMachineAuth => State::NeedsApproval,
        Backend::Running => match d.node {
            Some(n) if n.ipv4.is_some() || n.ipv6.is_some() => State::Connected(n),
            _ => State::Starting,
        },
        Backend::Other(_) => State::Error("Tailscale is in a state Melissa doesn't know.".into()),
    }
}

// --- the commands --------------------------------------------------------------------------

/// The Tailscale CLI (injected so the logic is tested without tailscaled).
pub trait Cli: Send + Sync {
    /// `tailscale status --json` (the JSON text).
    fn status(&self) -> BoxFuture<'_, Result<String, String>>;
    /// Start an interactive sign-in (`tailscale up`) and return without waiting for it.
    fn start_login<'a>(&'a self, node_name: &'a str) -> BoxFuture<'a, Result<(), String>>;
    /// `tailscale logout`: the node leaves the tailnet (its key expires).
    fn logout(&self) -> BoxFuture<'_, Result<(), String>>;
}

/// Starts and stops tailscaled (the unit).
pub trait Service: Send + Sync {
    fn start(&self) -> BoxFuture<'_, Result<(), String>>;
    fn stop(&self) -> BoxFuture<'_, Result<(), String>>;
}

/// The real CLI on tailscaled's socket. Output that may hold the login URL is never logged
/// or kept: `up`'s stdout and stderr go nowhere, `status` is parsed and dropped.
pub struct TailscaleCommand {
    pub path: PathBuf,
    pub socket: PathBuf,
    pub timeout: Duration,
}

impl Default for TailscaleCommand {
    fn default() -> Self {
        Self {
            path: PathBuf::from(CLI),
            socket: PathBuf::from(SOCKET),
            timeout: Duration::from_secs(15),
        }
    }
}

impl TailscaleCommand {
    fn command(&self) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.path);
        cmd.arg(format!("--socket={}", self.socket.display()))
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        cmd
    }

    async fn run(&self, args: &[&str]) -> Result<String, String> {
        let mut cmd = self.command();
        cmd.args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        match tokio::time::timeout(self.timeout, cmd.output()).await {
            Err(_) => Err(format!("`tailscale {}` did not answer in time", args[0])),
            Ok(Err(e)) => Err(format!("cannot run {}: {}", self.path.display(), e.kind())),
            Ok(Ok(out)) if out.status.success() => {
                Ok(String::from_utf8_lossy(&out.stdout).into_owned())
            }
            Ok(Ok(out)) => Err(format!("`tailscale {}` failed ({})", args[0], out.status)),
        }
    }
}

impl Cli for TailscaleCommand {
    fn status(&self) -> BoxFuture<'_, Result<String, String>> {
        Box::pin(self.run(&["status", "--json"]))
    }

    fn start_login<'a>(&'a self, node_name: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let mut cmd = self.command();
            // `--operator` keeps the server's right to run `up`/`logout` (an `up` without it
            // resets the operator). `--timeout`: the CLI gives up waiting after a while; the
            // sign-in itself stays open in tailscaled.
            cmd.args([
                "up",
                &format!("--hostname={node_name}"),
                "--accept-dns=false",
                &format!("--operator={OPERATOR}"),
                "--timeout=10m",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(false);
            let mut child = cmd
                .spawn()
                .map_err(|e| format!("cannot run {}: {}", self.path.display(), e.kind()))?;
            // Reap it whenever it ends (signed in, or the timeout).
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            Ok(())
        })
    }

    fn logout(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move { self.run(&["logout"]).await.map(|_| ()) })
    }
}

/// `systemctl start|stop melissa-tailscaled.service` (polkit lets the server do exactly that).
pub struct SystemdService {
    pub systemctl: PathBuf,
    pub timeout: Duration,
}

impl Default for SystemdService {
    fn default() -> Self {
        Self {
            systemctl: PathBuf::from("systemctl"),
            timeout: Duration::from_secs(60),
        }
    }
}

impl SystemdService {
    async fn run(&self, verb: &str) -> Result<(), String> {
        let mut cmd = tokio::process::Command::new(&self.systemctl);
        cmd.args([verb, UNIT])
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        match tokio::time::timeout(self.timeout, cmd.output()).await {
            Err(_) => Err(format!("{UNIT} did not {verb} in time")),
            Ok(Err(e)) => Err(format!("cannot run systemctl: {e}")),
            Ok(Ok(out)) if out.status.success() => Ok(()),
            Ok(Ok(_)) => Err(format!("systemctl {verb} {UNIT} failed")),
        }
    }
}

impl Service for SystemdService {
    fn start(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(self.run("start"))
    }

    fn stop(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(self.run("stop"))
    }
}

// --- the server's side -------------------------------------------------------------------

/// Errors from enabling, disabling and signing in.
#[derive(Debug, thiserror::Error)]
pub enum TailscaleError {
    /// The host runs Tailscale, or this install has none of Melissa's.
    #[error("Tailscale is not managed by Melissa on this system")]
    NotManaged,
    /// Nothing to do in this state (e.g. a sign-in while off or connected).
    #[error("not possible now: Tailscale is {0}")]
    Not(&'static str),
    #[error(transparent)]
    Save(#[from] SettingsError),
    /// For the log and the agents; the page shows a short line.
    #[error("{0}")]
    Failed(String),
}

/// What a sign-in request gives the owner's browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Login {
    /// Open this (never shown to agents, logged or stored).
    Url(LoginUrl),
    /// Already connected (e.g. the node was still signed in): nothing to open.
    Connected,
}

/// Melissa's own Tailscale: the setting, the daemon, the audit log. Cheap to clone.
#[derive(Clone)]
pub struct TailscaleControl {
    inner: Arc<Inner>,
}

struct Inner {
    management: Management,
    setting: PathBuf,
    node_name: String,
    cli: Arc<dyn Cli>,
    service: Arc<dyn Service>,
    audit: Option<AuditLog>,
    /// How long enable and sign-in wait for tailscaled.
    wait: Duration,
    lock: tokio::sync::Mutex<()>,
    cache: std::sync::Mutex<Option<(Instant, State)>>,
}

/// How a [`TailscaleControl`] is put together.
pub struct Parts {
    pub management: Management,
    pub setting: PathBuf,
    pub node_name: String,
    pub cli: Arc<dyn Cli>,
    pub service: Arc<dyn Service>,
    pub audit: Option<AuditLog>,
    pub wait: Duration,
}

impl TailscaleControl {
    pub fn new(parts: Parts) -> Self {
        Self {
            inner: Arc::new(Inner {
                management: parts.management,
                setting: parts.setting,
                node_name: parts.node_name,
                cli: parts.cli,
                service: parts.service,
                audit: parts.audit,
                wait: parts.wait,
                lock: tokio::sync::Mutex::new(()),
                cache: std::sync::Mutex::new(None),
            }),
        }
    }

    /// This install: the setting in `config_dir`, the facts install.sh passed in, the real CLI
    /// and unit.
    pub fn system(config_dir: &Path, audit: Option<AuditLog>) -> Self {
        let host = std::env::var(HOST_ENV).ok();
        let name = std::env::var(NODE_NAME_ENV).ok();
        Self::new(Parts {
            management: management(built_in(), host.as_deref()),
            setting: config_dir.join(FILE_NAME),
            node_name: node_name(name.as_deref()),
            cli: Arc::new(TailscaleCommand::default()),
            service: Arc::new(SystemdService::default()),
            audit,
            wait: Duration::from_secs(20),
        })
    }

    pub fn management(&self) -> Management {
        self.inner.management
    }

    /// The name this device gets on the tailnet.
    pub fn node_name(&self) -> &str {
        &self.inner.node_name
    }

    /// The saved setting (off when it can't be read).
    pub fn enabled(&self) -> bool {
        read_setting(&self.inner.setting).unwrap_or_else(|e| {
            tracing::warn!("{e}; Melissa's Tailscale is treated as off");
            false
        })
    }

    async fn daemon(&self) -> Result<Daemon, String> {
        parse_status(&self.inner.cli.status().await?)
    }

    /// The state now.
    pub async fn state(&self) -> State {
        let state = if self.inner.management != Management::Melissa {
            State::NotManaged
        } else if !self.enabled() {
            State::Off
        } else {
            let daemon = self.daemon().await;
            if let Err(e) = &daemon {
                tracing::warn!("tailscale status: {e}");
            }
            state_from(daemon)
        };
        if let Ok(mut c) = self.inner.cache.lock() {
            *c = Some((Instant::now(), state.clone()));
        }
        state
    }

    /// The state, reused for a few seconds (app links ask once per app).
    pub async fn cached_state(&self) -> State {
        let hit = self.inner.cache.lock().ok().and_then(|c| {
            c.as_ref()
                .filter(|(at, _)| at.elapsed() < CACHE_FOR)
                .map(|(_, s)| s.clone())
        });
        match hit {
            Some(s) => s,
            None => self.state().await,
        }
    }

    fn forget(&self) {
        if let Ok(mut c) = self.inner.cache.lock() {
            *c = None;
        }
    }

    /// Addresses of this device on the tailnet that reach the container directly (app links
    /// use the NodePort there, not the host's published port): the MagicDNS name, then the
    /// IPv4 address. Empty unless Melissa's Tailscale is connected.
    pub async fn own_addresses(&self) -> Vec<String> {
        match self.cached_state().await {
            State::Connected(n) => n
                .dns_name
                .into_iter()
                .chain(n.ipv4.map(|ip| ip.to_string()))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Wait (up to the configured time) for a state `done` accepts; the last state otherwise.
    async fn wait_for(&self, done: impl Fn(&Daemon) -> bool) -> Result<Daemon, String> {
        let until = Instant::now() + self.inner.wait;
        loop {
            let d = self.daemon().await;
            if d.as_ref().is_ok_and(&done) || Instant::now() >= until {
                return d;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    /// Turn Melissa's Tailscale on (audited `settings.tailscale`, by `actor`): save, start
    /// tailscaled, wait until it answers. The owner signs in next.
    pub async fn enable(&self, actor: &str) -> Result<State, TailscaleError> {
        if self.inner.management != Management::Melissa {
            return Err(TailscaleError::NotManaged);
        }
        let _one = self.inner.lock.lock().await;
        self.forget();
        write_setting(&self.inner.setting, true)?;
        let started = self.inner.service.start().await;
        let state = match &started {
            Ok(()) => state_from(
                self.wait_for(|d| !matches!(d.backend, Backend::NoState | Backend::Starting))
                    .await,
            ),
            Err(_) => State::Error("Tailscale didn't start.".into()),
        };
        let outcome = match &started {
            Ok(()) => state.as_str().to_string(),
            Err(e) => format!("not started: {e}"),
        };
        self.audit(
            "settings.tailscale",
            actor,
            &format!("enabled (node {}); {outcome}", self.inner.node_name),
        );
        self.forget();
        match started {
            Ok(()) => Ok(state),
            Err(e) => Err(TailscaleError::Failed(e)),
        }
    }

    /// Turn it off: log the node out (it leaves the tailnet), save, stop tailscaled.
    pub async fn disable(&self, actor: &str) -> Result<State, TailscaleError> {
        if self.inner.management != Management::Melissa {
            return Err(TailscaleError::NotManaged);
        }
        let _one = self.inner.lock.lock().await;
        self.forget();
        let logout = match self.daemon().await {
            Ok(d) if matches!(d.backend, Backend::NeedsLogin | Backend::NoState) => {
                "not signed in".to_string()
            }
            Ok(_) => match self.inner.cli.logout().await {
                Ok(()) => "logged out".into(),
                Err(e) => format!("not logged out: {e}"),
            },
            Err(_) => "not running".into(),
        };
        write_setting(&self.inner.setting, false)?;
        let stopped = self.inner.service.stop().await;
        self.audit(
            "settings.tailscale",
            actor,
            &format!(
                "disabled; {logout}; {}",
                match &stopped {
                    Ok(()) => "stopped".to_string(),
                    Err(e) => format!("not stopped: {e}"),
                }
            ),
        );
        self.forget();
        stopped.map(|()| State::Off).map_err(TailscaleError::Failed)
    }

    /// Start a sign-in and return the URL for the owner's browser. Web UI only: no tool calls
    /// this, and the URL is never logged (audited `tailscale.login_started` without it).
    pub async fn start_login(&self, actor: &str) -> Result<Login, TailscaleError> {
        if self.inner.management != Management::Melissa {
            return Err(TailscaleError::NotManaged);
        }
        if !self.enabled() {
            return Err(TailscaleError::Not("off"));
        }
        let _one = self.inner.lock.lock().await;
        self.forget();
        let now = self.daemon().await.map_err(TailscaleError::Failed)?;
        let found = match now.backend {
            Backend::Running => return Ok(Login::Connected),
            Backend::NeedsMachineAuth => return Err(TailscaleError::Not("waiting for approval")),
            _ if now.login_url.is_some() => now,
            _ => {
                self.inner
                    .cli
                    .start_login(&self.inner.node_name)
                    .await
                    .map_err(TailscaleError::Failed)?;
                self.wait_for(|d| d.login_url.is_some() || d.backend == Backend::Running)
                    .await
                    .map_err(TailscaleError::Failed)?
            }
        };
        match (found.backend, found.login_url) {
            (Backend::Running, _) => Ok(Login::Connected),
            (_, Some(url)) => {
                self.audit(
                    "tailscale.login_started",
                    actor,
                    &format!("sign-in started for node {}", self.inner.node_name),
                );
                Ok(Login::Url(url))
            }
            _ => Err(TailscaleError::Failed(
                "Tailscale gave no sign-in address (no connection to Tailscale?)".into(),
            )),
        }
    }

    fn audit(&self, action: &str, actor: &str, detail: &str) {
        let Some(log) = &self.inner.audit else {
            return;
        };
        let record = AuditRecord::now(action, 0, "", detail).by(actor);
        if let Err(e) = log.append(&record) {
            tracing::error!(path = %log.path().display(), "audit log write failed: {e}");
        }
    }
}
