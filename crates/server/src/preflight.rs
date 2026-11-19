//! First-run check, printed when the daemon starts: where the UI is, which agent backends
//! were found (and why the others are not usable), which one the lead agent will use, whether the
//! sandbox can start and whether k3s is there. Meant for people trying Melissa from source;
//! every problem line says what to do about it (details in docs/QUICKSTART.md).

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
    pub backends: Vec<BackendLine>,
    /// `[lead] backends`, in order.
    pub lead_order: Vec<String>,
    /// A cluster kubeconfig was found (k3s present or pointed at).
    pub k3s: bool,
    /// The app catalog directory and how many manifests it holds.
    pub catalog: PathBuf,
    pub catalog_apps: usize,
    /// `/proc/sys/kernel/apparmor_restrict_unprivileged_userns` is 1 (Ubuntu 23.10+).
    pub apparmor_userns_restricted: bool,
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
                         (Ubuntu 23.10+). See \"Ubuntu and AppArmor\" in docs/QUICKSTART.md."
                            .into(),
                    );
                } else {
                    line(
                        "            Unprivileged user namespaces may be off. See \
                         \"Troubleshooting\" in docs/QUICKSTART.md."
                            .into(),
                    );
                }
            }
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
        line("    echo            (built in) always works; it only repeats your message".into());
        match lead {
            Some(_) => line(
                "  The first chat message starts the lead agent. If that agent is not logged in, \
                 it tries the next one."
                    .into(),
            ),
            None => {
                line(String::new());
                line(
                    "  No agent CLI is usable, so the lead agent cannot answer yet. The UI still works \
                     (try the echo backend)."
                        .into(),
                );
                line(
                    "  Install and log in to one of: Claude Code (claude), Grok CLI (grok), \
                     OpenCode (opencode),"
                        .into(),
                );
                line("  then restart. See docs/QUICKSTART.md.".into());
            }
        }
        if self.k3s {
            line(format!(
                "  k3s:      found. Apps: {} in {}",
                self.catalog_apps,
                self.catalog.display()
            ));
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
    pub catalog: &'a Path,
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
        catalog,
    } = i;
    let bwrap = melissa_agent::sandbox::find_bwrap();
    let sandbox = match &bwrap {
        None => Sandbox::NoBwrap,
        Some(bwrap) => probe_bwrap(bwrap).await,
    };
    let netbridge = melissa_agent::egress::find_netbridge().is_some();
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
    let catalog_apps = std::fs::read_dir(catalog)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "toml"))
                .count()
        })
        .unwrap_or(0);
    Report {
        listen,
        state: state.to_path_buf(),
        config: config_path.to_path_buf(),
        config_exists: config_path.is_file(),
        sandbox,
        netbridge,
        backends,
        lead_order: lead_order.to_vec(),
        k3s: kubeconfigs.iter().any(|p| p.is_file()),
        catalog: catalog.to_path_buf(),
        catalog_apps,
        apparmor_userns_restricted: std::fs::read_to_string(
            "/proc/sys/kernel/apparmor_restrict_unprivileged_userns",
        )
        .is_ok_and(|s| s.trim() == "1"),
    }
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
            catalog: PathBuf::from("manifests"),
            catalog_apps: 1,
            apparmor_userns_restricted: false,
        }
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
    fn first_line_trims_and_handles_empty() {
        assert_eq!(first_line("bwrap: x\nmore"), "bwrap: x");
        assert_eq!(first_line(""), "no error message");
    }
}
