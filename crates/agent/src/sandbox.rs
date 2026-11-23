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
//! The command line is built by the pure [`command`] function from a [`SandboxPlan`];
//! [`SandboxPlan::resolve`] does the host lookups (HOME, binary path, which files exist).

pub mod seccomp;

use std::{
    ffi::OsString,
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
];

/// Where the bridge's socket directory appears inside the sandbox.
pub const BRIDGE_DIR: &str = "/run/melissa-net";

/// Per-backend sandbox settings (`[backend.sandbox]` in the lead agent config).
///
/// Paths are relative to `$HOME`. Missing `ro`/`rw` paths are skipped, so a CLI that is
/// not logged in simply sees no credentials. If the table is given, it replaces the
/// built-in preset for that CLI entirely.
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
        })
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
    let mut push = |items: &[&dyn AsRef<std::ffi::OsStr>]| {
        argv.extend(items.iter().map(|i| i.as_ref().to_os_string()));
    };

    // Namespaces and privileges. bwrap always sets no_new_privs for unprivileged users.
    // `--unshare-all` includes the network namespace (bwrap brings up its loopback).
    push(&[&"--unshare-all"]);
    let bridge = match &plan.network {
        Network::Host => {
            push(&[&"--share-net"]);
            None
        }
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
    // Syscall filter, read from an fd the spawner maps ([`pass_seccomp`]); bwrap installs it
    // right before executing the first program inside.
    if plan.seccomp {
        push(&[&"--seccomp", &SECCOMP_FD.to_string()]);
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
    if plan.network == Network::Host {
        push(&[
            &"--ro-bind-try",
            &"/run/systemd/resolve",
            &"/run/systemd/resolve",
        ]);
    }
    push(&[&"--proc", &"/proc", &"--dev", &"/dev"]);
    push(&[&"--tmpfs", &"/tmp", &"--tmpfs", &"/var/tmp"]);

    // Home: an empty tmpfs with only the configured paths; frozen read-only below.
    push(&[&"--tmpfs", &plan.home]);
    for mount in &plan.mounts {
        match mount {
            Mount::Tmpfs(p) => push(&[&"--tmpfs", p]),
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

/// Give `cmd` (the `bwrap` command built from `plan`) the seccomp program at [`SECCOMP_FD`],
/// if the plan uses one. The program is a sealed memfd; only the child gets it (the parent's
/// copy is close-on-exec and closes when `cmd` is dropped), so no other process spawned
/// meanwhile inherits it. Call once per command.
pub fn pass_seccomp<C: CommandFdExt>(cmd: &mut C, plan: &SandboxPlan) -> std::io::Result<()> {
    if !plan.seccomp {
        return Ok(());
    }
    let mapping = FdMapping {
        parent_fd: seccomp::memfd()?,
        child_fd: SECCOMP_FD,
    };
    cmd.fd_mappings(vec![mapping])
        .map_err(std::io::Error::other)?;
    Ok(())
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
        let dir = std::env::temp_dir().join(format!("melissa-resolv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
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
        let home = std::env::temp_dir().join(format!("melissa-sbx-resolve-{}", std::process::id()));
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
