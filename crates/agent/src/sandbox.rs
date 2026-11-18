//! Bubblewrap (`bwrap`) sandbox for agent CLI processes (defence in depth).
//!
//! Inside the sandbox the CLI sees: `/usr` and `/etc` read-only, a private `/tmp`, an
//! empty, read-only `$HOME` with only the paths its [`SandboxSpec`] names, the CLI binary
//! (read-only), and the per-task working directory (the only writable host path). It
//! shares the host network (it must reach its model API and the queen's MCP endpoint on
//! loopback) but gets its own user, PID, IPC, UTS and cgroup namespaces, no capabilities
//! and `no_new_privs`, and dies with its parent, so killing `bwrap` kills every process
//! inside, grandchildren included.
//!
//! The command line is built by the pure [`command`] function from a [`SandboxPlan`];
//! [`SandboxPlan::resolve`] does the host lookups (HOME, binary path, which files exist).

use std::{
    ffi::OsString,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};

/// Where bubblewrap is expected on Fedora (and the Melissa images).
pub const BWRAP_PATH: &str = "/usr/bin/bwrap";

/// Host name inside the sandbox (hides the real one).
const SANDBOX_HOSTNAME: &str = "melissa-agent";

/// Host environment variables passed into every sandbox when set. Everything else is
/// cleared; add names to [`SandboxSpec::env`] to pass more.
const BASE_ENV: &[&str] = &["USER", "LOGNAME", "LANG", "LC_ALL", "TZ"];

/// Environment variables the sandbox always sets itself; [`SandboxSpec::env`] may not name them.
const RESERVED_ENV: &[&str] = &["HOME", "PATH", "NO_COLOR", "TERM"];

/// Per-backend sandbox settings (`[backend.sandbox]` in the queen config).
///
/// Paths are relative to `$HOME`. Missing `ro`/`rw` paths are skipped, so a CLI that is
/// not logged in simply sees no credentials. If the table is given, it replaces the
/// built-in preset for that CLI entirely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxSpec {
    /// Run the CLI under bubblewrap. Turning this off runs it as a plain process of the
    /// queen's user, with access to all of that user's files.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    /// Empty, writable, throw-away directories (tmpfs), e.g. a CLI's state directory.
    #[serde(default)]
    pub tmpfs: Vec<String>,
    /// Paths bound read-only.
    #[serde(default)]
    pub ro: Vec<String>,
    /// Paths bound read-write (e.g. a credentials file the CLI refreshes).
    #[serde(default)]
    pub rw: Vec<String>,
    /// Names of extra host environment variables to pass through (e.g. an API key variable).
    #[serde(default)]
    pub env: Vec<String>,
}

fn enabled_by_default() -> bool {
    true
}

impl Default for SandboxSpec {
    fn default() -> Self {
        Self {
            enabled: true,
            tmpfs: Vec::new(),
            ro: Vec::new(),
            rw: Vec::new(),
            env: Vec::new(),
        }
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

impl SandboxSpec {
    /// A spec that runs the CLI without a sandbox.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    /// The built-in sandbox for a command, chosen by its file name. Known CLIs get the
    /// narrowest set of paths verified to work (docs/agent-skills/agent-cli-backends.md);
    /// unknown commands get an empty home.
    pub fn preset(command: &str) -> Self {
        let name = Path::new(command)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        match name {
            // Claude Code: only the OAuth credentials (refreshed in place). Sessions,
            // history and `~/.claude.json` stay hidden; Claude starts with empty ones.
            "claude" => Self {
                tmpfs: strings(&[".claude"]),
                rw: strings(&[".claude/.credentials.json"]),
                ..Self::default()
            },
            // Grok CLI: the login and the user's settings; sessions and memory stay hidden.
            "grok" => Self {
                tmpfs: strings(&[".grok"]),
                ro: strings(&[".grok/config.toml"]),
                rw: strings(&[".grok/auth.json"]),
                ..Self::default()
            },
            // OpenCode: its config read-only, provider logins (`auth.json`) if any; the
            // session database and caches are fresh per run.
            "opencode" => Self {
                tmpfs: strings(&[
                    ".local/share/opencode",
                    ".local/state/opencode",
                    ".cache/opencode",
                ]),
                ro: strings(&[".config/opencode"]),
                rw: strings(&[".local/share/opencode/auth.json"]),
                ..Self::default()
            },
            _ => Self::default(),
        }
    }

    /// Reject paths that escape or expose all of `$HOME`, and bad environment names.
    pub fn validate(&self) -> Result<(), SandboxError> {
        for path in self.tmpfs.iter().chain(&self.ro).chain(&self.rw) {
            let p = Path::new(path);
            let normal = p.components().all(|c| matches!(c, Component::Normal(_)));
            if path.is_empty() || !normal {
                return Err(SandboxError::BadPath(path.clone()));
            }
        }
        for name in &self.env {
            let valid = !name.is_empty()
                && !name.starts_with(|c: char| c.is_ascii_digit())
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !valid || RESERVED_ENV.contains(&name.as_str()) {
                return Err(SandboxError::BadEnv(name.clone()));
            }
        }
        Ok(())
    }
}

/// Errors from sandbox configuration or setup.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error(
        "sandbox path `{0}` must be relative to $HOME, non-empty and without `.`/`..` components"
    )]
    BadPath(String),
    #[error("sandbox env `{0}` is not a valid, non-reserved variable name")]
    BadEnv(String),
    #[error("HOME is not set or not absolute; cannot build the sandbox")]
    NoHome,
    #[error("cannot resolve the CLI binary {path}: {source}")]
    Program {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// How a host path appears inside the sandbox (always at the same path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mount {
    Tmpfs(PathBuf),
    ReadOnly(PathBuf),
    ReadWrite(PathBuf),
    /// A host directory bound read-write at another path (e.g. a Melissa-owned session
    /// store at the agent's `~/.grok/sessions`).
    BindAt {
        src: PathBuf,
        dst: PathBuf,
    },
}

impl Mount {
    fn path(&self) -> &Path {
        match self {
            Mount::Tmpfs(p) | Mount::ReadOnly(p) | Mount::ReadWrite(p) => p,
            Mount::BindAt { dst, .. } => dst,
        }
    }
}

/// Everything [`command`] needs, resolved from the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPlan {
    /// The `bwrap` binary.
    pub bwrap: PathBuf,
    /// `$HOME`, recreated empty and read-only inside the sandbox.
    pub home: PathBuf,
    /// The CLI binary with all symlinks resolved; bound read-only and executed directly.
    pub program: PathBuf,
    /// Home paths, in mount order (parents before children).
    pub mounts: Vec<Mount>,
    /// Environment inside the sandbox (besides HOME, PATH, NO_COLOR and TERM).
    pub env: Vec<(String, OsString)>,
}

impl SandboxPlan {
    /// Resolve `spec` against the host: `home` (normally `$HOME`), the CLI binary as
    /// found on `PATH`, and which of the configured paths exist. Reads the process
    /// environment for [`BASE_ENV`] and [`SandboxSpec::env`].
    pub fn resolve(
        spec: &SandboxSpec,
        bwrap: &Path,
        home: Option<&Path>,
        program: &Path,
    ) -> Result<Self, SandboxError> {
        let home = home
            .filter(|h| h.is_absolute())
            .ok_or(SandboxError::NoHome)?
            .to_path_buf();
        let program = program
            .canonicalize()
            .map_err(|source| SandboxError::Program {
                path: program.to_path_buf(),
                source,
            })?;
        let mut mounts: Vec<Mount> = spec
            .tmpfs
            .iter()
            .map(|p| Mount::Tmpfs(home.join(p)))
            .collect();
        // `exists()` follows symlinks; bwrap binds the symlink's target at the same path.
        mounts.extend(
            spec.ro
                .iter()
                .map(|p| home.join(p))
                .filter(|p| p.exists())
                .map(Mount::ReadOnly),
        );
        mounts.extend(
            spec.rw
                .iter()
                .map(|p| home.join(p))
                .filter(|p| p.exists())
                .map(Mount::ReadWrite),
        );
        // Parents first, so a bind inside a tmpfs is not hidden by it. Stable: keeps order otherwise.
        mounts.sort_by_key(|m| m.path().components().count());

        let env = BASE_ENV
            .iter()
            .map(|s| (*s).to_string())
            .chain(spec.env.iter().cloned())
            .filter_map(|name| std::env::var_os(&name).map(|value| (name, value)))
            .collect();
        Ok(Self {
            bwrap: bwrap.to_path_buf(),
            home,
            program,
            mounts,
            env,
        })
    }

    /// Add host directories bound read-write at home-relative paths (`(host dir, path
    /// under $HOME)`); paths must pass the same checks as [`SandboxSpec`] paths.
    pub fn with_binds(mut self, binds: &[(PathBuf, String)]) -> Result<Self, SandboxError> {
        for (src, rel) in binds {
            let p = Path::new(rel);
            if rel.is_empty() || !p.components().all(|c| matches!(c, Component::Normal(_))) {
                return Err(SandboxError::BadPath(rel.clone()));
            }
            self.mounts.push(Mount::BindAt {
                src: src.clone(),
                dst: self.home.join(p),
            });
        }
        self.mounts.sort_by_key(|m| m.path().components().count());
        Ok(self)
    }
}

/// Build the `bwrap` program and argv that run `args` under the sandbox in `workdir`.
/// `extra_ro` are further files bound read-only (e.g. an MCP config outside `workdir`).
/// Pure: no host lookups; every element is a separate argv entry, never a shell string.
pub fn command(
    plan: &SandboxPlan,
    workdir: &Path,
    extra_ro: &[PathBuf],
    args: &[String],
) -> (PathBuf, Vec<OsString>) {
    let mut argv: Vec<OsString> = Vec::new();
    let mut push = |items: &[&dyn AsRef<std::ffi::OsStr>]| {
        argv.extend(items.iter().map(|i| i.as_ref().to_os_string()));
    };

    // Namespaces and privileges. bwrap always sets no_new_privs for unprivileged users.
    push(&[
        &"--unshare-all",
        &"--share-net",
        &"--die-with-parent",
        &"--new-session",
        &"--cap-drop",
        &"ALL",
        &"--hostname",
        &SANDBOX_HOSTNAME,
    ]);

    // Environment: nothing from the host unless allowed.
    push(&[&"--clearenv"]);
    let path_env = match plan.program.parent() {
        Some(dir) => {
            let mut p = dir.as_os_str().to_os_string();
            p.push(":/usr/bin");
            p
        }
        None => OsString::from("/usr/bin"),
    };
    push(&[&"--setenv", &"HOME", &plan.home]);
    push(&[&"--setenv", &"PATH", &path_env]);
    push(&[&"--setenv", &"NO_COLOR", &"1"]);
    push(&[&"--setenv", &"TERM", &"dumb"]);
    for (name, value) in &plan.env {
        push(&[&"--setenv", name, value]);
    }

    // System: read-only /usr and /etc (Fedora is usr-merged), the resolver's runtime dir
    // (/etc/resolv.conf links there on Fedora), fresh /proc, minimal /dev, private /tmp.
    push(&[&"--ro-bind", &"/usr", &"/usr"]);
    for dir in ["bin", "sbin", "lib", "lib64"] {
        push(&[&"--symlink", &format!("usr/{dir}"), &format!("/{dir}")]);
    }
    push(&[&"--ro-bind", &"/etc", &"/etc"]);
    push(&[
        &"--ro-bind-try",
        &"/run/systemd/resolve",
        &"/run/systemd/resolve",
    ]);
    push(&[&"--proc", &"/proc", &"--dev", &"/dev"]);
    push(&[&"--tmpfs", &"/tmp", &"--tmpfs", &"/var/tmp"]);

    // Home: an empty tmpfs with only the configured paths; frozen read-only below.
    push(&[&"--tmpfs", &plan.home]);
    for mount in &plan.mounts {
        match mount {
            Mount::Tmpfs(p) => push(&[&"--tmpfs", p]),
            Mount::ReadOnly(p) => push(&[&"--ro-bind", p, p]),
            Mount::ReadWrite(p) => push(&[&"--bind", p, p]),
            Mount::BindAt { src, dst } => push(&[&"--bind", src, dst]),
        }
    }

    // The CLI binary, unless a bind already exposes it.
    let bound = |path: &Path| {
        path.starts_with("/usr")
            || plan.mounts.iter().any(|m| match m {
                Mount::ReadOnly(p) | Mount::ReadWrite(p) => path.starts_with(p),
                Mount::Tmpfs(_) | Mount::BindAt { .. } => false,
            })
    };
    if !bound(&plan.program) {
        push(&[&"--ro-bind", &plan.program, &plan.program]);
    }
    for file in extra_ro {
        if !file.starts_with(workdir) {
            push(&[&"--ro-bind", file, file]);
        }
    }

    // The task's working directory: the only writable host path. Then freeze the rest.
    push(&[&"--bind", &workdir, &workdir]);
    // `--remount-ro` affects one mount only: the binds and tmpfs mounts inside stay as set.
    push(&[&"--remount-ro", &plan.home]);
    push(&[&"--remount-ro", &"/"]);
    push(&[&"--chdir", &workdir]);

    push(&[&"--", &plan.program]);
    argv.extend(args.iter().map(OsString::from));
    (plan.bwrap.clone(), argv)
}

/// Find bubblewrap: the Fedora location first, then `PATH`.
pub fn find_bwrap() -> Option<PathBuf> {
    let fixed = Path::new(BWRAP_PATH);
    if fixed.is_file() {
        return Some(fixed.to_path_buf());
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("bwrap"))
            .find(|candidate| candidate.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(mounts: Vec<Mount>, program: &str) -> SandboxPlan {
        SandboxPlan {
            bwrap: BWRAP_PATH.into(),
            home: "/home/u".into(),
            program: program.into(),
            mounts,
            env: vec![("LANG".into(), "C.UTF-8".into())],
        }
    }

    fn strs(argv: &[OsString]) -> Vec<&str> {
        argv.iter().map(|a| a.to_str().unwrap()).collect()
    }

    /// True if `needle` appears as a contiguous run in `hay`.
    fn has(hay: &[&str], needle: &[&str]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn argv_isolates_and_binds_narrowly() {
        let p = plan(
            vec![
                Mount::Tmpfs("/home/u/.claude".into()),
                Mount::ReadWrite("/home/u/.claude/.credentials.json".into()),
            ],
            "/home/u/.local/share/claude/versions/9",
        );
        let prompt = "--always-approve; $(rm -rf ~)";
        let (prog, argv) = command(
            &p,
            Path::new("/state/work/task-1"),
            &[],
            &["-p".into(), prompt.into()],
        );
        assert_eq!(prog, PathBuf::from(BWRAP_PATH));
        let a = strs(&argv);
        for flag in [
            "--unshare-all",
            "--share-net",
            "--die-with-parent",
            "--new-session",
            "--clearenv",
        ] {
            assert!(a.contains(&flag), "missing {flag}");
        }
        assert!(has(&a, &["--cap-drop", "ALL"]));
        assert!(has(&a, &["--ro-bind", "/usr", "/usr"]));
        assert!(has(&a, &["--ro-bind", "/etc", "/etc"]));
        assert!(has(&a, &["--tmpfs", "/tmp"]));
        assert!(has(&a, &["--setenv", "HOME", "/home/u"]));
        assert!(has(&a, &["--setenv", "LANG", "C.UTF-8"]));
        assert!(has(&a, &["--tmpfs", "/home/u"]));
        assert!(has(&a, &["--remount-ro", "/home/u"]));
        assert!(has(&a, &["--tmpfs", "/home/u/.claude"]));
        let creds = "/home/u/.claude/.credentials.json";
        assert!(has(&a, &["--bind", creds, creds]));
        let bin = "/home/u/.local/share/claude/versions/9";
        assert!(has(&a, &["--ro-bind", bin, bin]));
        let wd = "/state/work/task-1";
        assert!(has(&a, &["--bind", wd, wd]));
        assert!(has(&a, &["--chdir", wd]));
        // The only writable host binds are the credentials and the workdir.
        let rw: Vec<_> = a
            .windows(2)
            .filter(|w| w[0] == "--bind")
            .map(|w| w[1])
            .collect();
        assert_eq!(rw, [creds, wd]);
        // `/` is frozen after all mounts, and the command follows `--` with the prompt
        // as one untouched element at the end.
        let remount = a.iter().position(|x| *x == "--remount-ro").unwrap();
        let last_bind = a.iter().rposition(|x| x.starts_with("--bind")).unwrap();
        assert!(remount > last_bind);
        assert_eq!(&a[a.len() - 4..], ["--", bin, "-p", prompt]);
    }

    #[test]
    fn binary_under_a_bind_or_usr_is_not_bound_twice() {
        let p = plan(
            vec![Mount::ReadOnly("/home/u/.tool".into())],
            "/home/u/.tool/bin/tool",
        );
        let (_, argv) = command(&p, Path::new("/w"), &[], &[]);
        // Only as the command after `--`, not as a bind.
        let a = strs(&argv);
        assert_eq!(
            a.iter().filter(|x| **x == "/home/u/.tool/bin/tool").count(),
            1
        );

        let p = plan(vec![], "/usr/bin/bash");
        let (_, argv) = command(&p, Path::new("/w"), &[], &[]);
        assert_eq!(
            strs(&argv)
                .iter()
                .filter(|x| **x == "/usr/bin/bash")
                .count(),
            1
        );
        assert!(has(
            &strs(&argv),
            &["--setenv", "PATH", "/usr/bin:/usr/bin"]
        ));
    }

    #[test]
    fn extra_files_outside_the_workdir_are_bound_read_only() {
        let p = plan(vec![], "/usr/bin/x");
        let (_, argv) = command(
            &p,
            Path::new("/w"),
            &["/w/mcp.json".into(), "/elsewhere/mcp.json".into()],
            &[],
        );
        let a = strs(&argv);
        assert!(has(
            &a,
            &["--ro-bind", "/elsewhere/mcp.json", "/elsewhere/mcp.json"]
        ));
        assert!(!a.contains(&"/w/mcp.json"));
    }

    #[test]
    fn session_store_binds_land_inside_the_home_tmpfs() {
        let p = plan(vec![Mount::Tmpfs("/home/u/.grok".into())], "/usr/bin/x")
            .with_binds(&[("/state/queen/store".into(), ".grok/sessions".into())])
            .unwrap();
        let (_, argv) = command(&p, Path::new("/w"), &[], &[]);
        let a = strs(&argv);
        assert!(has(
            &a,
            &["--bind", "/state/queen/store", "/home/u/.grok/sessions"]
        ));
        // After the tmpfs it lives in, so the tmpfs does not hide it.
        let tmpfs = a.iter().position(|x| *x == "/home/u/.grok").unwrap();
        let bind = a.iter().position(|x| *x == "/state/queen/store").unwrap();
        assert!(bind > tmpfs);
        for bad in ["", "../x", "/abs", ".grok/../.."] {
            assert!(plan(vec![], "/usr/bin/x")
                .with_binds(&[("/s".into(), bad.into())])
                .is_err());
        }
    }

    #[test]
    fn presets_for_known_clis() {
        let claude = SandboxSpec::preset("/opt/bin/claude");
        assert!(claude.enabled);
        assert_eq!(claude.rw, [".claude/.credentials.json"]);
        assert_eq!(SandboxSpec::preset("grok").rw, [".grok/auth.json"]);
        assert_eq!(SandboxSpec::preset("opencode").ro, [".config/opencode"]);
        assert_eq!(SandboxSpec::preset("other"), SandboxSpec::default());
        for name in ["claude", "grok", "opencode"] {
            SandboxSpec::preset(name).validate().unwrap();
        }
    }

    #[test]
    fn validation_rejects_escapes_and_reserved_env() {
        let bad_paths = ["", ".", "..", "../x", "a/../../x", "/etc", "./x"];
        for path in bad_paths {
            let spec = SandboxSpec {
                ro: vec![path.into()],
                ..SandboxSpec::default()
            };
            assert!(spec.validate().is_err(), "{path:?} accepted");
        }
        for name in ["HOME", "PATH", "", "1X", "A-B", "A B"] {
            let spec = SandboxSpec {
                env: vec![name.into()],
                ..SandboxSpec::default()
            };
            assert!(spec.validate().is_err(), "{name:?} accepted");
        }
        let ok = SandboxSpec {
            rw: vec![".config/x/token".into()],
            env: vec!["XAI_API_KEY".into()],
            ..SandboxSpec::default()
        };
        ok.validate().unwrap();
    }

    #[test]
    fn resolve_skips_missing_paths_and_orders_parents_first() {
        let home = std::env::temp_dir().join(format!("melissa-sbx-resolve-{}", std::process::id()));
        std::fs::create_dir_all(home.join(".cli")).unwrap();
        std::fs::write(home.join(".cli/token"), "t").unwrap();
        let spec = SandboxSpec {
            rw: vec![".cli/token".into(), ".cli/missing".into()],
            tmpfs: vec![".cli".into()],
            ..SandboxSpec::default()
        };
        let plan = SandboxPlan::resolve(
            &spec,
            Path::new(BWRAP_PATH),
            Some(&home),
            Path::new("/usr/bin/sh"),
        )
        .unwrap();
        assert_eq!(
            plan.mounts,
            [
                Mount::Tmpfs(home.join(".cli")),
                Mount::ReadWrite(home.join(".cli/token")),
            ]
        );
        assert!(plan.program.is_absolute());
        assert!(matches!(
            SandboxPlan::resolve(&spec, Path::new(BWRAP_PATH), None, Path::new("/usr/bin/sh")),
            Err(SandboxError::NoHome)
        ));
        std::fs::remove_dir_all(&home).unwrap();
    }
}
