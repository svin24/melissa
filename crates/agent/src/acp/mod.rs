//! ACP (Agent Client Protocol) backends: long-lived agent sessions over JSON-RPC on the
//! agent's stdin/stdout, e.g. `grok agent stdio`.
//!
//! [`AcpBackend`] spawns the agent under the bubblewrap sandbox ([`crate::sandbox`]) and
//! implements both [`SessionBackend`] (open a session, many prompts: the Queen) and
//! [`Backend`] (one task = one session with one prompt: workers on the task board).
//! Melissa declares no client file system or terminal capabilities: the agent acts through
//! its own sandboxed tools (each asked through `session/request_permission`, answered by
//! [`PolicyPermissions`]) and through Melissa's MCP tools.

mod client;
pub mod replay;
mod transport;

use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::{
    cli::{resolve_program, sandboxed_command},
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

/// Configuration of one ACP backend (`kind = "acp"` in the queen config).
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
    /// Sandbox settings. Absent: the built-in preset for this command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxSpec>,
    /// Debugging aid: write a scrubbed transcript of every session into this directory
    /// ([`replay`] format). Transcripts hold prompts and replies; off by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_dir: Option<PathBuf>,
    /// Where the agent keeps its sessions, relative to `$HOME` (e.g. `.grok/sessions`).
    /// For sessions opened with a [`OpenSession::state_dir`] (the Queen), that Melissa-owned
    /// directory is bound here inside the sandbox so the session can be resumed later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_store: Option<String>,
}

impl AcpSpec {
    pub fn new(name: &str, command: &str, args: &[&str]) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            args: args.iter().map(|a| (*a).to_string()).collect(),
            tools: true,
            sandbox: None,
            record_dir: None,
            session_store: None,
        }
    }

    pub fn with_session_store(mut self, path: &str) -> Self {
        self.session_store = Some(path.into());
        self
    }

    pub fn with_sandbox(mut self, spec: SandboxSpec) -> Self {
        self.sandbox = Some(spec);
        self
    }

    pub fn without_sandbox(self) -> Self {
        self.with_sandbox(SandboxSpec::disabled())
    }

    pub fn effective_sandbox(&self) -> SandboxSpec {
        self.sandbox
            .clone()
            .unwrap_or_else(|| SandboxSpec::preset(&self.command))
    }

    pub fn validate(&self) -> Result<(), SandboxError> {
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
        self.effective_sandbox().validate()
    }
}

/// An agent that speaks ACP over stdio.
#[derive(Clone)]
pub struct AcpBackend {
    spec: AcpSpec,
    sandbox: SandboxSpec,
    program: Option<PathBuf>,
    bwrap: Option<PathBuf>,
    home: Option<PathBuf>,
    workdir: PathBuf,
    /// Melissa's MCP server: its registry decides permissions, its sessions hold the
    /// per-task policy context. `None`: no tools, and requests that need a human are refused.
    mcp: Option<McpServer>,
}

impl AcpBackend {
    /// `workdir` is the default working directory (per-task dirs come from the board).
    pub fn new(spec: AcpSpec, workdir: PathBuf) -> Self {
        let program = resolve_program(&spec.command);
        let sandbox = spec.effective_sandbox();
        let bwrap = if sandbox.enabled {
            sandbox::find_bwrap()
        } else {
            None
        };
        Self {
            spec,
            sandbox,
            program,
            bwrap,
            home: std::env::var_os("HOME").map(PathBuf::from),
            workdir,
            mcp: None,
        }
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
        let registry = match &self.mcp {
            Some(server) => server.registry().clone(),
            // No inbox anyone watches: anything that needs a human is refused at once.
            None => ToolRegistry::new(Vec::new(), None).with_confirm_timeout(Duration::ZERO),
        };
        let mcp = match (&self.mcp, &req.mcp) {
            (Some(server), Some(endpoint)) => Some((server.clone(), endpoint.token.clone())),
            _ => None,
        };
        PolicyPermissions::new(
            registry,
            PermissionContext {
                task: req.task.unwrap_or_default(),
                backend: self.spec.name.clone(),
                mcp_name: MCP_SERVER_NAME.into(),
                mcp,
            },
        )
    }

    async fn spawn(&self, open: OpenSession) -> Result<OpenedSession, AgentError> {
        let program = self
            .program
            .as_ref()
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
        let (program_path, argv) = sandboxed_command(
            &self.sandbox,
            self.bwrap.as_deref(),
            self.home.as_deref(),
            program,
            &cwd,
            &[],
            &binds,
            self.spec.args.clone(),
        )?;
        let mut child = tokio::process::Command::new(&program_path)
            .args(argv)
            .current_dir(&cwd)
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Dropping the session (task cancelled, timed out) kills the agent; under bwrap
            // that takes the whole sandbox (PID namespace) with it.
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
        let open = OpenSession { cwd, ..open };
        client::start(transport, open, Some(client::process_shutdown(child)), tail).await
    }
}

impl SessionBackend for AcpBackend {
    fn info(&self) -> BackendInfo {
        Backend::info(self)
    }

    fn open(&self, req: OpenSession) -> BoxFuture<'_, Result<OpenedSession, AgentError>> {
        Box::pin(self.spawn(req))
    }
}

impl Backend for AcpBackend {
    fn info(&self) -> BackendInfo {
        let mut detail = self.spec.command.clone();
        for arg in &self.spec.args {
            detail.push(' ');
            detail.push_str(arg);
        }
        detail.push_str(if self.sandbox.enabled {
            " [acp, sandbox: bwrap]"
        } else {
            " [acp, no sandbox]"
        });
        BackendInfo {
            name: self.spec.name.clone(),
            kind: "acp",
            detail,
            available: self.program.is_some() && (!self.sandbox.enabled || self.bwrap.is_some()),
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
            let open = OpenSession::new(
                req.workdir.clone().unwrap_or_else(|| self.workdir.clone()),
                mcp_servers,
                Arc::new(self.permissions(req)),
            );
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

/// Create `dir` (and parents) with mode 0700: it holds an agent's conversation history.
fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
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
