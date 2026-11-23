//! Generic backend that drives a headless agent CLI (Claude Code, Grok, OpenCode, ...).
//!
//! The prompt is passed as a process argument, never through a shell, so it cannot be
//! interpreted as shell syntax. Prompts starting with `-` are rejected so they cannot be
//! mistaken for CLI flags (e.g. an auto-approve flag). By default the CLI runs inside a
//! bubblewrap sandbox ([`crate::sandbox`]).

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

use crate::{
    egress::{EgressLease, EgressProxy, EgressRequest, NetworkMode},
    sandbox::{
        self, Bridge, SandboxChild, SandboxCommand, SandboxConfig, SandboxError, SandboxPlan,
        SandboxSpec,
    },
    AgentError, Backend, BackendInfo, BoxFuture, EventSink, RunRequest, Stream, TaskId,
};

/// Placeholder in [`CliSpec::args`] that is replaced by the prompt.
pub const PROMPT_PLACEHOLDER: &str = "{prompt}";
/// Placeholder in [`CliSpec::mcp_args`] that is replaced by the MCP config file path.
pub const MCP_CONFIG_PLACEHOLDER: &str = "{mcp_config}";

/// Configuration for one CLI backend (deserialized from the lead agent config file).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliSpec {
    /// Unique backend name, e.g. `claude`.
    pub name: String,
    /// Program to run: a bare name looked up on `PATH`, or a path.
    pub command: String,
    /// Arguments. `{prompt}` is replaced by the prompt; if no argument contains it,
    /// the prompt is appended as the last argument.
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra arguments that give the agent Melissa's tools, appended only when the task
    /// has an MCP config. `{mcp_config}` is replaced by the config file path. Empty means
    /// the backend cannot use tools.
    #[serde(default)]
    pub mcp_args: Vec<String>,
    /// A whole sandbox set by code ([`CliSpec::with_sandbox`]); wins over the preset and
    /// [`CliSpec::sandbox_table`]. Never read from the config file.
    #[serde(skip)]
    pub sandbox: Option<SandboxSpec>,
    /// `[backend.sandbox]`: changes to the built-in preset for this command
    /// ([`SandboxSpec::preset`], enabled), see [`SandboxConfig`].
    #[serde(rename = "sandbox", default, skip_serializing_if = "Option::is_none")]
    pub sandbox_table: Option<SandboxConfig>,
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
}

impl CliSpec {
    pub fn new(name: &str, command: &str, args: &[&str]) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            args: args.iter().map(|a| (*a).to_string()).collect(),
            mcp_args: Vec::new(),
            sandbox: None,
            sandbox_table: None,
            network: crate::egress::NetworkMode::default(),
            allow: None,
        }
    }

    /// Use `spec` instead of the built-in sandbox preset.
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

    /// Run the CLI as a plain process (no sandbox).
    pub fn without_sandbox(self) -> Self {
        self.with_sandbox(SandboxSpec::disabled())
    }

    /// The sandbox that applies: one set by code, else the preset for the command with the
    /// config's `[backend.sandbox]` applied.
    pub fn effective_sandbox(&self) -> SandboxSpec {
        sandbox::effective(
            self.sandbox.as_ref(),
            self.sandbox_table.as_ref(),
            || SandboxSpec::preset(&self.command),
            self.network,
            self.allow.clone(),
        )
    }

    /// Check the configuration (currently: the sandbox paths and env names).
    pub fn validate(&self) -> Result<(), SandboxError> {
        if let Some(table) = &self.sandbox_table {
            table.validate()?;
        }
        self.effective_sandbox().validate()
    }

    pub fn with_mcp_args(mut self, args: &[&str]) -> Self {
        self.mcp_args = args.iter().map(|a| (*a).to_string()).collect();
        self
    }

    /// Build the argument list for a prompt.
    pub fn render_args(&self, prompt: &str) -> Vec<String> {
        let mut used = false;
        let mut args: Vec<String> = self
            .args
            .iter()
            .map(|a| {
                if a.contains(PROMPT_PLACEHOLDER) {
                    used = true;
                    a.replace(PROMPT_PLACEHOLDER, prompt)
                } else {
                    a.clone()
                }
            })
            .collect();
        if !used {
            args.push(prompt.to_string());
        }
        args
    }
}

/// A backend that runs [`CliSpec::command`] once per task.
#[derive(Debug, Clone)]
pub struct CliBackend {
    spec: CliSpec,
    sandbox: SandboxSpec,
    program: ProgramPath,
    bwrap: Option<PathBuf>,
    home: Option<PathBuf>,
    workdir: PathBuf,
    egress: Option<EgressProxy>,
}

impl CliBackend {
    /// `workdir` is the working directory for the child process (created if missing).
    pub fn new(spec: CliSpec, workdir: PathBuf) -> Self {
        let program = ProgramPath::new(&spec.command);
        let sandbox = spec.effective_sandbox();
        let bwrap = if sandbox.enabled {
            sandbox::find_bwrap()
        } else {
            None
        };
        warn_host_network(&spec.name, &sandbox);
        Self {
            spec,
            sandbox,
            program,
            bwrap,
            home: std::env::var_os("HOME").map(PathBuf::from),
            workdir,
            egress: None,
        }
    }

    /// Use `home` instead of `$HOME` for the sandbox (tests, service users).
    pub fn with_home(mut self, home: PathBuf) -> Self {
        self.home = Some(home);
        self
    }

    /// The egress proxy that gives sandboxes (`network = "proxy"`) their network.
    pub fn with_egress(mut self, egress: EgressProxy) -> Self {
        self.egress = Some(egress);
        self
    }

    /// The command for one run: the CLI itself, or `bwrap` wrapping it.
    fn command_line(
        &self,
        program: &Path,
        workdir: &Path,
        req: &RunRequest,
        bridge: Option<Bridge>,
        args: Vec<String>,
    ) -> Result<SandboxCommand, AgentError> {
        let extra: Vec<PathBuf> = req.mcp_config.iter().cloned().collect();
        sandboxed_command(
            &self.sandbox,
            None,
            self.bwrap.as_deref(),
            self.home.as_deref(),
            program,
            workdir,
            &extra,
            &[],
            &[],
            &[],
            bridge,
            args,
        )
    }
}

/// Log once per backend that it runs on the unfiltered host network.
pub(crate) fn warn_host_network(name: &str, sandbox: &SandboxSpec) {
    if sandbox.enabled && sandbox.network == NetworkMode::Host {
        tracing::warn!(
            backend = name,
            "network = \"host\": this agent can reach the whole network (LAN included) unfiltered"
        );
    } else if !sandbox.enabled {
        tracing::warn!(
            backend = name,
            "sandbox disabled: this agent runs with the lead's files and network"
        );
    }
}

/// Whether the backend's network can be set up (the egress proxy and the bridge exist
/// where the mode needs them).
pub(crate) fn network_ready(sandbox: &SandboxSpec, egress: Option<&EgressProxy>) -> bool {
    match sandbox.network {
        _ if !sandbox.enabled => true,
        NetworkMode::Host | NetworkMode::None => true,
        NetworkMode::Proxy | NetworkMode::Learn => egress.is_some_and(|e| e.netbridge().is_some()),
    }
}

/// Why a sandboxed agent CLI cannot run now ([`crate::session::SessionBackend::not_ready`]):
/// not installed, its sandbox or network cannot start, or (for CLIs whose login is a file
/// Melissa binds into the sandbox, and no API key variable is passed) not logged in.
pub(crate) fn not_ready(
    command: &str,
    program: bool,
    sandbox: &SandboxSpec,
    bwrap: bool,
    egress: Option<&EgressProxy>,
    home: Option<&std::path::Path>,
) -> Option<crate::session::NotReady> {
    use crate::session::NotReady;
    if !program {
        return Some(NotReady::NotInstalled);
    }
    if (sandbox.enabled && !bwrap) || !network_ready(sandbox, egress) {
        return Some(NotReady::Sandbox);
    }
    let login = match std::path::Path::new(command)
        .file_name()
        .and_then(|n| n.to_str())
    {
        Some("claude") => Some(".claude/.credentials.json"),
        Some("grok") => Some(".grok/auth.json"),
        Some("gemini") => Some(".gemini/oauth_creds.json"),
        Some("codex-acp") => Some(".codex/auth.json"),
        Some("muse") => Some(".config/muse/auth.json"),
        _ => None,
    };
    let checkable = sandbox.enabled && sandbox.env.is_empty() && sandbox.vars.is_empty();
    if let (Some(file), Some(home), true) = (login, home, checkable) {
        if sandbox.rw.iter().any(|p| p == file) && !home.join(file).is_file() {
            return Some(NotReady::NotLoggedIn);
        }
    }
    None
}

/// Short network description for backend details.
pub(crate) fn network_label(sandbox: &SandboxSpec) -> String {
    if sandbox.enabled {
        format!("sandbox: bwrap, net: {}", sandbox.network.as_str())
    } else {
        "no sandbox".into()
    }
}

/// How an agent in `sandbox` reaches the web, for its permission handler.
pub(crate) fn web_access(
    sandbox: &SandboxSpec,
    egress: Option<&EgressProxy>,
) -> crate::session::WebAccess {
    crate::session::WebAccess::for_network(
        sandbox.enabled,
        sandbox.network,
        egress.map(EgressProxy::web_read),
    )
}

/// Open the egress sockets for one sandboxed run. `None`: no bridge needed (host network,
/// no sandbox, or `network = "none"` without MCP). Fails closed: a mode that needs the
/// proxy and has none is an error, never a fallback to the host network.
pub(crate) fn open_egress(
    sandbox: &SandboxSpec,
    egress: Option<&EgressProxy>,
    command: &str,
    backend: &str,
    task: TaskId,
    mcp_tokens: Vec<String>,
) -> Result<Option<EgressLease>, AgentError> {
    if !sandbox.enabled || sandbox.network == NetworkMode::Host {
        return Ok(None);
    }
    if sandbox.network == NetworkMode::None && mcp_tokens.is_empty() {
        return Ok(None);
    }
    let Some(egress) = egress else {
        if sandbox.network == NetworkMode::None {
            return Ok(None);
        }
        return Err(AgentError::Unavailable(format!(
            "network = \"{}\" needs Melissa's egress proxy, which is not set up",
            sandbox.network.as_str()
        )));
    };
    let allow = match sandbox.network {
        NetworkMode::None => Vec::new(),
        _ => sandbox.egress_allow(command)?,
    };
    let lease = egress
        .open(EgressRequest {
            backend: backend.to_string(),
            task,
            mode: sandbox.network,
            allow,
            deny: crate::egress::default_deny(command),
            mcp_tokens,
        })
        .map_err(|e| AgentError::Unavailable(e.to_string()))?;
    Ok(Some(lease))
}

/// Program and argv that run `program args` in `workdir`: directly if the sandbox is
/// disabled, else `bwrap` wrapping it. `extra_ro` are further files bound read-only;
/// `binds` are host directories bound read-write at paths under `$HOME`; `rw_dirs` are host
/// directories bound read-write at their own path (the agent's file box); `env` are fixed
/// variables set inside the sandbox (without a sandbox the caller sets them on the process);
/// `user` is whose host user the sandbox runs as ([`crate::sandbox::users`]; the file box key,
/// None: the shared user of sandboxes without an owner).
#[allow(clippy::too_many_arguments)] // internal helper; each argument is a distinct input
pub(crate) fn sandboxed_command(
    sandbox: &SandboxSpec,
    user: Option<&str>,
    bwrap: Option<&Path>,
    home: Option<&Path>,
    program: &Path,
    workdir: &Path,
    extra_ro: &[PathBuf],
    binds: &[(PathBuf, String)],
    rw_dirs: &[PathBuf],
    env: &[(String, String)],
    bridge: Option<Bridge>,
    args: Vec<String>,
) -> Result<SandboxCommand, AgentError> {
    if !sandbox.enabled {
        let mut command = tokio::process::Command::new(program);
        command.args(args);
        return Ok(SandboxCommand::plain(command));
    }
    let bwrap = bwrap.ok_or_else(|| {
        AgentError::Unavailable("sandbox is enabled but `bwrap` was not found".into())
    })?;
    // Resolved per run: the CLI may have updated itself (new binary path) since startup.
    let plan = SandboxPlan::resolve(sandbox, bwrap, home, program)?
        .with_binds(binds)?
        .with_rw_dirs(rw_dirs)?
        .with_env(&sandbox.vars)?
        .with_env(env)?
        .with_bridge(bridge)
        .with_user(user.unwrap_or(crate::sandbox::users::SHARED))?;
    Ok(sandbox::spawn_command(&plan, workdir, extra_ro, &args)?)
}

impl Backend for CliBackend {
    fn info(&self) -> BackendInfo {
        let mut detail = self.spec.command.clone();
        for arg in &self.spec.args {
            detail.push(' ');
            detail.push_str(arg);
        }
        detail.push_str(&format!(" [{}]", network_label(&self.sandbox)));
        BackendInfo {
            name: self.spec.name.clone(),
            kind: "cli",
            detail,
            available: self.program.get().is_some()
                && (!self.sandbox.enabled || self.bwrap.is_some())
                && network_ready(&self.sandbox, self.egress.as_ref()),
            tools: !self.spec.mcp_args.is_empty(),
        }
    }

    fn run<'a>(
        &'a self,
        req: &'a RunRequest,
        sink: EventSink,
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            let program = self.program.get().ok_or_else(|| {
                AgentError::Unavailable(format!("`{}` not found", self.spec.command))
            })?;
            let prompt = req.prompt.trim();
            if prompt.starts_with('-') {
                return Err(AgentError::BadPrompt("prompts may not start with '-'"));
            }
            let workdir = req.workdir.as_ref().unwrap_or(&self.workdir);
            tokio::fs::create_dir_all(workdir).await?;
            let mut args = self.spec.render_args(prompt);
            if let Some(config) = &req.mcp_config {
                let config = config.to_string_lossy();
                args.extend(
                    self.spec
                        .mcp_args
                        .iter()
                        .map(|a| a.replace(MCP_CONFIG_PLACEHOLDER, &config)),
                );
            }

            // bwrap needs an absolute workdir for its binds.
            let workdir = std::path::absolute(workdir)?;
            // Held until the CLI has exited: dropping it closes the sandbox's sockets.
            let lease = open_egress(
                &self.sandbox,
                self.egress.as_ref(),
                &self.spec.command,
                &self.spec.name,
                req.task.unwrap_or_default(),
                req.mcp.iter().map(|m| m.token.clone()).collect(),
            )?;
            let bridge = lease.as_ref().and_then(|l| l.bridge().cloned());
            let mut command = self.command_line(&program, &workdir, req, bridge, args)?;
            if !self.sandbox.enabled {
                command
                    .command_mut()
                    .envs(self.sandbox.vars.iter().map(|(k, v)| (k, v)));
            }
            command
                .command_mut()
                .current_dir(&workdir)
                // Ask CLIs for plain text; any remaining escape codes are stripped in `pump`.
                .env("NO_COLOR", "1")
                .env("TERM", "dumb")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                // Cancellation and timeouts drop this future; make sure the child dies too.
                // Under bwrap that takes the whole sandbox (PID namespace) with it.
                .kill_on_drop(true);
            // `_cgroup` lives until the CLI has exited (dropping it removes the cgroup).
            let SandboxChild {
                mut child,
                cgroup: _cgroup,
            } = command.spawn().map_err(|source| AgentError::Spawn {
                command: self.spec.command.clone(),
                source,
            })?;

            let stdout = child.stdout.take();
            let stderr = child.stderr.take();
            let (out, err, status) = tokio::join!(
                pump(stdout, Stream::Stdout, &sink),
                pump(stderr, Stream::Stderr, &sink),
                child.wait(),
            );
            out?;
            err?;
            let status = status?;
            drop(lease);
            if status.success() {
                Ok(())
            } else {
                Err(AgentError::Exit(status.to_string()))
            }
        })
    }
}

/// Forward lines from a child pipe into the sink. Non-UTF-8 bytes are replaced.
async fn pump<R: AsyncRead + Unpin>(
    pipe: Option<R>,
    stream: Stream,
    sink: &EventSink,
) -> std::io::Result<()> {
    let Some(pipe) = pipe else { return Ok(()) };
    let mut reader = BufReader::new(pipe);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf).await? == 0 {
            return Ok(());
        }
        let text = strip_ansi(&String::from_utf8_lossy(&buf));
        let text = text.trim_end_matches(['\n', '\r']).to_string();
        match stream {
            Stream::Stdout => sink.stdout(text),
            Stream::Stderr => sink.stderr(text),
        }
    }
}

/// Remove ANSI escape sequences (colors, cursor movement) and other control characters
/// except tab, so CLI output renders as plain text.
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek() {
                // CSI: ESC [ params... final byte in @..~
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: ESC ] ... terminated by BEL or ESC \
                Some(']') => {
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                // Two-byte escape.
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
        } else if !c.is_control() || c == '\t' || c == '\n' {
            out.push(c);
        }
    }
    out
}

/// A backend's program, looked up again while it is missing: an agent CLI installed after
/// Melissa started (Settings → Providers → Install) is used without a restart. Once found,
/// the path is kept. Cheap to clone (clones share the lookup).
#[derive(Debug, Clone)]
pub(crate) struct ProgramPath {
    command: Arc<str>,
    found: Arc<Mutex<Option<PathBuf>>>,
}

impl ProgramPath {
    pub(crate) fn new(command: &str) -> Self {
        Self {
            command: command.into(),
            found: Arc::new(Mutex::new(resolve_program(command))),
        }
    }

    /// The program, if it is there now.
    pub(crate) fn get(&self) -> Option<PathBuf> {
        // A poisoned lock only means a panic elsewhere mid-lookup; the value is still usable.
        let mut found = self.found.lock().unwrap_or_else(|e| e.into_inner());
        if found.is_none() {
            *found = resolve_program(&self.command);
        }
        found.clone()
    }
}

/// Find a program: paths are used as given, bare names are looked up on `PATH`.
pub(crate) fn resolve_program(command: &str) -> Option<PathBuf> {
    let path = Path::new(command);
    if path.components().count() > 1 {
        return path.is_file().then(|| path.to_path_buf());
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(command))
            .find(|candidate| candidate.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_names_what_is_missing() {
        use crate::session::NotReady;
        let home = std::env::temp_dir().join(format!("melissa-ready-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let mut sandbox = SandboxSpec::preset("claude");
        sandbox.network = NetworkMode::None;
        let check = |program, sandbox: &SandboxSpec, bwrap| {
            not_ready("claude", program, sandbox, bwrap, None, Some(&home))
        };
        assert_eq!(check(false, &sandbox, true), Some(NotReady::NotInstalled));
        assert_eq!(check(true, &sandbox, false), Some(NotReady::Sandbox));
        assert_eq!(check(true, &sandbox, true), Some(NotReady::NotLoggedIn));
        std::fs::write(home.join(".claude/.credentials.json"), "{}").unwrap();
        assert_eq!(check(true, &sandbox, true), None);
        // An API key variable may stand in for the login file: not checked then.
        std::fs::remove_file(home.join(".claude/.credentials.json")).unwrap();
        let mut keyed = sandbox.clone();
        keyed.env = vec!["ANTHROPIC_API_KEY".into()];
        assert_eq!(check(true, &keyed, true), None);
        // OpenCode works without a login (free models).
        let oc = SandboxSpec::preset("opencode");
        let mut oc = oc;
        oc.network = NetworkMode::None;
        assert_eq!(
            not_ready("opencode", true, &oc, true, None, Some(&home)),
            None
        );
        // Gemini CLI, Codex and Muse: signed in by their file, or an API key variable.
        for (cli, file, key) in [
            ("gemini", ".gemini/oauth_creds.json", "GEMINI_API_KEY"),
            ("codex-acp", ".codex/auth.json", "OPENAI_API_KEY"),
            ("muse", ".config/muse/auth.json", "META_API_KEY"),
        ] {
            let mut sb = SandboxSpec::preset(cli);
            sb.network = NetworkMode::None;
            assert_eq!(
                not_ready(cli, true, &sb, true, None, Some(&home)),
                Some(NotReady::NotLoggedIn),
                "{cli}"
            );
            let mut keyed = sb.clone();
            keyed.vars = vec![(key.into(), "k".into())];
            assert_eq!(not_ready(cli, true, &keyed, true, None, Some(&home)), None);
            std::fs::create_dir_all(home.join(file).parent().unwrap()).unwrap();
            std::fs::write(home.join(file), "{}").unwrap();
            assert_eq!(not_ready(cli, true, &sb, true, None, Some(&home)), None);
        }
        let _ = std::fs::remove_dir_all(home);
    }
    use crate::OutputLine;

    fn sh_backend(script: &str) -> CliBackend {
        // `sh -c <script> melissa <prompt>`: the prompt becomes $1, never part of the script.
        let spec =
            CliSpec::new("sh", "sh", &["-c", script, "melissa", "{prompt}"]).without_sandbox();
        CliBackend::new(spec, std::env::temp_dir())
    }

    async fn collect(
        backend: &CliBackend,
        prompt: &str,
    ) -> (Result<(), AgentError>, Vec<OutputLine>) {
        let (sink, mut rx) = EventSink::channel();
        let result = backend.run(&RunRequest::prompt(prompt), sink).await;
        let mut lines = Vec::new();
        while let Ok(line) = rx.try_recv() {
            lines.push(line);
        }
        (result, lines)
    }

    #[test]
    fn placeholder_is_replaced_or_appended() {
        let spec = CliSpec::new("c", "claude", &["-p", "{prompt}"]);
        assert_eq!(spec.render_args("hi"), ["-p", "hi"]);
        let spec = CliSpec::new("o", "opencode", &["run"]);
        assert_eq!(spec.render_args("hi there"), ["run", "hi there"]);
    }

    #[tokio::test]
    async fn mcp_args_are_appended_only_with_a_config() {
        let spec = CliSpec::new("sh", "sh", &["-c", r#"echo "$@""#, "melissa", "{prompt}"])
            .with_mcp_args(&["--mcp-config", "{mcp_config}"])
            .without_sandbox();
        let backend = CliBackend::new(spec, std::env::temp_dir());
        assert!(backend.info().tools);
        let (sink, mut rx) = EventSink::channel();
        let req = RunRequest {
            prompt: "hi".into(),
            workdir: None,
            mcp_config: Some("/tmp/x/mcp.json".into()),
            ..RunRequest::default()
        };
        backend.run(&req, sink).await.unwrap();
        assert_eq!(
            rx.recv().await.unwrap().text,
            "hi --mcp-config /tmp/x/mcp.json"
        );
        let (sink, mut rx) = EventSink::channel();
        backend.run(&RunRequest::prompt("hi"), sink).await.unwrap();
        assert_eq!(rx.recv().await.unwrap().text, "hi");
    }

    #[test]
    fn missing_program_is_unavailable() {
        let spec = CliSpec::new("x", "melissa-definitely-not-installed", &[]);
        assert!(!CliBackend::new(spec, std::env::temp_dir()).info().available);
    }

    #[test]
    fn a_program_installed_later_is_found_without_a_restart() {
        let dir = std::env::temp_dir().join(format!("melissa-late-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent");
        let spec = CliSpec::new("x", path.to_str().unwrap(), &[]).without_sandbox();
        let backend = CliBackend::new(spec, std::env::temp_dir());
        let copy = backend.clone();
        assert!(!backend.info().available);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        assert!(backend.info().available);
        assert!(copy.info().available, "clones share the lookup");
        // Once found it is kept.
        assert_eq!(copy.program.get(), Some(path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn streams_stdout_and_stderr() {
        let backend = sh_backend(r#"echo "out: $1"; echo "err" >&2"#);
        let (result, lines) = collect(&backend, "$(touch /nonexistent) ; rm -rf x").await;
        assert!(result.is_ok(), "{result:?}");
        assert!(lines.contains(&OutputLine {
            stream: Stream::Stdout,
            text: "out: $(touch /nonexistent) ; rm -rf x".into()
        }));
        assert!(lines.contains(&OutputLine {
            stream: Stream::Stderr,
            text: "err".into()
        }));
    }

    #[test]
    fn ansi_codes_are_stripped() {
        assert_eq!(
            strip_ansi("\u{1b}[0m> build \u{1b}[1;32mok\u{1b}[0m"),
            "> build ok"
        );
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}text\r"), "text");
        assert_eq!(strip_ansi("tab\there"), "tab\there");
    }

    #[tokio::test]
    async fn nonzero_exit_is_an_error() {
        let backend = sh_backend("exit 3");
        let (result, _) = collect(&backend, "hi").await;
        assert!(matches!(result, Err(AgentError::Exit(_))), "{result:?}");
    }

    #[tokio::test]
    async fn flag_like_prompts_are_rejected() {
        let backend = sh_backend("echo should-not-run");
        let (result, lines) = collect(&backend, "  --always-approve").await;
        assert!(
            matches!(result, Err(AgentError::BadPrompt(_))),
            "{result:?}"
        );
        assert!(lines.is_empty());
    }
}
