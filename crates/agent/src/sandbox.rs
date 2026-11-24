//! Bubblewrap (`bwrap`) sandbox for agent CLI processes (defence in depth).
//!
//! Inside the sandbox the CLI sees: `/usr` and `/etc` read-only (plus, on NixOS, the Nix store
//! and the current system profile: see [`SystemRoot`]), a private `/tmp`, an
//! empty, read-only `$HOME` with only the paths its [`SandboxSpec`] names, the CLI binary
//! (read-only), and the per-task working directory (the only writable host path). It gets
//! its own user, PID, IPC, UTS, cgroup and (unless `network = "host"`) network namespaces,
//! no capabilities and `no_new_privs`, and dies with its parent, so killing `bwrap` kills
//! every process inside, grandchildren included.
//!
//! Network ([`NetworkMode`]): by default the sandbox has only loopback. A [`Bridge`]
//! (`melissa-netbridge`, started first inside the sandbox) forwards loopback ports to Unix
//! sockets served by the lead agent's egress proxy ([`crate::egress`]): an allowlisting HTTPS
//! proxy (`HTTPS_PROXY` etc. point at it) and Melissa's MCP endpoint. No real resolver is
//! reachable inside; the proxy resolves names itself. For CLIs that look a name up before
//! using the proxy, a DNS sinkhole on `127.0.0.1:53` ([`crate::egress::dns`]) answers every
//! name with one fixed placeholder address; nothing is forwarded.
//!
//! Syscalls ([`seccomp`]): a seccomp filter makes the syscalls no agent CLI needs (ptrace,
//! mounts, new namespaces, kernel modules, bpf, io_uring, keyrings, ...) fail with `EPERM`.
//! bwrap reads the program from fd [`SECCOMP_FD`] ([`pass_seccomp`] puts it there).
//!
//! Resources ([`limits`]): where the host delegates cgroups to Melissa, every sandbox runs in its
//! own cgroup with memory, process and CPU limits. bwrap then waits on [`BLOCK_FD`] until
//! [`SandboxCommand::spawn`] has moved it into that cgroup.
//!
//! Users ([`users`]): where the server's user has subordinate ids, every agent's sandbox runs as
//! its own host uid/gid: what it creates is its own on the host, and escaped it owns nothing.
//! bwrap is then started through `nsenter` in a user namespace mapped for that agent instead of
//! making its own, and `setpriv` is the first program inside (it becomes the agent's user).
//!
//! The command line is built by the pure [`command`] function from a [`SandboxPlan`];
//! [`SandboxPlan::resolve`] does the host lookups (HOME, binary path, which files exist).

pub mod acl;
pub mod limits;
pub mod seccomp;
pub mod users;

use std::{
    ffi::OsString,
    io::Write as _,
    os::fd::RawFd,
    path::{Component, Path, PathBuf},
};

use command_fds::{CommandFdExt, FdMapping};
use serde::{Deserialize, Serialize};

use crate::egress::HostRule;
pub use crate::egress::NetworkMode;

/// Where bubblewrap is expected on Fedora (and the Melissa images).
pub const BWRAP_PATH: &str = "/usr/bin/bwrap";

/// Host name inside the sandbox (hides the real one).
const SANDBOX_HOSTNAME: &str = "melissa-agent";

/// Host environment variables passed into every sandbox when set. Everything else is
/// cleared; add names to [`SandboxSpec::env`] to pass more.
const BASE_ENV: &[&str] = &["USER", "LOGNAME", "LANG", "LC_ALL", "TZ"];

/// Environment variables the sandbox always sets itself; [`SandboxSpec::env`] may not name them.
const RESERVED_ENV: &[&str] = &[
    "HOME",
    "PATH",
    "NO_COLOR",
    "TERM",
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "ALL_PROXY",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
    "GROK_WEB_FETCH_PROXY",
    "NODE_USE_ENV_PROXY",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_KEY_0",
    "GIT_CONFIG_VALUE_0",
];

/// Where the bridge's socket directory appears inside the sandbox.
pub const BRIDGE_DIR: &str = "/run/melissa-net";

/// The sandbox a backend runs in: its built-in preset ([`SandboxSpec::preset`]) with the
/// config's `[backend.sandbox]` table ([`SandboxConfig`]) applied on top.
///
/// Paths are relative to `$HOME`. Missing `ro`/`rw` paths are skipped, so a CLI that is
/// not logged in simply sees no credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxSpec {
    /// Run the CLI under bubblewrap. Turning this off runs it as a plain process of the
    /// lead agent's user, with access to all of that user's files.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    /// Run the CLI under Melissa's seccomp filter ([`seccomp`]). On by default; turn it off
    /// only for a CLI that verifiably needs a denied syscall.
    #[serde(default = "enabled_by_default")]
    pub seccomp: bool,
    /// Memory, process and CPU limits (`[backend.sandbox.limits]`); defaults in
    /// [`limits::Limits::default`]. Applied where the host delegates cgroups ([`limits::init`]).
    #[serde(default)]
    pub limits: limits::Limits,
    /// Empty, writable, throw-away directories (tmpfs), e.g. a CLI's state directory.
    #[serde(default)]
    pub tmpfs: Vec<String>,
    /// Paths bound read-only.
    #[serde(default)]
    pub ro: Vec<String>,
    /// Paths bound read-write (e.g. a credentials file the CLI refreshes).
    #[serde(default)]
    pub rw: Vec<String>,
    /// Names of extra host environment variables to pass through (e.g. an API key variable).
    #[serde(default)]
    pub env: Vec<String>,
    /// Network access (set from the backend's `network` key, not from this table).
    #[serde(skip)]
    pub network: NetworkMode,
    /// Egress allowlist (set from the backend's `allow` key, not from this table).
    #[serde(skip)]
    pub allow: Option<Vec<String>>,
    /// Fixed variables (name, value) from Melissa's provider settings (`providers.toml`),
    /// set inside the sandbox, or on the process when the sandbox is off. Never in the
    /// config file's `[backend.sandbox]` table.
    #[serde(skip)]
    pub vars: Vec<(String, String)>,
}

fn enabled_by_default() -> bool {
    true
}

impl Default for SandboxSpec {
    fn default() -> Self {
        Self {
            enabled: true,
            seccomp: true,
            limits: limits::Limits::default(),
            tmpfs: Vec::new(),
            ro: Vec::new(),
            rw: Vec::new(),
            env: Vec::new(),
            network: NetworkMode::default(),
            allow: None,
            vars: Vec::new(),
        }
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

impl SandboxSpec {
    /// A spec that runs the CLI without a sandbox.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    /// The built-in sandbox for a command, chosen by its file name. Known CLIs get the
    /// narrowest set of paths verified to work (docs/agent-skills/agent-cli-backends.md);
    /// unknown commands get an empty home.
    pub fn preset(command: &str) -> Self {
        let name = Path::new(command)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        match name {
            // Claude Code: only the OAuth credentials (refreshed in place). Sessions,
            // history and `~/.claude.json` stay hidden; Claude starts with empty ones.
            "claude" => Self {
                tmpfs: strings(&[".claude"]),
                rw: strings(&[".claude/.credentials.json"]),
                ..Self::default()
            },
            // Grok CLI: the login and the user's settings; sessions and memory stay hidden.
            "grok" => Self {
                tmpfs: strings(&[".grok"]),
                ro: strings(&[".grok/config.toml"]),
                rw: strings(&[".grok/auth.json"]),
                ..Self::default()
            },
            // OpenCode: its config files read-only (not the whole directory: it also holds
            // `service.json`, a local server password, and plugins), provider logins
            // (`auth.json`) if any; the session database and caches are fresh per run.
            // `~/.opencode` (the official installer's directory) is scratch too: OpenCode
            // 1.19 writes `~/.opencode/.gitignore` on start (verified 2026-11-18); the binary
            // in `~/.opencode/bin` is still bound read-only inside it.
            "opencode" => Self {
                tmpfs: strings(&[
                    ".opencode",
                    ".local/share/opencode",
                    ".local/state/opencode",
                    ".cache/opencode",
                    ".config/opencode",
                ]),
                ro: strings(&[
                    ".config/opencode/opencode.json",
                    ".config/opencode/opencode.jsonc",
                ]),
                rw: strings(&[".local/share/opencode/auth.json"]),
                ..Self::default()
            },
            // Gemini CLI (npm, installed by melissa-setup into its own folder): that folder
            // read-only (the wrapper runs `node` on the package there), its state fresh per run
            // (`~/.gemini`: sessions, history, the user's settings, which could auto-approve),
            // and a Google sign-in, if any.
            "gemini" => Self {
                tmpfs: strings(&[".gemini"]),
                ro: strings(&[".local/share/melissa-npm/gemini"]),
                rw: strings(&[".gemini/oauth_creds.json"]),
                ..Self::default()
            },
            // Codex's ACP adapter (npm, with Codex itself): the package folder read-only,
            // `~/.codex` fresh per run (config, sessions, databases), the ChatGPT sign-in
            // (refreshed in place).
            "codex-acp" => Self {
                tmpfs: strings(&[".codex"]),
                ro: strings(&[".local/share/melissa-npm/codex-acp"]),
                rw: strings(&[".codex/auth.json"]),
                ..Self::default()
            },
            // Muse Code (one static binary): its settings, data, state and cache fresh per run;
            // the account sign-in (`muse login`) read-write.
            "muse" => Self {
                tmpfs: strings(&[
                    ".config/muse",
                    ".local/share/muse",
                    ".local/state/muse",
                    ".cache/muse",
                ]),
                rw: strings(&[".config/muse/auth.json"]),
                ..Self::default()
            },
            _ => Self::default(),
        }
    }

    /// OpenCode with a config Melissa writes (ACP flavor `opencode`): like the `opencode`
    /// preset but without the user's config files, whose permission rules could allow tools
    /// without asking.
    pub fn preset_opencode_managed() -> Self {
        Self {
            ro: Vec::new(),
            ..Self::preset("opencode")
        }
    }

    /// Reject paths that escape or expose all of `$HOME`, and bad environment names.
    pub fn validate(&self) -> Result<(), SandboxError> {
        for path in self.tmpfs.iter().chain(&self.ro).chain(&self.rw) {
            let p = Path::new(path);
            let normal = p.components().all(|c| matches!(c, Component::Normal(_)));
            if path.is_empty() || !normal {
                return Err(SandboxError::BadPath(path.clone()));
            }
        }
        for rule in self.allow.iter().flatten() {
            HostRule::parse(rule)?;
        }
        for name in &self.env {
            check_env_name(name)?;
        }
        Ok(())
    }

    /// The egress allowlist for `command`: the configured one, else the built-in default.
    pub fn egress_allow(&self, command: &str) -> Result<Vec<HostRule>, SandboxError> {
        let list = match &self.allow {
            Some(list) => list.clone(),
            None => crate::egress::default_allow(command),
        };
        list.iter()
            .map(|r| HostRule::parse(r).map_err(SandboxError::from))
            .collect()
    }
}

/// `[backend.sandbox]` in the server config: changes to the backend's built-in preset
/// ([`SandboxConfig::apply`]). Keys left out keep the preset's values, so a table with only
/// `limits` or `seccomp` keeps the CLI's credential binds and state directories.
///
/// - `enabled`, `seccomp`: replace the preset's value when given.
/// - `limits`: key by key ([`limits::LimitsOverride`]).
/// - `tmpfs`, `ro`, `rw`, `env`: **added** to the preset's lists. Adding is the only thing a
///   list can do, so a list never drops a preset entry by accident; an empty list is refused
///   (it would say nothing).
/// - `preset = false`: start from an empty home instead of the preset (the lists are then
///   exactly what the table says). The only way to drop a preset path, e.g. a credential file
///   when the CLI should use an API key instead.
///
/// The egress allowlist and network mode are backend keys (`allow`, `network`), not part of
/// this table; `allow` replaces the CLI's built-in list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxConfig {
    /// Start from the CLI's preset (default) or from an empty home (`false`).
    #[serde(default = "enabled_by_default")]
    pub preset: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seccomp: Option<bool>,
    #[serde(default)]
    pub limits: limits::LimitsOverride,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmpfs: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ro: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rw: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<Vec<String>>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            preset: true,
            enabled: None,
            seccomp: None,
            limits: limits::LimitsOverride::default(),
            tmpfs: None,
            ro: None,
            rw: None,
            env: None,
        }
    }
}

impl SandboxConfig {
    /// The sandbox for a backend whose built-in preset is `preset`.
    pub fn apply(&self, preset: SandboxSpec) -> SandboxSpec {
        let mut spec = if self.preset {
            preset
        } else {
            SandboxSpec {
                network: preset.network,
                allow: preset.allow,
                vars: preset.vars,
                ..SandboxSpec::default()
            }
        };
        if let Some(on) = self.enabled {
            spec.enabled = on;
        }
        if let Some(on) = self.seccomp {
            spec.seccomp = on;
        }
        spec.limits = self.limits.apply(spec.limits);
        for (list, extra) in [
            (&mut spec.tmpfs, &self.tmpfs),
            (&mut spec.ro, &self.ro),
            (&mut spec.rw, &self.rw),
            (&mut spec.env, &self.env),
        ] {
            for item in extra.iter().flatten() {
                if !list.contains(item) {
                    list.push(item.clone());
                }
            }
        }
        spec
    }

    /// Refuse lists that would change nothing (`rw = []` once meant "no credentials"; now
    /// lists only add, so that needs `preset = false`).
    pub fn validate(&self) -> Result<(), SandboxError> {
        if !self.preset {
            return Ok(());
        }
        for (key, list) in [
            ("tmpfs", &self.tmpfs),
            ("ro", &self.ro),
            ("rw", &self.rw),
            ("env", &self.env),
        ] {
            if list.as_ref().is_some_and(Vec::is_empty) {
                return Err(SandboxError::EmptyList(key));
            }
        }
        Ok(())
    }
}

/// The sandbox a backend runs in: `fixed` (set by code, e.g. [`SandboxSpec::disabled`]) if
/// any, else `preset` with the config's table applied. `network` and `allow` come from the
/// backend's own keys.
pub fn effective(
    fixed: Option<&SandboxSpec>,
    config: Option<&SandboxConfig>,
    preset: impl FnOnce() -> SandboxSpec,
    network: NetworkMode,
    allow: Option<Vec<String>>,
) -> SandboxSpec {
    let mut spec = match (fixed, config) {
        (Some(spec), _) => spec.clone(),
        (None, Some(config)) => config.apply(preset()),
        (None, None) => preset(),
    };
    spec.network = network;
    spec.allow = allow;
    spec
}

/// A valid environment variable name that the sandbox does not set itself.
pub fn check_env_name(name: &str) -> Result<(), SandboxError> {
    let valid = !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid || RESERVED_ENV.contains(&name) {
        return Err(SandboxError::BadEnv(name.to_string()));
    }
    Ok(())
}

/// Errors from sandbox configuration or setup.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error(
        "sandbox path `{0}` must be relative to $HOME, non-empty and without `.`/`..` components"
    )]
    BadPath(String),
    #[error("sandbox directory `{0}` must be absolute and without `.`/`..` components")]
    BadDir(String),
    #[error("sandbox env `{0}` is not a valid, non-reserved variable name")]
    BadEnv(String),
    #[error(
        "`[backend.sandbox] {0} = []` changes nothing: lists add to the preset. \
         To drop the preset's paths, set `preset = false` and list what the CLI needs"
    )]
    EmptyList(&'static str),
    #[error(transparent)]
    BadRule(#[from] crate::egress::BadRule),
    #[error("model `{0}` is not valid here (`provider/model`, only for flavor = \"opencode\")")]
    BadModel(String),
    #[error("argument `{0}` is not allowed here (only --model, --fallback-model, --effort with a value)")]
    BadArg(String),
    #[error("HOME is not set or not absolute; cannot build the sandbox")]
    NoHome,
    #[error("cannot resolve the CLI binary {path}: {source}")]
    Program {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Users(#[from] users::UsersError),
}

/// How a host path appears inside the sandbox (always at the same path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mount {
    Tmpfs(PathBuf),
    ReadOnly(PathBuf),
    ReadWrite(PathBuf),
    /// A host directory bound read-write at another path (e.g. a Melissa-owned session
    /// store at the agent's `~/.grok/sessions`).
    BindAt {
        src: PathBuf,
        dst: PathBuf,
    },
}

impl Mount {
    fn path(&self) -> &Path {
        match self {
            Mount::Tmpfs(p) | Mount::ReadOnly(p) | Mount::ReadWrite(p) => p,
            Mount::BindAt { dst, .. } => dst,
        }
    }
}

/// The network bridge inside a sandbox: `melissa-netbridge` listens on loopback `ports`
/// and forwards each to a socket in `socket_dir` (bound at [`BRIDGE_DIR`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bridge {
    /// The `melissa-netbridge` binary on the host (bound read-only unless under `/usr`).
    pub netbridge: PathBuf,
    /// Host directory with the lead-side sockets of this sandbox.
    pub socket_dir: PathBuf,
    /// In-sandbox listen address → socket file name in `socket_dir`.
    pub ports: Vec<(std::net::SocketAddr, String)>,
    /// The egress proxy's in-sandbox address; sets `HTTPS_PROXY` and friends.
    pub proxy: Option<std::net::SocketAddr>,
    /// Socket file in `socket_dir` where the bridge hands its DNS socket to the lead agent
    /// ([`crate::egress::dns`]); `socket_dir` then also holds the sandbox's `resolv.conf`.
    pub dns: Option<String>,
    /// Host directories the bridge binary needs to run (its dynamic loader outside `/usr`,
    /// e.g. `/nix/store` for a dev-shell build), bound read-only.
    pub runtime: Vec<PathBuf>,
}

/// The sandbox's network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Network {
    /// The host's network namespace (and its resolver).
    Host,
    /// Own network namespace with loopback only, plus an optional bridge.
    Isolated(Option<Bridge>),
}

/// Everything [`command`] needs, resolved from the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPlan {
    /// The `bwrap` binary.
    pub bwrap: PathBuf,
    /// `$HOME`, recreated empty and read-only inside the sandbox.
    pub home: PathBuf,
    /// The CLI binary with all symlinks resolved; bound read-only and executed directly.
    pub program: PathBuf,
    /// Home paths, in mount order (parents before children).
    pub mounts: Vec<Mount>,
    /// Environment inside the sandbox (besides HOME, PATH, NO_COLOR and TERM).
    pub env: Vec<(String, OsString)>,
    /// Network namespace and bridge.
    pub network: Network,
    /// The host's system directories besides `/usr` and `/etc`.
    pub system: SystemRoot,
    /// Further host directories bound read-write at the same path, after the working
    /// directory: the agent's own file box ([`crate::files`]). Absolute, without `..`.
    pub rw_dirs: Vec<PathBuf>,
    /// Install the seccomp filter (`--seccomp`); the spawner must then [`pass_seccomp`].
    pub seccomp: bool,
    /// Run in a cgroup with these limits: bwrap waits on [`BLOCK_FD`] (`--block-fd`) until
    /// [`SandboxCommand::spawn`] has placed it. `None`: no limits (turned off for the backend,
    /// or the host does not delegate cgroups).
    pub limits: Option<limits::Limits>,
    /// Run as this agent's own host user ([`users`]; [`SandboxPlan::with_user`]). `None`: as the
    /// server's user (agent users off).
    pub user: Option<users::AgentUser>,
}

/// A top-level system directory (`/bin`, `/lib64`, ...) inside the sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopDir {
    /// A symlink to `usr/<name>`, as on usr-merged distributions (Fedora).
    UsrLink(&'static str),
    /// The host's directory, bound read-only (NixOS: `/bin/sh`, nix-ld's `/lib64`).
    Bind(PathBuf),
}

/// What the sandbox shows of the host's system besides `/usr` and `/etc`. The default is a
/// usr-merged distribution (Fedora, the Melissa images): `/bin`, `/sbin`, `/lib` and `/lib64`
/// link into `/usr`. On NixOS programs live in `/nix/store` and are found through
/// `/run/current-system/sw/bin`; the store holds no secrets (it is world-readable on the host).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemRoot {
    /// Top-level directories, in mount order.
    pub top: Vec<TopDir>,
    /// Further host directories bound read-only (`/nix/store`, `/run/current-system`).
    pub ro: Vec<PathBuf>,
    /// `PATH` entries after the CLI's own directory and `/usr/bin`.
    pub path: Vec<PathBuf>,
    /// The file `/etc/resolv.conf` leads to, as a path inside the sandbox: covered with the
    /// lease's own `resolv.conf` when the bridge serves DNS. `None`: there is none (the C
    /// libraries then ask 127.0.0.1 anyway).
    pub resolv_conf: Option<PathBuf>,
}

const TOP_DIRS: [&str; 4] = ["bin", "sbin", "lib", "lib64"];
const NIX_STORE: &str = "/nix/store";
const NIXOS_SYSTEM: &str = "/run/current-system";

impl Default for SystemRoot {
    fn default() -> Self {
        Self {
            top: TOP_DIRS.iter().map(|d| TopDir::UsrLink(d)).collect(),
            ro: Vec::new(),
            path: Vec::new(),
            resolv_conf: Some(PathBuf::from(RESOLV_CONF)),
        }
    }
}

impl SystemRoot {
    /// Look at the host: which top-level directories link into `/usr`, whether this is
    /// NixOS, and whether `program` needs the Nix store (its ELF loader lives there).
    pub fn detect(program: &Path) -> Self {
        let top = TOP_DIRS
            .iter()
            .filter_map(|d| {
                let host = Path::new("/").join(d);
                match std::fs::read_link(&host) {
                    Ok(t) if t == Path::new("usr").join(d) || t == Path::new("/usr").join(d) => {
                        Some(TopDir::UsrLink(d))
                    }
                    _ if host.is_dir() => Some(TopDir::Bind(host)),
                    // Missing on this host: nothing to show.
                    _ => None,
                }
            })
            .collect();
        let nixos = Path::new(NIXOS_SYSTEM).is_dir();
        let store_program = crate::egress::needs_nix_store(program);
        Self {
            resolv_conf: resolv_conf_target(Path::new(RESOLV_CONF)),
            ..Self::from_facts(top, nixos, store_program)
        }
    }

    /// [`SystemRoot::detect`] without the host lookups.
    pub fn from_facts(top: Vec<TopDir>, nixos: bool, store_program: bool) -> Self {
        let mut ro = Vec::new();
        let mut path = Vec::new();
        if nixos || store_program {
            ro.push(PathBuf::from(NIX_STORE));
        }
        if nixos {
            ro.push(PathBuf::from(NIXOS_SYSTEM));
            path.push(Path::new(NIXOS_SYSTEM).join("sw/bin"));
        }
        Self {
            top,
            ro,
            path,
            resolv_conf: Some(PathBuf::from(RESOLV_CONF)),
        }
    }
}

const RESOLV_CONF: &str = "/etc/resolv.conf";

/// Where `etc_resolv` (normally `/etc/resolv.conf`) leads, as a path bwrap can cover with a
/// file inside the sandbox: the file itself, an existing target under `/etc`, or a target
/// under `/run` (Fedora: `/run/systemd/resolve/stub-resolv.conf`; `/run` is not bound, so
/// bwrap creates it in the sandbox's own root). `None` if there is no such file.
fn resolv_conf_target(etc_resolv: &Path) -> Option<PathBuf> {
    let meta = std::fs::symlink_metadata(etc_resolv).ok()?;
    if !meta.file_type().is_symlink() {
        return Some(etc_resolv.to_path_buf());
    }
    if let Ok(target) = etc_resolv.canonicalize() {
        return (target.starts_with("/etc") || target.starts_with("/run")).then_some(target);
    }
    // A dangling link (e.g. the resolver service is not running): only `/run` targets.
    let link = std::fs::read_link(etc_resolv).ok()?;
    let joined = etc_resolv.parent().unwrap_or(Path::new("/")).join(link);
    let mut target = PathBuf::from("/");
    for c in joined.components() {
        match c {
            Component::ParentDir => {
                target.pop();
            }
            Component::Normal(n) => target.push(n),
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    target.starts_with("/run").then_some(target)
}

impl SandboxPlan {
    /// Resolve `spec` against the host: `home` (normally `$HOME`), the CLI binary as
    /// found on `PATH`, and which of the configured paths exist. Reads the process
    /// environment for [`BASE_ENV`] and [`SandboxSpec::env`].
    pub fn resolve(
        spec: &SandboxSpec,
        bwrap: &Path,
        home: Option<&Path>,
        program: &Path,
    ) -> Result<Self, SandboxError> {
        let home = home
            .filter(|h| h.is_absolute())
            .ok_or(SandboxError::NoHome)?
            .to_path_buf();
        let program = program
            .canonicalize()
            .map_err(|source| SandboxError::Program {
                path: program.to_path_buf(),
                source,
            })?;
        let mut mounts: Vec<Mount> = spec
            .tmpfs
            .iter()
            .map(|p| Mount::Tmpfs(home.join(p)))
            .collect();
        // `exists()` follows symlinks; bwrap binds the symlink's target at the same path.
        mounts.extend(
            spec.ro
                .iter()
                .map(|p| home.join(p))
                .filter(|p| p.exists())
                .map(Mount::ReadOnly),
        );
        mounts.extend(
            spec.rw
                .iter()
                .map(|p| home.join(p))
                .filter(|p| p.exists())
                .map(Mount::ReadWrite),
        );
        // Parents first, so a bind inside a tmpfs is not hidden by it. Stable: keeps order otherwise.
        mounts.sort_by_key(|m| m.path().components().count());

        let env = BASE_ENV
            .iter()
            .map(|s| (*s).to_string())
            .chain(spec.env.iter().cloned())
            .filter_map(|name| std::env::var_os(&name).map(|value| (name, value)))
            .collect();
        // Without a bridge (added by the caller), a non-host sandbox has no network at all.
        let network = match spec.network {
            NetworkMode::Host => Network::Host,
            _ => Network::Isolated(None),
        };
        let system = SystemRoot::detect(&program);
        let seccomp = spec.seccomp && seccomp::host_supported();
        if spec.seccomp && !seccomp {
            tracing::warn!(
                arch = std::env::consts::ARCH,
                "no seccomp filter for this architecture (only x86_64 and aarch64): \
                 agent sandboxes run without it"
            );
        }
        // Only where the host delegates cgroups ([`limits::init`] logged why not, once).
        let limits = (spec.limits.enabled && limits::tree().is_some()).then(|| spec.limits.clone());
        Ok(Self {
            bwrap: bwrap.to_path_buf(),
            home,
            program,
            mounts,
            env,
            network,
            system,
            rw_dirs: Vec::new(),
            seccomp,
            limits,
            user: None,
        })
    }

    /// Run as the host user of `key` (the file box key: `lead` or the agent's id;
    /// [`users::SHARED`] for a sandbox without an owner), when agent users are on ([`users::init`]).
    pub fn with_user(mut self, key: &str) -> Result<Self, SandboxError> {
        if let Some(setup) = users::active() {
            let mut user = setup.user(key)?;
            let (passwd, group) = setup.identity(&self.home)?;
            user.passwd = Some(passwd);
            user.group = Some(group);
            self.user = Some(user);
        }
        Ok(self)
    }

    /// Set fixed environment variables inside the sandbox (e.g. an agent's config chosen by
    /// Melissa). Names must be valid and not reserved ([`check_env_name`]).
    pub fn with_env(mut self, env: &[(String, String)]) -> Result<Self, SandboxError> {
        for (name, value) in env {
            check_env_name(name)?;
            self.env.retain(|(n, _)| n != name);
            self.env.push((name.clone(), OsString::from(value)));
        }
        Ok(self)
    }

    /// Run the CLI behind `bridge` (ignored for `network = "host"`).
    pub fn with_bridge(mut self, bridge: Option<Bridge>) -> Self {
        if let Network::Isolated(_) = self.network {
            self.network = Network::Isolated(bridge);
        }
        self
    }

    /// Add host directories bound read-write at home-relative paths (`(host dir, path
    /// under $HOME)`); paths must pass the same checks as [`SandboxSpec`] paths.
    pub fn with_binds(mut self, binds: &[(PathBuf, String)]) -> Result<Self, SandboxError> {
        for (src, rel) in binds {
            let p = Path::new(rel);
            if rel.is_empty() || !p.components().all(|c| matches!(c, Component::Normal(_))) {
                return Err(SandboxError::BadPath(rel.clone()));
            }
            self.mounts.push(Mount::BindAt {
                src: src.clone(),
                dst: self.home.join(p),
            });
        }
        self.mounts.sort_by_key(|m| m.path().components().count());
        Ok(self)
    }

    /// Bind `dirs` read-write at the same path (the agent's file box). Each must be an
    /// absolute path without `.`/`..` components.
    pub fn with_rw_dirs(mut self, dirs: &[PathBuf]) -> Result<Self, SandboxError> {
        for d in dirs {
            let clean = d.is_absolute()
                && d.components()
                    .all(|c| matches!(c, Component::RootDir | Component::Normal(_)));
            if !clean {
                return Err(SandboxError::BadDir(d.display().to_string()));
            }
            if !self.rw_dirs.contains(d) {
                self.rw_dirs.push(d.clone());
            }
        }
        Ok(self)
    }
}

/// Build the `bwrap` program and argv that run `args` under the sandbox in `workdir`.
/// `extra_ro` are further files bound read-only (e.g. an MCP config outside `workdir`).
/// Pure: no host lookups; every element is a separate argv entry, never a shell string.
pub fn command(
    plan: &SandboxPlan,
    workdir: &Path,
    extra_ro: &[PathBuf],
    args: &[String],
) -> (PathBuf, Vec<OsString>) {
    let mut argv: Vec<OsString> = Vec::new();
    // bwrap makes the missing parents of a bind's destination 0700, owned by whoever it runs as.
    // With an agent user that is the setup id, not the agent, so each bind first gets its parent
    // as a plain 0755 directory (`--dir`; its own parents follow that mode).
    let users = plan.user.is_some();
    let mut push = |items: &[&dyn AsRef<std::ffi::OsStr>]| {
        if let ([flag, _, dst], true) = (items, users) {
            let bind = ["--bind", "--ro-bind", "--ro-bind-try"]
                .iter()
                .any(|f| flag.as_ref() == *f);
            let parent = Path::new(dst.as_ref()).parent();
            if let (true, Some(parent)) = (bind, parent.filter(|p| *p != Path::new("/"))) {
                argv.push("--dir".into());
                argv.push(parent.as_os_str().to_os_string());
            }
        }
        argv.extend(items.iter().map(|i| i.as_ref().to_os_string()));
    };

    // Namespaces and privileges. bwrap always sets no_new_privs for unprivileged users.
    // `--unshare-all` includes the network namespace (bwrap brings up its loopback). With an
    // agent user the user namespace exists already (bwrap starts in it, [`users`]): the others
    // one by one.
    let host_net = plan.network == Network::Host;
    if plan.user.is_some() {
        push(&[
            &"--unshare-ipc",
            &"--unshare-pid",
            &"--unshare-uts",
            &"--unshare-cgroup-try",
        ]);
        if !host_net {
            push(&[&"--unshare-net"]);
        }
    } else {
        push(&[&"--unshare-all"]);
        if host_net {
            push(&[&"--share-net"]);
        }
    }
    let bridge = match &plan.network {
        Network::Host => None,
        Network::Isolated(bridge) => bridge.as_ref(),
    };
    push(&[
        &"--die-with-parent",
        &"--new-session",
        &"--cap-drop",
        &"ALL",
        &"--hostname",
        &SANDBOX_HOSTNAME,
    ]);
    // With an agent user bwrap runs as root of the agent's user namespace; `setpriv` needs these
    // to become the agent (and drops them, with the bounding set).
    if plan.user.is_some() {
        for cap in users::SETPRIV_CAPS {
            push(&[&"--cap-add", &cap]);
        }
    }
    // Syscall filter, read from an fd the spawner maps ([`pass_seccomp`]); bwrap installs it
    // right before executing the first program inside.
    if plan.seccomp {
        push(&[&"--seccomp", &SECCOMP_FD.to_string()]);
    }
    // Wait (namespaces made, nothing forked or executed inside yet) until the spawner has moved
    // bwrap into the sandbox's cgroup ([`SandboxCommand::spawn`]).
    if plan.limits.is_some() {
        push(&[&"--block-fd", &BLOCK_FD.to_string()]);
    }

    // Environment: nothing from the host unless allowed.
    push(&[&"--clearenv"]);
    let mut path_env = match plan.program.parent() {
        Some(dir) => {
            let mut p = dir.as_os_str().to_os_string();
            p.push(":/usr/bin");
            p
        }
        None => OsString::from("/usr/bin"),
    };
    for dir in &plan.system.path {
        path_env.push(":");
        path_env.push(dir);
    }
    push(&[&"--setenv", &"HOME", &plan.home]);
    push(&[&"--setenv", &"PATH", &path_env]);
    push(&[&"--setenv", &"NO_COLOR", &"1"]);
    push(&[&"--setenv", &"TERM", &"dumb"]);
    for (name, value) in &plan.env {
        push(&[&"--setenv", name, value]);
    }
    // With an agent user, files of the server's (a working directory it made) are another
    // user's inside; git would refuse to work in them ("dubious ownership").
    if plan.user.is_some() {
        push(&[&"--setenv", &"GIT_CONFIG_COUNT", &"1"]);
        push(&[&"--setenv", &"GIT_CONFIG_KEY_0", &"safe.directory"]);
        push(&[&"--setenv", &"GIT_CONFIG_VALUE_0", &"*"]);
    }
    if let Some(proxy) = bridge.and_then(|b| b.proxy) {
        let url = format!("http://{proxy}");
        for name in [
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "http_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            push(&[&"--setenv", &name, &url]);
        }
        // MCP is on loopback (through the bridge), never through the proxy.
        for name in ["NO_PROXY", "no_proxy"] {
            push(&[&"--setenv", &name, &"localhost,127.0.0.1,::1"]);
        }
        // Grok's `web_fetch` ignores HTTPS_PROXY and has its own setting (verified with grok
        // 1.0.89). Other CLIs ignore the variable.
        push(&[&"--setenv", &"GROK_WEB_FETCH_PROXY", &url]);
        // Node.js (>= 24.5, 22.21) ignores the proxy variables for `fetch` and `http` unless
        // this is set; the npm-installed CLIs (Gemini CLI, Pi) run on it. Others ignore it.
        push(&[&"--setenv", &"NODE_USE_ENV_PROXY", &"1"]);
    }

    // System: read-only /usr and /etc (Fedora is usr-merged; NixOS adds its store and system
    // profile), the resolver's runtime dir (/etc/resolv.conf links there on Fedora), fresh
    // /proc, minimal /dev, private /tmp.
    push(&[&"--ro-bind", &"/usr", &"/usr"]);
    for dir in &plan.system.top {
        match dir {
            TopDir::UsrLink(d) => push(&[&"--symlink", &format!("usr/{d}"), &format!("/{d}")]),
            TopDir::Bind(p) => push(&[&"--ro-bind", p, p]),
        }
    }
    for dir in &plan.system.ro {
        push(&[&"--ro-bind", dir, dir]);
    }
    push(&[&"--ro-bind", &"/etc", &"/etc"]);
    if let Some(user) = &plan.user {
        if let (Some(passwd), Some(group)) = (&user.passwd, &user.group) {
            push(&[&"--ro-bind", passwd, &"/etc/passwd"]);
            push(&[&"--ro-bind", group, &"/etc/group"]);
        }
        let store = Path::new(NIX_STORE);
        if user.setpriv.starts_with(store) && !plan.system.ro.iter().any(|d| d == store) {
            push(&[&"--ro-bind", &store, &store]);
        } else if !user.setpriv.starts_with("/usr") && !user.setpriv.starts_with(store) {
            push(&[&"--ro-bind", &user.setpriv, &user.setpriv]);
        }
    }
    if plan.network == Network::Host {
        push(&[
            &"--ro-bind-try",
            &"/run/systemd/resolve",
            &"/run/systemd/resolve",
        ]);
    }
    push(&[&"--proc", &"/proc", &"--dev", &"/dev"]);
    // A tmpfs belongs to whoever mounts it: with an agent user that is the setup id, so the
    // writable ones are open to all (only the agent is inside): /tmp sticky as anywhere, a CLI's
    // state directory not, since a sticky one would stop the CLI from rewriting a credential
    // file bound into it (fs.protected_regular: O_CREAT on another user's file there).
    let (tmp, state): (
        &[&dyn AsRef<std::ffi::OsStr>],
        &[&dyn AsRef<std::ffi::OsStr>],
    ) = if users {
        (&[&"--perms", &"01777"], &[&"--perms", &"0777"])
    } else {
        (&[], &[])
    };
    for dir in ["/tmp", "/var/tmp"] {
        push(tmp);
        push(&[&"--tmpfs", &dir]);
    }

    // Home: an empty tmpfs with only the configured paths; frozen read-only below.
    push(&[&"--tmpfs", &plan.home]);
    for mount in &plan.mounts {
        match mount {
            Mount::Tmpfs(p) => {
                push(state);
                push(&[&"--tmpfs", p]);
            }
            Mount::ReadOnly(p) => push(&[&"--ro-bind", p, p]),
            Mount::ReadWrite(p) => push(&[&"--bind", p, p]),
            Mount::BindAt { src, dst } => push(&[&"--bind", src, dst]),
        }
    }

    // The CLI binary, unless a bind already exposes it.
    let bound = |path: &Path| {
        path.starts_with("/usr")
            || plan.mounts.iter().any(|m| match m {
                Mount::ReadOnly(p) | Mount::ReadWrite(p) => path.starts_with(p),
                Mount::Tmpfs(_) | Mount::BindAt { .. } => false,
            })
    };
    if !bound(&plan.program) {
        push(&[&"--ro-bind", &plan.program, &plan.program]);
    }
    for file in extra_ro {
        if !file.starts_with(workdir) {
            push(&[&"--ro-bind", file, file]);
        }
    }
    if let Some(b) = bridge {
        if !bound(&b.netbridge) {
            push(&[&"--ro-bind", &b.netbridge, &b.netbridge]);
        }
        for dir in b.runtime.iter().filter(|d| !plan.system.ro.contains(d)) {
            push(&[&"--ro-bind", dir, dir]);
        }
        // Read-only is enough: connecting to a socket needs no write access to its mount.
        push(&[&"--ro-bind", &b.socket_dir, &BRIDGE_DIR]);
        if let (Some(_), Some(target)) = (&b.dns, &plan.system.resolv_conf) {
            let ours = b.socket_dir.join(crate::egress::RESOLV_CONF);
            push(&[&"--ro-bind", &ours, target]);
        }
    }

    // The task's working directory: the only writable host path. Then freeze the rest.
    push(&[&"--bind", &workdir, &workdir]);
    // The agent's file box: its own, read-write; no other agent's box is ever bound.
    for dir in &plan.rw_dirs {
        push(&[&"--bind", dir, dir]);
    }
    // `--remount-ro` affects one mount only: the binds and tmpfs mounts inside stay as set.
    push(&[&"--remount-ro", &plan.home]);
    push(&[&"--remount-ro", &"/"]);
    push(&[&"--chdir", &workdir]);

    push(&[&"--"]);
    if let Some(user) = &plan.user {
        // First inside: become the agent's user (no groups, no capabilities, empty bounding set).
        argv.extend(user.setpriv_args());
    }
    let mut push = |items: &[&dyn AsRef<std::ffi::OsStr>]| {
        argv.extend(items.iter().map(|i| i.as_ref().to_os_string()));
    };
    if let Some(b) = bridge {
        // The bridge starts first, then runs the CLI as its child.
        push(&[&b.netbridge]);
        for (addr, socket) in &b.ports {
            push(&[&"--tcp", &format!("{addr}={BRIDGE_DIR}/{socket}")]);
        }
        if let Some(socket) = &b.dns {
            push(&[&"--dns", &format!("{BRIDGE_DIR}/{socket}")]);
        }
        push(&[&"--"]);
    }
    push(&[&plan.program]);
    argv.extend(args.iter().map(OsString::from));
    (plan.bwrap.clone(), argv)
}

/// The fd number bwrap reads the seccomp program from (`--seccomp 3`): the first after stdio.
pub const SECCOMP_FD: RawFd = 3;

/// The fd number bwrap waits on before it starts anything inside (`--block-fd 4`), when the
/// sandbox has [`limits`].
pub const BLOCK_FD: RawFd = 4;

/// The seccomp program's fd mapping, if the plan uses one: a sealed memfd at [`SECCOMP_FD`].
fn seccomp_mapping(plan: &SandboxPlan) -> std::io::Result<Option<FdMapping>> {
    if !plan.seccomp {
        return Ok(None);
    }
    Ok(Some(FdMapping {
        parent_fd: seccomp::memfd()?,
        child_fd: SECCOMP_FD,
    }))
}

/// Give `cmd` (the `bwrap` command built from `plan`) the seccomp program at [`SECCOMP_FD`],
/// if the plan uses one. The program is a sealed memfd; only the child gets it (the parent's
/// copy is close-on-exec and closes when `cmd` is dropped), so no other process spawned
/// meanwhile inherits it. Call once per command ([`spawn_command`] does this itself).
pub fn pass_seccomp<C: CommandFdExt>(cmd: &mut C, plan: &SandboxPlan) -> std::io::Result<()> {
    if let Some(mapping) = seccomp_mapping(plan)? {
        cmd.fd_mappings(vec![mapping])
            .map_err(std::io::Error::other)?;
    }
    Ok(())
}

/// The one way to spawn a sandbox: the `bwrap` process for `plan` running `args` in `workdir`
/// ([`command`]), with the seccomp program at [`SECCOMP_FD`] ([`pass_seccomp`]), the cgroup gate
/// at [`BLOCK_FD`] when the plan has [`limits`], started through `nsenter` in the agent's user
/// namespace when the plan has a [`users`] user (after granting that user its paths), and an
/// **empty environment**. `--clearenv`
/// only clears what the CLI gets; bwrap's own initial environment stays readable inside the
/// sandbox through `/proc/1/environ` (bwrap is PID 1 there), so inheriting the lead agent's
/// environment would hand every variable of `melissa-server` (tokens, keys) to the agent. Found
/// by the escape suite (`crates/agent/tests/it/escape.rs`, `proc.environ-leak`). Everything the
/// sandbox needs is in the argv (`--setenv`). Callers may add variables for bwrap itself (they
/// are visible inside) and set stdio through [`SandboxCommand::command_mut`], then start it
/// with [`SandboxCommand::spawn`].
pub fn spawn_command(
    plan: &SandboxPlan,
    workdir: &Path,
    extra_ro: &[PathBuf],
    args: &[String],
) -> std::io::Result<SandboxCommand> {
    let (bwrap, argv) = command(plan, workdir, extra_ro, args);
    let launch = match &plan.user {
        None => None,
        Some(user) => {
            let setup = users::active().ok_or_else(|| {
                std::io::Error::other("the plan has an agent user but agent users are off")
            })?;
            setup
                .grant(plan, user, workdir, extra_ro)
                .map_err(std::io::Error::other)?;
            Some(setup.launch(user).map_err(std::io::Error::other)?)
        }
    };
    let (mut cmd, holder) = match launch {
        None => (tokio::process::Command::new(bwrap), None),
        Some(l) => {
            let mut cmd = tokio::process::Command::new(&l.program);
            cmd.args(&l.args).arg(bwrap);
            (cmd, Some(l.holder))
        }
    };
    cmd.env_clear().args(argv);
    let mut fds: Vec<FdMapping> = seccomp_mapping(plan)?.into_iter().collect();
    let gate = match &plan.limits {
        None => None,
        Some(l) => {
            // Both ends are close-on-exec; only bwrap gets the read end, at BLOCK_FD.
            let (read, write) = std::io::pipe()?;
            fds.push(FdMapping {
                parent_fd: read.into(),
                child_fd: BLOCK_FD,
            });
            let label = plan
                .program
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let cgroup = limits::tree().and_then(|tree| match tree.create(l, &label) {
                Ok(cg) => Some(cg),
                Err(e) => {
                    tracing::warn!(
                        sandbox = %label,
                        "cannot create the sandbox's cgroup; it runs without limits: {e}"
                    );
                    None
                }
            });
            Some(Gate {
                release: write,
                cgroup,
            })
        }
    };
    // One call: each call adds its own pre-exec step, and a later one could clobber the fd an
    // earlier one placed.
    if !fds.is_empty() {
        cmd.fd_mappings(fds).map_err(std::io::Error::other)?;
    }
    Ok(SandboxCommand {
        command: cmd,
        gate,
        holder,
    })
}

/// The cgroup a sandbox goes into, and the pipe that lets bwrap go on.
#[derive(Debug)]
struct Gate {
    release: std::io::PipeWriter,
    cgroup: Option<limits::SandboxCgroup>,
}

impl Gate {
    /// Place `pid` (bwrap) in the cgroup, then let it go on. A failed move is logged and the
    /// sandbox runs anyway: limits guard against exhaustion; the sandbox itself still holds.
    fn open(mut self, pid: Option<u32>) -> Option<limits::SandboxCgroup> {
        if let (Some(cg), Some(pid)) = (&self.cgroup, pid) {
            if let Err(e) = cg.adopt(pid) {
                tracing::warn!(
                    "cannot move the agent sandbox into its cgroup; limits may not apply: {e}"
                );
            }
        }
        // bwrap reads one byte. EPIPE: it is gone already, which the caller's `wait` shows.
        let _ = self.release.write_all(b"1");
        self.cgroup
    }
}

/// A sandboxed (or, with the sandbox off, plain) agent CLI command, ready to spawn.
#[derive(Debug)]
pub struct SandboxCommand {
    command: tokio::process::Command,
    gate: Option<Gate>,
    /// Holds the agent user's namespace until the sandbox has joined it.
    holder: Option<users::Holder>,
}

impl SandboxCommand {
    /// A command without a sandbox (the backend's sandbox is turned off).
    pub fn plain(command: tokio::process::Command) -> Self {
        Self {
            command,
            gate: None,
            holder: None,
        }
    }

    pub fn command(&self) -> &tokio::process::Command {
        &self.command
    }

    /// For stdio, working directory and the like. Start it with [`SandboxCommand::spawn`], never
    /// with the inner command's own `spawn` (bwrap would wait until this value is dropped and
    /// then run outside its cgroup).
    pub fn command_mut(&mut self) -> &mut tokio::process::Command {
        &mut self.command
    }

    /// Start the process; with limits, move it into its cgroup and only then let it go on.
    pub fn spawn(self) -> std::io::Result<SandboxChild> {
        let Self {
            mut command,
            gate,
            holder,
        } = self;
        let child = command.spawn()?;
        // Closes the parent's copy of the gate's read end.
        drop(command);
        if let Some(holder) = holder {
            holder.release_when_joined(child.id());
        }
        let cgroup = gate.and_then(|g| g.open(child.id()));
        Ok(SandboxChild { child, cgroup })
    }
}

/// A running sandbox. Keep `cgroup` until the process has exited: dropping it kills whatever is
/// left in the sandbox and removes the cgroup.
#[derive(Debug)]
pub struct SandboxChild {
    pub child: tokio::process::Child,
    pub cgroup: Option<limits::SandboxCgroup>,
}

/// Find bubblewrap: the Fedora location first, then `PATH`.
pub fn find_bwrap() -> Option<PathBuf> {
    let fixed = Path::new(BWRAP_PATH);
    if fixed.is_file() {
        return Some(fixed.to_path_buf());
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("bwrap"))
            .find(|candidate| candidate.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(mounts: Vec<Mount>, program: &str) -> SandboxPlan {
        SandboxPlan {
            bwrap: BWRAP_PATH.into(),
            home: "/home/u".into(),
            program: program.into(),
            mounts,
            env: vec![("LANG".into(), "C.UTF-8".into())],
            network: Network::Host,
            system: SystemRoot::default(),
            rw_dirs: Vec::new(),
            seccomp: true,
            limits: None,
            user: None,
        }
    }

    #[test]
    fn argv_carries_the_seccomp_fd_unless_turned_off() {
        let mut p = plan(Vec::new(), "/usr/bin/cli");
        let (_, argv) = command(&p, Path::new("/w"), &[], &[]);
        let a = strs(&argv);
        assert!(has(&a, &["--seccomp", "3"]), "{a:?}");
        // Before the program: bwrap options end at `--`.
        let sep = a.iter().position(|x| *x == "--").unwrap();
        assert!(a.iter().position(|x| *x == "--seccomp").unwrap() < sep);
        p.seccomp = false;
        let (_, argv) = command(&p, Path::new("/w"), &[], &[]);
        assert!(!strs(&argv).contains(&"--seccomp"));
    }

    #[test]
    fn argv_with_an_agent_user_starts_in_its_namespace_and_switches_with_setpriv() {
        let mut p = plan(
            vec![Mount::ReadWrite("/home/u/.claude/.credentials.json".into())],
            "/usr/bin/cli",
        );
        p.network = Network::Isolated(None);
        let (_, argv) = command(&p, Path::new("/state/work/a1"), &[], &["x".into()]);
        let a = strs(&argv);
        assert!(a.contains(&"--unshare-all") && !a.contains(&"--cap-add"));
        assert!(!a.contains(&"--dir") && !a.contains(&"/usr/bin/setpriv"));

        p.user = Some(users::AgentUser {
            key: "d.a1".into(),
            uid: 600_001,
            gid: 600_001,
            root_uid: 600_000,
            root_gid: 600_000,
            inner_uid: 1000,
            inner_gid: 1000,
            setpriv: "/usr/bin/setpriv".into(),
            passwd: Some("/state/agent-users/passwd-1".into()),
            group: Some("/state/agent-users/group-1".into()),
        });
        let (_, argv) = command(&p, Path::new("/state/work/a1"), &[], &["x".into()]);
        let a = strs(&argv);
        // The user namespace is made beforehand: no --unshare-all (it would add another).
        assert!(!a.contains(&"--unshare-all"), "{a:?}");
        for flag in [
            "--unshare-pid",
            "--unshare-net",
            "--unshare-ipc",
            "--unshare-uts",
        ] {
            assert!(a.contains(&flag), "{flag}: {a:?}");
        }
        for cap in users::SETPRIV_CAPS {
            assert!(has(&a, &["--cap-add", cap]), "{cap}: {a:?}");
        }
        assert!(has(
            &a,
            &["--ro-bind", "/state/agent-users/passwd-1", "/etc/passwd"]
        ));
        assert!(has(
            &a,
            &["--ro-bind", "/state/agent-users/group-1", "/etc/group"]
        ));
        // Each bind's parent is made 0755 first (bwrap would make it 0700, owned by the setup id).
        assert!(
            has(&a, &["--dir", "/state/work", "--bind", "/state/work/a1"]),
            "{a:?}"
        );
        assert!(has(&a, &["--dir", "/home/u/.claude", "--bind"]), "{a:?}");
        // Writable tmpfs mounts are the agent's too (bwrap's setup id mounts them).
        assert!(has(&a, &["--perms", "01777", "--tmpfs", "/tmp"]), "{a:?}");
        // setpriv is the first program; the CLI follows it.
        let sep = a.iter().position(|x| *x == "--").unwrap();
        assert_eq!(a[sep + 1], "/usr/bin/setpriv");
        let tail = &a[sep + 1..];
        assert!(has(
            tail,
            &["--reuid", "1000", "--regid", "1000", "--clear-groups"]
        ));
        assert!(has(tail, &["--", "/usr/bin/cli", "x"]), "{tail:?}");
        assert!(has(&a, &["--setenv", "GIT_CONFIG_KEY_0", "safe.directory"]));
        // Host network: the network namespace is simply not unshared.
        p.network = Network::Host;
        let (_, argv) = command(&p, Path::new("/state/work/a1"), &[], &[]);
        let a = strs(&argv);
        assert!(!a.contains(&"--unshare-net") && !a.contains(&"--share-net"));
    }

    #[test]
    fn argv_waits_on_the_cgroup_gate_only_with_limits() {
        let mut p = plan(Vec::new(), "/usr/bin/cli");
        let (_, argv) = command(&p, Path::new("/w"), &[], &[]);
        assert!(!strs(&argv).contains(&"--block-fd"));
        p.limits = Some(limits::Limits::default());
        let (_, argv) = command(&p, Path::new("/w"), &[], &[]);
        let a = strs(&argv);
        assert!(has(&a, &["--block-fd", "4"]), "{a:?}");
        let sep = a.iter().position(|x| *x == "--").unwrap();
        assert!(a.iter().position(|x| *x == "--block-fd").unwrap() < sep);
    }

    #[test]
    fn limits_default_on_and_come_from_the_sandbox_table() {
        assert_eq!(SandboxSpec::default().limits, limits::Limits::default());
        assert_eq!(
            SandboxSpec::preset("claude").limits,
            limits::Limits::default()
        );
        let spec: SandboxSpec =
            serde_json::from_str(r#"{"limits": {"memory": "1G", "pids": 64}}"#).unwrap();
        assert_eq!(spec.limits.memory, limits::Bytes(Some(1 << 30)));
        assert_eq!(spec.limits.pids, limits::Pids(Some(64)));
        assert_eq!(spec.limits.cpus, limits::Limits::default().cpus);
        assert!(serde_json::from_str::<SandboxSpec>(r#"{"limits": {"pids": 0}}"#).is_err());
        // Not set up in unit tests: a resolved plan has no limits (and no gate).
        assert!(limits::tree().is_none());
    }

    #[test]
    fn seccomp_is_on_by_default_and_can_be_turned_off_in_config() {
        assert!(SandboxSpec::default().seccomp);
        assert!(SandboxSpec::preset("claude").seccomp);
        let spec: SandboxSpec = serde_json::from_str(r#"{"seccomp": false}"#).unwrap();
        assert!(!spec.seccomp && spec.enabled);
        let spec: SandboxSpec = serde_json::from_str("{}").unwrap();
        assert!(spec.seccomp);
    }

    #[test]
    fn pass_seccomp_maps_only_when_planned() {
        let mut p = plan(Vec::new(), "/usr/bin/cli");
        let mut cmd = std::process::Command::new("true");
        pass_seccomp(&mut cmd, &p).unwrap();
        p.seccomp = false;
        let mut cmd = std::process::Command::new("true");
        pass_seccomp(&mut cmd, &p).unwrap();
    }

    fn bridge() -> Bridge {
        Bridge {
            netbridge: "/opt/melissa/melissa-netbridge".into(),
            socket_dir: "/run/user/1/melissa/egress/abcd".into(),
            ports: vec![
                ("127.0.0.1:3128".parse().unwrap(), "egress.sock".into()),
                ("127.0.0.1:8080".parse().unwrap(), "mcp.sock".into()),
            ],
            proxy: Some("127.0.0.1:3128".parse().unwrap()),
            dns: Some("dns.sock".into()),
            runtime: vec!["/nix/store".into()],
        }
    }

    fn strs(argv: &[OsString]) -> Vec<&str> {
        argv.iter().map(|a| a.to_str().unwrap()).collect()
    }

    /// True if `needle` appears as a contiguous run in `hay`.
    fn has(hay: &[&str], needle: &[&str]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn argv_isolates_and_binds_narrowly() {
        let p = plan(
            vec![
                Mount::Tmpfs("/home/u/.claude".into()),
                Mount::ReadWrite("/home/u/.claude/.credentials.json".into()),
            ],
            "/home/u/.local/share/claude/versions/9",
        );
        let prompt = "--always-approve; $(rm -rf ~)";
        let (prog, argv) = command(
            &p,
            Path::new("/state/work/task-1"),
            &[],
            &["-p".into(), prompt.into()],
        );
        assert_eq!(prog, PathBuf::from(BWRAP_PATH));
        let a = strs(&argv);
        for flag in [
            "--unshare-all",
            "--share-net",
            "--die-with-parent",
            "--new-session",
            "--clearenv",
        ] {
            assert!(a.contains(&flag), "missing {flag}");
        }
        assert!(has(&a, &["--cap-drop", "ALL"]));
        assert!(has(&a, &["--ro-bind", "/usr", "/usr"]));
        assert!(has(&a, &["--ro-bind", "/etc", "/etc"]));
        assert!(has(&a, &["--tmpfs", "/tmp"]));
        assert!(has(&a, &["--setenv", "HOME", "/home/u"]));
        assert!(has(&a, &["--setenv", "LANG", "C.UTF-8"]));
        assert!(has(&a, &["--tmpfs", "/home/u"]));
        assert!(has(&a, &["--remount-ro", "/home/u"]));
        assert!(has(&a, &["--tmpfs", "/home/u/.claude"]));
        let creds = "/home/u/.claude/.credentials.json";
        assert!(has(&a, &["--bind", creds, creds]));
        let bin = "/home/u/.local/share/claude/versions/9";
        assert!(has(&a, &["--ro-bind", bin, bin]));
        let wd = "/state/work/task-1";
        assert!(has(&a, &["--bind", wd, wd]));
        assert!(has(&a, &["--chdir", wd]));
        // The only writable host binds are the credentials and the workdir.
        let rw: Vec<_> = a
            .windows(2)
            .filter(|w| w[0] == "--bind")
            .map(|w| w[1])
            .collect();
        assert_eq!(rw, [creds, wd]);
        // `/` is frozen after all mounts, and the command follows `--` with the prompt
        // as one untouched element at the end.
        let remount = a.iter().position(|x| *x == "--remount-ro").unwrap();
        let last_bind = a.iter().rposition(|x| x.starts_with("--bind")).unwrap();
        assert!(remount > last_bind);
        assert_eq!(&a[a.len() - 4..], ["--", bin, "-p", prompt]);
    }

    #[test]
    fn the_agents_file_box_is_bound_and_nothing_else_of_the_state() {
        let ada = PathBuf::from("/state/files/boxes/d-0a1b2c3d.a1");
        let p = plan(vec![], "/usr/bin/cli")
            .with_rw_dirs(std::slice::from_ref(&ada))
            .unwrap();
        let (_, argv) = command(&p, Path::new("/state/work/agents/d-0a1b2c3d.a1"), &[], &[]);
        let a = strs(&argv);
        let rw: Vec<_> = a
            .windows(2)
            .filter(|w| w[0] == "--bind")
            .map(|w| w[1])
            .collect();
        assert_eq!(
            rw,
            [
                "/state/work/agents/d-0a1b2c3d.a1",
                "/state/files/boxes/d-0a1b2c3d.a1"
            ]
        );
        // Not the other boxes, not their parent directory.
        assert!(!a
            .iter()
            .any(|x| *x == "/state/files/boxes" || *x == "/state/files"));
        assert!(!a.iter().any(|x| x.contains("d-0a1b2c3d.a2")));
        let remount = a.iter().position(|x| *x == "--remount-ro").unwrap();
        let last_bind = a.iter().rposition(|x| *x == "--bind").unwrap();
        assert!(remount > last_bind);
        // Only clean absolute paths.
        for bad in ["relative/box", "/state/files/boxes/../boxes/x", ""] {
            assert!(
                plan(vec![], "/usr/bin/cli")
                    .with_rw_dirs(&[PathBuf::from(bad)])
                    .is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn isolated_network_runs_the_cli_behind_the_bridge() {
        let mut p = plan(vec![], "/home/u/bin/cli");
        p.network = Network::Isolated(None);
        let p = p.with_bridge(Some(bridge()));
        let (_, argv) = command(&p, Path::new("/w"), &[], &["-p".into(), "hi".into()]);
        let a = strs(&argv);
        assert!(a.contains(&"--unshare-all"));
        assert!(!a.contains(&"--share-net"), "{a:?}");
        // No host resolver inside: names are resolved by the proxy.
        assert!(!a.contains(&"/run/systemd/resolve"));
        let nb = "/opt/melissa/melissa-netbridge";
        assert!(has(&a, &["--ro-bind", nb, nb]));
        assert!(has(&a, &["--ro-bind", "/nix/store", "/nix/store"]));
        assert!(has(
            &a,
            &["--ro-bind", "/run/user/1/melissa/egress/abcd", BRIDGE_DIR]
        ));
        for name in [
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "ALL_PROXY",
            "all_proxy",
        ] {
            assert!(
                has(&a, &["--setenv", name, "http://127.0.0.1:3128"]),
                "{name}"
            );
        }
        assert!(has(
            &a,
            &["--setenv", "NO_PROXY", "localhost,127.0.0.1,::1"]
        ));
        // Grok's web_fetch has its own proxy setting.
        assert!(has(
            &a,
            &["--setenv", "GROK_WEB_FETCH_PROXY", "http://127.0.0.1:3128"]
        ));
        // Node's own proxy support is opt-in.
        assert!(has(&a, &["--setenv", "NODE_USE_ENV_PROXY", "1"]));
        // The DNS sinkhole: the lease's resolv.conf over the host's.
        assert!(has(
            &a,
            &[
                "--ro-bind",
                "/run/user/1/melissa/egress/abcd/resolv.conf",
                "/etc/resolv.conf"
            ]
        ));
        // bwrap runs the bridge, which runs the CLI with its untouched arguments.
        let dashdash = a.iter().position(|x| *x == "--").unwrap();
        assert_eq!(
            &a[dashdash..],
            [
                "--",
                nb,
                "--tcp",
                "127.0.0.1:3128=/run/melissa-net/egress.sock",
                "--tcp",
                "127.0.0.1:8080=/run/melissa-net/mcp.sock",
                "--dns",
                "/run/melissa-net/dns.sock",
                "--",
                "/home/u/bin/cli",
                "-p",
                "hi"
            ]
        );
        // Fedora's resolv.conf is a link into /run; with none, nothing is bound.
        let mut p2 = p.clone();
        p2.system.resolv_conf = Some("/run/systemd/resolve/stub-resolv.conf".into());
        let (_, argv) = command(&p2, Path::new("/w"), &[], &[]);
        assert!(has(
            &strs(&argv),
            &[
                "--ro-bind",
                "/run/user/1/melissa/egress/abcd/resolv.conf",
                "/run/systemd/resolve/stub-resolv.conf"
            ]
        ));
        p2.system.resolv_conf = None;
        let (_, argv) = command(&p2, Path::new("/w"), &[], &[]);
        assert!(!strs(&argv).iter().any(|x| x.ends_with("resolv.conf")));
    }

    #[test]
    fn resolv_conf_is_found_through_links() {
        let dir = crate::test_support::fresh_path("resolv");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("resolv.conf");
        std::fs::write(&file, "nameserver 192.0.2.1\n").unwrap();
        assert_eq!(resolv_conf_target(&file), Some(file.clone()));
        // A dangling link into /run (resolved not running): its target.
        let link = dir.join("link.conf");
        std::os::unix::fs::symlink("../../run/x-melissa-test/stub.conf", &link).unwrap();
        let want = PathBuf::from("/run/x-melissa-test/stub.conf");
        let got = resolv_conf_target(&link).unwrap();
        assert!(
            got.ends_with("run/x-melissa-test/stub.conf"),
            "{got:?} {want:?}"
        );
        // A link to an existing file outside /etc and /run: cannot be covered.
        let other = dir.join("other.conf");
        std::os::unix::fs::symlink(&file, &other).unwrap();
        assert_eq!(resolv_conf_target(&other), None);
        assert_eq!(resolv_conf_target(&dir.join("missing")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn isolated_without_a_bridge_has_no_network_and_host_ignores_bridges() {
        let mut p = plan(vec![], "/usr/bin/x");
        p.network = Network::Isolated(None);
        let (_, argv) = command(&p, Path::new("/w"), &[], &[]);
        let a = strs(&argv);
        assert!(a.contains(&"--unshare-all") && !a.contains(&"--share-net"));
        assert!(!a
            .iter()
            .any(|x| x.contains("PROXY") || x.contains("netbridge")));
        assert_eq!(&a[a.len() - 2..], ["--", "/usr/bin/x"]);

        // `network = "host"`: the bridge is ignored, the host resolver is visible.
        let p = plan(vec![], "/usr/bin/x").with_bridge(Some(bridge()));
        assert_eq!(p.network, Network::Host);
        let (_, argv) = command(&p, Path::new("/w"), &[], &[]);
        let a = strs(&argv);
        assert!(a.contains(&"--share-net"));
        assert!(a.contains(&"/run/systemd/resolve"));
        assert!(!a.iter().any(|x| x.contains("PROXY")));
    }

    #[test]
    fn network_modes_and_allowlists_from_config() {
        let spec = SandboxSpec::default();
        assert_eq!(spec.network, NetworkMode::Proxy);
        let rules = spec.egress_allow("/usr/local/bin/claude").unwrap();
        assert!(rules.iter().any(|r| r.matches("api.anthropic.com", 443)));
        assert!(spec.egress_allow("unknown-cli").unwrap().is_empty());
        let custom = SandboxSpec {
            allow: Some(vec!["api.example.com".into(), "*.example.net:8443".into()]),
            ..SandboxSpec::default()
        };
        custom.validate().unwrap();
        let rules = custom.egress_allow("claude").unwrap();
        assert!(!rules.iter().any(|r| r.matches("api.anthropic.com", 443)));
        assert!(rules.iter().any(|r| r.matches("a.example.net", 8443)));
        let bad = SandboxSpec {
            allow: Some(vec!["*".into()]),
            ..SandboxSpec::default()
        };
        assert!(bad.validate().is_err());
        for name in ["HTTPS_PROXY", "no_proxy", "ALL_PROXY"] {
            let spec = SandboxSpec {
                env: vec![name.into()],
                ..SandboxSpec::default()
            };
            assert!(spec.validate().is_err(), "{name} accepted");
        }
        // `resolve` maps the mode to a namespace: only `host` shares the host's.
        for (mode, host) in [
            (NetworkMode::Proxy, false),
            (NetworkMode::Learn, false),
            (NetworkMode::None, false),
            (NetworkMode::Host, true),
        ] {
            let spec = SandboxSpec {
                network: mode,
                ..SandboxSpec::default()
            };
            let plan = SandboxPlan::resolve(
                &spec,
                Path::new(BWRAP_PATH),
                Some(Path::new("/home/u")),
                Path::new("/usr/bin/sh"),
            )
            .unwrap();
            assert_eq!(plan.network == Network::Host, host, "{mode:?}");
        }
    }

    #[test]
    fn binary_under_a_bind_or_usr_is_not_bound_twice() {
        let p = plan(
            vec![Mount::ReadOnly("/home/u/.tool".into())],
            "/home/u/.tool/bin/tool",
        );
        let (_, argv) = command(&p, Path::new("/w"), &[], &[]);
        // Only as the command after `--`, not as a bind.
        let a = strs(&argv);
        assert_eq!(
            a.iter().filter(|x| **x == "/home/u/.tool/bin/tool").count(),
            1
        );

        let p = plan(vec![], "/usr/bin/bash");
        let (_, argv) = command(&p, Path::new("/w"), &[], &[]);
        assert_eq!(
            strs(&argv)
                .iter()
                .filter(|x| **x == "/usr/bin/bash")
                .count(),
            1
        );
        assert!(has(
            &strs(&argv),
            &["--setenv", "PATH", "/usr/bin:/usr/bin"]
        ));
    }

    #[test]
    fn nixos_layout_binds_the_store_and_system_profile() {
        let mut p = plan(vec![], "/nix/store/abc-claude/bin/claude").with_bridge(Some(bridge()));
        p.system = SystemRoot::from_facts(
            vec![TopDir::Bind("/bin".into()), TopDir::Bind("/lib64".into())],
            true,
            false,
        );
        let (_, argv) = command(&p, Path::new("/w"), &[], &[]);
        let a = strs(&argv);
        assert!(has(&a, &["--ro-bind", "/bin", "/bin"]));
        assert!(has(&a, &["--ro-bind", "/lib64", "/lib64"]));
        assert!(!a.contains(&"usr/lib64"), "{a:?}");
        assert!(has(
            &a,
            &["--ro-bind", "/run/current-system", "/run/current-system"]
        ));
        // The bridge's runtime dir is the store too: bound once.
        assert_eq!(a.iter().filter(|x| **x == "/nix/store").count(), 2, "{a:?}");
        assert!(has(
            &a,
            &[
                "--setenv",
                "PATH",
                "/nix/store/abc-claude/bin:/usr/bin:/run/current-system/sw/bin"
            ]
        ));
        // A usr-merged host with a CLI from Nix (e.g. `nix profile`): the store, no profile.
        let s = SystemRoot::from_facts(vec![TopDir::UsrLink("bin")], false, true);
        assert_eq!(s.ro, [PathBuf::from("/nix/store")]);
        assert!(s.path.is_empty());
        assert_eq!(
            SystemRoot::from_facts(vec![], false, false),
            SystemRoot {
                top: vec![],
                ..SystemRoot::default()
            }
        );
    }

    #[test]
    fn extra_files_outside_the_workdir_are_bound_read_only() {
        let p = plan(vec![], "/usr/bin/x");
        let (_, argv) = command(
            &p,
            Path::new("/w"),
            &["/w/mcp.json".into(), "/elsewhere/mcp.json".into()],
            &[],
        );
        let a = strs(&argv);
        assert!(has(
            &a,
            &["--ro-bind", "/elsewhere/mcp.json", "/elsewhere/mcp.json"]
        ));
        assert!(!a.contains(&"/w/mcp.json"));
    }

    #[test]
    fn session_store_binds_land_inside_the_home_tmpfs() {
        let p = plan(vec![Mount::Tmpfs("/home/u/.grok".into())], "/usr/bin/x")
            .with_binds(&[("/state/lead/store".into(), ".grok/sessions".into())])
            .unwrap();
        let (_, argv) = command(&p, Path::new("/w"), &[], &[]);
        let a = strs(&argv);
        assert!(has(
            &a,
            &["--bind", "/state/lead/store", "/home/u/.grok/sessions"]
        ));
        // After the tmpfs it lives in, so the tmpfs does not hide it.
        let tmpfs = a.iter().position(|x| *x == "/home/u/.grok").unwrap();
        let bind = a.iter().position(|x| *x == "/state/lead/store").unwrap();
        assert!(bind > tmpfs);
        for bad in ["", "../x", "/abs", ".grok/../.."] {
            assert!(plan(vec![], "/usr/bin/x")
                .with_binds(&[("/s".into(), bad.into())])
                .is_err());
        }
    }

    #[test]
    fn presets_for_known_clis() {
        let claude = SandboxSpec::preset("/opt/bin/claude");
        assert!(claude.enabled);
        assert_eq!(claude.rw, [".claude/.credentials.json"]);
        assert_eq!(SandboxSpec::preset("grok").rw, [".grok/auth.json"]);
        // Only OpenCode's config files, not its whole config dir (service password, plugins).
        let opencode = SandboxSpec::preset("opencode");
        assert_eq!(
            opencode.ro,
            [
                ".config/opencode/opencode.json",
                ".config/opencode/opencode.jsonc"
            ]
        );
        assert!(opencode.tmpfs.contains(&".config/opencode".to_string()));
        assert!(opencode.tmpfs.contains(&".opencode".to_string()));
        // Managed (ACP flavor): none of the user's config.
        let managed = SandboxSpec::preset_opencode_managed();
        assert!(managed.ro.is_empty());
        assert_eq!(managed.rw, opencode.rw);
        managed.validate().unwrap();
        assert_eq!(SandboxSpec::preset("other"), SandboxSpec::default());
        for name in ["claude", "grok", "opencode", "gemini", "codex-acp", "muse"] {
            SandboxSpec::preset(name).validate().unwrap();
        }
        // npm installs: the package folder read-only (the wrapper runs node on it there),
        // the agent's state fresh, its sign-in read-write.
        let gemini = SandboxSpec::preset("/var/lib/melissa/home/.local/bin/gemini");
        assert_eq!(gemini.ro, [".local/share/melissa-npm/gemini"]);
        assert_eq!(gemini.tmpfs, [".gemini"]);
        assert_eq!(gemini.rw, [".gemini/oauth_creds.json"]);
        let codex = SandboxSpec::preset("codex-acp");
        assert_eq!(codex.ro, [".local/share/melissa-npm/codex-acp"]);
        assert_eq!(codex.rw, [".codex/auth.json"]);
        let muse = SandboxSpec::preset("muse");
        assert!(muse.ro.is_empty());
        assert!(muse.tmpfs.contains(&".config/muse".to_string()));
        assert_eq!(muse.rw, [".config/muse/auth.json"]);
        // `codex` (the plain CLI) is not the adapter: no preset.
        assert_eq!(SandboxSpec::preset("codex"), SandboxSpec::default());
    }

    #[test]
    fn validation_rejects_escapes_and_reserved_env() {
        let bad_paths = ["", ".", "..", "../x", "a/../../x", "/etc", "./x"];
        for path in bad_paths {
            let spec = SandboxSpec {
                ro: vec![path.into()],
                ..SandboxSpec::default()
            };
            assert!(spec.validate().is_err(), "{path:?} accepted");
        }
        assert!(check_env_name("OPENCODE_CONFIG_CONTENT").is_ok());
        assert!(check_env_name("HTTPS_PROXY").is_err());
        assert!(check_env_name("GROK_WEB_FETCH_PROXY").is_err());
        for name in ["HOME", "PATH", "", "1X", "A-B", "A B"] {
            let spec = SandboxSpec {
                env: vec![name.into()],
                ..SandboxSpec::default()
            };
            assert!(spec.validate().is_err(), "{name:?} accepted");
        }
        let ok = SandboxSpec {
            rw: vec![".config/x/token".into()],
            env: vec!["XAI_API_KEY".into()],
            ..SandboxSpec::default()
        };
        ok.validate().unwrap();
    }

    #[test]
    fn resolve_skips_missing_paths_and_orders_parents_first() {
        let home = crate::test_support::fresh_path("sbx-resolve");
        std::fs::create_dir_all(home.join(".cli")).unwrap();
        std::fs::write(home.join(".cli/token"), "t").unwrap();
        let spec = SandboxSpec {
            rw: vec![".cli/token".into(), ".cli/missing".into()],
            tmpfs: vec![".cli".into()],
            ..SandboxSpec::default()
        };
        let plan = SandboxPlan::resolve(
            &spec,
            Path::new(BWRAP_PATH),
            Some(&home),
            Path::new("/usr/bin/sh"),
        )
        .unwrap();
        assert_eq!(
            plan.mounts,
            [
                Mount::Tmpfs(home.join(".cli")),
                Mount::ReadWrite(home.join(".cli/token")),
            ]
        );
        assert!(plan.program.is_absolute());
        assert!(matches!(
            SandboxPlan::resolve(&spec, Path::new(BWRAP_PATH), None, Path::new("/usr/bin/sh")),
            Err(SandboxError::NoHome)
        ));
        std::fs::remove_dir_all(&home).unwrap();
    }
}
