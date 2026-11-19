//! The installed agent CLIs: where the program is and what `--version` says.
//!
//! `--version` runs as the lead agent's user, outside the sandbox (it only prints), with a clean
//! environment (PATH, HOME, NO_COLOR), no stdin, a 5 s timeout and the answer cached for
//! 10 minutes ("Check again" forgets it). Its output is untrusted: cleaned and cut short.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

/// How long a version answer is kept.
pub const CACHE_FOR: Duration = Duration::from_secs(600);
/// How long "no version" is kept.
pub const RETRY_AFTER: Duration = Duration::from_secs(15);
/// How long `--version` may take.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// Find a program: paths are used as given, bare names are looked up on `PATH`.
pub fn resolve(command: &str) -> Option<PathBuf> {
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

/// The version in `--version` output: the first token that looks like `1.2` (a leading `v`
/// dropped), else the cleaned first line.
pub fn parse_version(output: &str) -> Option<String> {
    let line = output.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = crate::clean_text(line, 80);
    let token = line
        .split(|c: char| c.is_whitespace() || c == ',' || c == '(' || c == ')')
        .map(|t| t.trim_start_matches(['v', 'V']))
        .find(|t| {
            let mut parts = t.split('.');
            matches!((parts.next(), parts.next()), (Some(a), Some(b))
                if !a.is_empty() && a.bytes().all(|c| c.is_ascii_digit())
                    && b.starts_with(|c: char| c.is_ascii_digit()))
        });
    Some(match token {
        Some(t) => crate::clean_text(t, 40),
        None => line,
    })
}

#[derive(Clone)]
struct Entry {
    at: Instant,
    version: Option<String>,
}

/// Cached `--version` answers per program. Cheap to clone.
#[derive(Clone, Default)]
pub struct VersionProbe {
    cache: Arc<Mutex<HashMap<PathBuf, Entry>>>,
    checked: Arc<Mutex<Option<SystemTime>>>,
}

impl VersionProbe {
    pub fn new() -> Self {
        Self::default()
    }

    /// The program's version (None: it gave none in time).
    pub async fn version(&self, program: &Path) -> Option<String> {
        if let Some(e) = lock(&self.cache).get(program) {
            // A missing answer is asked again soon (e.g. a program that was just installed).
            let keep = if e.version.is_some() {
                CACHE_FOR
            } else {
                RETRY_AFTER
            };
            if e.at.elapsed() < keep {
                return e.version.clone();
            }
        }
        let version = run_version(program).await;
        lock(&self.cache).insert(
            program.to_path_buf(),
            Entry {
                at: Instant::now(),
                version: version.clone(),
            },
        );
        *lock(&self.checked) = Some(SystemTime::now());
        version
    }

    /// Forget every answer ("Check again").
    pub fn forget(&self) {
        lock(&self.cache).clear();
    }

    /// When a program was last asked.
    pub fn checked_at(&self) -> Option<SystemTime> {
        *lock(&self.checked)
    }
}

async fn run_version(program: &Path) -> Option<String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.arg("--version")
        .env_clear()
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for var in ["PATH", "HOME"] {
        if let Some(v) = std::env::var_os(var) {
            cmd.env(var, v);
        }
    }
    let out = tokio::time::timeout(TIMEOUT, cmd.output()).await;
    match out {
        Ok(Ok(out)) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            parse_version(&text).or_else(|| parse_version(&String::from_utf8_lossy(&out.stderr)))
        }
        Ok(Ok(_)) => None,
        Ok(Err(e)) => {
            tracing::debug!(program = %program.display(), "--version failed: {e}");
            None
        }
        Err(_) => {
            tracing::warn!(program = %program.display(), "--version timed out");
            None
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // Poisoning only means another thread panicked mid-update; the data is still usable.
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_found_in_typical_output() {
        assert_eq!(
            parse_version("2.1.328 (Claude Code)\n").as_deref(),
            Some("2.1.328")
        );
        assert_eq!(parse_version("grok v0.4.2").as_deref(), Some("0.4.2"));
        assert_eq!(
            parse_version("\n  opencode 1.0.0-beta.3\n").as_deref(),
            Some("1.0.0-beta.3")
        );
        assert_eq!(parse_version("weird tool").as_deref(), Some("weird tool"));
        assert_eq!(parse_version("  \n"), None);
        assert_eq!(
            parse_version("\u{1b}[31m<script>\u{1b}[0m").as_deref(),
            Some("<script>")
        );
    }

    #[tokio::test]
    async fn probe_runs_caches_and_times_out_quietly() {
        let dir = std::env::temp_dir().join(format!("melissa-version-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let prog = dir.join("fake-cli");
        let count = dir.join("count");
        std::fs::write(
            &prog,
            format!(
                "#!/bin/sh\necho x >> {}\necho 'fake-cli 3.4.5'\n",
                count.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).unwrap();
        let probe = VersionProbe::new();
        assert_eq!(probe.version(&prog).await.as_deref(), Some("3.4.5"));
        assert_eq!(probe.version(&prog).await.as_deref(), Some("3.4.5"));
        assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 1);
        probe.forget();
        probe.version(&prog).await;
        assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 2);
        assert!(probe.checked_at().is_some());
        assert_eq!(probe.version(&dir.join("missing")).await, None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
