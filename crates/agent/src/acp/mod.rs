//! ACP (Agent Client Protocol) backends: long-lived agent sessions over JSON-RPC on the
//! agent's stdin/stdout, e.g. `grok agent stdio`.
//!
//! [`AcpBackend`] spawns the agent under the bubblewrap sandbox ([`crate::sandbox`]) and
//! implements both [`SessionBackend`] (open a session, many prompts: the lead agent) and
//! [`Backend`] (one task = one session with one prompt: agents on the task board).
//! Melissa declares no client file system or terminal capabilities: the agent acts through
//! its own sandboxed tools (each asked through `session/request_permission`, answered by
//! [`PolicyPermissions`]) and through Melissa's MCP tools.

pub(crate) mod client;
pub mod probe;
pub mod replay;
pub(crate) mod transport;

use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::{
    cli::{network_label, network_ready, open_egress, sandboxed_command, ProgramPath},
    egress::EgressProxy,
    mcp::McpServer,
    sandbox::{self, SandboxError, SandboxSpec},
    session::{
        McpServerSpec, OpenSession, OpenedSession, PermissionContext, PolicyPermissions,
        SessionBackend, SessionEvent, StopReason, ToolStatus,
    },
    tools::ToolRegistry,
    AgentError, Backend, BackendInfo, BoxFuture, EventSink, RunRequest,
};

pub use client::OPEN_TIMEOUT;

/// Name under which Melissa's MCP server is given to agents (tool names get this prefix).
pub const MCP_SERVER_NAME: &str = "melissa";

fn yes() -> bool {
    true
}

/// Agent-specific setup on top of plain ACP.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AcpFlavor {
    /// Plain ACP: Melissa's MCP server goes into `session/new`.
    #[default]
    Generic,
    /// OpenCode (`opencode acp`): Melissa writes the agent's whole config into
    /// `OPENCODE_CONFIG_CONTENT`: every tool asks (`permission: {"*": "ask"}`), Melissa's MCP
    /// server is configured there with Code Mode off (else OpenCode hides MCP tools behind
    /// its JavaScript `execute` tool), project config and auto-update are off.
    Opencode,
    /// Gemini CLI (`gemini --acp`): credentials in files (no keyring), and with no API key
    /// set, the Google sign-in in `~/.gemini/oauth_creds.json` (`GOOGLE_GENAI_USE_GCA`).
    Gemini,
    /// Codex through its ACP adapter (`codex-acp`): it wants `authenticate` even with its API
    /// key set, so Melissa signs in with `api-key` when `CODEX_API_KEY` or `OPENAI_API_KEY`
    /// is set (and then keeps the ChatGPT sign-in file out of the sandbox: `authenticate`
    /// would overwrite it with the key); else the ChatGPT sign-in in `~/.codex/auth.json`.
    Codex,
    /// Muse Code (`muse acp`): ACP is behind `MUSE_EXPERIMENTAL_ACP=on`.
    Muse,
}

/// API key variables Codex reads (`CODEX_API_KEY` wins).
const CODEX_KEYS: [&str; 2] = ["CODEX_API_KEY", "OPENAI_API_KEY"];
/// API key variables Gemini CLI reads.
const GEMINI_KEYS: [&str; 2] = ["GEMINI_API_KEY", "GOOGLE_API_KEY"];

/// The ACP flavor for a command (chosen by its file name), used by the built-in backends.
pub fn flavor_for(command: &str) -> AcpFlavor {
    match std::path::Path::new(command)
        .file_name()
        .and_then(|n| n.to_str())
    {
        Some("opencode") => AcpFlavor::Opencode,
        Some("gemini") => AcpFlavor::Gemini,
        Some("codex-acp") => AcpFlavor::Codex,
        Some("muse") => AcpFlavor::Muse,
        _ => AcpFlavor::Generic,
    }
}

/// Whether the sandbox sets one of `names` (a provider setting or a passed-through variable).
fn sets_any(sandbox: &SandboxSpec, names: &[&str]) -> bool {
    sandbox
        .vars
        .iter()
        .map(|(n, _)| n.as_str())
        .chain(sandbox.env.iter().map(String::as_str))
        .any(|n| names.contains(&n))
}

/// Fixed variables a flavor sets inside the sandbox (OpenCode's are [`opencode_env`]).
pub fn flavor_env(flavor: AcpFlavor, sandbox: &SandboxSpec) -> Vec<(String, String)> {
    let vars: &[(&str, &str)] = match flavor {
        AcpFlavor::Gemini if sets_any(sandbox, &GEMINI_KEYS) => {
            &[("GEMINI_FORCE_FILE_STORAGE", "true")]
        }
        AcpFlavor::Gemini => &[
            ("GEMINI_FORCE_FILE_STORAGE", "true"),
            ("GOOGLE_GENAI_USE_GCA", "true"),
        ],
        // No browser to open in the sandbox (hides the ChatGPT browser sign-in).
        AcpFlavor::Codex => &[("NO_BROWSER", "1")],
        AcpFlavor::Muse => &[("MUSE_EXPERIMENTAL_ACP", "on")],
        AcpFlavor::Generic | AcpFlavor::Opencode => &[],
    };
    vars.iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

/// The ACP sign-in method Melissa calls `authenticate` with for a flavor (None: none).
pub fn sign_in_method(flavor: AcpFlavor, sandbox: &SandboxSpec) -> Option<&'static str> {
    (flavor == AcpFlavor::Codex && sets_any(sandbox, &CODEX_KEYS)).then_some("api-key")
}

/// Configuration of one ACP backend (`kind = "acp"` in the lead agent config).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcpSpec {
    /// Unique backend name, e.g. `grok-acp`.
    pub name: String,
    /// Program to run: a bare name looked up on `PATH`, or a path.
    pub command: String,
    /// Arguments that start the agent's ACP server on stdio. Put the agent's strictest
    /// permission mode here explicitly, so its own config cannot turn on auto-approve.
    #[serde(default)]
    pub args: Vec<String>,
    /// Give the agent Melissa's tools (MCP over HTTP, passed in `session/new`).
    #[serde(default = "yes")]
    pub tools: bool,
    /// A whole sandbox set by code ([`AcpSpec::with_sandbox`]); never from the config file.
    #[serde(skip)]
    pub sandbox: Option<SandboxSpec>,
    /// `[backend.sandbox]`: changes to the built-in preset for this command
    /// ([`sandbox::SandboxConfig`]).
    #[serde(rename = "sandbox", default, skip_serializing_if = "Option::is_none")]
    pub sandbox_table: Option<sandbox::SandboxConfig>,
    /// Network access of the sandboxed agent: `proxy` (default: only the allowlisted hosts,
    /// through Melissa's egress proxy, plus Melissa's MCP endpoint), `learn` (every public
    /// host, logged), `none` (MCP only) or `host` (the unfiltered host network).
    #[serde(default)]
    pub network: crate::egress::NetworkMode,
    /// Hosts the agent may reach with `network = "proxy"`: `host` or `*.domain`, optionally
    /// `:port` (default 443). Absent: the built-in list for the command
    /// ([`crate::egress::default_allow`]); given: replaces it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<String>>,
    /// Debugging aid: write a scrubbed transcript of every session into this directory
    /// ([`replay`] format). Transcripts hold prompts and replies; off by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_dir: Option<PathBuf>,
    /// Where the agent keeps its sessions, relative to `$HOME` (e.g. `.grok/sessions`).
    /// For sessions opened with a [`OpenSession::state_dir`] (the lead agent), that Melissa-owned
    /// directory is bound here inside the sandbox so the session can be resumed later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_store: Option<String>,
    /// Agent-specific setup (`generic` or `opencode`).
    #[serde(default)]
    pub flavor: AcpFlavor,
    /// Model to use, where the flavor sets the agent's config (`opencode`: `provider/model`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// ACP session mode every session is switched to when it opens: the agent's strictest mode
    /// that still asks (Codex `read-only`, Muse `promptUnmatched`), so its own default (which
    /// may run tools without asking) never applies. A session fails if the agent lacks it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

impl AcpSpec {
    pub fn new(name: &str, command: &str, args: &[&str]) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            args: args.iter().map(|a| (*a).to_string()).collect(),
            tools: true,
            sandbox: None,
            sandbox_table: None,
            network: crate::egress::NetworkMode::default(),
            allow: None,
            record_dir: None,
            session_store: None,
            flavor: AcpFlavor::Generic,
            model: None,
            mode: None,
        }
    }

    /// Switch every session to this ACP mode ([`AcpSpec::mode`]).
    pub fn with_mode(mut self, mode: &str) -> Self {
        self.mode = Some(mode.into());
        self
    }

    pub fn with_flavor(mut self, flavor: AcpFlavor) -> Self {
        self.flavor = flavor;
        self
    }

    pub fn with_session_store(mut self, path: &str) -> Self {
        self.session_store = Some(path.into());
        self
    }

    pub fn with_sandbox(mut self, spec: SandboxSpec) -> Self {
        self.sandbox = Some(spec);
        self
    }

    /// Set the sandbox's network access.
    pub fn with_network(mut self, mode: crate::egress::NetworkMode) -> Self {
        self.network = mode;
        self
    }

    /// Replace the built-in egress allowlist.
    pub fn with_allow(mut self, hosts: Vec<String>) -> Self {
        self.allow = Some(hosts);
        self
    }

    pub fn without_sandbox(self) -> Self {
        self.with_sandbox(SandboxSpec::disabled())
    }

    pub fn effective_sandbox(&self) -> SandboxSpec {
        sandbox::effective(
            self.sandbox.as_ref(),
            self.sandbox_table.as_ref(),
            || match self.flavor {
                // Melissa writes OpenCode's whole config; the user's own (which may allow
                // tools without asking) stays hidden.
                AcpFlavor::Opencode => SandboxSpec::preset_opencode_managed(),
                _ => SandboxSpec::preset(&self.command),
            },
            self.network,
            self.allow.clone(),
        )
    }

    pub fn validate(&self) -> Result<(), SandboxError> {
        if let Some(model) = &self.model {
            let ok = self.flavor == AcpFlavor::Opencode
                && !model.is_empty()
                && model.len() <= 200
                && model
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | ':'));
            if !ok {
                return Err(SandboxError::BadModel(model.clone()));
            }
        }
        if let Some(store) = &self.session_store {
            let p = std::path::Path::new(store);
            if store.is_empty()
                || !p
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_)))
            {
                return Err(SandboxError::BadPath(store.clone()));
            }
        }
        if let Some(table) = &self.sandbox_table {
            table.validate()?;
        }
        self.effective_sandbox().validate()
    }
}

/// An agent that speaks ACP over stdio.
#[derive(Clone)]
pub struct AcpBackend {
    spec: AcpSpec,
    sandbox: SandboxSpec,
    program: ProgramPath,
    bwrap: Option<PathBuf>,
    home: Option<PathBuf>,
    workdir: PathBuf,
    /// Melissa's MCP server: its registry decides permissions, its sessions hold the
    /// per-task policy context. `None`: no tools, and requests that need a human are refused.
    mcp: Option<McpServer>,
    egress: Option<EgressProxy>,
}

impl AcpBackend {
    /// `workdir` is the default working directory (per-task dirs come from the board).
    pub fn new(spec: AcpSpec, workdir: PathBuf) -> Self {
        let program = ProgramPath::new(&spec.command);
        let mut sandbox = spec.effective_sandbox();
        if sign_in_method(spec.flavor, &sandbox).is_some() {
            // `authenticate` with the key writes it into `~/.codex/auth.json`: keep that file
            // in the sandbox's throw-away home, not the ChatGPT sign-in on the host.
            sandbox.rw.retain(|p| p != ".codex/auth.json");
        }
        let bwrap = if sandbox.enabled {
            sandbox::find_bwrap()
        } else {
            None
        };
        crate::cli::warn_host_network(&spec.name, &sandbox);
        Self {
            spec,
            sandbox,
            program,
            bwrap,
            home: std::env::var_os("HOME").map(PathBuf::from),
            workdir,
            mcp: None,
            egress: None,
        }
    }

    /// The egress proxy that gives sandboxes (`network = "proxy"`) their network.
    pub fn with_egress(mut self, egress: EgressProxy) -> Self {
        self.egress = Some(egress);
        self
    }

    /// Use Melissa's MCP server for tools and permission decisions.
    pub fn with_mcp(mut self, server: McpServer) -> Self {
        self.mcp = Some(server);
        self
    }

    /// Use `home` instead of `$HOME` for the sandbox (tests, service users).
    pub fn with_home(mut self, home: PathBuf) -> Self {
        self.home = Some(home);
        self
    }

    /// Permission handler for one task.
    fn permissions(&self, req: &RunRequest) -> PolicyPermissions {
        task_permissions(self.mcp.as_ref(), req, &self.spec.name)
    }

    async fn spawn(&self, open: OpenSession) -> Result<OpenedSession, AgentError> {
        let program = self
            .program
            .get()
            .ok_or_else(|| AgentError::Unavailable(format!("`{}` not found", self.spec.command)))?;
        let cwd = std::path::absolute(&open.cwd)?;
        tokio::fs::create_dir_all(&cwd).await?;
        let mut binds = Vec::new();
        if let (Some(dir), Some(store), true) = (
            &open.state_dir,
            &self.spec.session_store,
            self.sandbox.enabled,
        ) {
            let dir = std::path::absolute(dir)?;
            create_private_dir(&dir)?;
            binds.push((dir, store.clone()));
        }
        // Lives as long as the agent process (moved into the shutdown hook below).
        let tokens = open
            .mcp_servers
            .iter()
            .filter_map(|s| s.url.rsplit('/').next())
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect();
        let lease = open_egress(
            &self.sandbox,
            self.egress.as_ref(),
            &self.spec.command,
            &self.spec.name,
            open.task.unwrap_or_default(),
            tokens,
        )?;
        open.permissions
            .set_web_access(crate::cli::web_access(&self.sandbox, self.egress.as_ref()));
        let bridge = lease.as_ref().and_then(|l| l.bridge().cloned());
        // OpenCode gets Melissa's MCP server through its config, not `session/new`.
        let (env, session_servers) = match self.spec.flavor {
            AcpFlavor::Opencode => (
                opencode_env(
                    &open.mcp_servers,
                    open.model.as_deref().or(self.spec.model.as_deref()),
                ),
                Vec::new(),
            ),
            other => (flavor_env(other, &self.sandbox), open.mcp_servers.clone()),
        };
        let rw_dirs: Vec<std::path::PathBuf> = open.files_dir.iter().cloned().collect();
        let mut command = sandboxed_command(
            &self.sandbox,
            self.bwrap.as_deref(),
            self.home.as_deref(),
            &program,
            &cwd,
            &[],
            &binds,
            &rw_dirs,
            &env,
            bridge,
            self.spec.args.clone(),
        )?;
        if !self.sandbox.enabled {
            // Provider settings first, so Melissa's own variables win.
            let c = command.command_mut();
            c.envs(self.sandbox.vars.iter().map(|(k, v)| (k, v)));
            c.envs(env.iter().map(|(k, v)| (k, v)));
        }
        command
            .command_mut()
            .current_dir(&cwd)
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Dropping the session (task cancelled, timed out) kills the agent; under bwrap
            // that takes the whole sandbox (PID namespace) with it.
            .kill_on_drop(true);
        let crate::sandbox::SandboxChild { mut child, cgroup } =
            command.spawn().map_err(|source| AgentError::Spawn {
                command: self.spec.command.clone(),
                source,
            })?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(AgentError::Protocol("agent pipes missing".into()));
        };

        let tail = client::StderrTail::default();
        tokio::spawn({
            let tail = tail.clone();
            let name = self.spec.name.clone();
            async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(backend = %name, "agent stderr: {line}");
                    tail.push(line);
                }
            }
        });

        let recorder = match &self.spec.record_dir {
            Some(dir) => {
                let secrets = open
                    .mcp_servers
                    .iter()
                    .flat_map(|s| {
                        let token = s.url.rsplit('/').next().unwrap_or_default().to_string();
                        std::iter::once(token).chain(s.headers.iter().map(|h| h.1.clone()))
                    })
                    .collect();
                match replay::Recorder::create(
                    dir,
                    &self.spec.name,
                    self.home.as_deref(),
                    &cwd,
                    secrets,
                ) {
                    Ok(r) => {
                        tracing::info!(path = %r.path().display(), "recording ACP session");
                        Some(Arc::new(r))
                    }
                    Err(e) => {
                        tracing::warn!("cannot record ACP session: {e}");
                        None
                    }
                }
            }
            None => None,
        };

        let transport = transport::lines(stdout, stdin, recorder);
        let open = OpenSession {
            cwd,
            mcp_servers: session_servers,
            authenticate: sign_in_method(self.spec.flavor, &self.sandbox)
                .map(str::to_string)
                .or(open.authenticate),
            mode: self.spec.mode.clone().or(open.mode),
            ..open
        };
        let stop = client::process_shutdown(child);
        // The egress sockets close after the agent has stopped (or when the session task is
        // aborted, which drops this hook, the lease and the agent process together).
        let shutdown: client::Shutdown = Box::new(move || {
            Box::pin(async move {
                stop().await;
                // Kills anything left in the sandbox's cgroup and removes it.
                drop(cgroup);
                drop(lease);
            })
        });
        // Grok answers its billing summary over ACP (`_x.ai/billing`, see `provider_usage::grok`).
        let usage = (crate::provider_usage::provider_for_command(&self.spec.command) == "grok")
            .then_some(crate::provider_usage::grok::BILLING_METHOD);
        client::start(transport, open, Some(shutdown), tail, usage).await
    }
}

impl SessionBackend for AcpBackend {
    fn info(&self) -> BackendInfo {
        Backend::info(self)
    }

    fn not_ready(&self) -> Option<crate::session::NotReady> {
        crate::cli::not_ready(
            &self.spec.command,
            self.program.get().is_some(),
            &self.sandbox,
            self.bwrap.is_some(),
            self.egress.as_ref(),
            self.home.as_deref(),
        )
    }

    fn open(&self, req: OpenSession) -> BoxFuture<'_, Result<OpenedSession, AgentError>> {
        Box::pin(self.spawn(req))
    }

    fn provider_id(&self) -> Option<String> {
        Some(crate::provider_usage::provider_for_command(
            &self.spec.command,
        ))
    }
}

impl Backend for AcpBackend {
    fn info(&self) -> BackendInfo {
        let mut detail = self.spec.command.clone();
        for arg in &self.spec.args {
            detail.push(' ');
            detail.push_str(arg);
        }
        detail.push_str(&format!(" [acp, {}]", network_label(&self.sandbox)));
        BackendInfo {
            name: self.spec.name.clone(),
            kind: "acp",
            detail,
            available: self.program.get().is_some()
                && (!self.sandbox.enabled || self.bwrap.is_some())
                && network_ready(&self.sandbox, self.egress.as_ref()),
            tools: self.spec.tools,
        }
    }

    fn run<'a>(
        &'a self,
        req: &'a RunRequest,
        sink: EventSink,
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            let mcp_servers = match (&req.mcp, self.spec.tools) {
                (Some(endpoint), true) => vec![McpServerSpec {
                    name: MCP_SERVER_NAME.into(),
                    url: endpoint.url.clone(),
                    headers: Vec::new(),
                }],
                _ => Vec::new(),
            };
            let mut open = OpenSession::new(
                req.workdir.clone().unwrap_or_else(|| self.workdir.clone()),
                mcp_servers,
                Arc::new(self.permissions(req)),
            );
            open.task = req.task;
            let OpenedSession {
                session,
                mut events,
                ..
            } = self.spawn(open).await?;
            let stop = one_turn(session.as_ref(), &mut events, &req.prompt, &sink).await;
            let _ = session.close().await;
            match stop? {
                StopReason::EndTurn => Ok(()),
                other => Err(AgentError::Exit(format!("turn ended: {}", other.as_str()))),
            }
        })
    }
}

/// Environment for an OpenCode agent whose config Melissa owns ([`AcpFlavor::Opencode`]):
/// every tool (built-in or MCP) asks for permission, Melissa's MCP servers with Code Mode
/// off (each tool is its own tool call, named `<server>_<tool>`), no project config, no
/// auto-update, no Claude Code compatibility (reading `~/.claude`).
pub fn opencode_env(servers: &[McpServerSpec], model: Option<&str>) -> Vec<(String, String)> {
    let mut mcp = serde_json::Map::new();
    for s in servers {
        let headers: serde_json::Map<String, serde_json::Value> = s
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();
        mcp.insert(
            s.name.clone(),
            serde_json::json!({
                "type": "remote",
                "url": s.url,
                "headers": headers,
                "codemode": false,
            }),
        );
    }
    let mut config = serde_json::json!({
        "permission": {"*": "ask"},
        "mcp": mcp,
    });
    if let Some(model) = model {
        config["model"] = serde_json::Value::String(model.into());
    }
    [
        ("OPENCODE_CONFIG_CONTENT", config.to_string()),
        ("OPENCODE_DISABLE_PROJECT_CONFIG", "1".into()),
        ("OPENCODE_DISABLE_AUTOUPDATE", "1".into()),
        ("OPENCODE_DISABLE_CLAUDE_CODE", "1".into()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

/// Permission handler for one board task of `backend`: Melissa's policy over the task's MCP
/// session. Without an MCP server there is no inbox anyone watches, so anything that needs
/// a human is refused at once.
pub(crate) fn task_permissions(
    server: Option<&McpServer>,
    req: &RunRequest,
    backend: &str,
) -> PolicyPermissions {
    let registry = match server {
        Some(server) => server.registry().clone(),
        None => ToolRegistry::new(Vec::new(), None).with_confirm_timeout(Duration::ZERO),
    };
    let mcp = match (server, &req.mcp) {
        (Some(server), Some(endpoint)) => Some((server.clone(), endpoint.token.clone())),
        _ => None,
    };
    PolicyPermissions::new(
        registry,
        PermissionContext {
            task: req.task.unwrap_or_default(),
            backend: backend.to_string(),
            mcp_name: MCP_SERVER_NAME.into(),
            mcp,
        },
    )
}

/// Create `dir` (and parents) with mode 0700: it holds an agent's conversation history.
pub(crate) fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Run one prompt, rendering its events into `sink` until the turn ends.
pub async fn one_turn(
    session: &dyn crate::session::AgentSession,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<SessionEvent>,
    prompt: &str,
    sink: &EventSink,
) -> Result<StopReason, AgentError> {
    let mut render = Render::new(sink);
    let turn = session.prompt(prompt);
    tokio::pin!(turn);
    let stop = loop {
        tokio::select! {
            Some(event) = events.recv() => render.event(event),
            stop = &mut turn => break stop,
        }
    };
    // Updates sent before the turn's response are already queued.
    while let Ok(event) = events.try_recv() {
        render.event(event);
    }
    render.flush();
    stop
}

/// Turns session events into task output lines: the reply on stdout (line by line), tool
/// calls, permission answers and plans as short `[…]` lines on stderr.
struct Render<'a> {
    sink: &'a EventSink,
    line: String,
}

impl<'a> Render<'a> {
    fn new(sink: &'a EventSink) -> Self {
        Self {
            sink,
            line: String::new(),
        }
    }

    fn event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::Text(text) => {
                self.line.push_str(&text);
                while let Some(pos) = self.line.find('\n') {
                    let rest = self.line.split_off(pos + 1);
                    let line = std::mem::replace(&mut self.line, rest);
                    self.sink.stdout(line.trim_end_matches(['\n', '\r']));
                }
            }
            SessionEvent::ToolStarted { title, status, .. } => {
                self.note(format!("[tool] {} → {}", short(&title), status.as_str()));
            }
            SessionEvent::ToolFinished { title, status, .. } => {
                let title = title.as_deref().unwrap_or("tool call");
                self.note(format!("[tool] {} → {}", short(title), status.as_str()));
            }
            SessionEvent::Permission {
                title, decision, ..
            } => {
                self.note(format!(
                    "[permission] {} → {}",
                    short(&title),
                    decision.as_str()
                ));
            }
            SessionEvent::Plan(steps) => {
                let steps: Vec<String> = steps.iter().map(|s| short(&s.content)).collect();
                self.note(format!("[plan] {}", steps.join(" | ")));
            }
            SessionEvent::ToolUpdated {
                status: Some(ToolStatus::Failed),
                title,
                ..
            } => {
                let title = title.as_deref().unwrap_or("tool call");
                self.note(format!("[tool] {} → failed", short(title)));
            }
            SessionEvent::ToolUpdated { .. }
            | SessionEvent::Thought(_)
            | SessionEvent::Usage { .. }
            | SessionEvent::Cost { .. }
            | SessionEvent::ProviderUsage(_)
            | SessionEvent::TurnFinished(_)
            | SessionEvent::Other(_) => {}
        }
    }

    /// A status line; finishes a partial reply line first so lines don't interleave.
    fn note(&mut self, text: String) {
        self.flush();
        self.sink.stderr(text);
    }

    fn flush(&mut self) {
        if !self.line.is_empty() {
            let line = std::mem::take(&mut self.line);
            self.sink.stdout(line.trim_end_matches(['\n', '\r']));
        }
    }
}

/// One line, at most 120 characters.
fn short(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    if line.chars().count() > 120 {
        let cut: String = line.chars().take(117).collect();
        format!("{cut}...")
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests;
