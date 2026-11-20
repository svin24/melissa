//! Claude Code as a session backend, over its own streaming JSON protocol (Claude Code has
//! no ACP mode).
//!
//! One `claude -p --input-format stream-json --output-format stream-json` process per
//! session: user messages go in as JSON lines on stdin, the agent streams JSON lines on
//! stdout (partial text, tool calls, a `result` per turn). With `--permission-prompt-tool
//! stdio` every permission prompt comes to Melissa as a `can_use_tool` control request,
//! answered by [`PolicyPermissions`]; `interrupt` control requests cancel a turn.
//! Built-in tools are removed (`--tools ""`): Claude acts only through Melissa's MCP tools.
//! The process runs under the bubblewrap sandbox and the egress proxy like every agent CLI.
//! Resume: Claude keeps sessions in `~/.claude/projects`; for sessions with a
//! [`OpenSession::state_dir`] (the lead agent) that Melissa-owned directory is bound there, and
//! `--resume <id>` reopens the session after a restart. See [`client`] for the protocol.

pub(crate) mod client;

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::{
    acp::{create_private_dir, one_turn, replay, task_permissions, MCP_SERVER_NAME},
    cli::{network_label, network_ready, open_egress, sandboxed_command, ProgramPath},
    egress::EgressProxy,
    mcp::McpServer,
    sandbox::{self, SandboxError, SandboxSpec},
    session::{McpServerSpec, OpenSession, OpenedSession, SessionBackend, StopReason},
    AgentError, Backend, BackendInfo, BoxFuture, EventSink, RunRequest,
};

fn yes() -> bool {
    true
}

/// Where Claude Code keeps its sessions, relative to `$HOME`.
pub const CLAUDE_SESSION_STORE: &str = ".claude/projects";

/// How long Claude may wait for one of Melissa's MCP tools (`MCP_TOOL_TIMEOUT`, ms). In
/// practice claude 2.1.327 gives up on an HTTP MCP call after 300 s regardless (and leaves
/// the request open), which is why the lead agent's `confirm_timeout_secs` stays below that.
const MCP_TOOL_TIMEOUT_MS: &str = "960000";

/// How long Claude Code actually waits for one MCP call (see [`MCP_TOOL_TIMEOUT_MS`]).
pub const CLAUDE_TOOL_CALL_LIMIT: std::time::Duration = std::time::Duration::from_secs(300);

/// Extra arguments a config may add (each followed by one value). Everything else is set by
/// Melissa: permission mode, tools, MCP config and session flags must not be overridable.
const ALLOWED_ARGS: &[&str] = &["--model", "--fallback-model", "--effort"];

/// Configuration of one Claude session backend (`kind = "claude"` in the lead agent config).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeSpec {
    /// Unique backend name, e.g. `claude-session`.
    pub name: String,
    /// Program to run: a bare name looked up on `PATH`, or a path.
    pub command: String,
    /// Extra arguments, only `--model`, `--fallback-model` and `--effort` with a value each.
    #[serde(default)]
    pub args: Vec<String>,
    /// Give the agent Melissa's tools (an MCP config file per session).
    #[serde(default = "yes")]
    pub tools: bool,
    /// Sandbox settings. Absent: the built-in preset for this command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxSpec>,
    /// Network access of the sandboxed agent (see [`crate::AcpSpec::network`]).
    #[serde(default)]
    pub network: crate::egress::NetworkMode,
    /// Egress allowlist; absent: the built-in list for the command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<String>>,
    /// Debugging aid: write a scrubbed transcript of every session into this directory
    /// ([`replay`] format, protocol `claude-stream-json`). Off by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_dir: Option<PathBuf>,
    /// Where Claude keeps its sessions, relative to `$HOME` (default `.claude/projects`).
    /// The lead agent's Melissa-owned session directory is bound here so it can resume.
    #[serde(default = "default_store")]
    pub session_store: String,
}

fn default_store() -> String {
    CLAUDE_SESSION_STORE.into()
}

impl ClaudeSpec {
    pub fn new(name: &str, command: &str) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            args: Vec::new(),
            tools: true,
            sandbox: None,
            network: crate::egress::NetworkMode::default(),
            allow: None,
            record_dir: None,
            session_store: default_store(),
        }
    }

    pub fn with_sandbox(mut self, spec: SandboxSpec) -> Self {
        self.sandbox = Some(spec);
        self
    }

    pub fn with_network(mut self, mode: crate::egress::NetworkMode) -> Self {
        self.network = mode;
        self
    }

    pub fn with_args(mut self, args: &[&str]) -> Self {
        self.args = args.iter().map(|a| (*a).to_string()).collect();
        self
    }

    pub fn without_sandbox(self) -> Self {
        self.with_sandbox(SandboxSpec::disabled())
    }

    pub fn effective_sandbox(&self) -> SandboxSpec {
        let mut spec = self
            .sandbox
            .clone()
            .unwrap_or_else(|| SandboxSpec::preset(&self.command));
        spec.network = self.network;
        spec.allow = self.allow.clone();
        spec
    }

    pub fn validate(&self) -> Result<(), SandboxError> {
        let mut args = self.args.iter();
        while let Some(flag) = args.next() {
            let value = args.next();
            let ok = ALLOWED_ARGS.contains(&flag.as_str())
                && value.is_some_and(|v| !v.is_empty() && !v.starts_with('-'));
            if !ok {
                return Err(SandboxError::BadArg(flag.clone()));
            }
        }
        let store = Path::new(&self.session_store);
        if self.session_store.is_empty()
            || !store
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
        {
            return Err(SandboxError::BadPath(self.session_store.clone()));
        }
        self.effective_sandbox().validate()
    }
}

/// How a process starts its session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionArg {
    /// A new session with this id (`--session-id`), kept on disk if `persist`.
    New { id: String, persist: bool },
    /// Reopen a stored session (`--resume`).
    Resume(String),
}

/// The full argument list. Fixed flags first; the config's extra flags last.
pub(crate) fn claude_args(
    extra: &[String],
    mcp_config: Option<&Path>,
    session: &SessionArg,
) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        // No built-in tools (no shell, no file access): only Melissa's MCP tools.
        "--tools",
        "",
        // Ask for everything (the CLI's own name for its default mode), overriding any
        // settings; every prompt goes to Melissa over the control channel.
        "--permission-mode",
        "manual",
        "--permission-prompt-tool",
        "stdio",
        // Ignore the user's own MCP servers.
        "--strict-mcp-config",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    if let Some(path) = mcp_config {
        args.push("--mcp-config".into());
        args.push(path.display().to_string());
    }
    match session {
        SessionArg::New { id, persist } => {
            args.push("--session-id".into());
            args.push(id.clone());
            if !persist {
                args.push("--no-session-persistence".into());
            }
        }
        SessionArg::Resume(id) => {
            args.push("--resume".into());
            args.push(id.clone());
        }
    }
    args.extend(extra.iter().cloned());
    args
}

/// The config's extra arguments with `--model` replaced by `model` (the user's choice).
fn with_model(extra: &[String], model: Option<&str>) -> Result<Vec<String>, AgentError> {
    let Some(model) = model else {
        return Ok(extra.to_vec());
    };
    if !crate::session::valid_model_id(model) {
        return Err(AgentError::Protocol(format!("`{model}` is not a model id")));
    }
    let mut out = Vec::new();
    let mut args = extra.iter();
    while let Some(flag) = args.next() {
        if flag == "--model" {
            args.next();
            continue;
        }
        out.push(flag.clone());
    }
    out.push("--model".into());
    out.push(model.into());
    Ok(out)
}

/// The `--model` value in an argument list, if any.
fn model_arg(args: &[String]) -> Option<String> {
    args.iter()
        .position(|a| a == "--model")
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// Fixed environment for the agent: a long MCP tool timeout (confirmations), and no
/// auto-update, telemetry or error reports (they would only be denied by the egress proxy).
fn claude_env() -> Vec<(String, String)> {
    [
        ("MCP_TOOL_TIMEOUT", MCP_TOOL_TIMEOUT_MS),
        ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
        ("DISABLE_AUTOUPDATER", "1"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// Claude Code driven over stream-json, one process per session.
#[derive(Clone)]
pub struct ClaudeBackend {
    spec: ClaudeSpec,
    sandbox: SandboxSpec,
    program: ProgramPath,
    bwrap: Option<PathBuf>,
    home: Option<PathBuf>,
    workdir: PathBuf,
    mcp: Option<McpServer>,
    egress: Option<EgressProxy>,
}

impl ClaudeBackend {
    pub fn new(spec: ClaudeSpec, workdir: PathBuf) -> Self {
        let program = ProgramPath::new(&spec.command);
        let sandbox = spec.effective_sandbox();
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

    pub fn with_egress(mut self, egress: EgressProxy) -> Self {
        self.egress = Some(egress);
        self
    }

    pub fn with_mcp(mut self, server: McpServer) -> Self {
        self.mcp = Some(server);
        self
    }

    pub fn with_home(mut self, home: PathBuf) -> Self {
        self.home = Some(home);
        self
    }

    async fn open_session(&self, open: OpenSession) -> Result<OpenedSession, AgentError> {
        let cwd = std::path::absolute(&open.cwd)?;
        tokio::fs::create_dir_all(&cwd).await?;
        let open = OpenSession { cwd, ..open };
        let persist = open.state_dir.is_some() && self.sandbox.enabled;
        if let (Some(id), true) = (&open.resume, persist) {
            match self
                .spawn(&open, SessionArg::Resume(id.clone()), true)
                .await
            {
                Ok(opened) => return Ok(opened),
                // Claude exits at once when it does not know the session.
                Err(e) => tracing::info!(
                    backend = %self.spec.name,
                    "cannot resume Claude session {id}: {e}; starting a new one"
                ),
            }
        }
        let id = new_session_id()?;
        self.spawn(&open, SessionArg::New { id, persist }, false)
            .await
    }

    async fn spawn(
        &self,
        open: &OpenSession,
        session: SessionArg,
        resumed: bool,
    ) -> Result<OpenedSession, AgentError> {
        let program = self
            .program
            .get()
            .ok_or_else(|| AgentError::Unavailable(format!("`{}` not found", self.spec.command)))?;
        let cwd = &open.cwd;
        let mut binds = Vec::new();
        if let (Some(dir), true) = (&open.state_dir, self.sandbox.enabled) {
            let dir = std::path::absolute(dir)?;
            create_private_dir(&dir)?;
            binds.push((dir, self.spec.session_store.clone()));
        }
        let servers: Vec<&McpServerSpec> = if self.spec.tools {
            open.mcp_servers.iter().collect()
        } else {
            Vec::new()
        };
        // The MCP config (with the session token) is a 0600 file in the workdir, not an
        // argument: command lines are readable by every local user.
        let mcp_file = if servers.is_empty() {
            None
        } else {
            Some(write_mcp_config(cwd, &servers)?)
        };
        let tokens = servers
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
        let env = claude_env();
        let extra = with_model(&self.spec.args, open.model.as_deref())?;
        let args = claude_args(&extra, mcp_file.as_ref().map(|f| f.path()), &session);
        let rw_dirs: Vec<std::path::PathBuf> = open.files_dir.iter().cloned().collect();
        let (program_path, argv) = sandboxed_command(
            &self.sandbox,
            self.bwrap.as_deref(),
            self.home.as_deref(),
            &program,
            cwd,
            &[],
            &binds,
            &rw_dirs,
            &env,
            bridge,
            args,
        )?;
        let mut command = tokio::process::Command::new(&program_path);
        if !self.sandbox.enabled {
            // Provider settings first, so Melissa's own variables win.
            command.envs(self.sandbox.vars.iter().map(|(k, v)| (k, v)));
            command.envs(env.iter().map(|(k, v)| (k, v)));
        }
        let mut child = command
            .args(argv)
            .current_dir(cwd)
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Dropping the session kills the agent (and under bwrap the whole sandbox).
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| AgentError::Spawn {
                command: self.spec.command.clone(),
                source,
            })?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(AgentError::Protocol("agent pipes missing".into()));
        };
        let tail = crate::acp::client::StderrTail::default();
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
        let recorder = self.recorder(open, cwd);
        let stop = crate::acp::client::process_shutdown(child);
        let shutdown: crate::acp::client::Shutdown = Box::new(move || {
            Box::pin(async move {
                stop().await;
                // The sockets close after the agent has stopped; the MCP file goes with it.
                drop(lease);
                drop(mcp_file);
            })
        });
        let (id, persistent) = match &session {
            SessionArg::New { id, persist } => (id.clone(), *persist),
            SessionArg::Resume(id) => (id.clone(), true),
        };
        client::start(
            stdout,
            stdin,
            client::Start {
                session_id: id,
                resumed,
                persistent,
                model: model_arg(&extra),
                permissions: open.permissions.clone(),
                recorder,
                stderr: tail,
                shutdown: Some(shutdown),
            },
        )
        .await
    }

    fn recorder(&self, open: &OpenSession, cwd: &Path) -> Option<Arc<replay::Recorder>> {
        let dir = self.spec.record_dir.as_ref()?;
        let secrets = open
            .mcp_servers
            .iter()
            .flat_map(|s| {
                let token = s.url.rsplit('/').next().unwrap_or_default().to_string();
                std::iter::once(token).chain(s.headers.iter().map(|h| h.1.clone()))
            })
            .collect();
        match replay::Recorder::create_for(
            dir,
            &self.spec.name,
            "claude-stream-json",
            self.home.as_deref(),
            cwd,
            secrets,
        ) {
            Ok(r) => {
                tracing::info!(path = %r.path().display(), "recording Claude session");
                Some(Arc::new(r))
            }
            Err(e) => {
                tracing::warn!("cannot record Claude session: {e}");
                None
            }
        }
    }
}

/// The MCP config file; removed when dropped.
pub(crate) struct McpFile(PathBuf);

impl McpFile {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for McpFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Write Claude's MCP config (`{"mcpServers": {name: {type: http, url, headers}}}`) into
/// `dir` with mode 0600 under a fresh name.
fn write_mcp_config(dir: &Path, servers: &[&McpServerSpec]) -> std::io::Result<McpFile> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let mut map = serde_json::Map::new();
    for s in servers {
        let headers: serde_json::Map<String, serde_json::Value> = s
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();
        map.insert(
            s.name.clone(),
            serde_json::json!({"type": "http", "url": s.url, "headers": headers}),
        );
    }
    let body = serde_json::json!({ "mcpServers": map }).to_string();
    let path = dir.join(format!(".melissa-mcp-{}.json", &new_session_id()?[..8]));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(body.as_bytes())?;
    Ok(McpFile(path))
}

/// A random (version 4) UUID from the kernel CSPRNG, for `--session-id`.
fn new_session_id() -> std::io::Result<String> {
    use std::io::Read;
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

impl SessionBackend for ClaudeBackend {
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
        Box::pin(self.open_session(req))
    }

    fn tool_call_limit(&self) -> Option<std::time::Duration> {
        Some(CLAUDE_TOOL_CALL_LIMIT)
    }
}

impl Backend for ClaudeBackend {
    fn info(&self) -> BackendInfo {
        let mut detail = format!("{} -p --input-format stream-json", self.spec.command);
        for arg in &self.spec.args {
            detail.push(' ');
            detail.push_str(arg);
        }
        detail.push_str(&format!(" [claude, {}]", network_label(&self.sandbox)));
        BackendInfo {
            name: self.spec.name.clone(),
            kind: "claude",
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
                Arc::new(task_permissions(self.mcp.as_ref(), req, &self.spec.name)),
            );
            open.task = req.task;
            let OpenedSession {
                session,
                mut events,
                ..
            } = self.open_session(open).await?;
            let stop = one_turn(session.as_ref(), &mut events, &req.prompt, &sink).await;
            let _ = session.close().await;
            match stop? {
                StopReason::EndTurn => Ok(()),
                other => Err(AgentError::Exit(format!("turn ended: {}", other.as_str()))),
            }
        })
    }
}

#[cfg(test)]
mod tests;
