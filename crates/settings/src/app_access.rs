//! Who may open the apps on this device: Settings → Apps ("Apps open to") and the agents'
//! `set_app_access` tool. Container install only (decided 2026-11-21, DECISIONS.md).
//!
//! The host publishes the app ports (NodePorts 30000-30079, TCP and UDP) on every address;
//! Melissa decides who gets through, with an nftables table inside the (privileged)
//! container. It matches the traffic podman forwards in on the **source address**, in
//! `prerouting` before k3s's NodePort DNAT (priority -150, ahead of `dstnat`), so it sees
//! the NodePort itself:
//!
//! - **This device**: the podman bridge gateway only. Rootful netavark masquerades
//!   connections to the host's loopback (`127.0.0.1:<port>`) to that address; it is the
//!   container's default gateway, read from `/proc/net/route`.
//! - **Tailnet**: also Tailscale's ranges, 100.64.0.0/10 and fd7a:115c:a1e0::/48.
//! - **Home network**: also the private ranges (RFC 1918, 169.254.0.0/16, IPv6 ULA and
//!   link-local). The container can't see the host's interfaces, and a laptop's network
//!   changes, so the fixed private ranges are used rather than the host's current subnet.
//!
//! Everything else to those ports is dropped. The container itself (`lo`) and k3s's pods
//! (`cni0`) are not filtered. A drop is final in nftables whatever other tables (kube-proxy,
//! flannel, network policies) say, so the table takes precedence without ordering tricks.
//!
//! **Fail closed.** The root oneshot `melissa-app-access.service` runs
//! `melissa-server app-access apply` at every start (before k3s, which doesn't start without
//! the table) and when the setting changes. It loads the table, reads it back to verify it,
//! and writes [`Status`] to [`STATUS_PATH`]. If that fails it falls back to "this device",
//! then to a table that lets nothing in from outside the container. Loads are atomic: a
//! failed one leaves the previous table in place.
//!
//! The server (user `melissa`) writes the setting to `app-access.toml` in its config
//! directory and starts that unit (polkit allows exactly that); [`AppAccessControl`] wraps
//! this for the UI, the tool and the app links.

use std::{
    io::Read,
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use melissa_agent::{AuditLog, AuditRecord, BoxFuture};
use serde::{Deserialize, Serialize};

use crate::{write_private, SettingsError};

pub mod tool;

#[cfg(test)]
mod tests;

/// The setting, next to `providers.toml` (`/etc/melissa` in the container).
pub const FILE_NAME: &str = "app-access.toml";
/// What the root helper last did ([`Status`], JSON). Root-owned, readable by the server.
pub const STATUS_PATH: &str = "/run/melissa-app-access/status.json";
/// The root oneshot that applies the setting.
pub const UNIT: &str = "melissa-app-access.service";
/// The nftables table (family `inet`).
pub const TABLE: &str = "melissa_app_access";
/// The app NodePorts inside the container (the host publishes them, maybe at other ports).
pub const FIRST_PORT: u16 = 30000;
pub const LAST_PORT: u16 = 30079;

/// Interfaces that are never filtered: the container itself and k3s's pod bridge.
const INSIDE: [&str; 2] = ["lo", "cni0"];
const TAILNET_V4: &str = "100.64.0.0/10";
const TAILNET_V6: &str = "fd7a:115c:a1e0::/48";
const HOME_V4: [&str; 4] = [
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
];
const HOME_V6: [&str; 2] = ["fc00::/7", "fe80::/10"];
/// The comment on the final drop rule (verification looks for it).
const DROP: &str = "melissa:drop";
/// Largest setting file read (it holds one line).
const MAX_SETTING_BYTES: u64 = 4096;

/// Who may open the apps. Each level includes the ones before it.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum AppAccess {
    /// Only this device (the host's loopback). Same exposure as the old loopback BIND.
    #[default]
    ThisDevice,
    /// This device and devices on the owner's tailnet.
    Tailnet,
    /// This device, the tailnet and devices on the home network.
    HomeNetwork,
}

impl AppAccess {
    pub const ALL: [Self; 3] = [Self::ThisDevice, Self::Tailnet, Self::HomeNetwork];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ThisDevice => "this_device",
            Self::Tailnet => "tailnet",
            Self::HomeNetwork => "home_network",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.as_str() == value.trim())
    }

    /// The UI's name for it.
    pub fn label(self) -> &'static str {
        match self {
            Self::ThisDevice => "This device",
            Self::Tailnet => "Tailnet",
            Self::HomeNetwork => "Home network",
        }
    }

    /// Who can open apps, as a sentence.
    pub fn sentence(self) -> &'static str {
        match self {
            Self::ThisDevice => "Only this device can open apps.",
            Self::Tailnet => "This device and devices on your tailnet can open apps.",
            Self::HomeNetwork => {
                "This device and devices on your tailnet and home network can open apps."
            }
        }
    }

    /// Do links with `address` work at this level?
    pub fn allows(self, address: &str) -> bool {
        needed_for(address).is_some_and(|needed| needed <= self)
    }
}

impl std::fmt::Display for AppAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The level a link address needs: loopback and `localhost` this device; Tailscale addresses
/// and MagicDNS names (`*.ts.net`) the tailnet; private and link-local addresses, and other
/// host names (`box.local`, a name in the owner's DNS), the home network. `None`: a public
/// address, which no level opens (clients there have public source addresses).
pub fn needed_for(address: &str) -> Option<AppAccess> {
    let bare = address.trim().trim_start_matches('[').trim_end_matches(']');
    let Ok(ip) = bare.parse::<IpAddr>() else {
        let name = bare.trim_end_matches('.').to_ascii_lowercase();
        return Some(if name == "localhost" {
            AppAccess::ThisDevice
        } else if name.ends_with(".ts.net") {
            AppAccess::Tailnet
        } else {
            AppAccess::HomeNetwork
        });
    };
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            if v4.is_loopback() {
                Some(AppAccess::ThisDevice)
            } else if v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64 {
                Some(AppAccess::Tailnet)
            } else if v4.is_private() || v4.is_link_local() {
                Some(AppAccess::HomeNetwork)
            } else {
                None
            }
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            if v6.is_loopback() {
                Some(AppAccess::ThisDevice)
            } else if s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0 {
                Some(AppAccess::Tailnet)
            } else if (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80 {
                Some(AppAccess::HomeNetwork)
            } else {
                None
            }
        }
    }
}

/// Is the filter part of this install? Only the container image ships its unit.
pub fn available() -> bool {
    Path::new("/usr/lib/systemd/system").join(UNIT).exists()
}

// --- the setting file --------------------------------------------------------------------

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingFile {
    #[serde(default)]
    apps: AppAccess,
}

/// Read the setting. A missing file is the default (this device). Errors never quote the
/// file (root reads a file the server's user writes): not a regular file (a symlink is
/// refused), too big, or not a valid setting.
pub fn read_setting(path: &Path) -> Result<AppAccess, String> {
    let shown = path.display();
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(AppAccess::default()),
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
        .map(|f| f.apps)
        .map_err(|_| format!("{shown} is not a valid setting"))
}

fn write_setting(path: &Path, access: AppAccess) -> Result<(), SettingsError> {
    let mut text = String::from(
        "# Who may open apps on this device (Settings → Apps), written by Melissa.\n\
         # this_device, tailnet or home_network.\n",
    );
    text.push_str(
        &toml::to_string(&SettingFile { apps: access })
            .map_err(|e| SettingsError::Failed(e.to_string()))?,
    );
    write_private(path, text.as_bytes()).map_err(|source| SettingsError::Write {
        path: path.display().to_string(),
        source,
    })
}

// --- rules ---------------------------------------------------------------------------------

/// The default route's gateway in `/proc/net/route` text (the podman bridge address, where
/// the host's loopback connections come from). The kernel prints addresses as native-endian
/// hex of the network-order bytes.
pub fn default_gateway(proc_net_route: &str) -> Option<Ipv4Addr> {
    const UP_GATEWAY: u32 = 0x1 | 0x2; // RTF_UP | RTF_GATEWAY
    proc_net_route.lines().skip(1).find_map(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        let hex = |i: usize| f.get(i).and_then(|v| u32::from_str_radix(v, 16).ok());
        let (dest, gateway, flags, mask) = (hex(1)?, hex(2)?, hex(3)?, hex(7)?);
        (dest == 0 && mask == 0 && flags & UP_GATEWAY == UP_GATEWAY && gateway != 0)
            .then(|| Ipv4Addr::from(gateway.to_ne_bytes()))
    })
}

/// The nftables script for a filter that lets `open` in: `Some((level, gateway))`, or
/// `None` for the closed table (nothing from outside the container). One transaction:
/// it replaces the table atomically, or changes nothing.
pub fn ruleset(open: Option<(AppAccess, Ipv4Addr)>) -> String {
    let inside = INSIDE.map(|i| format!("\"{i}\"")).join(", ");
    let ports = format!("{FIRST_PORT}-{LAST_PORT}");
    let mut allow = Vec::new();
    if let Some((level, gateway)) = open {
        allow.push(format!(
            "ip saddr {gateway} accept comment \"{}\"",
            comment(AppAccess::ThisDevice)
        ));
        if level >= AppAccess::Tailnet {
            let c = comment(AppAccess::Tailnet);
            allow.push(format!("ip saddr {TAILNET_V4} accept comment \"{c}\""));
            allow.push(format!("ip6 saddr {TAILNET_V6} accept comment \"{c}\""));
        }
        if level >= AppAccess::HomeNetwork {
            let c = comment(AppAccess::HomeNetwork);
            allow.push(format!(
                "ip saddr {{ {} }} accept comment \"{c}\"",
                HOME_V4.join(", ")
            ));
            allow.push(format!(
                "ip6 saddr {{ {} }} accept comment \"{c}\"",
                HOME_V6.join(", ")
            ));
        }
    }
    let mut out = format!(
        "# Melissa: who may open apps ({}). Generated; see `melissa-server app-access`.\n\
         table inet {TABLE}\n\
         delete table inet {TABLE}\n\
         table inet {TABLE} {{\n\
         \tchain prerouting {{\n\
         \t\ttype filter hook prerouting priority -150; policy accept;\n\
         \t\tiifname != {{ {inside} }} tcp dport {ports} jump app_ports\n\
         \t\tiifname != {{ {inside} }} udp dport {ports} jump app_ports\n\
         \t}}\n\
         \tchain app_ports {{\n",
        open.map_or("closed", |(l, _)| l.as_str())
    );
    for rule in allow {
        out.push_str(&format!("\t\t{rule}\n"));
    }
    out.push_str(&format!("\t\tcounter drop comment \"{DROP}\"\n\t}}\n}}\n"));
    out
}

fn comment(level: AppAccess) -> String {
    format!("melissa:{}", level.as_str())
}

/// Check `listing` (`nft list table inet melissa_app_access`) against what [`ruleset`]
/// loaded for `open`: both hooks on the app ports, the final drop, an accept for each level
/// that is open (the gateway for this device) and none for a level that isn't.
pub fn verify(listing: &str, open: Option<(AppAccess, Ipv4Addr)>) -> Result<(), String> {
    let ports = format!("{FIRST_PORT}-{LAST_PORT}");
    let mut missing = Vec::new();
    for want in [
        format!("table inet {TABLE}"),
        "hook prerouting".to_string(),
        format!("tcp dport {ports} jump app_ports"),
        format!("udp dport {ports} jump app_ports"),
        format!("drop comment \"{DROP}\""),
    ] {
        if !listing.contains(&want) {
            missing.push(want);
        }
    }
    for level in AppAccess::ALL {
        let tag = format!("comment \"{}\"", comment(level));
        let wanted = open.is_some_and(|(l, _)| level <= l);
        match (wanted, listing.contains(&tag)) {
            (true, false) => missing.push(tag),
            (false, true) => return Err(format!("the filter also lets in {}", level.label())),
            _ => {}
        }
    }
    if let Some((_, gateway)) = open {
        let rule = format!("ip saddr {gateway} accept");
        if !listing.contains(&rule) {
            missing.push(rule);
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("the filter in place lacks: {}", missing.join("; ")))
    }
}

/// Runs `nft` (injected so the fail-closed logic is tested without root).
pub trait Nft {
    /// Load a script (`nft -f -`).
    fn load(&self, script: &str) -> Result<(), String>;
    /// The table as nft prints it.
    fn list(&self) -> Result<String, String>;
}

/// The real `nft`.
pub struct NftCommand {
    pub path: PathBuf,
}

impl Default for NftCommand {
    fn default() -> Self {
        Self {
            path: PathBuf::from("/usr/sbin/nft"),
        }
    }
}

impl NftCommand {
    fn run(&self, args: &[&str], stdin: Option<&str>) -> Result<String, String> {
        use std::{io::Write, process::Stdio};
        let mut child = std::process::Command::new(&self.path)
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run {}: {e}", self.path.display()))?;
        if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(text.as_bytes())
                .map_err(|e| format!("cannot write to nft: {e}"))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| format!("nft failed: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            let first = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
            Err(format!("nft failed ({}): {}", out.status, first.trim()))
        }
    }
}

impl Nft for NftCommand {
    fn load(&self, script: &str) -> Result<(), String> {
        self.run(&["-f", "-"], Some(script)).map(|_| ())
    }

    fn list(&self) -> Result<String, String> {
        self.run(&["list", "table", "inet", TABLE], None)
    }
}

/// What the root helper did, for the server ([`STATUS_PATH`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// The setting it read (this device when it couldn't read it).
    pub wanted: AppAccess,
    /// What the verified filter lets in. `None`: closed (nothing from outside the
    /// container, not even this device), or nothing could be verified.
    pub open_to: Option<AppAccess>,
    /// A table [`ruleset`] made is in place and checked. False: even the closed table
    /// failed; the previous table (if any) stays, and k3s doesn't start without one.
    pub verified: bool,
    /// What went wrong, in order (empty: all good).
    pub errors: Vec<String>,
    /// When (seconds since the epoch).
    pub at: u64,
}

impl Status {
    /// The setting is in force, nothing went wrong.
    pub fn ok(&self) -> bool {
        self.verified && self.open_to == Some(self.wanted) && self.errors.is_empty()
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn load_verified(nft: &dyn Nft, open: Option<(AppAccess, Ipv4Addr)>) -> Result<(), String> {
    nft.load(&ruleset(open))?;
    verify(&nft.list()?, open)
}

/// Put the filter in place, failing closed: the wanted level, else this device, else the
/// closed table. `wanted`: the setting as read (an error means this device). `gateway`:
/// [`default_gateway`] (unknown: the closed table, since this device can't be told apart).
pub fn apply(
    nft: &dyn Nft,
    wanted: Result<AppAccess, String>,
    gateway: Option<Ipv4Addr>,
) -> Status {
    let (wanted, mut errors) = match wanted {
        Ok(a) => (a, Vec::new()),
        Err(e) => (AppAccess::ThisDevice, vec![e]),
    };
    let status = |open_to, verified, errors| Status {
        wanted,
        open_to,
        verified,
        errors,
        at: now(),
    };
    match gateway {
        Some(gw) => {
            let mut tries = vec![wanted];
            if wanted != AppAccess::ThisDevice {
                tries.push(AppAccess::ThisDevice);
            }
            for level in tries {
                match load_verified(nft, Some((level, gw))) {
                    Ok(()) => return status(Some(level), true, errors),
                    Err(e) => errors.push(e),
                }
            }
        }
        None => errors
            .push("no default route in the container: the host's bridge address is unknown".into()),
    }
    match load_verified(nft, None) {
        Ok(()) => status(None, true, errors),
        Err(e) => {
            errors.push(e);
            status(None, false, errors)
        }
    }
}

/// Write the status for the server (root; the directory is the unit's runtime directory).
pub fn write_status(path: &Path, status: &Status) -> std::io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let data = serde_json::to_vec_pretty(status).map_err(std::io::Error::other)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o644)
        .open(&tmp)?;
    f.write_all(&data)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

fn read_status(path: &Path) -> Option<Status> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

// --- the server's side -------------------------------------------------------------------

/// Starts the root helper and waits for it.
pub trait Applier: Send + Sync {
    fn apply(&self) -> BoxFuture<'_, Result<(), String>>;
}

/// `systemctl start melissa-app-access.service` (a oneshot: returns when it is done).
pub struct SystemdApplier {
    pub systemctl: PathBuf,
    pub timeout: Duration,
}

impl Default for SystemdApplier {
    fn default() -> Self {
        Self {
            systemctl: PathBuf::from("systemctl"),
            timeout: Duration::from_secs(30),
        }
    }
}

impl Applier for SystemdApplier {
    fn apply(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            let mut cmd = tokio::process::Command::new(&self.systemctl);
            cmd.args(["start", UNIT])
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true);
            match tokio::time::timeout(self.timeout, cmd.output()).await {
                Err(_) => Err("the filter did not answer in time".into()),
                Ok(Err(e)) => Err(format!("cannot run systemctl: {e}")),
                Ok(Ok(out)) if out.status.success() => Ok(()),
                Ok(Ok(_)) => Err(format!("{UNIT} failed")),
            }
        })
    }
}

/// The setting and what is in force, as the UI, the tool and app links see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub setting: AppAccess,
    pub status: Option<Status>,
}

impl View {
    /// What the filter in place lets in, if known: `None` when it is closed, unverified,
    /// or has not run.
    pub fn open_to(&self) -> Option<AppAccess> {
        self.status
            .as_ref()
            .filter(|s| s.verified)
            .and_then(|s| s.open_to)
    }

    /// The level app links may assume: what is in force, at most the setting (a change
    /// that is still being applied doesn't widen links early). Unknown or closed: this
    /// device (and [`View::problem`] says why).
    pub fn for_links(&self) -> AppAccess {
        self.open_to()
            .map_or(AppAccess::ThisDevice, |o| o.min(self.setting))
    }

    /// The setting is not what is in force: why, in words for the owner and agents.
    pub fn problem(&self) -> Option<String> {
        let Some(s) = &self.status else {
            return Some("The filter for app ports hasn't run yet.".into());
        };
        if s.ok() && s.wanted == self.setting {
            return None;
        }
        let now = match self.open_to() {
            Some(AppAccess::ThisDevice) => "Apps are open to this device only.",
            Some(AppAccess::Tailnet) => "Apps are open to this device and the tailnet.",
            Some(AppAccess::HomeNetwork) => {
                "Apps are open to this device, the tailnet and the home network."
            }
            None if s.verified => "No device can open apps.",
            None => "The filter for app ports isn't working; apps may be closed.",
        };
        if s.wanted != self.setting {
            return Some(format!("Not applied yet. {now}"));
        }
        Some(format!("Couldn't apply this. {now}"))
    }

    /// The technical detail (the helper's errors), for logs and agents.
    pub fn errors(&self) -> Vec<String> {
        self.status
            .as_ref()
            .map(|s| s.errors.clone())
            .unwrap_or_default()
    }
}

/// Errors from [`AppAccessControl::set`].
#[derive(Debug, thiserror::Error)]
pub enum AppAccessError {
    #[error(transparent)]
    Save(#[from] SettingsError),
    /// Saved, but the filter didn't take it: what is in force is in the view.
    #[error("saved, but not applied: {0}")]
    Apply(String),
}

/// The setting, the helper that applies it and the audit log. Cheap to clone.
#[derive(Clone)]
pub struct AppAccessControl {
    inner: Arc<Inner>,
}

struct Inner {
    setting: PathBuf,
    status: PathBuf,
    applier: Arc<dyn Applier>,
    audit: Option<AuditLog>,
    lock: tokio::sync::Mutex<()>,
}

impl AppAccessControl {
    pub fn new(
        setting: PathBuf,
        status: PathBuf,
        applier: Arc<dyn Applier>,
        audit: Option<AuditLog>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                setting,
                status,
                applier,
                audit,
                lock: tokio::sync::Mutex::new(()),
            }),
        }
    }

    /// The container install: the setting in `config_dir`, the systemd unit, its status file.
    pub fn system(config_dir: &Path, audit: Option<AuditLog>) -> Self {
        Self::new(
            config_dir.join(FILE_NAME),
            PathBuf::from(STATUS_PATH),
            Arc::new(SystemdApplier::default()),
            audit,
        )
    }

    /// The saved setting (this device when it can't be read; the helper does the same).
    pub fn setting(&self) -> AppAccess {
        read_setting(&self.inner.setting).unwrap_or_else(|e| {
            tracing::warn!("{e}; apps are treated as open to this device only");
            AppAccess::ThisDevice
        })
    }

    pub fn view(&self) -> View {
        View {
            setting: self.setting(),
            status: read_status(&self.inner.status),
        }
    }

    /// Save `access` and apply it now (audited `settings.app_access`, by `actor`).
    /// `Err(Apply)`: saved but not in force; [`AppAccessControl::view`] says what is.
    pub async fn set(&self, access: AppAccess, actor: &str) -> Result<View, AppAccessError> {
        let _one_at_a_time = self.inner.lock.lock().await;
        let before = self.setting();
        write_setting(&self.inner.setting, access)?;
        let ran = self.inner.applier.apply().await;
        let view = self.view();
        // In force: the helper ran for this very setting and reports no problem.
        let applied = ran.is_ok()
            && view.status.as_ref().is_some_and(|s| s.wanted == access)
            && view.problem().is_none();
        let outcome = match (&ran, applied) {
            (_, true) => "applied".to_string(),
            (Err(e), false) => format!("not applied: {e}"),
            (Ok(()), false) => format!(
                "not applied: {}",
                view.errors()
                    .first()
                    .map_or("the filter did not take it", String::as_str)
            ),
        };
        self.audit(
            actor,
            &format!("apps open to: {access} (was {before}); {outcome}"),
        );
        if applied {
            Ok(view)
        } else {
            Err(AppAccessError::Apply(
                view.problem()
                    .unwrap_or_else(|| "the filter did not take it".into()),
            ))
        }
    }

    fn audit(&self, actor: &str, detail: &str) {
        let Some(log) = &self.inner.audit else {
            return;
        };
        let record = AuditRecord::now("settings.app_access", 0, "", detail).by(actor);
        if let Err(e) = log.append(&record) {
            tracing::error!(path = %log.path().display(), "audit log write failed: {e}");
        }
    }
}
