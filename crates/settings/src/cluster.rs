//! This device's place in a multi-device k3s cluster (layer 2, opt-in; groundwork from the
//! 2026-11-27 experiment, docs/design/INTERCONNECT-2026-11-27.md). Container install only.
//!
//! Default: a single-node k3s **server**, exactly as before. The owner may make this device a
//! k3s **agent** of another Melissa (join), make it a server that others join (invitation), and
//! go back (leave). The pieces:
//!
//! - **The setting** ([`FILE_NAME`], `/etc/melissa/cluster.toml`, written by the server's user
//!   `melissa`): role, server URL, this device's tailnet address and interface, and the cluster
//!   peers (tailnet addresses) the firewall lets in ([`crate::app_access`]). No secret.
//! - **The join token**: the server writes it to [`JOIN_TOKEN_FILE`] (0600) next to the
//!   setting; root moves it into [`STATE_DIR`] (0700 root) at the next k3s start and k3s reads
//!   it from there (`token-file`). It is never logged, never in the setting, never shown to agents.
//! - **The node name** ([`node_label`], [`decide_name`]): from the device name set in setup
//!   (`ui.toml`), else the Tailscale name install.sh passed (`melissa-<host>`), else the host
//!   name. It is pinned in [`STATE_DIR`] the first time k3s starts, because k3s registers a
//!   *new* node when the name changes: local-path volumes stay bound to the old one. A pinned
//!   name changes only while that is harmless (a server with no app volumes and no peers), or
//!   when the device joins a cluster (a fresh agent data directory).
//! - **`melissa-server cluster prepare`** (root, from k3s-prepare before every k3s start): reads
//!   all that, switches the k3s data directory when the role changed (kept as a backup, never
//!   deleted), and renders k3s's configuration: a server gets
//!   `/etc/rancher/k3s/config.yaml.d/50-melissa-cluster.yaml`; an agent gets its own config
//!   directory ([`AGENT_CONFIG`]) without the server-only keys. It writes the mode for the unit
//!   ([`ENV_FILE`]) and a status for the server ([`STATUS_FILE`]: role and node name, no secret).
//! - **`melissa-server cluster run`** (root, `melissa-cluster.service`, started by the server
//!   through polkit): the actions that need k3s's admin rights ([`Request`]): a join token for
//!   an invitation, draining and deleting a removed device's node, restarting k3s to apply a
//!   role change.
//! - [`ClusterControl`]: the server's side (owner-only actions; no agent tool sees a token).

use std::{
    fs::OpenOptions,
    io::{Read, Write},
    net::IpAddr,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use melissa_agent::{AuditLog, AuditRecord, BoxFuture};
use serde::{Deserialize, Serialize};

use crate::{factory_reset::at, write_private, SettingsError};

#[cfg(test)]
mod tests;

/// The setting, next to `app-access.toml` (`/etc/melissa` in the container).
pub const FILE_NAME: &str = "cluster.toml";
/// A join token on its way from the server to root (0600, server's user; root removes it).
pub const JOIN_TOKEN_FILE: &str = "cluster-join.token";
/// What the server asks root to do ([`Request`], JSON), next to the setting.
pub const REQUEST_FILE: &str = "cluster-request.json";
/// Root's answer ([`Answer`], JSON), next to the setting, owned by the server's user.
pub const ANSWER_FILE: &str = "cluster-answer.json";
/// A new invitation's join token, from root to the server (0600, server's user; read once).
pub const INVITATION_TOKEN_FILE: &str = "cluster-invitation.token";
/// Root's own state: the pinned node name, the agent's token, the data role (0700 root).
pub const STATE_DIR: &str = "/var/lib/melissa-container/cluster";
/// k3s's data directory.
pub const K3S_DATA: &str = "/var/lib/rancher/k3s";
/// Where a single-node server's data waits while this device is an agent.
pub const SERVER_BACKUP: &str = "/var/lib/rancher/k3s.single-node-backup";
/// The last agent data directory, kept after leaving (one; an older one is replaced).
pub const AGENT_BACKUP: &str = "/var/lib/rancher/k3s.agent-backup";
/// k3s's drop-in directory (server role).
pub const K3S_CONFIG_D: &str = "/etc/rancher/k3s/config.yaml.d";
/// The drop-in this module writes in server role.
pub const SERVER_DROPIN: &str = "/etc/rancher/k3s/config.yaml.d/50-melissa-cluster.yaml";
/// Server-only k3s settings shipped for every packaging (`write-kubeconfig-mode`): never
/// copied into the agent's config.
const SERVER_ONLY_DROPINS: [&str; 2] = ["10-melissa.yaml", "50-melissa-cluster.yaml"];
/// Root's runtime directory (0755 root): status, the unit's environment, the agent config.
pub const RUN_DIR: &str = "/run/melissa-cluster";
pub const STATUS_FILE: &str = "/run/melissa-cluster/status.json";
pub const ENV_FILE: &str = "/run/melissa-cluster/k3s.env";
/// The agent's k3s config (`--config`; k3s reads `config.yaml.d` next to it).
pub const AGENT_CONFIG: &str = "/run/melissa-cluster/k3s-agent/config.yaml";
/// The root oneshot that runs [`Request`]s.
pub const UNIT: &str = "melissa-cluster.service";
/// k3s's API port, the kubelet port and flannel's VXLAN port (k3s inbound rules).
pub const API_PORT: u16 = 6443;
pub const KUBELET_PORT: u16 = 10250;
pub const VXLAN_PORT: u16 = 8472;
/// The node label that says a node's role (`server`, `agent`), for placement. Under the
/// `node-restriction.kubernetes.io/` prefix, which k3s's NodeRestriction admission keeps a
/// kubelet from setting on its own node: only the server's root sets it ([`label_nodes`]), so an
/// agent can't relabel itself to draw work meant for another device.
pub const ROLE_LABEL: &str = "node-restriction.kubernetes.io/melissa-role";
/// How long an invitation's join token is valid.
pub const INVITATION_TTL: &str = "1h";
/// Most peers in the setting.
pub const MAX_PEERS: usize = 64;
const MAX_SETTING_BYTES: u64 = 16 * 1024;
/// Join tokens are short (k3s: `K10<hash>::<id>.<secret>` or `<id>.<secret>`).
const MAX_TOKEN_BYTES: u64 = 512;

// --- the setting -------------------------------------------------------------------------------

/// This device's k3s role.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Runs the control plane (single-node by default).
    #[default]
    Server,
    /// Joined another Melissa's k3s server as a worker.
    Agent,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Agent => "agent",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "server" => Some(Self::Server),
            "agent" => Some(Self::Agent),
            _ => None,
        }
    }
}

/// Another device in the cluster: a k3s node name and its tailnet address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Peer {
    pub name: String,
    pub address: IpAddr,
}

/// `cluster.toml`. The default is today's single-node server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Setting {
    pub role: Role,
    /// Agent: the server's API, `https://<tailnet address>:6443`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_url: Option<String>,
    /// This device's tailnet address, used as k3s's node IP once it is in a cluster.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_ip: Option<IpAddr>,
    /// The interface flannel uses between nodes (`tailscale0`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flannel_iface: Option<String>,
    /// The other devices: the firewall lets in k3s's ports from these addresses only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub peers: Vec<Peer>,
}

impl Setting {
    /// In a cluster (or about to be): peers, or the agent role.
    pub fn clustered(&self) -> bool {
        self.role == Role::Agent || !self.peers.is_empty()
    }

    /// Check every value (root renders them into k3s's config).
    pub fn validate(&self) -> Result<(), String> {
        if let Some(url) = &self.server_url {
            server_url_host(url)?;
        }
        if let Some(iface) = &self.flannel_iface {
            if !valid_iface(iface) {
                return Err("flannel_iface is not an interface name".into());
            }
        }
        if self.role == Role::Agent {
            if self.server_url.is_none() {
                return Err("an agent needs server_url".into());
            }
            if self.node_ip.is_none() {
                return Err("an agent needs node_ip (its tailnet address)".into());
            }
        }
        if self.peers.len() > MAX_PEERS {
            return Err(format!("more than {MAX_PEERS} peers"));
        }
        for p in &self.peers {
            if node_label(&p.name).as_deref() != Some(p.name.as_str()) {
                return Err("a peer's name is not a node name".into());
            }
        }
        Ok(())
    }

    /// The peers' addresses (the firewall's sources).
    pub fn peer_addresses(&self) -> Vec<IpAddr> {
        self.peers.iter().map(|p| p.address).collect()
    }
}

/// The host part of `https://<host>:<port>` (IP address or DNS name), checked.
pub fn server_url_host(url: &str) -> Result<String, String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or("server_url must start with https://")?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let (host, port) = if let Some(v6) = rest.strip_prefix('[') {
        let (h, p) = v6.split_once("]:").ok_or("server_url needs a port")?;
        h.parse::<std::net::Ipv6Addr>()
            .map_err(|_| "server_url has a bad IPv6 address")?;
        (h.to_string(), p)
    } else {
        let (h, p) = rest.rsplit_once(':').ok_or("server_url needs a port")?;
        let dns_ok = !h.is_empty()
            && h.len() <= 253
            && h.split('.').all(|l| {
                !l.is_empty()
                    && l.len() <= 63
                    && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                    && !l.starts_with('-')
                    && !l.ends_with('-')
            });
        if h.parse::<std::net::Ipv4Addr>().is_err() && !dns_ok {
            return Err("server_url has a bad host".into());
        }
        (h.to_string(), p)
    };
    match port.parse::<u16>() {
        Ok(p) if p > 0 => Ok(host),
        _ => Err("server_url has a bad port".into()),
    }
}

fn valid_iface(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 15
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Read a small file written by the server's user: no symlink, a regular file, bounded.
/// Errors never quote the file.
fn read_small(path: &Path, max: u64) -> Result<Option<String>, String> {
    let shown = path.display();
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path);
    let file = match file {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {shown}: {}", e.kind())),
    };
    let meta = file
        .metadata()
        .map_err(|e| format!("cannot read {shown}: {}", e.kind()))?;
    if !meta.is_file() {
        return Err(format!("{shown} is not a regular file"));
    }
    let mut text = String::new();
    file.take(max + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("cannot read {shown}: {}", e.kind()))?;
    if text.len() as u64 > max {
        return Err(format!("{shown} is too big"));
    }
    Ok(Some(text))
}

/// Read and check the setting. Missing: the default (single-node server).
pub fn read_setting(path: &Path) -> Result<Setting, String> {
    let Some(text) = read_small(path, MAX_SETTING_BYTES)? else {
        return Ok(Setting::default());
    };
    let setting: Setting =
        toml::from_str(&text).map_err(|_| format!("{} is not a valid setting", path.display()))?;
    setting
        .validate()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(setting)
}

fn write_setting(path: &Path, setting: &Setting) -> Result<(), SettingsError> {
    setting.validate().map_err(SettingsError::Invalid)?;
    let mut text = String::from(
        "# This device's place in a Melissa cluster, written by Melissa (no secrets here).\n\
         # Default (empty): a single-node k3s server.\n",
    );
    text.push_str(&toml::to_string(setting).map_err(|e| SettingsError::Failed(e.to_string()))?);
    write_private(path, text.as_bytes()).map_err(|source| SettingsError::Write {
        path: path.display().to_string(),
        source,
    })
}

// --- the node name -----------------------------------------------------------------------------

/// A Kubernetes node name from free text: lower case letters, digits and dashes (a DNS-1123
/// label), at most 63, no leading or trailing dash. `None` when nothing usable is left.
pub fn node_label(raw: &str) -> Option<String> {
    let mut name = String::new();
    for c in raw.trim().chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            name.push(c);
        } else if !name.is_empty() && !name.ends_with('-') {
            name.push('-');
        }
    }
    name.truncate(63);
    let name = name.trim_matches('-').to_string();
    (!name.is_empty()).then_some(name)
}

/// The node name this device should have: the device name from setup, else the Tailscale name
/// install.sh passed (`melissa-<host>`, unique per host), else the host name, else `melissa`.
pub fn desired_name(device: Option<&str>, tailscale: Option<&str>, host: Option<&str>) -> String {
    [device, tailscale, host]
        .into_iter()
        .flatten()
        .find_map(node_label)
        .unwrap_or_else(|| "melissa".into())
}

/// What [`decide_name`] needs to know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameFacts {
    /// The name pinned earlier, if any.
    pub pinned: Option<String>,
    /// [`desired_name`] now.
    pub desired: String,
    /// The name k3s used before names were pinned (the host name; `melissa` in the container).
    pub legacy: String,
    /// k3s has a data directory (it ran before).
    pub data_exists: bool,
    /// Apps keep data on this node (local-path volumes exist).
    pub app_volumes: bool,
    /// The setting puts this device in a cluster.
    pub clustered: bool,
}

/// The node name for this start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameDecision {
    /// Pin this name (first start).
    New(String),
    /// Keep the pinned name. `wanted`: a different desired name that can't be applied now.
    Keep {
        name: String,
        wanted: Option<String>,
    },
    /// Use the desired name; delete the old node object once k3s is up.
    Rename { from: String, to: String },
}

impl NameDecision {
    pub fn name(&self) -> &str {
        match self {
            Self::New(n) | Self::Keep { name: n, .. } | Self::Rename { to: n, .. } => n,
        }
    }
}

/// Decide the node name. A renamed node is a new node to k3s, and local-path volumes stay
/// bound to the old one, so a pinned name changes only on a server with no app volumes and no
/// peers. (Joining a cluster starts a fresh agent data directory, which takes the desired name:
/// [`prepare`].)
pub fn decide_name(f: &NameFacts) -> NameDecision {
    let pinned = match &f.pinned {
        Some(p) => p.clone(),
        None if !f.data_exists => return NameDecision::New(f.desired.clone()),
        // Ran before names were pinned: k3s registered the host name.
        None => f.legacy.clone(),
    };
    if pinned == f.desired {
        return NameDecision::Keep {
            name: pinned,
            wanted: None,
        };
    }
    if !f.app_volumes && !f.clustered {
        return NameDecision::Rename {
            from: pinned,
            to: f.desired.clone(),
        };
    }
    NameDecision::Keep {
        name: pinned,
        wanted: Some(f.desired.clone()),
    }
}

// --- rendering ---------------------------------------------------------------------------------

fn yaml_str(s: &str) -> String {
    // Every value is checked (node label, IP, interface, URL), so no quote or newline is in it;
    // JSON string syntax is valid YAML all the same.
    serde_json::Value::String(s.to_string()).to_string()
}

/// The drop-in for a server.
pub fn server_dropin(node_name: &str, setting: &Setting) -> String {
    let mut out = String::from(
        "# Written by `melissa-server cluster prepare` before every k3s start (cluster.toml).\n\
         # Do not edit; local overrides go in 90-local.yaml.\n",
    );
    out.push_str(&format!("node-name: {}\n", yaml_str(node_name)));
    if let Some(ip) = setting.node_ip {
        out.push_str(&format!("node-ip: {}\n", yaml_str(&ip.to_string())));
        // Agents reach the API on the tailnet address: it must be in the certificate.
        out.push_str(&format!("tls-san:\n  - {}\n", yaml_str(&ip.to_string())));
    }
    if let Some(iface) = &setting.flannel_iface {
        out.push_str(&format!("flannel-iface: {}\n", yaml_str(iface)));
    }
    out
}

/// The agent's main config file (its `config.yaml.d` holds the shared, agent-valid drop-ins).
pub fn agent_config(node_name: &str, setting: &Setting, token_file: &Path) -> String {
    let mut out = String::from(
        "# Written by `melissa-server cluster prepare` before every k3s start (cluster.toml).\n",
    );
    if let Some(url) = &setting.server_url {
        out.push_str(&format!("server: {}\n", yaml_str(url)));
    }
    out.push_str(&format!(
        "token-file: {}\n",
        yaml_str(&token_file.display().to_string())
    ));
    out.push_str(&format!("node-name: {}\n", yaml_str(node_name)));
    if let Some(ip) = setting.node_ip {
        out.push_str(&format!("node-ip: {}\n", yaml_str(&ip.to_string())));
    }
    if let Some(iface) = &setting.flannel_iface {
        out.push_str(&format!("flannel-iface: {}\n", yaml_str(iface)));
    }
    out
}

/// The unit's environment: `MELISSA_K3S_MODE` (`k3s server|agent`), and for an agent the
/// arguments (its own config; overrides k3s-node-ip's default-route node IP).
pub fn env_file(role: Role, agent_config: &Path) -> String {
    match role {
        Role::Server => "MELISSA_K3S_MODE=server\n".into(),
        Role::Agent => format!(
            "MELISSA_K3S_MODE=agent\nMELISSA_K3S_ARGS=--config={}\n",
            agent_config.display()
        ),
    }
}

// --- root: prepare -----------------------------------------------------------------------------

/// What the server reads back (no secret). Root writes it at every k3s start.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub role: Role,
    /// This device's k3s node name.
    pub node_name: String,
    /// A desired name that can't be applied (app volumes or peers): `node_name` stays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wanted_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_url: Option<String>,
    #[serde(default)]
    pub peers: usize,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub at: u64,
}

/// Read the status root wrote (the server's side).
pub fn read_status(path: &Path) -> Option<Status> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// What `prepare` reads besides the files under `root`.
#[derive(Debug, Clone, Default)]
pub struct PrepareInput {
    /// The device name from setup (`ui.toml`), if any.
    pub device_name: Option<String>,
    /// `MELISSA_TAILSCALE_NAME` from install.sh.
    pub tailscale_name: Option<String>,
    /// The host name (`/proc/sys/kernel/hostname`).
    pub host_name: Option<String>,
}

impl PrepareInput {
    /// The real system: the device name from `ui.toml` in `config_dir` (read without following
    /// a symlink; anything unreadable is no name), the environment, the host name.
    pub fn system(config_dir: &Path) -> Self {
        let device_name = read_small(&config_dir.join(crate::prefs::FILE_NAME), 64 * 1024)
            .ok()
            .flatten()
            .and_then(|t| toml::from_str::<crate::prefs::Prefs>(&t).ok())
            .and_then(|p| p.device_name);
        Self {
            device_name,
            tailscale_name: std::env::var(crate::tailscale::NODE_NAME_ENV).ok(),
            host_name: std::fs::read_to_string("/proc/sys/kernel/hostname")
                .ok()
                .map(|h| h.trim().to_string()),
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Write a root-owned file: atomic, `mode`, never through a symlink.
fn write_root(path: &Path, data: &[u8], mode: u32) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let _ = std::fs::remove_file(&tmp);
    {
        let mut f = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(mode)
            .open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

fn read_line_file(path: &Path) -> Option<String> {
    read_small(path, 256)
        .ok()
        .flatten()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Does `dir` hold anything?
fn non_empty_dir(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_some())
}

/// Get k3s's data directory out of the way: kill what the last k3s left running and unmount
/// below it, then rename it to `to` (never deleted).
fn move_data(host: &dyn crate::factory_reset::Host, from: &Path, to: &Path) -> Result<(), String> {
    if to.symlink_metadata().is_ok() {
        return Err(format!(
            "{} exists; move or remove it by hand first",
            to.display()
        ));
    }
    if from.symlink_metadata().is_err() {
        return Ok(());
    }
    crate::factory_reset::kill_leftovers(host);
    crate::factory_reset::unmount_below(host, from, true)?;
    std::fs::rename(from, to)
        .map_err(|e| format!("moving {} to {}: {e}", from.display(), to.display()))
}

/// Root, before every k3s start (`k3s-prepare`): switch the data directory when the role
/// changed, pin the node name, render k3s's config and the unit's environment, write the
/// status. `say`: one line per step for the journal (never a token). An error means k3s must
/// not start as asked (the unit fails and systemd retries).
pub fn prepare(
    root: &Path,
    config_dir: &Path,
    input: &PrepareInput,
    host: &dyn crate::factory_reset::Host,
    say: &mut dyn FnMut(&str),
) -> Result<Status, String> {
    let state = at(root, STATE_DIR);
    std::fs::create_dir_all(&state).map_err(|e| format!("creating {}: {e}", state.display()))?;
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("{}: {e}", state.display()))?;
    let mut errors = Vec::new();
    let setting = match read_setting(&config_dir.join(FILE_NAME)) {
        Ok(s) => s,
        Err(e) => {
            // Never guess a cluster from a broken file: stay what the data directory is, alone.
            say(&format!("cluster: {e}; staying a single-node server"));
            errors.push(e);
            Setting::default()
        }
    };
    let data = at(root, K3S_DATA);
    let role_file = state.join("data-role");
    let data_role = read_line_file(&role_file)
        .and_then(|r| Role::parse(&r))
        .unwrap_or_default();
    let pin_file = state.join("node-name");
    let token = state.join("agent-token");
    let desired = desired_name(
        input.device_name.as_deref(),
        input.tailscale_name.as_deref(),
        input.host_name.as_deref(),
    );

    // 1. A new join token from the server: root's, 0600.
    let handed = config_dir.join(JOIN_TOKEN_FILE);
    match read_small(&handed, MAX_TOKEN_BYTES) {
        Ok(Some(t)) if !t.trim().is_empty() => {
            write_root(&token, t.trim().as_bytes(), 0o600)
                .map_err(|e| format!("saving the join token: {e}"))?;
            let _ = std::fs::remove_file(&handed);
            say("cluster: took the new join token");
        }
        Ok(_) => {}
        Err(e) => errors.push(e),
    }

    // 2. Role switch.
    let mut role = setting.role;
    if role == Role::Agent && !token.exists() {
        let e = "no join token: staying a server".to_string();
        say(&format!("cluster: {e}"));
        errors.push(e);
        role = Role::Server;
    }
    let mut fresh_agent = false;
    if role != data_role {
        match role {
            Role::Agent => {
                // The single-node server's data (apps, volumes) waits in the backup.
                move_data(host, &data, &at(root, SERVER_BACKUP))?;
                if let Some(name) = read_line_file(&pin_file) {
                    write_root(&state.join("server-node-name"), name.as_bytes(), 0o600)
                        .map_err(|e| e.to_string())?;
                }
                let _ = std::fs::remove_file(&pin_file);
                fresh_agent = true;
                say("cluster: becoming a k3s agent; the server's data is kept in k3s.single-node-backup");
            }
            Role::Server => {
                let backup = at(root, AGENT_BACKUP);
                if backup.symlink_metadata().is_ok() {
                    std::fs::remove_dir_all(&backup)
                        .map_err(|e| format!("removing the old {}: {e}", backup.display()))?;
                }
                move_data(host, &data, &backup)?;
                let server = at(root, SERVER_BACKUP);
                if server.exists() {
                    std::fs::rename(&server, &data)
                        .map_err(|e| format!("restoring the server's data: {e}"))?;
                    say("cluster: back to a server; restored the single-node data");
                } else {
                    say("cluster: back to a server (no single-node data kept: a fresh one)");
                }
                match read_line_file(&state.join("server-node-name")) {
                    Some(name) => {
                        write_root(&pin_file, name.as_bytes(), 0o600).map_err(|e| e.to_string())?
                    }
                    None => {
                        let _ = std::fs::remove_file(&pin_file);
                    }
                }
                let _ = std::fs::remove_file(state.join("server-node-name"));
                let _ = std::fs::remove_file(&token);
            }
        }
        write_root(&role_file, role.as_str().as_bytes(), 0o600).map_err(|e| e.to_string())?;
    }

    // 3. The node name.
    let decision = if fresh_agent {
        NameDecision::New(desired.clone())
    } else {
        decide_name(&NameFacts {
            pinned: read_line_file(&pin_file).and_then(|n| node_label(&n)),
            desired: desired.clone(),
            legacy: input
                .host_name
                .as_deref()
                .and_then(node_label)
                .unwrap_or_else(|| "melissa".into()),
            data_exists: data.exists(),
            app_volumes: non_empty_dir(&data.join("storage")),
            clustered: setting.clustered(),
        })
    };
    let stale = state.join("stale-node");
    match &decision {
        NameDecision::Rename { from, to } if role == Role::Server => {
            write_root(&stale, from.as_bytes(), 0o600).map_err(|e| e.to_string())?;
            say(&format!("cluster: node name {from} → {to} (no app volumes yet)"));
        }
        NameDecision::Keep {
            name,
            wanted: Some(w),
        } => say(&format!(
            "cluster: node name stays {name} (wanted {w}: apps keep data on this node or it is in a cluster)"
        )),
        _ => {}
    }
    let name = decision.name().to_string();
    write_root(&pin_file, name.as_bytes(), 0o600).map_err(|e| e.to_string())?;

    // 4. k3s's config.
    let dropin = at(root, SERVER_DROPIN);
    let agent_cfg = at(root, AGENT_CONFIG);
    let agent_dir = agent_cfg
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    match role {
        Role::Server => {
            write_root(&dropin, server_dropin(&name, &setting).as_bytes(), 0o644)
                .map_err(|e| format!("writing {}: {e}", dropin.display()))?;
            let _ = std::fs::remove_dir_all(&agent_dir);
        }
        Role::Agent => {
            let _ = std::fs::remove_file(&dropin);
            let _ = std::fs::remove_dir_all(&agent_dir);
            let d = agent_dir.join("config.yaml.d");
            std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
            // The shared drop-ins (container settings, local overrides), minus server-only ones.
            if let Ok(entries) = std::fs::read_dir(at(root, K3S_CONFIG_D)) {
                let mut names: Vec<_> = entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.ends_with(".yaml") && !SERVER_ONLY_DROPINS.contains(&n.as_str()))
                    .collect();
                names.sort();
                for n in names {
                    let src = at(root, K3S_CONFIG_D).join(&n);
                    if let Ok(bytes) = std::fs::read(&src) {
                        write_root(&d.join(&n), &bytes, 0o644).map_err(|e| e.to_string())?;
                    }
                }
            }
            // k3s reads the token file itself; the path is root's 0600 file.
            let token_in_k3s = Path::new(STATE_DIR).join("agent-token");
            write_root(
                &agent_cfg,
                agent_config(&name, &setting, &token_in_k3s).as_bytes(),
                0o600,
            )
            .map_err(|e| e.to_string())?;
        }
    }
    let env = at(root, ENV_FILE);
    write_root(
        &env,
        env_file(role, Path::new(AGENT_CONFIG)).as_bytes(),
        0o644,
    )
    .map_err(|e| e.to_string())?;

    let wanted_name = match &decision {
        NameDecision::Keep { wanted, .. } => wanted.clone(),
        _ => None,
    };
    let status = Status {
        role,
        node_name: name.clone(),
        wanted_name,
        server_url: (role == Role::Agent)
            .then(|| setting.server_url.clone())
            .flatten(),
        peers: setting.peers.len(),
        errors,
        at: now(),
    };
    let data = serde_json::to_vec_pretty(&status).map_err(|e| e.to_string())?;
    write_root(&at(root, STATUS_FILE), &data, 0o644).map_err(|e| e.to_string())?;
    say(&format!("cluster: {} {name}", role.as_str()));
    Ok(status)
}

/// The old node object after a rename (server only): `Some(name)` once, then the marker goes.
pub fn take_stale_node(root: &Path) -> Option<String> {
    let path = at(root, STATE_DIR).join("stale-node");
    let name = read_line_file(&path).and_then(|n| node_label(&n));
    let _ = std::fs::remove_file(&path);
    name
}

// --- root: requests ----------------------------------------------------------------------------

/// What the server asks root for (`melissa-cluster.service`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// A join token for one device (`k3s token create --ttl 1h`), back to the server only.
    CreateToken,
    /// Drain and delete a device's node.
    RemoveNode { name: String },
    /// Restart k3s so `prepare` applies the setting (a role change, a new node IP).
    Restart,
}

/// Root's answer (no secret: a token goes in its own file).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    pub at: u64,
}

/// What a request needs from the system (injected for tests).
pub trait Runner {
    /// `k3s token create --ttl <ttl>` (stdout: the token; never logged).
    fn create_token(&self, ttl: &str) -> Result<String, String>;
    /// `k3s kubectl drain` then `delete node`.
    fn remove_node(&self, name: &str) -> Result<(), String>;
    /// `systemctl restart k3s.service`.
    fn restart_k3s(&self) -> Result<(), String>;
    /// The nodes and their [`ROLE_LABEL`]: one `<name> <value>` line each (value may be empty).
    fn node_roles(&self) -> Result<String, String>;
    /// Set [`ROLE_LABEL`] on a node (`kubectl label --overwrite`).
    fn label_node(&self, name: &str, role: Role) -> Result<(), String>;
}

/// The role labels to set: this device's node `own` gets `server`, the peers' nodes `agent`;
/// only nodes that exist (`listed`, [`Runner::node_roles`]) and don't have it yet. Nothing on an
/// agent (its server labels the nodes) or on a server without peers.
pub fn role_labels_needed(setting: &Setting, own: &str, listed: &str) -> Vec<(String, Role)> {
    if setting.role != Role::Server || setting.peers.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for line in listed.lines() {
        let mut parts = line.split_whitespace();
        let (Some(name), value) = (parts.next(), parts.next().unwrap_or("")) else {
            continue;
        };
        let want = if name == own {
            Role::Server
        } else if setting.peers.iter().any(|p| p.name == name) {
            Role::Agent
        } else {
            continue;
        };
        if value != want.as_str() && node_label(name).as_deref() == Some(name) {
            out.push((name.to_string(), want));
        }
    }
    out
}

/// Root on the server (`melissa-server cluster label`: after k3s starts and every minute,
/// `melissa-cluster-label.timer`): label this device's and the joined devices' nodes with their
/// role ([`ROLE_LABEL`]). Returns how many were labelled. Cheap when there is nothing to do: no
/// peers, no kubectl.
pub fn label_nodes(root: &Path, config_dir: &Path, runner: &dyn Runner) -> Result<usize, String> {
    let setting = read_setting(&config_dir.join(FILE_NAME))?;
    if setting.role != Role::Server || setting.peers.is_empty() {
        return Ok(0);
    }
    let own = read_status(&at(root, STATUS_FILE))
        .map(|s| s.node_name)
        .unwrap_or_default();
    let needed = role_labels_needed(&setting, &own, &runner.node_roles()?);
    let mut errors = Vec::new();
    for (name, role) in &needed {
        if let Err(e) = runner.label_node(name, *role) {
            errors.push(format!("{name}: {e}"));
        }
    }
    if errors.is_empty() {
        Ok(needed.len())
    } else {
        Err(errors.join("; "))
    }
}

/// Write a file into the server's directory as root, owned by the directory's owner, 0600,
/// never through an existing path (symlink or not).
fn hand_over(dir: &Path, name: &str, data: &[u8]) -> Result<(), String> {
    let owner = std::fs::metadata(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .uid();
    let path = dir.join(name);
    let _ = std::fs::remove_file(&path);
    let mut f = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    rustix::fs::fchown(&f, Some(rustix::fs::Uid::from_raw(owner)), None)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(data).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())
}

/// Root: run the request the server left in `config_dir`, answer there. The request file goes
/// either way.
pub fn run_request(config_dir: &Path, runner: &dyn Runner) -> Answer {
    let path = config_dir.join(REQUEST_FILE);
    let request = read_small(&path, 4096).and_then(|t| {
        let t = t.ok_or("no request")?;
        serde_json::from_str::<Request>(&t).map_err(|_| "not a valid request".to_string())
    });
    let _ = std::fs::remove_file(&path);
    let result = request.and_then(|r| match r {
        Request::CreateToken => {
            let token = runner.create_token(INVITATION_TTL)?;
            hand_over(config_dir, INVITATION_TOKEN_FILE, token.as_bytes())
        }
        Request::RemoveNode { name } => {
            if node_label(&name).as_deref() != Some(name.as_str()) {
                return Err("not a node name".into());
            }
            runner.remove_node(&name)
        }
        Request::Restart => runner.restart_k3s(),
    });
    let answer = Answer {
        ok: result.is_ok(),
        error: result.err(),
        at: now(),
    };
    let data = serde_json::to_vec(&answer).unwrap_or_default();
    if let Err(e) = hand_over(config_dir, ANSWER_FILE, &data) {
        tracing::error!("cluster: cannot answer the server: {e}");
    }
    answer
}

/// Run `program args` with a timeout; stdout back (never logged), the first stderr line on
/// failure.
fn output(program: &str, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {program}: {}", e.kind()))?;
    let until = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(100)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{program} did not finish in time"));
            }
            Err(e) => return Err(format!("waiting for {program}: {e}")),
        }
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let first = crate::clean_text(
            err.lines().find(|l| !l.trim().is_empty()).unwrap_or(""),
            200,
        );
        Err(format!("{program} failed ({}): {first}", out.status))
    }
}

/// The real k3s and systemd.
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn create_token(&self, ttl: &str) -> Result<String, String> {
        let ttl = format!("--ttl={ttl}");
        let out = output(
            "/usr/bin/k3s",
            &["token", "create", &ttl, "--description=melissa invitation"],
            Duration::from_secs(60),
        )?;
        let token = out.trim();
        if token.is_empty()
            || token.len() > MAX_TOKEN_BYTES as usize
            || token.contains(char::is_whitespace)
        {
            return Err("k3s gave no usable token".into());
        }
        Ok(token.to_string())
    }

    fn remove_node(&self, name: &str) -> Result<(), String> {
        let node = format!("node/{name}");
        // Best effort: an offline node can't be drained; its pods go when the node goes.
        let _ = output(
            "/usr/bin/k3s",
            &[
                "kubectl",
                "drain",
                name,
                "--ignore-daemonsets",
                "--delete-emptydir-data",
                "--timeout=120s",
            ],
            Duration::from_secs(150),
        );
        output(
            "/usr/bin/k3s",
            &["kubectl", "delete", &node, "--ignore-not-found"],
            Duration::from_secs(60),
        )
        .map(|_| ())
    }

    fn restart_k3s(&self) -> Result<(), String> {
        output(
            "systemctl",
            &["restart", "--no-block", "k3s.service"],
            Duration::from_secs(30),
        )
        .map(|_| ())
    }

    fn node_roles(&self) -> Result<String, String> {
        let key = ROLE_LABEL.replace('.', "\\.");
        let template = format!(
            "jsonpath={{range .items[*]}}{{.metadata.name}} {{.metadata.labels.{key}}}{{\"\\n\"}}{{end}}"
        );
        output(
            "/usr/bin/k3s",
            &["kubectl", "get", "nodes", "-o", &template],
            Duration::from_secs(60),
        )
    }

    fn label_node(&self, name: &str, role: Role) -> Result<(), String> {
        let label = format!("{ROLE_LABEL}={}", role.as_str());
        output(
            "/usr/bin/k3s",
            &[
                "kubectl",
                "label",
                "--overwrite",
                &format!("node/{name}"),
                &label,
            ],
            Duration::from_secs(60),
        )
        .map(|_| ())
    }
}

/// Root, after k3s is up (`ExecStartPost`): delete the old node object after a rename.
pub fn after_start(root: &Path) -> Result<(), String> {
    let Some(old) = take_stale_node(root) else {
        return Ok(());
    };
    let status = read_status(&at(root, STATUS_FILE));
    if status.as_ref().is_some_and(|s| s.node_name == old) {
        return Ok(());
    }
    output(
        "/usr/bin/k3s",
        &[
            "kubectl",
            "delete",
            &format!("node/{old}"),
            "--ignore-not-found",
            "--wait=false",
        ],
        Duration::from_secs(60),
    )
    .map(|_| ())
}

// --- the server's side ---------------------------------------------------------------------------

/// Starts the root oneshot and waits for it (the server's user, through polkit).
pub trait Starter: Send + Sync {
    fn start(&self) -> BoxFuture<'_, Result<(), String>>;
    /// Re-apply the firewall (`melissa-app-access.service`) after the peers changed.
    fn refresh_firewall(&self) -> BoxFuture<'_, Result<(), String>>;
}

async fn systemctl_start(unit: &str, timeout: Duration) -> Result<(), String> {
    let mut cmd = tokio::process::Command::new("systemctl");
    cmd.args(["start", unit])
        .stdin(Stdio::null())
        .kill_on_drop(true);
    match tokio::time::timeout(timeout, cmd.output()).await {
        Err(_) => Err(format!("{unit} did not answer in time")),
        Ok(Err(e)) => Err(format!("cannot run systemctl: {e}")),
        Ok(Ok(out)) if out.status.success() => Ok(()),
        Ok(Ok(_)) => Err(format!("{unit} failed")),
    }
}

/// `systemctl start melissa-cluster.service` and `melissa-app-access.service`.
pub struct SystemdStarter;

impl Starter for SystemdStarter {
    fn start(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(systemctl_start(UNIT, Duration::from_secs(240)))
    }

    fn refresh_firewall(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(systemctl_start(
            crate::app_access::UNIT,
            Duration::from_secs(30),
        ))
    }
}

/// A join token, shown to the owner once. Never logged, never `Debug`-printed, never in a tool
/// result.
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(…)")
    }
}

/// What the owner hands to the joining device.
#[derive(Debug)]
pub struct Invitation {
    /// `https://<this device's tailnet address>:6443`.
    pub server_url: String,
    pub token: Secret,
}

/// Errors from [`ClusterControl`]'s actions (no message holds a token).
#[derive(Debug, thiserror::Error)]
pub enum ClusterError {
    #[error(transparent)]
    Save(#[from] SettingsError),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Failed(String),
}

/// The setting, the root oneshot and the audit log, for the owner's actions. Cheap to clone.
/// **Owner-only**: callers gate every action like Settings' actions (signed-in owner, password
/// again); no agent tool calls these (agents get [`ClusterControl::status`] only).
#[derive(Clone)]
pub struct ClusterControl {
    inner: Arc<Inner>,
}

struct Inner {
    config_dir: PathBuf,
    status: PathBuf,
    starter: Arc<dyn Starter>,
    audit: Option<AuditLog>,
    lock: tokio::sync::Mutex<()>,
}

impl ClusterControl {
    pub fn new(
        config_dir: PathBuf,
        status: PathBuf,
        starter: Arc<dyn Starter>,
        audit: Option<AuditLog>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                config_dir,
                status,
                starter,
                audit,
                lock: tokio::sync::Mutex::new(()),
            }),
        }
    }

    /// The container install.
    pub fn system(config_dir: &Path, audit: Option<AuditLog>) -> Self {
        Self::new(
            config_dir.to_path_buf(),
            PathBuf::from(STATUS_FILE),
            Arc::new(SystemdStarter),
            audit,
        )
    }

    fn setting_path(&self) -> PathBuf {
        self.inner.config_dir.join(FILE_NAME)
    }

    /// The setting (default when unreadable).
    pub fn setting(&self) -> Setting {
        read_setting(&self.setting_path()).unwrap_or_else(|e| {
            tracing::warn!("{e}; treated as a single-node server");
            Setting::default()
        })
    }

    /// What root last applied (role and node name; no secret). Safe for agents.
    pub fn status(&self) -> Option<Status> {
        read_status(&self.inner.status)
    }

    /// This device's k3s node name, as root applied it.
    pub fn own_node_name(&self) -> Option<String> {
        self.status().map(|s| s.node_name).filter(|n| !n.is_empty())
    }

    async fn request(&self, request: &Request) -> Result<(), ClusterError> {
        let dir = &self.inner.config_dir;
        let _ = std::fs::remove_file(dir.join(ANSWER_FILE));
        let data = serde_json::to_vec(request).map_err(|e| ClusterError::Failed(e.to_string()))?;
        write_private(&dir.join(REQUEST_FILE), &data).map_err(|source| {
            ClusterError::Save(SettingsError::Write {
                path: REQUEST_FILE.into(),
                source,
            })
        })?;
        self.inner
            .starter
            .start()
            .await
            .map_err(ClusterError::Failed)?;
        let answer = read_small(&dir.join(ANSWER_FILE), 4096)
            .ok()
            .flatten()
            .and_then(|t| serde_json::from_str::<Answer>(&t).ok());
        let _ = std::fs::remove_file(dir.join(ANSWER_FILE));
        match answer {
            Some(a) if a.ok => Ok(()),
            Some(a) => Err(ClusterError::Failed(
                a.error
                    .unwrap_or_else(|| "the cluster helper failed".into()),
            )),
            None => Err(ClusterError::Failed(
                "the cluster helper did not answer".into(),
            )),
        }
    }

    fn audit(&self, actor: &str, action: &str, detail: &str) {
        let Some(log) = &self.inner.audit else {
            return;
        };
        let record = AuditRecord::now(action, 0, "", detail).by(actor);
        if let Err(e) = log.append(&record) {
            tracing::error!(path = %log.path().display(), "audit log write failed: {e}");
        }
    }

    /// Invite a device: this device stays (or becomes) the server on its tailnet address
    /// `own_address`, lets `peer` in through the firewall, and gets a join token valid
    /// [`INVITATION_TTL`] for the owner to hand over. A first invitation restarts k3s (node IP
    /// and flannel move to the tailnet).
    pub async fn create_invitation(
        &self,
        own_address: IpAddr,
        peer: Peer,
        actor: &str,
    ) -> Result<Invitation, ClusterError> {
        let _one = self.inner.lock.lock().await;
        let mut s = self.setting();
        if s.role != Role::Server {
            return Err(ClusterError::Invalid(
                "this device is an agent in another cluster".into(),
            ));
        }
        if node_label(&peer.name).as_deref() != Some(peer.name.as_str()) {
            return Err(ClusterError::Invalid("not a device name".into()));
        }
        let moved = s.node_ip != Some(own_address)
            || s.flannel_iface.as_deref() != Some(crate::tailscale::INTERFACE);
        s.node_ip = Some(own_address);
        s.flannel_iface = Some(crate::tailscale::INTERFACE.into());
        s.peers
            .retain(|p| p.name != peer.name && p.address != peer.address);
        s.peers.push(peer.clone());
        write_setting(&self.setting_path(), &s)?;
        if let Err(e) = self.inner.starter.refresh_firewall().await {
            tracing::warn!("cluster: firewall not refreshed: {e}");
        }
        // The token first (from the running server), then the restart that moves the node IP
        // and flannel to the tailnet (and puts the address in the API certificate).
        self.request(&Request::CreateToken).await?;
        let path = self.inner.config_dir.join(INVITATION_TOKEN_FILE);
        let token = read_small(&path, MAX_TOKEN_BYTES)
            .ok()
            .flatten()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        let _ = std::fs::remove_file(&path);
        let token = token.ok_or_else(|| ClusterError::Failed("no join token came back".into()))?;
        if moved {
            self.request(&Request::Restart).await?;
        }
        self.audit(
            actor,
            "cluster.invitation_created",
            &format!(
                "invited {} ({}), token valid {INVITATION_TTL}",
                peer.name, peer.address
            ),
        );
        let host = match own_address {
            IpAddr::V4(v4) => v4.to_string(),
            IpAddr::V6(v6) => format!("[{v6}]"),
        };
        Ok(Invitation {
            server_url: format!("https://{host}:{API_PORT}"),
            token: Secret(token),
        })
    }

    /// Join a server: this device becomes its k3s agent. The single-node data is kept as a
    /// backup (apps here stop until [`ClusterControl::leave`]).
    pub async fn join(
        &self,
        server_url: &str,
        server_name: &str,
        own_address: IpAddr,
        token: Secret,
        actor: &str,
    ) -> Result<(), ClusterError> {
        let _one = self.inner.lock.lock().await;
        let host = server_url_host(server_url).map_err(ClusterError::Invalid)?;
        let server_address: IpAddr = host.parse().map_err(|_| {
            ClusterError::Invalid("server_url must name the server's tailnet address".into())
        })?;
        let server_name = node_label(server_name)
            .ok_or_else(|| ClusterError::Invalid("not a device name".into()))?;
        let t = token.expose().trim();
        if t.is_empty() || t.len() > MAX_TOKEN_BYTES as usize || t.contains(char::is_whitespace) {
            return Err(ClusterError::Invalid("that is not a join token".into()));
        }
        let s = Setting {
            role: Role::Agent,
            server_url: Some(server_url.trim_end_matches('/').to_string()),
            node_ip: Some(own_address),
            flannel_iface: Some(crate::tailscale::INTERFACE.into()),
            peers: vec![Peer {
                name: server_name.clone(),
                address: server_address,
            }],
        };
        s.validate().map_err(ClusterError::Invalid)?;
        write_private(&self.inner.config_dir.join(JOIN_TOKEN_FILE), t.as_bytes()).map_err(
            |source| {
                ClusterError::Save(SettingsError::Write {
                    path: JOIN_TOKEN_FILE.into(),
                    source,
                })
            },
        )?;
        write_setting(&self.setting_path(), &s)?;
        if let Err(e) = self.inner.starter.refresh_firewall().await {
            tracing::warn!("cluster: firewall not refreshed: {e}");
        }
        let restarted = self.request(&Request::Restart).await;
        self.audit(
            actor,
            "cluster.join",
            &format!(
                "joining {server_name} ({server_address}) as an agent; {}",
                if restarted.is_ok() {
                    "k3s restarting"
                } else {
                    "k3s not restarted"
                }
            ),
        );
        restarted
    }

    /// Remove a device from this server's cluster: drain and delete its node, drop it from the
    /// peers (the firewall closes to it).
    pub async fn remove_device(&self, name: &str, actor: &str) -> Result<(), ClusterError> {
        let _one = self.inner.lock.lock().await;
        let mut s = self.setting();
        if s.role != Role::Server {
            return Err(ClusterError::Invalid(
                "only the server removes devices".into(),
            ));
        }
        if node_label(name).as_deref() != Some(name) {
            return Err(ClusterError::Invalid("not a device name".into()));
        }
        if self.own_node_name().as_deref() == Some(name) {
            return Err(ClusterError::Invalid("that is this device".into()));
        }
        let removed = self
            .request(&Request::RemoveNode { name: name.into() })
            .await;
        s.peers.retain(|p| p.name != name);
        write_setting(&self.setting_path(), &s)?;
        if let Err(e) = self.inner.starter.refresh_firewall().await {
            tracing::warn!("cluster: firewall not refreshed: {e}");
        }
        self.audit(
            actor,
            "cluster.device_removed",
            &format!(
                "removed {name}; {}",
                if removed.is_ok() {
                    "node deleted"
                } else {
                    "node not deleted"
                }
            ),
        );
        removed
    }

    /// Leave the cluster: back to a single-node server (its kept data comes back), no peers.
    pub async fn leave(&self, actor: &str) -> Result<(), ClusterError> {
        let _one = self.inner.lock.lock().await;
        let before = self.setting();
        write_setting(&self.setting_path(), &Setting::default())?;
        let _ = std::fs::remove_file(self.inner.config_dir.join(JOIN_TOKEN_FILE));
        if let Err(e) = self.inner.starter.refresh_firewall().await {
            tracing::warn!("cluster: firewall not refreshed: {e}");
        }
        let restarted = self.request(&Request::Restart).await;
        self.audit(
            actor,
            "cluster.leave",
            &format!(
                "left the cluster (was {}, {} peers); {}",
                before.role.as_str(),
                before.peers.len(),
                if restarted.is_ok() {
                    "k3s restarting"
                } else {
                    "k3s not restarted"
                }
            ),
        );
        restarted
    }
}
