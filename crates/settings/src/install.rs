//! Installing an agent CLI from Settings → Providers.
//!
//! The lead agent runs as the unprivileged user `melissa`; installing needs root (it runs the
//! vendor's installer as `melissa` through `melissa-setup`). The narrow path is a systemd
//! template unit, `melissa-agent-install@.service` (packaging/container/rootfs), and a polkit rule
//! (`51-melissa-agent-install.rules`, container image only) that
//! lets `melissa` **start** only its instances `claude`, `grok` and `opencode`. Nothing from the
//! web becomes an argument: the provider id is checked against [`INSTALLABLE`] here, picks one of
//! three fixed unit names, polkit checks the name again and `melissa-setup` checks it a third time.
//!
//! Progress: `systemctl show` (read-only, allowed for every user) and the unit's output in
//! `/run/melissa-agent-install/<id>.log` (root:melissa, 0640).
//!
//! How a provider gets installed depends on the host ([`HostKind`], [`InstallWay::detect`]): the
//! unit in the container image (when its files are there), the Nix configuration on Nix installs,
//! else a command to run by hand.

use std::{
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use crate::{clean_text, updates::HostKind, SettingsError};

/// The agent CLIs `melissa-setup agent <id>` (and so the install unit) knows.
pub const INSTALLABLE: [&str; 3] = ["claude", "grok", "opencode"];
/// Where the install unit writes its output.
pub const LOG_DIR: &str = "/run/melissa-agent-install";

const UNIT_FILES: [&str; 2] = [
    "usr/lib/systemd/system/melissa-agent-install@.service",
    "etc/systemd/system/melissa-agent-install@.service",
];
const POLKIT_RULE: &str = "usr/share/polkit-1/rules.d/51-melissa-agent-install.rules";
/// Lines of the unit's output shown on the page.
const LOG_LINES: usize = 12;
/// Only the end of the log is read.
const LOG_TAIL_BYTES: u64 = 16 * 1024;

/// How a provider that is not installed gets installed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallWay {
    /// The Install button (the unit and its polkit rule are installed).
    Button,
    /// A Nix install: the owner adds the agent to the Nix configuration.
    Nix,
    /// Neither (a run from source, an older image): a command to run by hand.
    Manual,
}

impl InstallWay {
    /// From the host kind and the files under `root`. Only the container image uses the unit:
    /// a Nix install never does, even if the files were there (agents come from the Nix
    /// configuration, and the NixOS module grants no polkit rights for the units).
    pub fn detect(root: &Path, kind: HostKind) -> Self {
        match kind {
            HostKind::Nix => Self::Nix,
            HostKind::Container => {
                let unit = UNIT_FILES.iter().any(|f| root.join(f).is_file());
                if unit && root.join(POLKIT_RULE).is_file() {
                    Self::Button
                } else {
                    Self::Manual
                }
            }
            HostKind::Other => Self::Manual,
        }
    }
}

/// The install unit's instance for `id`, only for the fixed list (None for anything else).
pub fn unit_name(id: &str) -> Option<&'static str> {
    match id {
        "claude" => Some("melissa-agent-install@claude.service"),
        "grok" => Some("melissa-agent-install@grok.service"),
        "opencode" => Some("melissa-agent-install@opencode.service"),
        _ => None,
    }
}

/// What a unit does: install a CLI, or sign it in. Sign-in: Grok's `login --device-auth`
/// prints a URL and a code and waits (no input); Claude's `auth login` prints a URL and waits
/// for the code the person pastes on the page ([`Installer::send_code`]). OpenCode needs no
/// sign-in for its free models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    Install,
    Login,
}

impl Job {
    /// The unit instance for `id`, only for the fixed lists (None for anything else).
    pub fn unit(self, id: &str) -> Option<&'static str> {
        match self {
            Self::Install => unit_name(id),
            Self::Login => match id {
                "claude" => Some("melissa-agent-login@claude.service"),
                "grok" => Some("melissa-agent-login@grok.service"),
                _ => None,
            },
        }
    }

    fn log_file(self, id: &str) -> String {
        match self {
            Self::Install => format!("{id}.log"),
            Self::Login => format!("{id}-login.log"),
        }
    }
}

/// Where Claude's sign-in reads the pasted code (tmpfiles: a FIFO, root:melissa 0620).
pub const CODE_FIFO: &str = "claude-login.code";
/// Longest code accepted from the page.
pub const CODE_MAX: usize = 1024;

/// Claude's sign-in URL in a login log, only on Anthropic's own hosts.
pub fn claude_login_url(log: &[String]) -> Option<String> {
    const HOSTS: [&str; 3] = [
        "https://claude.com/",
        "https://claude.ai/",
        "https://platform.claude.com/",
    ];
    log.iter().find_map(|l| {
        l.split_whitespace()
            .find(|w| HOSTS.iter().any(|h| w.starts_with(h)) && w.contains("oauth"))
            .map(str::to_string)
    })
}

/// How a sign-in unit ended, from its exit status (melissa-setup login_claude_from_page).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginEnd {
    /// 3: the vendor did not accept the code.
    WrongCode,
    /// 4: the person cancelled.
    Cancelled,
    /// 5 (or the unit's timeout): no code in time.
    TimedOut,
    /// Anything else.
    Failed,
}

impl LoginEnd {
    pub fn of(status: &InstallStatus) -> Self {
        match status.exit {
            Some(3) => Self::WrongCode,
            Some(4) => Self::Cancelled,
            Some(5) => Self::TimedOut,
            _ if status.timed_out => Self::TimedOut,
            _ => Self::Failed,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::WrongCode => "code not accepted",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed out",
            Self::Failed => "failed",
        }
    }
}

/// A pasted sign-in code: one line of visible ASCII, no spaces, at most [`CODE_MAX`] bytes.
pub fn valid_code(code: &str) -> bool {
    !code.is_empty() && code.len() <= CODE_MAX && code.bytes().all(|b| b.is_ascii_graphic())
}

/// A device-code sign-in in a login log: the vendor's URL (only on its own host) and the code.
pub fn device_prompt(log: &[String]) -> Option<(String, String)> {
    let url = log.iter().find_map(|l| {
        l.split_whitespace()
            .find(|w| w.starts_with("https://accounts.x.ai/"))
            .map(str::to_string)
    })?;
    let code = log.iter().map(|l| l.trim()).find(|l| {
        let b = l.as_bytes();
        b.len() == 9
            && b[4] == b'-'
            && b.iter()
                .enumerate()
                .all(|(i, c)| i == 4 || c.is_ascii_uppercase() || c.is_ascii_digit())
    })?;
    Some((url, code.to_string()))
}

/// Where an install stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallState {
    /// Not started since Melissa (the container) started.
    Idle,
    Running,
    /// The installer finished without an error.
    Done,
    Failed,
}

impl InstallState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

/// An install's state, why it failed and the last lines of its output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallStatus {
    pub state: InstallState,
    /// For a failure: "exit status 1", "timeout", …
    pub detail: Option<String>,
    /// The program's exit status, when it exited with one.
    pub exit: Option<i32>,
    /// systemd stopped it at its time limit.
    pub timed_out: bool,
    /// When the last run ended (systemd's monotonic µs; 0: not yet). Tells runs apart.
    pub ended: u64,
    /// The last lines of the output, cleaned for display.
    pub log: Vec<String>,
}

impl InstallStatus {
    pub fn idle() -> Self {
        Self {
            state: InstallState::Idle,
            detail: None,
            exit: None,
            timed_out: false,
            ended: 0,
            log: Vec::new(),
        }
    }
}

/// Starts the install unit and reads its state.
#[derive(Debug, Clone)]
pub struct Installer {
    pub way: InstallWay,
    log_dir: PathBuf,
    systemctl: PathBuf,
}

impl Installer {
    /// The real one: `/`, [`LOG_DIR`] and `systemctl` from PATH.
    pub fn system(kind: HostKind) -> Self {
        Self::new(
            InstallWay::detect(Path::new("/"), kind),
            PathBuf::from(LOG_DIR),
            PathBuf::from("systemctl"),
        )
    }

    /// For tests: a given way, log directory and `systemctl` program.
    pub fn new(way: InstallWay, log_dir: PathBuf, systemctl: PathBuf) -> Self {
        Self {
            way,
            log_dir,
            systemctl,
        }
    }

    /// Ask systemd (through polkit) to start the install unit for `id`. Returns once systemd
    /// took the job; [`Installer::status`] follows it.
    pub async fn start(&self, id: &str) -> Result<(), SettingsError> {
        self.start_job(Job::Install, id).await
    }

    /// The install's state for `id` (idle for an unknown id or when systemd can't tell).
    pub async fn status(&self, id: &str) -> InstallStatus {
        self.job_status(Job::Install, id).await
    }

    /// Start `job`'s unit for `id` through polkit.
    pub async fn start_job(&self, job: Job, id: &str) -> Result<(), SettingsError> {
        let Some(unit) = job.unit(id) else {
            return Err(SettingsError::Invalid(
                "Melissa can't do that for this provider.".into(),
            ));
        };
        if self.way != InstallWay::Button {
            return Err(SettingsError::Invalid(
                "Installing from here isn't available on this device.".into(),
            ));
        }
        let out = self
            .systemctl(&["start", "--no-block", unit], Duration::from_secs(15))
            .await?;
        if out.status.success() {
            Ok(())
        } else {
            Err(SettingsError::Failed(format!(
                "systemd refused: {}",
                clean_text(&String::from_utf8_lossy(&out.stderr), 200)
            )))
        }
    }

    /// `job`'s state for `id` (idle for an unknown id or when systemd can't tell).
    pub async fn job_status(&self, job: Job, id: &str) -> InstallStatus {
        let Some(unit) = job.unit(id) else {
            return InstallStatus::idle();
        };
        if self.way != InstallWay::Button {
            return InstallStatus::idle();
        }
        let shown = self
            .systemctl(
                &[
                    "show",
                    "--property=LoadState,ActiveState,Result,ExecMainStatus,ExecMainExitTimestampMonotonic",
                    unit,
                ],
                Duration::from_secs(5),
            )
            .await;
        let text = match shown {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).into_owned(),
            Ok(_) | Err(_) => String::new(),
        };
        let (state, detail) = parse_show(&text);
        let field = |key: &str| show_field(&text, key);
        let exit = (state == InstallState::Failed && field("Result") == "exit-code")
            .then(|| field("ExecMainStatus").parse().ok())
            .flatten();
        let log = if state == InstallState::Idle {
            Vec::new()
        } else {
            read_log(&self.log_dir.join(job.log_file(id)))
        };
        InstallStatus {
            state,
            detail,
            exit,
            timed_out: field("Result") == "timeout",
            ended: field("ExecMainExitTimestampMonotonic").parse().unwrap_or(0),
            log,
        }
    }

    /// Hand the code the person pasted to Claude's waiting sign-in (an empty `code` cancels
    /// it). The code goes only into the FIFO the login unit reads; it is never logged. Fails
    /// when no sign-in is waiting (nobody reads the FIFO).
    pub fn send_code(&self, code: &str) -> Result<(), SettingsError> {
        use std::{
            io::Write,
            os::unix::fs::{FileTypeExt, OpenOptionsExt},
        };
        if !code.is_empty() && !valid_code(code) {
            return Err(SettingsError::Invalid(
                "That doesn't look like a sign-in code.".into(),
            ));
        }
        let waiting = || SettingsError::Failed("No sign-in is waiting. Start again.".into());
        // Non-blocking: with no reader the open fails at once (ENXIO) instead of hanging.
        let mut fifo = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
            .open(self.log_dir.join(CODE_FIFO))
            .map_err(|_| waiting())?;
        // Only ever into the FIFO, never into a file someone put there.
        if !fifo.metadata().is_ok_and(|m| m.file_type().is_fifo()) {
            return Err(waiting());
        }
        // One write below PIPE_BUF is atomic: the reader gets the whole line or nothing.
        let mut line = Vec::with_capacity(code.len() + 1);
        line.extend_from_slice(code.as_bytes());
        line.push(b'\n');
        fifo.write_all(&line).map_err(|_| waiting())
    }

    async fn systemctl(
        &self,
        args: &[&str],
        limit: Duration,
    ) -> Result<std::process::Output, SettingsError> {
        let mut cmd = tokio::process::Command::new(&self.systemctl);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        match tokio::time::timeout(limit, cmd.output()).await {
            Ok(Ok(out)) => Ok(out),
            Ok(Err(e)) => Err(SettingsError::Failed(format!("cannot run systemctl: {e}"))),
            Err(_) => Err(SettingsError::Failed(
                "systemctl did not answer in time".into(),
            )),
        }
    }
}

fn show_field<'a>(text: &'a str, key: &str) -> &'a str {
    text.lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
        .map(str::trim)
        .unwrap_or("")
}

/// `systemctl show` output (KEY=value lines) → state and, for a failure, why.
pub fn parse_show(text: &str) -> (InstallState, Option<String>) {
    let get = |key: &str| show_field(text, key);
    if get("LoadState") == "not-found" {
        return (InstallState::Idle, None);
    }
    let ran = !matches!(get("ExecMainExitTimestampMonotonic"), "" | "0");
    match get("ActiveState") {
        "activating" | "active" | "reloading" | "deactivating" => (InstallState::Running, None),
        "failed" => {
            let detail = match (get("Result"), get("ExecMainStatus")) {
                ("exit-code", code) if !code.is_empty() => format!("exit status {code}"),
                ("timeout", _) => "it took too long and was stopped".to_string(),
                ("signal", _) => "it was stopped".to_string(),
                (r, _) if !r.is_empty() => r.to_string(),
                _ => "failed".to_string(),
            };
            (InstallState::Failed, Some(clean_text(&detail, 80)))
        }
        _ if ran && get("Result") == "success" => (InstallState::Done, None),
        _ => (InstallState::Idle, None),
    }
}

/// The last [`LOG_LINES`] non-empty lines of `path` (a carriage return starts a line, so
/// download progress shows its latest state), cleaned for display.
pub fn read_log(path: &Path) -> Vec<String> {
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(LOG_TAIL_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text
        .split('\n')
        .skip(usize::from(start > 0)) // a partial first line
        .filter_map(|l| l.rsplit('\r').find(|s| !s.trim().is_empty()))
        .map(|l| clean_text(l, 200))
        .filter(|l| !l.trim().is_empty())
        .collect();
    let cut = lines.len().saturating_sub(LOG_LINES);
    lines.drain(..cut);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("melissa-inst-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn only_the_three_known_ids_have_a_unit() {
        assert_eq!(
            unit_name("opencode"),
            Some("melissa-agent-install@opencode.service")
        );
        for id in INSTALLABLE {
            assert!(unit_name(id).is_some());
        }
        for bad in [
            "",
            "echo",
            "foo",
            "..",
            "claude.service",
            "claude ",
            "Claude",
            "grok;id",
            "../claude",
            "claude\n",
        ] {
            assert_eq!(unit_name(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn login_is_claude_and_grok_only_and_the_device_prompt_is_found() {
        assert_eq!(
            Job::Login.unit("grok"),
            Some("melissa-agent-login@grok.service")
        );
        assert_eq!(
            Job::Login.unit("claude"),
            Some("melissa-agent-login@claude.service")
        );
        for id in [
            "opencode",
            "echo",
            "..",
            "grok.service",
            "Claude",
            "claude ",
        ] {
            assert_eq!(Job::Login.unit(id), None, "{id}");
        }
        let log: Vec<String> = [
            "To sign in, open this URL in your browser:",
            "https://accounts.x.ai/oauth2/device?user_code=ABCD-1234",
            "Confirm this code in your browser:",
            "ABCD-1234",
            "Waiting for authorization...",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            device_prompt(&log),
            Some((
                "https://accounts.x.ai/oauth2/device?user_code=ABCD-1234".into(),
                "ABCD-1234".into()
            ))
        );
        // Another host is never offered as a link.
        let evil: Vec<String> = ["https://evil.example.com/x", "ABCD-1234"]
            .map(String::from)
            .to_vec();
        assert_eq!(device_prompt(&evil), None);
    }

    #[test]
    fn claude_url_only_from_anthropic_hosts() {
        let log: Vec<String> = [
            "Opening browser to sign in…",
            "If the browser didn't open, visit: https://claude.com/cai/oauth/authorize?code=true&state=x",
            "Paste code here if prompted >",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            claude_login_url(&log).as_deref(),
            Some("https://claude.com/cai/oauth/authorize?code=true&state=x")
        );
        for bad in [
            "visit: https://claude.com.evil.example/oauth/x",
            "visit: http://claude.com/oauth/x",
            "visit: https://evil.example/https://claude.com/oauth",
            "visit: https://claude.com/download",
        ] {
            assert_eq!(claude_login_url(&[bad.to_string()]), None, "{bad}");
        }
    }

    #[test]
    fn codes_are_one_visible_line() {
        assert!(valid_code("AbC-123_x#yZ.9"));
        for bad in ["", "a b", "a\nb", "a\rb", "ä", &"x".repeat(CODE_MAX + 1)] {
            assert!(!valid_code(bad), "{bad:?}");
        }
    }

    #[test]
    fn login_end_follows_the_exit_status() {
        let mut st = InstallStatus::idle();
        st.state = InstallState::Failed;
        for (code, end) in [
            (3, LoginEnd::WrongCode),
            (4, LoginEnd::Cancelled),
            (5, LoginEnd::TimedOut),
            (1, LoginEnd::Failed),
        ] {
            st.exit = Some(code);
            assert_eq!(LoginEnd::of(&st), end);
        }
        st.exit = None;
        st.timed_out = true;
        assert_eq!(LoginEnd::of(&st), LoginEnd::TimedOut);
    }

    #[test]
    fn the_code_goes_only_to_a_waiting_fifo() {
        use std::io::BufRead;
        let dir = temp("fifo");
        let inst = Installer::new(InstallWay::Button, dir.clone(), PathBuf::from("false"));
        // Nothing there, or a plain file: refused, nothing written.
        assert!(inst.send_code("abc").is_err());
        std::fs::write(dir.join(CODE_FIFO), "").unwrap();
        assert!(inst.send_code("abc").is_err());
        assert_eq!(std::fs::read_to_string(dir.join(CODE_FIFO)).unwrap(), "");
        std::fs::remove_file(dir.join(CODE_FIFO)).unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(dir.join(CODE_FIFO))
            .status()
            .unwrap();
        assert!(made.success());
        // A FIFO nobody reads: refused at once, no hang.
        assert!(inst.send_code("abc").is_err());
        // Bad codes never reach it.
        assert!(inst.send_code("a b").is_err());
        // A reader (read-write, like melissa-setup): the whole line arrives.
        let reader = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join(CODE_FIFO))
            .unwrap();
        inst.send_code("code#state").unwrap();
        inst.send_code("").unwrap();
        let mut lines = std::io::BufReader::new(reader).lines();
        assert_eq!(lines.next().unwrap().unwrap(), "code#state");
        assert_eq!(lines.next().unwrap().unwrap(), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn way_follows_the_host_kind_and_the_installed_files() {
        let root = temp("way");
        assert_eq!(
            InstallWay::detect(&root, HostKind::Other),
            InstallWay::Manual
        );
        assert_eq!(
            InstallWay::detect(&root, HostKind::Container),
            InstallWay::Manual
        );
        std::fs::create_dir_all(root.join("usr/lib/systemd/system")).unwrap();
        std::fs::write(root.join(UNIT_FILES[0]), "").unwrap();
        assert_eq!(
            InstallWay::detect(&root, HostKind::Container),
            InstallWay::Manual
        );
        std::fs::create_dir_all(root.join("usr/share/polkit-1/rules.d")).unwrap();
        std::fs::write(root.join(POLKIT_RULE), "").unwrap();
        assert_eq!(
            InstallWay::detect(&root, HostKind::Container),
            InstallWay::Button
        );
        // Outside the container image (a source run) the unit is never offered.
        assert_eq!(
            InstallWay::detect(&root, HostKind::Other),
            InstallWay::Manual
        );
        // A Nix install never uses the unit, even when the files are there.
        assert_eq!(InstallWay::detect(&root, HostKind::Nix), InstallWay::Nix);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parses_systemctl_show() {
        let show = |active: &str, result: &str, status: &str, ts: &str| {
            format!(
                "LoadState=loaded\nActiveState={active}\nResult={result}\nExecMainStatus={status}\n\
                 ExecMainExitTimestampMonotonic={ts}\n"
            )
        };
        assert_eq!(
            parse_show(&show("inactive", "success", "0", "0")),
            (InstallState::Idle, None)
        );
        assert_eq!(
            parse_show(&show("activating", "success", "0", "0")).0,
            InstallState::Running
        );
        assert_eq!(
            parse_show(&show("inactive", "success", "0", "123456")),
            (InstallState::Done, None)
        );
        assert_eq!(
            parse_show(&show("failed", "exit-code", "1", "123456")),
            (InstallState::Failed, Some("exit status 1".into()))
        );
        assert_eq!(
            parse_show(&show("failed", "timeout", "0", "1"))
                .1
                .as_deref(),
            Some("it took too long and was stopped")
        );
        assert_eq!(
            parse_show("LoadState=not-found\nActiveState=inactive\n").0,
            InstallState::Idle
        );
        assert_eq!(parse_show("").0, InstallState::Idle);
    }

    #[test]
    fn log_tail_keeps_the_last_lines_and_the_latest_progress() {
        let dir = temp("log");
        let path = dir.join("opencode.log");
        let mut text = String::new();
        for i in 0..30 {
            text.push_str(&format!("line {i}\n"));
        }
        text.push_str("\x1b[32m 10%\r 55%\r100%\r\n\nInstalled: opencode 1.2.3\n");
        std::fs::write(&path, text).unwrap();
        let log = read_log(&path);
        assert_eq!(log.len(), LOG_LINES);
        assert_eq!(log[LOG_LINES - 2], "100%");
        assert_eq!(log[LOG_LINES - 1], "Installed: opencode 1.2.3");
        assert!(log.iter().all(|l| !l.contains('\x1b')));
        assert!(read_log(&dir.join("missing.log")).is_empty());
        // A big log: only the end is read, the cut first line dropped.
        std::fs::write(&path, "x".repeat(40_000) + "\nlast\n").unwrap();
        assert_eq!(read_log(&path), ["last"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    fn fake_systemctl(dir: &Path, show: &str, start_ok: bool) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("systemctl");
        let calls = dir.join("calls");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"$@\" >> '{}'\nif [ \"$1\" = show ]; then printf '{}'; exit 0; fi\n{}\n",
                calls.display(),
                show.replace('\n', "\\n"),
                if start_ok {
                    "exit 0"
                } else {
                    "echo 'Access denied' >&2; exit 4"
                }
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn starts_only_fixed_units_and_reads_their_state() {
        let dir = temp("run");
        let show = "LoadState=loaded\nActiveState=failed\nResult=exit-code\nExecMainStatus=1\n\
                    ExecMainExitTimestampMonotonic=5\n";
        let ctl = fake_systemctl(&dir, show, true);
        std::fs::write(dir.join("grok.log"), "melissa-setup: download failed\n").unwrap();
        let inst = Installer::new(InstallWay::Button, dir.clone(), ctl.clone());
        inst.start("grok").await.unwrap();
        assert!(inst.start("../grok").await.is_err());
        assert!(inst.start("echo").await.is_err());
        let st = inst.status("grok").await;
        assert_eq!(st.state, InstallState::Failed);
        assert_eq!(st.detail.as_deref(), Some("exit status 1"));
        assert_eq!(st.log, ["melissa-setup: download failed"]);
        let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
        assert!(calls.starts_with("start --no-block melissa-agent-install@grok.service\n"));
        assert_eq!(calls.matches("start").count(), 1, "{calls}");
        // Other ways never call systemd.
        for way in [InstallWay::Nix, InstallWay::Manual] {
            let other = Installer::new(way, dir.clone(), ctl.clone());
            assert!(other.start("grok").await.is_err());
            assert_eq!(other.status("grok").await, InstallStatus::idle());
        }
        // A refusal carries systemd's words.
        let denied = Installer::new(
            InstallWay::Button,
            dir.clone(),
            fake_systemctl(&dir, show, false),
        );
        let err = denied.start("claude").await.unwrap_err().to_string();
        assert!(err.contains("Access denied"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
