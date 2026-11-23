//! First-run check, printed when the daemon starts: where the UI is, which agent backends
//! were found (and why the others are not usable), which one the lead agent will use, whether the
//! sandbox can start and whether k3s is there. Meant for people trying Melissa from source;
//! every problem line says what to do about it (details in docs/DEV-ENV.md, "Run from source").

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use melissa_agent::BackendInfo;

use crate::config::BackendSpec;

/// Why the sandbox (bubblewrap in its own user, PID and network namespaces) can or cannot start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sandbox {
    Ok,
    /// `bwrap` is not installed.
    NoBwrap,
    /// `bwrap` is there but could not create its namespaces (stderr, trimmed).
    Blocked(String),
}

/// One agent backend as the user should see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendLine {
    pub name: String,
    pub command: String,
    pub available: bool,
    /// For an unavailable backend: what is missing.
    pub reason: Option<String>,
}

/// Everything the startup summary shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub listen: SocketAddr,
    pub state: PathBuf,
    pub config: PathBuf,
    pub config_exists: bool,
    pub sandbox: Sandbox,
    pub netbridge: bool,
    /// Resource limits per agent sandbox: `Ok(the defaults)` when applied, `Err(why not)`.
    pub limits: Result<String, String>,
    /// Agent users (each sandbox its own host user): `Ok(the id range)` when on, `Err(why not)`.
    pub users: Result<String, String>,
    /// gVisor for apps (`runsc` and its containerd shim here): `Ok(runsc's version)`, `Err(why
    /// not)`. The deploy itself checks the cluster's RuntimeClass and fails closed.
    pub gvisor: Result<String, String>,
    /// Why an agent's memory use could panic the host (`vm.panic_on_oom`), if it could.
    pub oom_panic: Option<&'static str>,
    pub backends: Vec<BackendLine>,
    /// `[lead] backends`, in order.
    pub lead_order: Vec<String>,
    /// A cluster kubeconfig was found (k3s present or pointed at).
    pub k3s: bool,
    /// `/proc/sys/kernel/apparmor_restrict_unprivileged_userns` is 1 (Ubuntu 23.10+).
    pub apparmor_userns_restricted: bool,
    /// Sandboxed backends that run without Melissa's seccomp filter, with why
    /// (`sandbox.seccomp = false`, or no filter for this architecture).
    pub seccomp_off: Vec<(String, &'static str)>,
}

impl Report {
    /// The session backend the lead agent opens first: the first of its list that is usable.
    pub fn lead_backend(&self) -> Option<&str> {
        self.lead_order
            .iter()
            .map(String::as_str)
            .find(|name| self.backends.iter().any(|b| b.available && b.name == *name))
    }

    /// The text printed at startup. Plain ASCII, no colours: it may end up in a log.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let mut line = |s: String| {
            out.push_str(&s);
            out.push('\n');
        };
        line(String::new());
        line("Melissa (pre-alpha) is running.".into());
        line(format!(
            "  Open:     http://{}   (this machine only)",
            self.listen
        ));
        line(format!("  State:    {}", self.state.display()));
        line(format!(
            "  Config:   {}{}",
            self.config.display(),
            if self.config_exists {
                ""
            } else {
                "  (not present: defaults)"
            }
        ));
        match &self.sandbox {
            Sandbox::Ok if self.netbridge => line("  Sandbox:  ok (bubblewrap)".into()),
            Sandbox::Ok => line(
                "  Sandbox:  PROBLEM: melissa-netbridge not found next to melissa-server. \
                 Build both: `just demo` or `cargo build --bins -p melissa-server -p melissa-agent`."
                    .into(),
            ),
            Sandbox::NoBwrap => line(
                "  Sandbox:  PROBLEM: bwrap not found. Install bubblewrap \
                 (apt/dnf/pacman install bubblewrap) and restart."
                    .into(),
            ),
            Sandbox::Blocked(err) => {
                line(format!(
                    "  Sandbox:  PROBLEM: bubblewrap cannot create namespaces: {err}"
                ));
                if self.apparmor_userns_restricted {
                    line(
                        "            AppArmor restricts unprivileged user namespaces here \
                         (Ubuntu 23.10+). See \"Ubuntu and AppArmor\" in docs/DEV-ENV.md."
                            .into(),
                    );
                } else {
                    line(
                        "            Unprivileged user namespaces may be off. See \
                         \"Troubleshooting\" in docs/DEV-ENV.md."
                            .into(),
                    );
                }
            }
        }
        if !self.seccomp_off.is_empty() {
            let names: Vec<String> = self
                .seccomp_off
                .iter()
                .map(|(name, why)| format!("{name} ({why})"))
                .collect();
            line(format!(
                "  Seccomp:  OFF for {}. Those agents can reach ptrace, keyrings, bpf, perf \
                 events and nested namespaces.",
                names.join(", ")
            ));
        }
        let oom = self.oom_panic.map_or_else(String::new, |risk| {
            format!(
                " PROBLEM: {risk}. Fix: {}.",
                melissa_agent::sandbox::limits::OOM_PANIC_FIX
            )
        });
        match &self.limits {
            Ok(defaults) => line(format!("  Limits:   on (each agent: {defaults}).{oom}")),
            Err(why) => line(format!(
                "  Limits:   off: {why}. Agents run without memory, process and CPU limits.{oom}"
            )),
        }
        match &self.users {
            Ok(range) => line(format!(
                "  Agent users: on (each agent its own host user, ids {range})."
            )),
            Err(why) => line(format!(
                "  Agent users: off: {why}. Agents run as the server's own user."
            )),
        }
        match &self.gvisor {
            Ok(version) => line(format!(
                "  gVisor:   on (runsc {version}). Apps run in their own sandboxed kernel by default."
            )),
            Err(why) => line(format!(
                "  gVisor:   off: {why}. Apps that run under gVisor (the default) are not deployed."
            )),
        }
        line("  Agents:".into());
        let lead = self.lead_backend();
        for b in &self.backends {
            let state = if b.available {
                "found".to_string()
            } else {
                b.reason.clone().unwrap_or_else(|| "unavailable".into())
            };
            let role = if Some(b.name.as_str()) == lead {
                "   <- the lead agent uses this"
            } else {
                ""
            };
            line(format!(
                "    {:<15} {:<9} {}{}",
                b.name, b.command, state, role
            ));
        }
        match lead {
            Some(_) => line(
                "  The first chat message starts the lead agent. If that agent is not logged in, \
                 it tries the next one."
                    .into(),
            ),
            None => {
                line(String::new());
                line(
                    "  No agent CLI is usable, so the lead agent cannot answer yet. The UI still works."
                        .into(),
                );
                line(
                    "  Install and log in to one of: Claude Code (claude), Grok CLI (grok), \
                     OpenCode (opencode),"
                        .into(),
                );
                line("  then restart. See docs/DEV-ENV.md.".into());
            }
        }
        if self.k3s {
            line("  k3s:      found. Agents can deploy apps.".into());
        } else {
            line(
                "  k3s:      not found on this machine. Chat, agents and machine checks work; \
                 installing apps needs k3s."
                    .into(),
            );
        }
        line("  Stop with Ctrl-C.".into());
        out
    }
}

/// Look at this machine. `infos` are the configured backends' own views (`available` already
/// accounts for the binary, bwrap and the egress bridge); this adds the reason when not.
pub struct Inputs<'a> {
    pub listen: SocketAddr,
    pub state: &'a Path,
    pub config_path: &'a Path,
    pub specs: &'a [BackendSpec],
    pub infos: &'a [BackendInfo],
    pub lead_order: &'a [String],
    pub kubeconfigs: &'a [PathBuf],
}

pub async fn gather(i: Inputs<'_>) -> Report {
    let Inputs {
        listen,
        state,
        config_path,
        specs,
        infos,
        lead_order,
        kubeconfigs,
    } = i;
    let bwrap = melissa_agent::sandbox::find_bwrap();
    let sandbox = match &bwrap {
        None => Sandbox::NoBwrap,
        Some(bwrap) => probe_bwrap(bwrap).await,
    };
    let netbridge = melissa_agent::egress::find_netbridge().is_some();
    let seccomp_off = seccomp_off(specs, melissa_agent::sandbox::seccomp::host_supported());
    for (name, why) in &seccomp_off {
        tracing::warn!(
            backend = %name,
            reason = *why,
            "agent sandbox runs WITHOUT the seccomp filter: ptrace, keyrings, bpf, perf events \
             and nested namespaces are reachable from it"
        );
    }
    let backends = specs
        .iter()
        .map(|spec| {
            let (name, command) = match spec {
                BackendSpec::Cli(s) => (&s.name, &s.command),
                BackendSpec::Acp(s) => (&s.name, &s.command),
                BackendSpec::Claude(s) => (&s.name, &s.command),
                BackendSpec::Echo(s) => (&s.name, &s.name),
            };
            let available = infos.iter().any(|i| i.name == *name && i.available);
            let reason = (!available).then(|| {
                if find_program(command).is_none() {
                    format!("not installed (`{command}` not on PATH)")
                } else if bwrap.is_none() {
                    "sandbox: bwrap not found".into()
                } else if !netbridge {
                    "sandbox: melissa-netbridge not found".into()
                } else {
                    "unavailable (see the log)".into()
                }
            });
            BackendLine {
                name: name.clone(),
                command: command.clone(),
                available,
                reason,
            }
        })
        .collect();
    let k3s = kubeconfigs.iter().any(|p| p.is_file());
    let gvisor = gvisor_line().await;
    if let (true, Err(why)) = (k3s, &gvisor) {
        tracing::warn!(
            reason = %why,
            "gVisor is off: apps that run under gVisor (the default isolation) are not deployed"
        );
    }
    Report {
        listen,
        state: state.to_path_buf(),
        config: config_path.to_path_buf(),
        config_exists: config_path.is_file(),
        sandbox,
        netbridge,
        limits: limits_line(),
        users: users_line(),
        gvisor,
        oom_panic: oom_panic(),
        backends,
        lead_order: lead_order.to_vec(),
        k3s,
        apparmor_userns_restricted: std::fs::read_to_string(
            "/proc/sys/kernel/apparmor_restrict_unprivileged_userns",
        )
        .is_ok_and(|s| s.trim() == "1"),
        seccomp_off,
    }
}

/// Sandboxed backends without the seccomp filter, and why. `arch_supported`: Melissa has a
/// filter for this architecture (`seccomp::host_supported`).
fn seccomp_off(specs: &[BackendSpec], arch_supported: bool) -> Vec<(String, &'static str)> {
    specs
        .iter()
        .filter_map(|spec| {
            let sb = spec.effective_sandbox();
            if !sb.enabled {
                return None;
            }
            if !sb.seccomp {
                Some((spec.name().to_string(), "sandbox.seccomp = false"))
            } else if !arch_supported {
                Some((spec.name().to_string(), "no filter for this architecture"))
            } else {
                None
            }
        })
        .collect()
}

/// Start bwrap the way the sandbox does (all namespaces, loopback brought up) and run `true`.
async fn probe_bwrap(bwrap: &Path) -> Sandbox {
    let child = tokio::process::Command::new(bwrap)
        .args([
            "--unshare-all",
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "true",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(Duration::from_secs(10), child).await {
        Ok(Ok(out)) if out.status.success() => Sandbox::Ok,
        Ok(Ok(out)) => Sandbox::Blocked(first_line(&String::from_utf8_lossy(&out.stderr))),
        Ok(Err(e)) => Sandbox::Blocked(e.to_string()),
        Err(_) => Sandbox::Blocked("timed out".into()),
    }
}

/// Whether agent sandboxes get resource limits ([`melissa_agent::sandbox::limits::init`] ran at
/// startup); the default limits when they do.
fn limits_line() -> Result<String, String> {
    use melissa_agent::sandbox::limits::{status, Limits, Status};
    match status() {
        Some(Status::Active(_)) => Ok(Limits::default().to_string()),
        Some(Status::Off(why)) => Err(why.clone()),
        None => Err("not set up".into()),
    }
}

/// Whether agent sandboxes run as their own host users
/// ([`melissa_agent::sandbox::users::init`] ran at startup); their id range when they do.
fn users_line() -> Result<String, String> {
    use melissa_agent::sandbox::users::{status, Status};
    match status() {
        Some(Status::Active(s)) => Ok(format!("{}-{}", s.uids().start, s.uids().last())),
        Some(Status::Off(why)) => Err(why.clone()),
        None => Err("not set up".into()),
    }
}

/// The containerd shim k3s starts for the `runsc` handler (next to runsc on PATH).
const RUNSC_SHIM: &str = "containerd-shim-runsc-v1";

/// Whether gVisor can run apps here: runsc and its containerd shim on PATH (the image ships
/// both in /usr/bin; the NixOS module puts nixpkgs' gvisor on the server's and k3s's PATH), and
/// runsc's version. k3s gets the RuntimeClass and its containerd entry from the packaging.
async fn gvisor_line() -> Result<String, String> {
    let Some(runsc) = find_program("runsc") else {
        return Err("runsc not found (gVisor is not installed)".into());
    };
    if find_program(RUNSC_SHIM).is_none() {
        return Err(format!("{RUNSC_SHIM} not found next to runsc"));
    }
    let child = tokio::process::Command::new(&runsc)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let out = match tokio::time::timeout(Duration::from_secs(10), child).await {
        Ok(Ok(out)) if out.status.success() => out,
        Ok(Ok(out)) => {
            return Err(format!(
                "`runsc --version` failed: {}",
                first_line(&String::from_utf8_lossy(&out.stderr))
            ))
        }
        Ok(Err(e)) => return Err(format!("runsc does not start: {e}")),
        Err(_) => return Err("`runsc --version` timed out".into()),
    };
    runsc_version(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| "runsc printed no version".into())
}

/// `release-20261116.0` from runsc's `runsc version release-20261116.0` (first line).
fn runsc_version(stdout: &str) -> Option<String> {
    let v = stdout
        .lines()
        .next()?
        .trim()
        .strip_prefix("runsc version ")?
        .trim();
    (!v.is_empty() && v.len() <= 64 && v.chars().all(|c| c.is_ascii_graphic()))
        .then(|| v.to_string())
}

/// Whether `vm.panic_on_oom` turns an agent's out-of-memory into a host panic, given whether
/// agents get their own memory cgroup. The memory limit stays on either way
/// ([`melissa_agent::sandbox::limits::oom_panic_risk`]); the startup log already warned.
fn oom_panic() -> Option<&'static str> {
    use melissa_agent::sandbox::limits::{oom_panic_risk, read_panic_on_oom, tree};
    oom_panic_risk(read_panic_on_oom(), tree().is_some())
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        "no error message".into()
    } else {
        line.chars().take(200).collect()
    }
}

/// A bare name on `PATH`, or a path as given.
fn find_program(command: &str) -> Option<PathBuf> {
    let path = Path::new(command);
    if path.components().count() > 1 {
        return path.is_file().then(|| path.to_path_buf());
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(command))
            .find(|p| p.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> Report {
        Report {
            listen: SocketAddr::from(([127, 0, 0, 1], 8080)),
            state: PathBuf::from("/home/u/.local/state/melissa"),
            config: PathBuf::from("/home/u/.config/melissa/server.toml"),
            config_exists: false,
            sandbox: Sandbox::Ok,
            netbridge: true,
            limits: Ok("2 GiB memory, 512 processes, 2 CPUs".into()),
            users: Ok("1879048192-1879113727".into()),
            gvisor: Ok("release-20261116.0".into()),
            oom_panic: None,
            backends: vec![
                BackendLine {
                    name: "grok-acp".into(),
                    command: "grok".into(),
                    available: false,
                    reason: Some("not installed (`grok` not on PATH)".into()),
                },
                BackendLine {
                    name: "claude-session".into(),
                    command: "claude".into(),
                    available: true,
                    reason: None,
                },
            ],
            lead_order: vec!["grok-acp".into(), "claude-session".into()],
            k3s: false,
            apparmor_userns_restricted: false,
            seccomp_off: Vec::new(),
        }
    }

    #[test]
    fn seccomp_off_backends_are_named_in_the_summary() {
        let mut r = report();
        assert!(!r.render().contains("Seccomp:"));
        r.seccomp_off = vec![("grok-acp".into(), "sandbox.seccomp = false")];
        let text = r.render();
        assert!(
            text.contains("Seccomp:  OFF for grok-acp (sandbox.seccomp = false)"),
            "{text}"
        );
    }

    #[test]
    fn seccomp_off_comes_from_the_backend_config() {
        let config: crate::config::ServerConfig = toml::from_str(
            "[[backend]]\nname = \"on\"\ncommand = \"x\"\n\
             [[backend]]\nname = \"off\"\ncommand = \"x\"\nsandbox = { seccomp = false }\n\
             [[backend]]\nname = \"bare\"\ncommand = \"x\"\nsandbox = { enabled = false }",
        )
        .unwrap();
        assert_eq!(
            seccomp_off(&config.backends, true),
            [("off".to_string(), "sandbox.seccomp = false")]
        );
        let all: Vec<String> = seccomp_off(&config.backends, false)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(all, ["on", "off"]);
    }

    #[test]
    fn lead_uses_first_available_backend_in_her_order() {
        let r = report();
        assert_eq!(r.lead_backend(), Some("claude-session"));
        let text = r.render();
        assert!(text.contains("http://127.0.0.1:8080"));
        assert!(text.contains("claude-session  claude    found   <- the lead agent uses this"));
        assert!(text.contains("not installed (`grok` not on PATH)"));
        assert!(text.contains("k3s:      not found"));
        assert!(text.contains("(not present: defaults)"));
    }

    #[test]
    fn no_agent_and_sandbox_problems_say_what_to_do() {
        let mut r = report();
        r.backends[1].available = false;
        r.sandbox = Sandbox::NoBwrap;
        let text = r.render();
        assert_eq!(r.lead_backend(), None);
        assert!(text.contains("bwrap not found. Install bubblewrap"));
        assert!(text.contains("No agent CLI is usable"));
        r.sandbox = Sandbox::Blocked("setting up uid map: Permission denied".into());
        r.apparmor_userns_restricted = true;
        assert!(r.render().contains("Ubuntu and AppArmor"));
    }

    #[test]
    fn limits_line_says_whether_agents_are_limited() {
        let mut r = report();
        assert!(r
            .render()
            .contains("Limits:   on (each agent: 2 GiB memory, 512 processes, 2 CPUs)"));
        r.limits = Err("no cgroup v2 (unified hierarchy) on this host".into());
        assert!(r.render().contains(
            "Limits:   off: no cgroup v2 (unified hierarchy) on this host. Agents run without"
        ));
    }

    #[test]
    fn gvisor_line_says_whether_apps_get_their_own_kernel() {
        let mut r = report();
        assert!(r.render().contains(
            "  gVisor:   on (runsc release-20261116.0). Apps run in their own sandboxed kernel"
        ));
        r.gvisor = Err("runsc not found (gVisor is not installed)".into());
        assert!(r.render().contains(
            "  gVisor:   off: runsc not found (gVisor is not installed). Apps that run under \
             gVisor (the default) are not deployed."
        ));
        assert_eq!(
            runsc_version("runsc version release-20261116.0\nspec: 1.2.1\n").as_deref(),
            Some("release-20261116.0")
        );
        assert_eq!(runsc_version("runsc version \n"), None);
        assert_eq!(runsc_version("something else"), None);
        assert_eq!(runsc_version("runsc version a\u{1b}[31m"), None);
    }

    #[test]
    fn limits_line_reports_a_dangerous_panic_on_oom() {
        use melissa_agent::sandbox::limits::{oom_panic_risk, OomPanic};
        let mut r = report();
        assert!(!r.render().contains("panic_on_oom"));
        r.oom_panic = oom_panic_risk(Some(OomPanic::Always), true);
        let text = r.render();
        let limits = text.lines().find(|l| l.contains("Limits:")).unwrap();
        assert!(limits.contains("on (each agent: 2 GiB memory"));
        assert!(limits.contains(
            "PROBLEM: vm.panic_on_oom = 2: an agent that hits its memory limit panics the whole host"
        ));
        assert!(limits.contains("sysctl -w vm.panic_on_oom=0"));
        // 1 is safe while agents have their own memory cgroup.
        r.oom_panic = oom_panic_risk(Some(OomPanic::SystemWide), true);
        assert!(!r.render().contains("panic_on_oom"));
    }

    #[test]
    fn first_line_trims_and_handles_empty() {
        assert_eq!(first_line("bwrap: x\nmore"), "bwrap: x");
        assert_eq!(first_line(""), "no error message");
    }
}
