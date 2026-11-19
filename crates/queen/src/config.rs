//! Queen bee configuration, read from `$XDG_CONFIG_HOME/melissa/queen.toml`.
//! Every field is optional; a missing file means "use the defaults".

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{bail, Context};
use melissa_agent::{AcpSpec, ClaudeSpec, CliSpec, EchoSpec};
use serde::{de::Error as _, Deserialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct QueenConfig {
    /// Address for the web UI. Must be loopback until accounts and sessions exist.
    pub listen: SocketAddr,
    /// Maximum run time for one agent task, in seconds.
    pub task_timeout_secs: u64,
    /// How long a tool call waits for a human in the confirmation inbox, in seconds; then
    /// it is withdrawn and the agent is told. Keep it below the agents' own MCP request
    /// timeouts: Claude Code gives up after 300 s but leaves the request open (no cancel,
    /// no close), so a longer wait would leave a confirmation that runs, once approved,
    /// for an agent that stopped waiting.
    pub confirm_timeout_secs: u64,
    /// Agent backends: one-shot CLIs (`kind = "cli"`, the default), ACP agents
    /// (`kind = "acp"`) and Claude Code sessions over stream-json (`kind = "claude"`). The
    /// built-in `echo` backend is always present in addition.
    #[serde(rename = "backend")]
    pub backends: Vec<BackendSpec>,
    /// Kubeconfig for the local k3s API, used when no `melissa.kubeconfig` systemd credential
    /// is present (e.g. a desktop queen pointed at a cluster); checked before the headless
    /// default `/run/melissa-kube/queen.kubeconfig`. Re-read on every cluster call.
    pub kubeconfig: Option<PathBuf>,
    /// The Queen (head agent) and its bees.
    pub queen: QueenSection,
    /// The app catalog and app installs.
    pub apps: AppsSection,
}

/// `[apps]`: where the catalog is, which domain apps get, the write identity's kubeconfig.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AppsSection {
    /// Directory of app manifests (`<name>.toml`).
    pub catalog: PathBuf,
    /// Apps are also served at `<app>.<domain>` (for people with their own DNS).
    pub domain: String,
    /// Addresses (IPs or host names) people reach this device at, for app links
    /// `http://<address>:<port>`, most useful first. Unset: found automatically (the LAN
    /// address of the default route and Tailscale addresses).
    pub addresses: Option<Vec<String>>,
    /// Kubeconfig of the write identity (`melissa-apps`); default
    /// `/run/melissa-kube/apps.kubeconfig`. Re-read on every call.
    pub kubeconfig: Option<PathBuf>,
}

impl Default for AppsSection {
    fn default() -> Self {
        Self {
            catalog: PathBuf::from(melissa_catalog::DEFAULT_CATALOG_DIR),
            domain: melissa_catalog::DEFAULT_DOMAIN.into(),
            addresses: None,
            kubeconfig: None,
        }
    }
}

/// An IP address or a host name (one label or more): goes into app links and apps'
/// trusted hosts.
fn valid_address(a: &str) -> bool {
    a.parse::<std::net::IpAddr>().is_ok()
        || (a.len() <= 253
            && a.split('.').all(|l| {
                !l.is_empty()
                    && l.len() <= 63
                    && !l.starts_with('-')
                    && !l.ends_with('-')
                    && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }))
}

/// A DNS name of lowercase labels (`melissa.home.arpa`).
fn valid_domain(d: &str) -> bool {
    d.len() <= 200
        && d.split('.').count() >= 2
        && d.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

impl Default for QueenConfig {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 8080)),
            task_timeout_secs: 300,
            confirm_timeout_secs: 240,
            backends: default_backends(),
            kubeconfig: None,
            queen: QueenSection::default(),
            apps: AppsSection::default(),
        }
    }
}

/// `[queen]`: the head agent's session backends, tools and bee limits. The old `worker_*`
/// and `max_workers` keys are still read (aliases, one release).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct QueenSection {
    /// Session backends (`kind = "acp"` or `"claude"`) to try in order; the next is used
    /// if one fails to start.
    pub backends: Vec<String>,
    /// Tools the Queen may use (default: all). Include the bee tools (`create_bee`,
    /// `give_bee_job`, `bee_status`, `list_bees`, `stop_bee`, `message_bee`) to let her
    /// direct bees, and `reassign_cell` to move cells. Bees never get more than this.
    pub tools: Option<Vec<String>>,
    /// Backend for new bees when the Queen names none (default: the first Queen backend).
    /// Must be a session backend (`kind = "acp"` or `"claude"`).
    #[serde(alias = "worker_backend")]
    pub bee_backend: Option<String>,
    /// Bees working on a job at the same time.
    #[serde(alias = "max_workers")]
    pub max_bees: usize,
    /// Bee time limit when the Queen gives none, in seconds.
    #[serde(alias = "worker_timeout_secs")]
    pub bee_timeout_secs: u64,
    /// Largest bee time limit the Queen may ask for, in seconds.
    #[serde(alias = "worker_timeout_max_secs")]
    pub bee_timeout_max_secs: u64,
    /// Size at which the transcript is rotated (one old file is kept), in KiB.
    pub transcript_max_kib: u64,
}

impl Default for QueenSection {
    fn default() -> Self {
        Self {
            backends: vec![
                "grok-acp".into(),
                "claude-session".into(),
                "opencode-acp".into(),
            ],
            tools: None,
            bee_backend: None,
            max_bees: 2,
            bee_timeout_secs: 300,
            bee_timeout_max_secs: 900,
            transcript_max_kib: 4096,
        }
    }
}

/// One `[[backend]]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendSpec {
    /// A headless CLI run once per task (`claude -p`, `grok -p`, ...).
    Cli(CliSpec),
    /// An agent speaking ACP on stdio (`grok agent stdio`).
    Acp(AcpSpec),
    /// Claude Code over its stream-json protocol (one process per session).
    Claude(ClaudeSpec),
    /// A session partner with no LLM that repeats messages (`kind = "echo"`): demos, and
    /// trying the chat and backend switching without an agent subscription.
    Echo(EchoSpec),
}

impl BackendSpec {
    pub fn name(&self) -> &str {
        match self {
            Self::Cli(s) => &s.name,
            Self::Acp(s) => &s.name,
            Self::Claude(s) => &s.name,
            Self::Echo(s) => &s.name,
        }
    }

    /// Can hold a long-lived session (the Queen's).
    pub fn is_session(&self) -> bool {
        matches!(self, Self::Acp(_) | Self::Claude(_) | Self::Echo(_))
    }

    #[cfg(test)]
    fn effective_sandbox(&self) -> melissa_agent::SandboxSpec {
        match self {
            Self::Cli(s) => s.effective_sandbox(),
            Self::Acp(s) => s.effective_sandbox(),
            Self::Claude(s) => s.effective_sandbox(),
            Self::Echo(_) => melissa_agent::SandboxSpec::disabled(),
        }
    }

    fn validate(&self) -> Result<(), melissa_agent::sandbox::SandboxError> {
        match self {
            Self::Cli(s) => s.validate(),
            Self::Acp(s) => s.validate(),
            Self::Claude(s) => s.validate(),
            Self::Echo(_) => Ok(()),
        }
    }
}

impl<'de> Deserialize<'de> for BackendSpec {
    /// `kind` selects the variant (default `cli`); the remaining keys must fit it exactly.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut table = toml::Table::deserialize(d)?;
        let kind = match table.remove("kind") {
            None => "cli".to_string(),
            Some(toml::Value::String(kind)) => kind,
            Some(other) => {
                return Err(D::Error::custom(format!(
                    "backend `kind` must be a string, got {other}"
                )))
            }
        };
        let value = toml::Value::Table(table);
        match kind.as_str() {
            "cli" => value.try_into().map(Self::Cli).map_err(D::Error::custom),
            "acp" => value.try_into().map(Self::Acp).map_err(D::Error::custom),
            "claude" => value.try_into().map(Self::Claude).map_err(D::Error::custom),
            "echo" => value.try_into().map(Self::Echo).map_err(D::Error::custom),
            other => Err(D::Error::custom(format!(
                "unknown backend kind `{other}` (expected `cli`, `acp`, `claude` or `echo`)"
            ))),
        }
    }
}

/// Headless invocations of the agent CLIs Melissa knows about. Backends whose binary is
/// missing show up as "not installed" and cannot be selected. Each runs in its built-in
/// bubblewrap sandbox preset (`SandboxSpec::preset`) unless the config says otherwise.
pub fn default_backends() -> Vec<BackendSpec> {
    let cli = [
        // `--tools ""` removes Claude's built-in tools (shell, file access); it acts only
        // through Melissa's tools, offered via a per-task MCP config.
        CliSpec::new("claude", "claude", &["-p", "{prompt}", "--tools", ""]).with_mcp_args(&[
            "--mcp-config",
            "{mcp_config}",
            "--strict-mcp-config",
            "--allowedTools",
            "mcp__melissa",
        ]),
        CliSpec::new("grok", "grok", &["-p", "{prompt}"]),
        CliSpec::new("opencode", "opencode", &["run", "{prompt}"]),
    ];
    let acp = [
        // Grok over ACP. `--permission-mode default` is passed explicitly so a user config
        // with auto-approve (`permission_mode = "always-approve"`) cannot apply: every
        // built-in tool call comes to Melissa as a permission request. `--no-leader`: one
        // agent process per session, inside our sandbox (no shared leader socket).
        // Its sessions live in `~/.grok/sessions`; for the Queen, Melissa binds its own
        // directory there so a session can be resumed after a restart.
        AcpSpec::new(
            "grok-acp",
            "grok",
            &[
                "--permission-mode",
                "default",
                "agent",
                "--no-leader",
                "stdio",
            ],
        )
        .with_session_store(".grok/sessions"),
        // OpenCode over ACP. Melissa writes its whole config (flavor `opencode`): every tool
        // asks (`permission: {"*": "ask"}`), Melissa's MCP server without Code Mode, the
        // user's own config hidden. Sessions live in its database under
        // `~/.local/share/opencode`; for the Queen that directory is Melissa's own.
        AcpSpec::new("opencode-acp", "opencode", &["acp"])
            .with_flavor(melissa_agent::acp::AcpFlavor::Opencode)
            .with_session_store(".local/share/opencode"),
    ];
    // Claude Code as a session: stream-json in and out, no built-in tools, every permission
    // prompt answered by Melissa (`--permission-prompt-tool stdio`); sessions in
    // `~/.claude/projects` (the Queen's: a Melissa-owned directory bound there).
    let claude = ClaudeSpec::new("claude-session", "claude");
    cli.into_iter()
        .map(BackendSpec::Cli)
        .chain(acp.into_iter().map(BackendSpec::Acp))
        .chain(std::iter::once(BackendSpec::Claude(claude)))
        .collect()
}

impl QueenConfig {
    /// Load from `path`. A missing file yields the defaults; a malformed one is an error.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let config: Self =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if !self.listen.ip().is_loopback() {
            bail!(
                "listen = {} is not a loopback address; the UI has no login yet, so only \
                 127.0.0.1/::1 are allowed",
                self.listen
            );
        }
        if self.task_timeout_secs == 0 {
            bail!("task_timeout_secs must be greater than 0");
        }
        if !(1..=3600).contains(&self.confirm_timeout_secs) {
            bail!("confirm_timeout_secs must be 1-3600");
        }
        if !valid_domain(&self.apps.domain) {
            bail!(
                "[apps] domain `{}` must be a lowercase DNS name like `melissa.home.arpa`",
                self.apps.domain
            );
        }
        for a in self.apps.addresses.iter().flatten() {
            if !valid_address(a) {
                bail!("[apps] addresses: `{a}` must be an IP address or a host name");
            }
        }
        let mut names = vec!["echo"];
        for b in &self.backends {
            if names.contains(&b.name()) {
                bail!("duplicate backend name `{}`", b.name());
            }
            names.push(b.name());
            b.validate()
                .with_context(|| format!("backend `{}`", b.name()))?;
        }
        let q = &self.queen;
        if q.backends.is_empty() {
            bail!("[queen] backends must name at least one session backend");
        }
        for name in &q.backends {
            match self.backends.iter().find(|b| b.name() == name) {
                Some(b) if b.is_session() => {}
                Some(_) => bail!(
                    "[queen] backend `{name}` is a one-shot CLI backend; the Queen needs a \
                     session backend (`kind = \"acp\"` or `\"claude\"`)"
                ),
                // Not fatal: a config that replaces the built-in backends still serves the UI;
                // the Queen reports the backend as unavailable when someone talks to it.
                None => tracing::warn!(
                    "[queen] backend `{name}` is not configured; the Queen cannot use it"
                ),
            }
        }
        if let Some(w) = &q.bee_backend {
            match self.backends.iter().find(|b| b.name() == w) {
                Some(b) if b.is_session() => {}
                Some(_) => bail!(
                    "[queen] bee_backend `{w}` is a one-shot CLI backend; bees need a session \
                     backend (`kind = \"acp\"` or `\"claude\"`)"
                ),
                None if names.contains(&w.as_str()) => {
                    bail!("[queen] bee_backend `{w}` is not a session backend; bees need one")
                }
                None => bail!("[queen] bee_backend `{w}` is not a configured backend"),
            }
        }
        if q.bee_timeout_secs == 0 || q.bee_timeout_secs > q.bee_timeout_max_secs {
            bail!("[queen] bee_timeout_secs must be between 1 and bee_timeout_max_secs");
        }
        Ok(())
    }

    pub fn task_timeout(&self) -> Duration {
        Duration::from_secs(self.task_timeout_secs)
    }

    pub fn confirm_timeout(&self) -> Duration {
        Duration::from_secs(self.confirm_timeout_secs)
    }
}

/// XDG base directories for Melissa (`~/.config/melissa`, `~/.local/state/melissa`).
pub struct Dirs {
    pub config: PathBuf,
    pub state: PathBuf,
    /// Per-sandbox egress sockets: `$XDG_RUNTIME_DIR/melissa/egress`, else under `state`.
    pub egress: PathBuf,
}

impl Dirs {
    pub fn from_env() -> anyhow::Result<Self> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let base = |var: &str, fallback: &str| -> anyhow::Result<PathBuf> {
            match std::env::var_os(var).map(PathBuf::from) {
                Some(p) if p.is_absolute() => Ok(p),
                _ => home
                    .as_ref()
                    .map(|h| h.join(fallback))
                    .with_context(|| format!("neither {var} nor HOME is set")),
            }
        };
        let state = base("XDG_STATE_HOME", ".local/state")?.join("melissa");
        let egress = match std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
            Some(p) if p.is_absolute() => p.join("melissa").join("egress"),
            _ => state.join("egress"),
        };
        Ok(Self {
            config: base("XDG_CONFIG_HOME", ".config")?.join("melissa"),
            state,
            egress,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apps_section_defaults_and_domain_check() {
        let config: QueenConfig = toml::from_str("").unwrap();
        assert_eq!(config.apps.domain, "melissa.home.arpa");
        assert_eq!(config.apps.catalog, Path::new("/usr/share/melissa/catalog"));
        let config: QueenConfig = toml::from_str(
            "[apps]\ndomain = \"home.example.com\"\ncatalog = \"/tmp/c\"\nkubeconfig = \"/tmp/k\"",
        )
        .unwrap();
        config.validate().unwrap();
        for bad in ["Home.Example", "localhost", "a..b", "-x.y", "x y.z"] {
            let config: QueenConfig =
                toml::from_str(&format!("[apps]\ndomain = \"{bad}\"")).unwrap();
            assert!(config.validate().is_err(), "{bad}");
        }
        assert!(toml::from_str::<QueenConfig>("[apps]\nimage = \"x\"").is_err());
        let config: QueenConfig = toml::from_str(
            "[apps]\naddresses = [\"192.0.2.15\", \"2001:db8::1\", \"box.example.com\", \"melissa\"]",
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(config.apps.addresses.as_ref().map(Vec::len), Some(4));
        for bad in ["a b", "http://x", "x..y", ""] {
            let config: QueenConfig =
                toml::from_str(&format!("[apps]\naddresses = [\"{bad}\"]")).unwrap();
            assert!(config.validate().is_err(), "{bad}");
        }
    }

    #[test]
    fn empty_file_means_defaults() {
        let config: QueenConfig = toml::from_str("").unwrap();
        assert_eq!(config, QueenConfig::default());
        config.validate().unwrap();
    }

    #[test]
    fn parses_custom_backends() {
        let config: QueenConfig = toml::from_str(
            r#"
            listen = "127.0.0.1:9000"
            task_timeout_secs = 60
            confirm_timeout_secs = 120
            kubeconfig = "/run/example/kubeconfig"
            [[backend]]
            name = "gemma"
            command = "opencode"
            args = ["run", "-m", "ollama/gemma3", "{prompt}"]
            "#,
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(config.listen.port(), 9000);
        assert_eq!(config.confirm_timeout(), Duration::from_secs(120));
        assert_eq!(config.backends.len(), 1);
        assert_eq!(
            config.kubeconfig.as_deref(),
            Some(Path::new("/run/example/kubeconfig"))
        );
        let BackendSpec::Cli(gemma) = &config.backends[0] else {
            panic!("expected a cli backend: {:?}", config.backends[0]);
        };
        assert_eq!(
            gemma.render_args("hi"),
            ["run", "-m", "ollama/gemma3", "hi"]
        );
    }

    #[test]
    fn rejects_unsafe_or_invalid_config() {
        let public: QueenConfig = toml::from_str(r#"listen = "0.0.0.0:8080""#).unwrap();
        assert!(public.validate().is_err());
        let dup: QueenConfig =
            toml::from_str("[[backend]]\nname = \"echo\"\ncommand = \"x\"").unwrap();
        assert!(dup.validate().is_err());
        assert!(toml::from_str::<QueenConfig>("nonsense = 1").is_err());
        for bad in ["confirm_timeout_secs = 0", "confirm_timeout_secs = 3601"] {
            let c: QueenConfig = toml::from_str(bad).unwrap();
            assert!(c.validate().is_err(), "{bad}");
        }
        assert!(QueenConfig::default().confirm_timeout() < Duration::from_secs(300));
    }

    #[test]
    fn sandbox_is_on_by_default_and_configurable() {
        for b in default_backends() {
            assert!(b.effective_sandbox().enabled, "{}", b.name());
        }
        // Old configs without a sandbox table get the preset for their command.
        let config: QueenConfig =
            toml::from_str("[[backend]]\nname = \"gemma\"\ncommand = \"opencode\"").unwrap();
        config.validate().unwrap();
        assert_eq!(
            config.backends[0].effective_sandbox(),
            melissa_agent::SandboxSpec::preset("opencode")
        );
        let config: QueenConfig = toml::from_str(
            r#"
            [[backend]]
            name = "x"
            command = "x"
            [backend.sandbox]
            rw = [".x/token"]
            env = ["X_API_KEY"]
            "#,
        )
        .unwrap();
        config.validate().unwrap();
        let sandbox = config.backends[0].effective_sandbox();
        assert!(sandbox.enabled);
        assert_eq!(sandbox.rw, [".x/token"]);
        let off: QueenConfig = toml::from_str(
            "[[backend]]\nname = \"x\"\ncommand = \"x\"\nsandbox = { enabled = false }",
        )
        .unwrap();
        assert!(!off.backends[0].effective_sandbox().enabled);
        // Unknown keys and paths that escape $HOME are rejected.
        assert!(toml::from_str::<QueenConfig>(
            "[[backend]]\nname = \"x\"\ncommand = \"x\"\nsandbox = { network = false }",
        )
        .is_err());
        let escape: QueenConfig = toml::from_str(
            "[[backend]]\nname = \"x\"\ncommand = \"x\"\nsandbox = { ro = [\"../..\"] }",
        )
        .unwrap();
        assert!(escape.validate().is_err());
    }

    #[test]
    fn network_and_allowlist_are_backend_keys() {
        use melissa_agent::NetworkMode;
        // Default: deny-by-default proxy with the built-in allowlist for each CLI.
        for b in default_backends() {
            assert_eq!(
                b.effective_sandbox().network,
                NetworkMode::Proxy,
                "{}",
                b.name()
            );
        }
        let config: QueenConfig = toml::from_str(
            r#"
            [[backend]]
            name = "claude"
            command = "claude"
            network = "learn"
            [[backend]]
            kind = "acp"
            name = "g"
            command = "grok"
            allow = ["api.example.com", "*.example.net:8443"]
            [backend.sandbox]
            rw = [".grok/auth.json"]
            [[backend]]
            name = "h"
            command = "x"
            network = "host"
            "#,
        )
        .unwrap();
        config.validate().unwrap();
        let s = config.backends[0].effective_sandbox();
        assert_eq!(s.network, NetworkMode::Learn);
        // The preset's paths stay when only the network is changed.
        assert_eq!(s.rw, [".claude/.credentials.json"]);
        let s = config.backends[1].effective_sandbox();
        assert_eq!(s.network, NetworkMode::Proxy);
        assert_eq!(s.allow.as_ref().unwrap().len(), 2);
        assert_eq!(
            config.backends[2].effective_sandbox().network,
            NetworkMode::Host
        );
        for bad in [
            "[[backend]]\nname = \"x\"\ncommand = \"x\"\nnetwork = \"open\"",
            "[[backend]]\nname = \"x\"\ncommand = \"x\"\nallow = [\"*\"]",
            "[[backend]]\nname = \"x\"\ncommand = \"x\"\nsandbox = { network = \"host\" }",
        ] {
            let parsed = toml::from_str::<QueenConfig>(bad);
            assert!(
                parsed.is_err() || parsed.unwrap().validate().is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn parses_acp_backends() {
        let config: QueenConfig = toml::from_str(
            r#"
            [[backend]]
            kind = "acp"
            name = "fx"
            command = "fx"
            args = ["acp"]
            tools = false
            [[backend]]
            kind = "cli"
            name = "g"
            command = "grok"
            "#,
        )
        .unwrap();
        config.validate().unwrap();
        let BackendSpec::Acp(fx) = &config.backends[0] else {
            panic!("expected acp: {:?}", config.backends[0]);
        };
        assert_eq!(fx.args, ["acp"]);
        assert!(!fx.tools);
        assert!(fx.effective_sandbox().enabled);
        assert!(matches!(config.backends[1], BackendSpec::Cli(_)));
        // CLI-only keys are rejected on ACP backends and vice versa; unknown kinds too.
        for bad in [
            "[[backend]]\nkind = \"acp\"\nname = \"x\"\ncommand = \"x\"\nmcp_args = []",
            "[[backend]]\nname = \"x\"\ncommand = \"x\"\ntools = true",
            "[[backend]]\nkind = \"ssh\"\nname = \"x\"\ncommand = \"x\"",
            "[[backend]]\nkind = 1\nname = \"x\"\ncommand = \"x\"",
        ] {
            assert!(toml::from_str::<QueenConfig>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn grok_acp_is_built_in_with_a_strict_permission_mode() {
        let backends = default_backends();
        let grok = backends
            .iter()
            .find_map(|b| match b {
                BackendSpec::Acp(s) if s.name == "grok-acp" => Some(s),
                _ => None,
            })
            .unwrap();
        assert!(grok
            .args
            .windows(2)
            .any(|w| w == ["--permission-mode", "default"]));
        assert!(!grok.args.iter().any(|a| a.contains("always-approve")));
        assert!(grok.tools);
        // The one-shot `grok` backend stays.
        assert!(backends.iter().any(|b| b.name() == "grok"));
    }

    #[test]
    fn claude_and_opencode_session_backends_are_built_in() {
        let backends = default_backends();
        let claude = backends
            .iter()
            .find_map(|b| match b {
                BackendSpec::Claude(s) if s.name == "claude-session" => Some(s),
                _ => None,
            })
            .unwrap();
        assert_eq!(claude.session_store, ".claude/projects");
        assert!(claude.args.is_empty() && claude.tools);
        let opencode = backends
            .iter()
            .find_map(|b| match b {
                BackendSpec::Acp(s) if s.name == "opencode-acp" => Some(s),
                _ => None,
            })
            .unwrap();
        assert_eq!(opencode.flavor, melissa_agent::acp::AcpFlavor::Opencode);
        assert_eq!(opencode.args, ["acp"]);
        assert_eq!(
            opencode.session_store.as_deref(),
            Some(".local/share/opencode")
        );
        // The user's own OpenCode config (which may allow tools) is not bound.
        assert!(opencode.effective_sandbox().ro.is_empty());

        let config: QueenConfig = toml::from_str(
            r#"
            [[backend]]
            kind = "claude"
            name = "c"
            command = "claude"
            args = ["--model", "sonnet"]
            network = "learn"
            [[backend]]
            kind = "acp"
            name = "o"
            command = "opencode"
            args = ["acp"]
            flavor = "opencode"
            model = "opencode/big-pickle"
            [queen]
            backends = ["c", "o"]
            "#,
        )
        .unwrap();
        config.validate().unwrap();
        assert!(config.backends.iter().all(BackendSpec::is_session));

        // Echo session backends: no LLM, for demos and for trying backend switching.
        let config: QueenConfig = toml::from_str(
            "[[backend]]\nkind = \"echo\"\nname = \"echo-a\"\n[[backend]]\nkind = \"echo\"\n\
             name = \"echo-b\"\n[queen]\nbackends = [\"echo-a\", \"echo-b\"]",
        )
        .unwrap();
        config.validate().unwrap();
        // `echo` is the built-in one-shot backend's name.
        let dup: QueenConfig =
            toml::from_str("[[backend]]\nkind = \"echo\"\nname = \"echo\"").unwrap();
        assert!(dup.validate().is_err());
        assert!(toml::from_str::<QueenConfig>(
            "[[backend]]\nkind = \"echo\"\nname = \"e\"\ncommand = \"x\""
        )
        .is_err());
        for bad in [
            "[[backend]]\nkind = \"claude\"\nname = \"c\"\ncommand = \"claude\"\nargs = [\"--dangerously-skip-permissions\"]",
            "[[backend]]\nkind = \"claude\"\nname = \"c\"\ncommand = \"claude\"\nmcp_args = []",
            "[[backend]]\nkind = \"acp\"\nname = \"o\"\ncommand = \"opencode\"\nflavor = \"other\"",
            "[[backend]]\nkind = \"acp\"\nname = \"g\"\ncommand = \"grok\"\nmodel = \"a/b\"",
        ] {
            let parsed = toml::from_str::<QueenConfig>(bad);
            assert!(
                parsed.is_err() || parsed.unwrap().validate().is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn queen_section_defaults_and_checks() {
        let config = QueenConfig::default();
        assert_eq!(
            config.queen.backends,
            ["grok-acp", "claude-session", "opencode-acp"]
        );
        // Every default Queen backend is a built-in session backend.
        for name in &config.queen.backends {
            let b = config.backends.iter().find(|b| b.name() == name).unwrap();
            assert!(b.is_session(), "{name}");
        }
        assert_eq!(config.queen.max_bees, 2);
        let grok = config
            .backends
            .iter()
            .find_map(|b| match b {
                BackendSpec::Acp(s) if s.name == "grok-acp" => Some(s),
                _ => None,
            })
            .unwrap();
        assert_eq!(grok.session_store.as_deref(), Some(".grok/sessions"));

        let config: QueenConfig = toml::from_str(
            r#"
            [queen]
            backends = ["grok-acp"]
            tools = ["comb_info", "create_bee", "give_bee_job", "bee_status"]
            bee_backend = "claude-session"
            max_bees = 1
            bee_timeout_secs = 60
            bee_timeout_max_secs = 120
            "#,
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(config.queen.bee_backend.as_deref(), Some("claude-session"));
        // Keys from before the hive model still work.
        let old: QueenConfig =
            toml::from_str("[queen]\nmax_workers = 3\nworker_timeout_secs = 30").unwrap();
        assert_eq!(old.queen.max_bees, 3);
        assert_eq!(old.queen.bee_timeout_secs, 30);
        for bad in [
            "[queen]\nbackends = []",
            "[queen]\nbackends = [\"claude\"]",
            "[queen]\nbee_backend = \"nope\"",
            "[queen]\nbee_backend = \"echo\"",
            "[queen]\nbee_timeout_secs = 1000",
        ] {
            let c: QueenConfig = toml::from_str(bad).unwrap();
            assert!(c.validate().is_err(), "{bad}");
        }
        assert!(toml::from_str::<QueenConfig>("[queen]\nbogus = 1").is_err());
    }

    #[test]
    fn missing_file_yields_defaults() {
        let path = std::env::temp_dir().join("melissa-no-such-config.toml");
        assert_eq!(QueenConfig::load(&path).unwrap(), QueenConfig::default());
    }
}
