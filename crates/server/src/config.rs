//! Server configuration, read from `$XDG_CONFIG_HOME/melissa/server.toml`.
//! Every field is optional; a missing file means "use the defaults".

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{bail, Context};
use melissa_agent::{AcpSpec, ClaudeSpec, CliSpec, EchoSpec};
use melissa_settings::providers::{BackendFacts, ProviderFile, ProviderOverride};
use serde::{de::Error as _, Deserialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
    /// Address for the web UI. Must be loopback until accounts and sessions exist.
    pub listen: SocketAddr,
    /// Maximum run time for one agent task, in seconds.
    pub task_timeout_secs: u64,
    /// How long a tool call waits for a human in the confirmation inbox, in seconds; then
    /// it is withdrawn and the agent is told. Keep it below the agents' own MCP request
    /// timeouts: Claude Code gives up after 300 s but leaves the request open (no cancel,
    /// no close), so a longer wait would leave a confirmation that runs, once approved,
    /// for an agent that stopped waiting (until its turn ends, which withdraws it).
    /// Scheduled runs use their own wait, capped below the backend's limit.
    pub confirm_timeout_secs: u64,
    /// Agent backends: one-shot CLIs (`kind = "cli"`, the default), ACP agents
    /// (`kind = "acp"`) and Claude Code sessions over stream-json (`kind = "claude"`). The
    /// built-in `echo` backend is always present in addition.
    #[serde(rename = "backend")]
    pub backends: Vec<BackendSpec>,
    /// Kubeconfig for the local k3s API, used when no `melissa.kubeconfig` systemd credential
    /// is present (e.g. a desktop lead agent pointed at a cluster); checked before the headless
    /// default `/run/melissa-kube/server.kubeconfig`. Re-read on every cluster call.
    pub kubeconfig: Option<PathBuf>,
    /// The lead agent and its agents. `[queen]` is the old name (alias, one release).
    #[serde(alias = "queen")]
    pub lead: LeadSection,
    /// App deploys.
    pub apps: AppsSection,
    /// Sandboxed agents' network beyond their backends' allowlists.
    pub egress: EgressSection,
}

/// `[egress]`: what sandboxed agents may reach besides their backend's own service.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EgressSection {
    /// Agents may read any public web page through Melissa's proxy: HTTPS (port 443) and
    /// plain HTTP GET/HEAD (port 80), read-only, every fetch audited, and a task that read
    /// the web needs the owner's OK for its changes. Off: only the backends' allowlists.
    pub web_read: bool,
}

impl Default for EgressSection {
    fn default() -> Self {
        Self { web_read: true }
    }
}

/// `[apps]`: which domain apps get, how app links are built, the write identity's kubeconfig.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AppsSection {
    /// Apps are also served at `<app>.<domain>` (for people with their own DNS).
    pub domain: String,
    /// Addresses (IPs or host names) people reach this device at, for app links
    /// `http://<address>:<port>`, most useful first. Unset: found automatically (the LAN
    /// address of the default route and Tailscale addresses).
    pub addresses: Option<Vec<String>>,
    /// Added to an app's NodePort (30000-30079) in app links, when the host publishes the
    /// NodePorts at other ports: container mode with `APP_PORT_BASE=31000` publishes 30000+n
    /// as 31000+n, so the offset is 1000. Default 0.
    pub port_offset: i32,
    /// Kubeconfig of the write identity (`melissa-apps`); default
    /// `/run/melissa-kube/apps.kubeconfig`. Re-read on every call.
    pub kubeconfig: Option<PathBuf>,
    /// Kubeconfig of the setup password reader (`melissa-credentials`, the owner's reveal in
    /// the web UI only); default `/run/melissa-kube/credentials.kubeconfig`.
    pub credentials_kubeconfig: Option<PathBuf>,
}

impl Default for AppsSection {
    fn default() -> Self {
        Self {
            domain: melissa_appspec::DEFAULT_DOMAIN.into(),
            addresses: None,
            port_offset: 0,
            kubeconfig: None,
            credentials_kubeconfig: None,
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

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 8080)),
            task_timeout_secs: 300,
            confirm_timeout_secs: 240,
            backends: default_backends(),
            kubeconfig: None,
            lead: LeadSection::default(),
            apps: AppsSection::default(),
            egress: EgressSection::default(),
        }
    }
}

/// `[lead]`: the lead agent's session backends, tools and agent limits. The old `[queen]`
/// section and its `bee_*`, `max_bees`, `worker_*` and `max_workers` keys are still read
/// (aliases, one release; old tool names in `tools` are mapped to the new ones).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LeadSection {
    /// Session backends (`kind = "acp"` or `"claude"`) to try in order; the next is used
    /// if one fails to start.
    pub backends: Vec<String>,
    /// Tools the lead agent may use (default: all). Include the agent tools (`create_agent`,
    /// `give_agent_job`, `agent_status`, `list_agents`, `stop_agent`, `message_agent`) to let its
    /// direct agents, `reassign_workspace` to move workspaces and `delete_agent` to delete agents
    /// (always confirmed). Agents never get more than this.
    pub tools: Option<Vec<String>>,
    /// Backend for new agents when the lead agent names none (default: the first lead agent backend).
    /// Once the user switches the lead agent's backend, new agents follow that instead.
    /// Must be a session backend (`kind = "acp"` or `"claude"`).
    #[serde(alias = "worker_backend", alias = "bee_backend")]
    pub agent_backend: Option<String>,
    /// Agents working on a job at the same time.
    #[serde(alias = "max_workers", alias = "max_bees")]
    pub max_agents: usize,
    /// Agent time limit when the lead agent gives none, in seconds.
    #[serde(alias = "worker_timeout_secs", alias = "bee_timeout_secs")]
    pub agent_timeout_secs: u64,
    /// Largest agent time limit the lead agent may ask for, in seconds.
    #[serde(alias = "worker_timeout_max_secs", alias = "bee_timeout_max_secs")]
    pub agent_timeout_max_secs: u64,
    /// Size at which the transcript is rotated (one old file is kept), in KiB.
    pub transcript_max_kib: u64,
    /// Context use (percent of the model's window) from which Melissa condenses a
    /// conversation's older messages once it is idle (0: never). Backends that compact on
    /// their own (Claude Code) are left alone.
    pub compact_at_percent: u8,
    /// How long a conversation must be idle before its context is condensed, in seconds.
    pub compact_idle_secs: u64,
}

impl Default for LeadSection {
    fn default() -> Self {
        Self {
            backends: vec![
                "grok-acp".into(),
                "claude-session".into(),
                "opencode-acp".into(),
                "gemini-acp".into(),
                "codex-acp".into(),
                "muse-acp".into(),
            ],
            tools: None,
            agent_backend: None,
            max_agents: 2,
            agent_timeout_secs: 300,
            agent_timeout_max_secs: 900,
            transcript_max_kib: 4096,
            compact_at_percent: 70,
            compact_idle_secs: 120,
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

    /// Can hold a long-lived session (the lead agent's).
    pub fn is_session(&self) -> bool {
        matches!(self, Self::Acp(_) | Self::Claude(_) | Self::Echo(_))
    }

    /// The program it runs (None for the built-in echo partner).
    pub fn command(&self) -> Option<&str> {
        match self {
            Self::Cli(s) => Some(&s.command),
            Self::Acp(s) => Some(&s.command),
            Self::Claude(s) => Some(&s.command),
            Self::Echo(_) => None,
        }
    }

    /// The provider (agent program) behind it: the command's file name, or `echo`.
    pub fn provider(&self) -> String {
        self.command()
            .map(melissa_settings::providers::provider_id)
            .unwrap_or_else(|| "echo".into())
    }

    /// Put the owner's provider settings into this backend: another program path, extra
    /// arguments before Melissa's own, and variables set inside the sandbox.
    fn apply_override(&mut self, o: &ProviderOverride) {
        fn args(own: &mut Vec<String>, extra: &[String]) {
            if !extra.is_empty() {
                let mut all = extra.to_vec();
                all.append(own);
                *own = all;
            }
        }
        let vars = o.vars();
        match self {
            Self::Cli(s) => {
                if !vars.is_empty() {
                    let mut sb = s.effective_sandbox();
                    sb.vars = vars;
                    s.sandbox = Some(sb);
                }
                if let Some(c) = &o.command {
                    s.command = c.clone();
                }
                args(&mut s.args, &o.args);
            }
            Self::Acp(s) => {
                if !vars.is_empty() {
                    let mut sb = s.effective_sandbox();
                    sb.vars = vars;
                    s.sandbox = Some(sb);
                }
                if let Some(c) = &o.command {
                    s.command = c.clone();
                }
                args(&mut s.args, &o.args);
            }
            Self::Claude(s) => {
                if !vars.is_empty() {
                    let mut sb = s.effective_sandbox();
                    sb.vars = vars;
                    s.sandbox = Some(sb);
                }
                if let Some(c) = &o.command {
                    s.command = c.clone();
                }
                args(&mut s.args, &o.args);
            }
            Self::Echo(_) => {}
        }
    }

    /// The sandbox this backend runs in: its preset with `[backend.sandbox]` applied.
    pub fn effective_sandbox(&self) -> melissa_agent::SandboxSpec {
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
/// bubblewrap sandbox preset (`SandboxSpec::preset`), changed by `[backend.sandbox]` if given.
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
        // Its sessions live in `~/.grok/sessions`; for the lead agent, Melissa binds its own
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
        // `~/.local/share/opencode`; for the lead agent that directory is Melissa's own.
        AcpSpec::new("opencode-acp", "opencode", &["acp"])
            .with_flavor(melissa_agent::acp::AcpFlavor::Opencode)
            .with_session_store(".local/share/opencode"),
        // Gemini CLI over ACP (`--acp`). `--approval-mode default` and the `default` session
        // mode: every tool asks (a user setting can't pick YOLO; its settings stay hidden).
        // No session store: Gemini loads sessions but doesn't resume them over ACP.
        AcpSpec::new(
            "gemini-acp",
            "gemini",
            &["--acp", "--approval-mode", "default"],
        )
        .with_flavor(melissa_agent::acp::AcpFlavor::Gemini)
        .with_mode("default"),
        // Codex through its ACP adapter (`@agentclientprotocol/codex-acp`, which runs `codex
        // app-server`). Its default mode ("Auto review") runs what it deems safe without
        // asking; every session is switched to `read-only` (asks before edits and network).
        // Sessions live in `~/.codex/sessions`.
        AcpSpec::new("codex-acp", "codex-acp", &[])
            .with_flavor(melissa_agent::acp::AcpFlavor::Codex)
            .with_mode("read-only")
            .with_session_store(".codex/sessions"),
        // Muse Code over ACP (`muse acp`, behind `MUSE_EXPERIMENTAL_ACP=on`): telemetry off,
        // no LLM approval judge (it could approve on Melissa's behalf), and the session mode
        // that asks for every action no permission rule covers (its default asks only "when
        // the action requires it"). Sessions live in `~/.local/share/muse`.
        AcpSpec::new(
            "muse-acp",
            "muse",
            &[
                "acp",
                "-c",
                "telemetry.enabled=false",
                "-c",
                "approval_judge=\"off\"",
            ],
        )
        .with_flavor(melissa_agent::acp::AcpFlavor::Muse)
        .with_mode("promptUnmatched")
        .with_session_store(".local/share/muse"),
    ];
    // Claude Code as a session: stream-json in and out, no built-in tools, every permission
    // prompt answered by Melissa (`--permission-prompt-tool stdio`); sessions in
    // `~/.claude/projects` (the lead agent's: a Melissa-owned directory bound there).
    let claude = ClaudeSpec::new("claude-session", "claude");
    cli.into_iter()
        .map(BackendSpec::Cli)
        .chain(acp.into_iter().map(BackendSpec::Acp))
        .chain(std::iter::once(BackendSpec::Claude(claude)))
        .collect()
}

/// Keys from before the rename to generic terms (docs/AGENT-MODEL.md) that `text` still uses.
pub fn legacy_keys(text: &str) -> Vec<&'static str> {
    const OLD: [&str; 5] = [
        "bee_backend",
        "max_bees",
        "bee_timeout_secs",
        "bee_timeout_max_secs",
        "queen",
    ];
    let Ok(table) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    let mut found: Vec<&'static str> = Vec::new();
    if table.contains_key("queen") {
        found.push("[queen]");
    }
    for section in ["queen", "lead"] {
        if let Some(toml::Value::Table(s)) = table.get(section) {
            found.extend(
                OLD.iter()
                    .copied()
                    .filter(|k| *k != "queen" && s.contains_key(*k)),
            );
        }
    }
    found
}

impl ServerConfig {
    /// Load from `path`. A missing file yields the defaults; a malformed one is an error.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let mut config: Self =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        if let Some(tools) = &mut config.lead.tools {
            for tool in tools.iter_mut() {
                if let Some(new) = melissa_agent::migrate::legacy_tool_name(tool) {
                    *tool = new.to_string();
                }
            }
        }
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if !self.listen.ip().is_loopback() {
            bail!(
                "listen = {} is not a loopback address; sign-in runs over plain HTTP, so only \
                 127.0.0.1/::1 are allowed (from elsewhere: SSH or `tailscale serve`, which adds HTTPS)",
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
        let offset = self.apps.port_offset;
        if !(1 - 30000..=65535 - 30079).contains(&offset) {
            bail!(
                "[apps] port_offset {offset} puts app ports outside 1-65535 (NodePorts are \
                 30000-30079)"
            );
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
        let q = &self.lead;
        if q.backends.is_empty() {
            bail!("[lead] backends must name at least one session backend");
        }
        for name in &q.backends {
            match self.backends.iter().find(|b| b.name() == name) {
                Some(b) if b.is_session() => {}
                Some(_) => bail!(
                    "[lead] backend `{name}` is a one-shot CLI backend; the lead agent needs a \
                     session backend (`kind = \"acp\"` or `\"claude\"`)"
                ),
                // Not fatal: a config that replaces the built-in backends still serves the UI;
                // the lead agent reports the backend as unavailable when someone talks to it.
                None => tracing::warn!(
                    "[lead] backend `{name}` is not configured; the lead agent cannot use it"
                ),
            }
        }
        if let Some(w) = &q.agent_backend {
            match self.backends.iter().find(|b| b.name() == w) {
                Some(b) if b.is_session() => {}
                Some(_) => bail!(
                    "[lead] agent_backend `{w}` is a one-shot CLI backend; agents need a session \
                     backend (`kind = \"acp\"` or `\"claude\"`)"
                ),
                None if names.contains(&w.as_str()) => {
                    bail!("[lead] agent_backend `{w}` is not a session backend; agents need one")
                }
                None => bail!("[lead] agent_backend `{w}` is not a configured backend"),
            }
        }
        if q.agent_timeout_secs == 0 || q.agent_timeout_secs > q.agent_timeout_max_secs {
            bail!("[lead] agent_timeout_secs must be between 1 and agent_timeout_max_secs");
        }
        Ok(())
    }

    /// Merge the owner's provider settings (`providers.toml`) over this config: they win over
    /// `server.toml`, which wins over the defaults. Backends of a disabled provider are left
    /// out, and so are their names in `[lead] backends` and `agent_backend` (unless that would
    /// leave the lead agent with none: then the setting is ignored, with a warning). Returns every
    /// backend before leaving any out, for the Settings page.
    pub fn apply_providers(&mut self, file: &ProviderFile) -> Vec<BackendFacts> {
        // ACP agents the owner added (Settings → Providers → Add provider): one session backend
        // each, offered to the lead agent after the configured ones. Deny-by-default network:
        // only the hosts the owner gave, through the egress proxy.
        for (id, a) in &file.added {
            let name = format!("{id}-acp");
            if self.backends.iter().any(|b| b.name() == name) {
                tracing::warn!(backend = %name, "providers.toml adds a backend that exists; ignored");
                continue;
            }
            let args: Vec<&str> = a.args.iter().map(String::as_str).collect();
            let spec = AcpSpec::new(&name, &a.command, &args).with_allow(a.allow.clone());
            if let Err(e) = spec.validate() {
                tracing::warn!(backend = %name, "ignoring added provider: {e}");
                continue;
            }
            self.backends.push(BackendSpec::Acp(spec));
            if !self.lead.backends.contains(&name) {
                self.lead.backends.push(name);
            }
        }
        for b in &mut self.backends {
            let o = file.get(&b.provider());
            let before = b.clone();
            b.apply_override(&o);
            // A hand-edited file must not keep Melissa from starting: drop the override.
            if let Err(e) = b.validate() {
                tracing::warn!(
                    backend = %b.name(),
                    "ignoring providers.toml for this backend: {e}"
                );
                *b = before;
            }
        }
        let facts = |b: &BackendSpec, enabled: bool| BackendFacts {
            name: b.name().to_string(),
            provider: b.provider(),
            session: b.is_session(),
            command: b.command().map(str::to_string),
            enabled,
        };
        let disabled: Vec<String> = self
            .backends
            .iter()
            .filter(|b| !file.get(&b.provider()).is_enabled())
            .map(|b| b.name().to_string())
            .collect();
        let lead_left = self
            .lead
            .backends
            .iter()
            .filter(|n| !disabled.contains(n))
            .count();
        if !disabled.is_empty() && lead_left == 0 {
            tracing::warn!(
                ?disabled,
                "providers.toml disables every Lead backend; ignoring that until one is enabled"
            );
            return self.backends.iter().map(|b| facts(b, true)).collect();
        }
        let all = self
            .backends
            .iter()
            .map(|b| facts(b, !disabled.contains(&b.name().to_string())))
            .collect();
        self.backends
            .retain(|b| !disabled.iter().any(|d| d == b.name()));
        self.lead.backends.retain(|n| !disabled.contains(n));
        if self
            .lead
            .agent_backend
            .as_ref()
            .is_some_and(|b| disabled.contains(b))
        {
            self.lead.agent_backend = None;
        }
        all
    }

    /// Whether `o` as provider `id`'s settings gives backends that pass their own checks (for
    /// the Settings page, before saving). `self` is the config without provider settings.
    pub fn check_provider(&self, id: &str, o: &ProviderOverride) -> Result<(), String> {
        for b in self.backends.iter().filter(|b| b.provider() == id) {
            let mut b = b.clone();
            b.apply_override(o);
            b.validate().map_err(|e| format!("{}: {e}", b.name()))?;
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

/// XDG base directories for Melissa (`~/.config/melissa`, `~/.local/state/melissa`,
/// `~/.local/share/melissa`).
pub struct Dirs {
    pub config: PathBuf,
    pub state: PathBuf,
    /// Downloaded data that can be fetched again (the speech model):
    /// `$XDG_DATA_HOME/melissa` (`/var/lib/melissa` in the service).
    pub data: PathBuf,
    /// Per-sandbox egress sockets: `$XDG_RUNTIME_DIR/melissa/egress`, else under `state`.
    pub egress: PathBuf,
    /// Short-lived private files (a voice clip while it is transcribed):
    /// `$XDG_RUNTIME_DIR/melissa` (tmpfs), else `state/tmp`.
    pub scratch: PathBuf,
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
        let runtime = match std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
            Some(p) if p.is_absolute() => Some(p.join("melissa")),
            _ => None,
        };
        let egress = match &runtime {
            Some(r) => r.join("egress"),
            None => state.join("egress"),
        };
        let scratch = runtime.unwrap_or_else(|| state.join("tmp"));
        Ok(Self {
            config: base("XDG_CONFIG_HOME", ".config")?.join("melissa"),
            data: base("XDG_DATA_HOME", ".local/share")?.join("melissa"),
            state,
            egress,
            scratch,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apps_section_defaults_and_domain_check() {
        let config: ServerConfig = toml::from_str("").unwrap();
        assert_eq!(config.apps.domain, "melissa.home.arpa");
        let config: ServerConfig =
            toml::from_str("[apps]\ndomain = \"home.example.com\"\nkubeconfig = \"/tmp/k\"")
                .unwrap();
        config.validate().unwrap();
        for bad in ["Home.Example", "localhost", "a..b", "-x.y", "x y.z"] {
            let config: ServerConfig =
                toml::from_str(&format!("[apps]\ndomain = \"{bad}\"")).unwrap();
            assert!(config.validate().is_err(), "{bad}");
        }
        assert!(toml::from_str::<ServerConfig>("[apps]\nimage = \"x\"").is_err());
        let config: ServerConfig = toml::from_str(
            "[apps]\naddresses = [\"192.0.2.15\", \"2001:db8::1\", \"box.example.com\", \"melissa\"]",
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(config.apps.addresses.as_ref().map(Vec::len), Some(4));
        assert_eq!(config.apps.port_offset, 0);
        let shifted: ServerConfig = toml::from_str("[apps]\nport_offset = 1000").unwrap();
        shifted.validate().unwrap();
        assert_eq!(shifted.apps.port_offset, 1000);
        for bad in ["40000", "-30000"] {
            let c: ServerConfig = toml::from_str(&format!("[apps]\nport_offset = {bad}")).unwrap();
            assert!(c.validate().is_err(), "{bad}");
        }
        for bad in ["a b", "http://x", "x..y", ""] {
            let config: ServerConfig =
                toml::from_str(&format!("[apps]\naddresses = [\"{bad}\"]")).unwrap();
            assert!(config.validate().is_err(), "{bad}");
        }
    }

    #[test]
    fn empty_file_means_defaults() {
        let config: ServerConfig = toml::from_str("").unwrap();
        assert_eq!(config, ServerConfig::default());
        config.validate().unwrap();
    }

    #[test]
    fn parses_custom_backends() {
        let config: ServerConfig = toml::from_str(
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
        let public: ServerConfig = toml::from_str(r#"listen = "0.0.0.0:8080""#).unwrap();
        assert!(public.validate().is_err());
        let dup: ServerConfig =
            toml::from_str("[[backend]]\nname = \"echo\"\ncommand = \"x\"").unwrap();
        assert!(dup.validate().is_err());
        assert!(toml::from_str::<ServerConfig>("nonsense = 1").is_err());
        for bad in ["confirm_timeout_secs = 0", "confirm_timeout_secs = 3601"] {
            let c: ServerConfig = toml::from_str(bad).unwrap();
            assert!(c.validate().is_err(), "{bad}");
        }
        assert!(ServerConfig::default().confirm_timeout() < Duration::from_secs(300));
    }

    #[test]
    fn sandbox_is_on_by_default_and_configurable() {
        for b in default_backends() {
            assert!(b.effective_sandbox().enabled, "{}", b.name());
        }
        // Old configs without a sandbox table get the preset for their command.
        let config: ServerConfig =
            toml::from_str("[[backend]]\nname = \"gemma\"\ncommand = \"opencode\"").unwrap();
        config.validate().unwrap();
        assert_eq!(
            config.backends[0].effective_sandbox(),
            melissa_agent::SandboxSpec::preset("opencode")
        );
        let config: ServerConfig = toml::from_str(
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
        let off: ServerConfig = toml::from_str(
            "[[backend]]\nname = \"x\"\ncommand = \"x\"\nsandbox = { enabled = false }",
        )
        .unwrap();
        assert!(!off.backends[0].effective_sandbox().enabled);
        // Unknown keys and paths that escape $HOME are rejected.
        assert!(toml::from_str::<ServerConfig>(
            "[[backend]]\nname = \"x\"\ncommand = \"x\"\nsandbox = { network = false }",
        )
        .is_err());
        let escape: ServerConfig = toml::from_str(
            "[[backend]]\nname = \"x\"\ncommand = \"x\"\nsandbox = { ro = [\"../..\"] }",
        )
        .unwrap();
        assert!(escape.validate().is_err());
    }

    /// One backend with the given lines after `command`.
    fn sandbox_of(command: &str, rest: &str) -> melissa_agent::SandboxSpec {
        let config: ServerConfig = toml::from_str(&format!(
            "[[backend]]\nname = \"b\"\ncommand = \"{command}\"\n{rest}"
        ))
        .unwrap();
        config.validate().unwrap();
        config.backends[0].effective_sandbox()
    }

    #[test]
    fn partial_sandbox_tables_keep_the_preset() {
        use melissa_agent::sandbox::limits::{Bytes, Limits};
        let preset = melissa_agent::SandboxSpec::preset("claude");
        let paths = |s: &melissa_agent::SandboxSpec| (s.tmpfs.clone(), s.ro.clone(), s.rw.clone());

        // Only limits: the preset's paths stay; unset limit keys keep their defaults.
        let s = sandbox_of("claude", "[backend.sandbox.limits]\nmemory = \"3G\"");
        assert_eq!(paths(&s), paths(&preset));
        assert!(s.enabled && s.seccomp);
        assert_eq!(s.limits.memory, Bytes(Some(3 << 30)));
        assert_eq!(s.limits.pids, Limits::default().pids);

        // Only seccomp = false: everything else is the preset.
        let s = sandbox_of("claude", "[backend.sandbox]\nseccomp = false");
        assert!(!s.seccomp);
        assert_eq!(
            s,
            melissa_agent::SandboxSpec {
                seccomp: false,
                ..sandbox_of("claude", "")
            }
        );

        // Only egress (backend keys): the preset's paths and limits stay.
        let s = sandbox_of(
            "claude",
            "network = \"learn\"\nallow = [\"api.example.com\"]",
        );
        assert_eq!(paths(&s), paths(&preset));
        assert_eq!(s.limits, Limits::default());
        assert_eq!(s.allow, Some(vec!["api.example.com".to_string()]));

        // Lists add to the preset (no duplicates).
        let s = sandbox_of(
            "claude",
            "[backend.sandbox]\nro = [\".config/x\"]\nrw = [\".claude/.credentials.json\"]\nenv = [\"X_KEY\"]",
        );
        assert_eq!(s.rw, preset.rw);
        assert_eq!(s.ro, [".config/x"]);
        assert_eq!(s.tmpfs, preset.tmpfs);
        assert_eq!(s.env, ["X_KEY"]);

        // The managed OpenCode preset (no user config files) stays the base for its flavor.
        let s = sandbox_of(
            "opencode",
            "kind = \"acp\"\nflavor = \"opencode\"\n[backend.sandbox]\nseccomp = false",
        );
        assert!(s.ro.is_empty());
        assert_eq!(
            s.tmpfs,
            melissa_agent::SandboxSpec::preset("opencode").tmpfs
        );
    }

    #[test]
    fn full_sandbox_override_and_opt_out() {
        use melissa_agent::sandbox::limits::Pids;
        // preset = false: exactly the table, nothing from the preset.
        let s = sandbox_of(
            "claude",
            r#"[backend.sandbox]
            preset = false
            enabled = true
            seccomp = true
            tmpfs = [".claude"]
            rw = [".claude/token"]
            env = ["ANTHROPIC_API_KEY"]
            [backend.sandbox.limits]
            pids = 64"#,
        );
        assert_eq!(s.tmpfs, [".claude"]);
        assert_eq!(s.rw, [".claude/token"]);
        assert!(s.ro.is_empty());
        assert_eq!(s.env, ["ANTHROPIC_API_KEY"]);
        assert_eq!(s.limits.pids, Pids(Some(64)));
        let bare = sandbox_of("claude", "[backend.sandbox]\npreset = false");
        assert!(bare.rw.is_empty() && bare.tmpfs.is_empty() && bare.enabled);
        // An empty list would once have dropped the preset's paths; now it is refused.
        for bad in ["rw = []", "ro = []", "tmpfs = []", "env = []"] {
            let config: ServerConfig = toml::from_str(&format!(
                "[[backend]]\nname = \"b\"\ncommand = \"claude\"\n[backend.sandbox]\n{bad}"
            ))
            .unwrap();
            assert!(config.validate().is_err(), "{bad}");
        }
        let ok: ServerConfig = toml::from_str(
            "[[backend]]\nname = \"b\"\ncommand = \"claude\"\n[backend.sandbox]\npreset = false\nrw = []",
        )
        .unwrap();
        ok.validate().unwrap();
        // Unknown keys inside limits are rejected too.
        assert!(toml::from_str::<ServerConfig>(
            "[[backend]]\nname = \"b\"\ncommand = \"claude\"\n[backend.sandbox.limits]\nmem = 1",
        )
        .is_err());
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
        let config: ServerConfig = toml::from_str(
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
            let parsed = toml::from_str::<ServerConfig>(bad);
            assert!(
                parsed.is_err() || parsed.unwrap().validate().is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn parses_acp_backends() {
        let config: ServerConfig = toml::from_str(
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
            assert!(toml::from_str::<ServerConfig>(bad).is_err(), "{bad}");
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

        let config: ServerConfig = toml::from_str(
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
            [lead]
            backends = ["c", "o"]
            "#,
        )
        .unwrap();
        config.validate().unwrap();
        assert!(config.backends.iter().all(BackendSpec::is_session));

        // Echo session backends: no LLM, for demos and for trying backend switching.
        let config: ServerConfig = toml::from_str(
            "[[backend]]\nkind = \"echo\"\nname = \"echo-a\"\n[[backend]]\nkind = \"echo\"\n\
             name = \"echo-b\"\n[lead]\nbackends = [\"echo-a\", \"echo-b\"]",
        )
        .unwrap();
        config.validate().unwrap();
        // `echo` is the built-in one-shot backend's name.
        let dup: ServerConfig =
            toml::from_str("[[backend]]\nkind = \"echo\"\nname = \"echo\"").unwrap();
        assert!(dup.validate().is_err());
        assert!(toml::from_str::<ServerConfig>(
            "[[backend]]\nkind = \"echo\"\nname = \"e\"\ncommand = \"x\""
        )
        .is_err());
        for bad in [
            "[[backend]]\nkind = \"claude\"\nname = \"c\"\ncommand = \"claude\"\nargs = [\"--dangerously-skip-permissions\"]",
            "[[backend]]\nkind = \"claude\"\nname = \"c\"\ncommand = \"claude\"\nmcp_args = []",
            "[[backend]]\nkind = \"acp\"\nname = \"o\"\ncommand = \"opencode\"\nflavor = \"other\"",
            "[[backend]]\nkind = \"acp\"\nname = \"g\"\ncommand = \"grok\"\nmodel = \"a/b\"",
        ] {
            let parsed = toml::from_str::<ServerConfig>(bad);
            assert!(
                parsed.is_err() || parsed.unwrap().validate().is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn lead_section_defaults_and_checks() {
        let config = ServerConfig::default();
        assert_eq!(
            config.lead.backends,
            [
                "grok-acp",
                "claude-session",
                "opencode-acp",
                "gemini-acp",
                "codex-acp",
                "muse-acp"
            ]
        );
        // Every default lead agent backend is a built-in session backend.
        for name in &config.lead.backends {
            let b = config.backends.iter().find(|b| b.name() == name).unwrap();
            assert!(b.is_session(), "{name}");
        }
        assert_eq!(config.lead.max_agents, 2);
        let grok = config
            .backends
            .iter()
            .find_map(|b| match b {
                BackendSpec::Acp(s) if s.name == "grok-acp" => Some(s),
                _ => None,
            })
            .unwrap();
        assert_eq!(grok.session_store.as_deref(), Some(".grok/sessions"));

        let config: ServerConfig = toml::from_str(
            r#"
            [lead]
            backends = ["grok-acp"]
            tools = ["device_info", "create_agent", "give_agent_job", "agent_status"]
            agent_backend = "claude-session"
            max_agents = 1
            agent_timeout_secs = 60
            agent_timeout_max_secs = 120
            "#,
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(config.lead.agent_backend.as_deref(), Some("claude-session"));
        // Keys from before the cluster model still work.
        let old: ServerConfig =
            toml::from_str("[lead]\nmax_workers = 3\nworker_timeout_secs = 30").unwrap();
        assert_eq!(old.lead.max_agents, 3);
        assert_eq!(old.lead.agent_timeout_secs, 30);
        for bad in [
            "[lead]\nbackends = []",
            "[lead]\nbackends = [\"claude\"]",
            "[lead]\nagent_backend = \"nope\"",
            "[lead]\nagent_backend = \"echo\"",
            "[lead]\nagent_timeout_secs = 1000",
        ] {
            let c: ServerConfig = toml::from_str(bad).unwrap();
            assert!(c.validate().is_err(), "{bad}");
        }
        assert!(toml::from_str::<ServerConfig>("[lead]\nbogus = 1").is_err());
    }

    #[test]
    fn missing_file_yields_defaults() {
        let path = std::env::temp_dir().join("melissa-no-such-config.toml");
        assert_eq!(ServerConfig::load(&path).unwrap(), ServerConfig::default());
    }

    fn providers(text: &str) -> ProviderFile {
        toml::from_str(text).unwrap()
    }

    fn spec<'a>(config: &'a ServerConfig, name: &str) -> &'a BackendSpec {
        config.backends.iter().find(|b| b.name() == name).unwrap()
    }

    #[test]
    fn provider_settings_win_over_lead_toml_and_defaults() {
        // server.toml changes the claude-session args; providers.toml adds to them and
        // replaces the program; defaults fill in the rest.
        let mut config: ServerConfig = toml::from_str(
            r#"
            [[backend]]
            kind = "claude"
            name = "claude-session"
            command = "claude"
            args = ["--model", "sonnet"]
            [[backend]]
            kind = "acp"
            name = "grok-acp"
            command = "grok"
            args = ["agent", "stdio"]
            [lead]
            backends = ["claude-session", "grok-acp"]
            "#,
        )
        .unwrap();
        let file = providers(
            r#"
            [provider.claude]
            command = "/opt/claude/bin/claude"
            args = ["--effort", "high"]
            [provider.claude.env]
            ANTHROPIC_BASE_URL = "https://llm.example.com"
            ANTHROPIC_API_KEY = "sk-test"
            "#,
        );
        let facts = config.apply_providers(&file);
        config.validate().unwrap();
        assert_eq!(facts.len(), 2);
        assert!(facts.iter().all(|f| f.enabled));
        let BackendSpec::Claude(c) = spec(&config, "claude-session") else {
            panic!("not claude")
        };
        assert_eq!(c.command, "/opt/claude/bin/claude");
        assert_eq!(c.args, ["--effort", "high", "--model", "sonnet"]);
        let sandbox = c.effective_sandbox();
        assert_eq!(sandbox.vars.len(), 2);
        // The preset (chosen by the file name `claude`) is kept.
        assert!(sandbox.rw.iter().any(|p| p == ".claude/.credentials.json"));
        // Other providers are untouched.
        let BackendSpec::Acp(g) = spec(&config, "grok-acp") else {
            panic!("not acp")
        };
        assert_eq!(g.args, ["agent", "stdio"]);
        assert!(g.sandbox.is_none());
    }

    #[test]
    fn gemini_codex_and_muse_session_backends_are_built_in() {
        use melissa_agent::acp::AcpFlavor;
        let backends = default_backends();
        let acp = |name: &str| {
            backends
                .iter()
                .find_map(|b| match b {
                    BackendSpec::Acp(s) if s.name == name => Some(s.clone()),
                    _ => None,
                })
                .unwrap()
        };
        let gemini = acp("gemini-acp");
        assert_eq!(gemini.command, "gemini");
        assert_eq!(gemini.args, ["--acp", "--approval-mode", "default"]);
        assert_eq!(gemini.flavor, AcpFlavor::Gemini);
        assert_eq!(gemini.mode.as_deref(), Some("default"));
        let codex = acp("codex-acp");
        assert_eq!(codex.command, "codex-acp");
        assert_eq!(codex.flavor, AcpFlavor::Codex);
        // Its own default ("Auto review") runs tools without asking.
        assert_eq!(codex.mode.as_deref(), Some("read-only"));
        assert_eq!(codex.session_store.as_deref(), Some(".codex/sessions"));
        let muse = acp("muse-acp");
        assert_eq!(muse.command, "muse");
        assert_eq!(
            muse.args,
            [
                "acp",
                "-c",
                "telemetry.enabled=false",
                "-c",
                "approval_judge=\"off\""
            ]
        );
        assert_eq!(muse.flavor, AcpFlavor::Muse);
        assert_eq!(muse.mode.as_deref(), Some("promptUnmatched"));
        // Each runs in its own preset with its own egress list (by the program's name).
        for (spec, host) in [
            (&gemini, "generativelanguage.googleapis.com"),
            (&codex, "api.openai.com"),
            (&muse, "api.meta.ai"),
        ] {
            spec.validate().unwrap();
            let sandbox = spec.effective_sandbox();
            assert!(sandbox.enabled && !sandbox.rw.is_empty(), "{}", spec.name);
            let allow = sandbox.egress_allow(&spec.command).unwrap();
            assert!(
                allow.iter().any(|r| r.matches(host, 443)),
                "{}: {host}",
                spec.name
            );
        }
        // They are providers of their own (the program's file name).
        let providers: Vec<String> = ["gemini-acp", "codex-acp", "muse-acp"]
            .iter()
            .map(|n| backends.iter().find(|b| b.name() == *n).unwrap().provider())
            .collect();
        assert_eq!(providers, ["gemini", "codex-acp", "muse"]);
    }

    #[test]
    fn disabled_providers_are_left_out_but_never_all_lead_backends() {
        let mut config = ServerConfig::default();
        let facts = config.apply_providers(&providers("[provider.grok]\nenabled = false\n"));
        config.validate().unwrap();
        assert!(config.backends.iter().all(|b| b.provider() != "grok"));
        assert_eq!(
            config.lead.backends,
            [
                "claude-session",
                "opencode-acp",
                "gemini-acp",
                "codex-acp",
                "muse-acp"
            ]
        );
        let grok: Vec<_> = facts.iter().filter(|f| f.provider == "grok").collect();
        assert_eq!(grok.len(), 2);
        assert!(grok.iter().all(|f| !f.enabled));
        // Disabling every lead agent backend is ignored.
        let mut config = ServerConfig::default();
        config.apply_providers(&providers(
            "[provider.grok]\nenabled = false\n[provider.claude]\nenabled = false\n[provider.opencode]\nenabled = false\n\
             [provider.gemini]\nenabled = false\n[provider.codex-acp]\nenabled = false\n[provider.muse]\nenabled = false\n",
        ));
        assert_eq!(config.lead.backends.len(), 6);
        assert_eq!(config.backends.len(), default_backends().len());
        // A disabled agent default falls back to the lead agent's first backend.
        let mut config: ServerConfig =
            toml::from_str("[lead]\nagent_backend = \"opencode-acp\"").unwrap();
        config.apply_providers(&providers("[provider.opencode]\nenabled = false\n"));
        assert_eq!(config.lead.agent_backend, None);
        config.validate().unwrap();
    }

    #[test]
    fn overrides_a_backend_refuses_are_dropped() {
        let mut config = ServerConfig::default();
        config.apply_providers(&providers("[provider.claude]\nargs = [\"--verbose\"]\n"));
        config.validate().unwrap();
        let BackendSpec::Claude(c) = spec(&config, "claude-session") else {
            panic!("not claude")
        };
        assert!(c.args.is_empty());
        // The one-shot `claude` takes any argument, so it keeps it.
        let BackendSpec::Cli(c) = spec(&config, "claude") else {
            panic!("not cli")
        };
        assert_eq!(c.args[0], "--verbose");
    }

    #[test]
    fn no_provider_settings_change_nothing() {
        let mut config = ServerConfig::default();
        let facts = config.apply_providers(&ProviderFile::default());
        assert_eq!(config, ServerConfig::default());
        assert_eq!(facts.len(), config.backends.len());
    }

    #[test]
    fn added_providers_become_lead_backends_with_their_hosts() {
        let mut config = ServerConfig::default();
        let facts = config.apply_providers(&providers(
            r#"
            [added.kimi]
            label = "Kimi"
            command = "/opt/agents/kimi"
            args = ["acp"]
            allow = ["api.example.com"]
            [provider.kimi.env]
            KIMI_API_KEY = "k"
            "#,
        ));
        config.validate().unwrap();
        let BackendSpec::Acp(a) = spec(&config, "kimi-acp") else {
            panic!("not acp")
        };
        assert_eq!(a.command, "/opt/agents/kimi");
        assert_eq!(a.args, ["acp"]);
        let sb = a.effective_sandbox();
        assert!(sb.enabled);
        assert_eq!(
            sb.allow.as_deref(),
            Some(&["api.example.com".to_string()][..])
        );
        // Its API key from the provider settings reaches the sandbox.
        assert_eq!(sb.vars, [("KIMI_API_KEY".to_string(), "k".to_string())]);
        assert!(config.lead.backends.contains(&"kimi-acp".to_string()));
        assert!(facts
            .iter()
            .any(|f| f.provider == "kimi" && f.session && f.enabled));
    }

    #[test]
    fn a_removed_provider_is_gone_from_the_lead_agent_and_the_picker() {
        let mut config = ServerConfig::default();
        let facts = config.apply_providers(&providers("[provider.grok]\nremoved = true\n"));
        config.validate().unwrap();
        // No grok backend runs, so the lead agent (and the picker built from it) never
        // offers it; the Settings page still knows it, to add it again.
        assert!(!config.backends.iter().any(|b| b.provider() == "grok"));
        assert!(!config.lead.backends.iter().any(|b| b.starts_with("grok")));
        assert!(facts.iter().any(|f| f.provider == "grok" && !f.enabled));
    }
}
