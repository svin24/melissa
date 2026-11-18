//! Generic backend that drives a headless agent CLI (Claude Code, Grok, OpenCode, ...).
//!
//! The prompt is passed as a process argument, never through a shell, so it cannot be
//! interpreted as shell syntax. Prompts starting with `-` are rejected so they cannot be
//! mistaken for CLI flags (e.g. an auto-approve flag).

use std::{
    path::{Path, PathBuf},
    process::Stdio,
};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

use crate::{AgentError, Backend, BackendInfo, BoxFuture, EventSink, RunRequest, Stream};

/// Placeholder in [`CliSpec::args`] that is replaced by the prompt.
pub const PROMPT_PLACEHOLDER: &str = "{prompt}";
/// Placeholder in [`CliSpec::mcp_args`] that is replaced by the MCP config file path.
pub const MCP_CONFIG_PLACEHOLDER: &str = "{mcp_config}";

/// Configuration for one CLI backend (deserialized from the queen config file).
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
}

impl CliSpec {
    pub fn new(name: &str, command: &str, args: &[&str]) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            args: args.iter().map(|a| (*a).to_string()).collect(),
            mcp_args: Vec::new(),
        }
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
    program: Option<PathBuf>,
    workdir: PathBuf,
}

impl CliBackend {
    /// `workdir` is the working directory for the child process (created if missing).
    pub fn new(spec: CliSpec, workdir: PathBuf) -> Self {
        let program = resolve_program(&spec.command);
        Self {
            spec,
            program,
            workdir,
        }
    }
}

impl Backend for CliBackend {
    fn info(&self) -> BackendInfo {
        let mut detail = self.spec.command.clone();
        for arg in &self.spec.args {
            detail.push(' ');
            detail.push_str(arg);
        }
        BackendInfo {
            name: self.spec.name.clone(),
            kind: "cli",
            detail,
            available: self.program.is_some(),
            tools: !self.spec.mcp_args.is_empty(),
        }
    }

    fn run<'a>(
        &'a self,
        req: &'a RunRequest,
        sink: EventSink,
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            let program = self.program.as_ref().ok_or_else(|| {
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

            let mut child = tokio::process::Command::new(program)
                .args(args)
                .current_dir(workdir)
                // Ask CLIs for plain text; any remaining escape codes are stripped in `pump`.
                .env("NO_COLOR", "1")
                .env("TERM", "dumb")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                // Cancellation and timeouts drop this future; make sure the child dies too.
                .kill_on_drop(true)
                .spawn()
                .map_err(|source| AgentError::Spawn {
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

/// Find a program: paths are used as given, bare names are looked up on `PATH`.
fn resolve_program(command: &str) -> Option<PathBuf> {
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
    use crate::OutputLine;

    fn sh_backend(script: &str) -> CliBackend {
        // `sh -c <script> melissa <prompt>`: the prompt becomes $1, never part of the script.
        let spec = CliSpec::new("sh", "sh", &["-c", script, "melissa", "{prompt}"]);
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
            .with_mcp_args(&["--mcp-config", "{mcp_config}"]);
        let backend = CliBackend::new(spec, std::env::temp_dir());
        assert!(backend.info().tools);
        let (sink, mut rx) = EventSink::channel();
        let req = RunRequest {
            prompt: "hi".into(),
            workdir: None,
            mcp_config: Some("/tmp/x/mcp.json".into()),
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
