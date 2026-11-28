//! The backup engine: restic today ([`Restic`]), behind [`BackupEngine`] so rustic_core (same
//! repository format) can replace it.
//!
//! restic is a pinned static binary in the image (packaging/container/restic.lock), run with
//! `--json` and a cleared environment. Two options this relies on:
//! - `--insecure-no-password` (restic 0.17.0, 2024-07): an unencrypted-in-effect repository
//!   with an empty password, for owners who turn encryption off;
//! - `backup --stdin-from-command` (restic 0.17.0): restic runs the dump command itself, stores
//!   its standard output and fails the snapshot if the command exits non-zero.
//!
//! Exit codes used (restic 0.17.0+; 12 since 0.17.1): 3 = some source files unreadable,
//! 10 = no repository, 11 = locked, 12 = wrong password. Fatal errors end with an
//! `{"message_type": "exit_error"}` line on stderr.

use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use jiff::{tz::TimeZone, Timestamp};
use serde::Deserialize;

use crate::{config::Retention, BackupError};

/// How to open one repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub path: PathBuf,
    /// The recovery key's file; `None`: no password (`--insecure-no-password`).
    pub password_file: Option<PathBuf>,
}

/// One snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub id: String,
    /// Unix seconds (the run's time; every snapshot of a run has the same).
    pub time: i64,
    pub tags: Vec<String>,
    pub paths: Vec<String>,
}

impl Snapshot {
    pub fn has_tags(&self, tags: &[String]) -> bool {
        tags.iter().all(|t| self.tags.contains(t))
    }
}

/// What a backup added.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Added {
    /// `None` when restic skipped the snapshot (nothing changed and `--skip-if-unchanged`).
    pub snapshot: Option<String>,
    pub bytes_added: u64,
    pub files: u64,
    /// Some source files could not be read (exit code 3); the snapshot exists without them.
    pub incomplete: bool,
}

/// One backup to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source<'a> {
    pub tags: &'a [String],
    /// The run's time (Unix seconds).
    pub time: i64,
}

pub trait BackupEngine {
    /// Make a new repository; returns its id.
    fn init(&self, repo: &Repo) -> Result<String, BackupError>;
    /// The id of an existing repository (also proves the key opens it).
    fn repo_id(&self, repo: &Repo) -> Result<String, BackupError>;
    /// Back up `paths` without `excludes`.
    fn backup_paths(
        &self,
        repo: &Repo,
        source: &Source<'_>,
        paths: &[PathBuf],
        excludes: &[PathBuf],
    ) -> Result<Added, BackupError>;
    /// Back up what `argv` prints, stored as `/<file>`; fails if it exits non-zero.
    fn backup_command(
        &self,
        repo: &Repo,
        source: &Source<'_>,
        file: &str,
        argv: &[String],
    ) -> Result<Added, BackupError>;
    /// Snapshots with every one of `tags`, oldest first.
    fn snapshots(&self, repo: &Repo, tags: &[String]) -> Result<Vec<Snapshot>, BackupError>;
    /// Restore a whole snapshot below `target` (paths keep their absolute form under it).
    fn restore(&self, repo: &Repo, snapshot: &str, target: &Path) -> Result<(), BackupError>;
    /// Feed file `/<file>` of a snapshot to `consumer`'s standard input and wait for both.
    fn dump_into(
        &self,
        repo: &Repo,
        snapshot: &str,
        file: &str,
        consumer: &mut Command,
    ) -> Result<(), BackupError>;
    /// Apply the retention policy to every group of snapshots (host + tags) and prune.
    /// Returns how many snapshots went.
    fn forget(&self, repo: &Repo, retention: &Retention) -> Result<usize, BackupError>;
}

/// restic as a child process.
#[derive(Debug, Clone)]
pub struct Restic {
    pub binary: PathBuf,
    /// `--host` of new snapshots and the forget filter.
    pub host: String,
}

impl Restic {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            host: crate::SNAPSHOT_HOST.into(),
        }
    }

    /// The image's restic.
    pub fn system() -> Self {
        Self::new(crate::RESTIC_BIN)
    }

    fn command(&self, repo: &Repo, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.binary);
        cmd.env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("TZ", "UTC")
            .arg("--repo")
            .arg(&repo.path)
            .args(["--no-cache", "--json", "--quiet"]);
        match &repo.password_file {
            Some(f) => {
                cmd.arg("--password-file").arg(f);
            }
            None => {
                cmd.arg("--insecure-no-password");
            }
        }
        cmd.args(args).stdin(Stdio::null());
        cmd
    }

    fn backup_command_line(&self, repo: &Repo, source: &Source<'_>) -> Command {
        let time = restic_time(source.time);
        let mut cmd = self.command(repo, &["backup", "--host", &self.host, "--time", &time]);
        for t in source.tags {
            cmd.arg("--tag").arg(t);
        }
        cmd
    }
}

/// `--time` for restic (UTC, as TZ is set for it).
fn restic_time(unix: i64) -> String {
    Timestamp::from_second(unix)
        .unwrap_or(Timestamp::UNIX_EPOCH)
        .to_zoned(TimeZone::UTC)
        .strftime("%Y-%m-%d %H:%M:%S")
        .to_string()
}

/// Map a failed run to an error: restic's exit code and its `exit_error` message.
fn failure(code: Option<i32>, stderr: &str) -> BackupError {
    let message = stderr
        .lines()
        .rev()
        .find_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l.trim()).ok()?;
            (v["message_type"] == "exit_error")
                .then(|| v["message"].as_str().map(str::to_string))?
        })
        .unwrap_or_else(|| {
            let tail: Vec<&str> = stderr.lines().rev().take(3).collect();
            tail.into_iter().rev().collect::<Vec<_>>().join(" / ")
        });
    let message: String = message.chars().take(500).collect();
    match code {
        Some(10) => BackupError::NoRepository,
        Some(11) => BackupError::Locked,
        Some(12) => BackupError::WrongKey,
        _ => BackupError::Engine(format!(
            "{} (exit {})",
            if message.is_empty() {
                "failed"
            } else {
                &message
            },
            code.map_or("signal".to_string(), |c| c.to_string())
        )),
    }
}

/// Run to the end; stdout on success (exit 0, or 3 when `partial_ok`).
fn run(mut cmd: Command, partial_ok: bool) -> Result<(String, bool), BackupError> {
    let out = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| BackupError::Engine(format!("cannot run restic: {e}")))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    match out.status.code() {
        Some(0) => Ok((stdout, false)),
        Some(3) if partial_ok => Ok((stdout, true)),
        code => Err(failure(code, &stderr)),
    }
}

/// The `summary` line of `backup --json`.
fn parse_summary(stdout: &str, incomplete: bool) -> Result<Added, BackupError> {
    #[derive(Deserialize)]
    struct Summary {
        snapshot_id: Option<String>,
        #[serde(default)]
        data_added: u64,
        #[serde(default)]
        total_files_processed: u64,
    }
    let summary = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["message_type"] == "summary")
        .ok_or_else(|| BackupError::Engine("backup gave no summary".into()))?;
    let s: Summary =
        serde_json::from_value(summary).map_err(|e| BackupError::Engine(e.to_string()))?;
    Ok(Added {
        snapshot: s.snapshot_id,
        bytes_added: s.data_added,
        files: s.total_files_processed,
        incomplete,
    })
}

/// `snapshots --json`: a JSON array.
pub fn parse_snapshots(stdout: &str) -> Result<Vec<Snapshot>, BackupError> {
    #[derive(Deserialize)]
    struct Raw {
        id: String,
        time: String,
        #[serde(default)]
        tags: Vec<String>,
        #[serde(default)]
        paths: Vec<String>,
    }
    let raw: Vec<Raw> = serde_json::from_str(stdout.trim())
        .map_err(|e| BackupError::Engine(format!("snapshot list: {e}")))?;
    let mut out: Vec<Snapshot> = raw
        .into_iter()
        .map(|r| {
            let time = r
                .time
                .parse::<Timestamp>()
                .map(|t| t.as_second())
                .map_err(|e| BackupError::Engine(format!("snapshot time `{}`: {e}", r.time)))?;
            Ok(Snapshot {
                id: r.id,
                time,
                tags: r.tags,
                paths: r.paths,
            })
        })
        .collect::<Result<_, BackupError>>()?;
    out.sort_by_key(|s| s.time);
    Ok(out)
}

/// A snapshot id as restic prints them (hex).
fn snapshot_id(id: &str) -> Result<&str, BackupError> {
    if !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(id)
    } else {
        Err(BackupError::Invalid(format!("`{id}` is not a snapshot id")))
    }
}

impl BackupEngine for Restic {
    fn init(&self, repo: &Repo) -> Result<String, BackupError> {
        let (out, _) = run(self.command(repo, &["init"]), false)?;
        out.lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|v| v["message_type"] == "initialized")
            .and_then(|v| v["id"].as_str().map(str::to_string))
            .ok_or_else(|| BackupError::Engine("init gave no repository id".into()))
    }

    fn repo_id(&self, repo: &Repo) -> Result<String, BackupError> {
        let (out, _) = run(self.command(repo, &["cat", "config"]), false)?;
        let v: serde_json::Value = serde_json::from_str(out.trim())
            .map_err(|e| BackupError::Engine(format!("repository config: {e}")))?;
        v["id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| BackupError::Engine("the repository has no id".into()))
    }

    fn backup_paths(
        &self,
        repo: &Repo,
        source: &Source<'_>,
        paths: &[PathBuf],
        excludes: &[PathBuf],
    ) -> Result<Added, BackupError> {
        if paths.is_empty() {
            return Err(BackupError::Invalid("nothing to back up".into()));
        }
        let mut cmd = self.backup_command_line(repo, source);
        for e in excludes {
            cmd.arg("--exclude").arg(e);
        }
        cmd.arg("--").args(paths);
        let (out, incomplete) = run(cmd, true)?;
        parse_summary(&out, incomplete)
    }

    fn backup_command(
        &self,
        repo: &Repo,
        source: &Source<'_>,
        file: &str,
        argv: &[String],
    ) -> Result<Added, BackupError> {
        if argv.is_empty() || !melissa_appspec::backup::file_name(file) {
            return Err(BackupError::Invalid("bad dump command or file name".into()));
        }
        let mut cmd = self.backup_command_line(repo, source);
        cmd.args(["--stdin-filename", file, "--stdin-from-command", "--"])
            .args(argv);
        let (out, _) = run(cmd, false)?;
        parse_summary(&out, false)
    }

    fn snapshots(&self, repo: &Repo, tags: &[String]) -> Result<Vec<Snapshot>, BackupError> {
        let mut cmd = self.command(repo, &["snapshots", "--host", &self.host]);
        if !tags.is_empty() {
            cmd.arg("--tag").arg(tags.join(","));
        }
        let (out, _) = run(cmd, false)?;
        parse_snapshots(&out)
    }

    fn restore(&self, repo: &Repo, snapshot: &str, target: &Path) -> Result<(), BackupError> {
        let mut cmd = self.command(repo, &["restore", snapshot_id(snapshot)?, "--target"]);
        cmd.arg(target);
        run(cmd, false).map(|_| ())
    }

    fn dump_into(
        &self,
        repo: &Repo,
        snapshot: &str,
        file: &str,
        consumer: &mut Command,
    ) -> Result<(), BackupError> {
        if !melissa_appspec::backup::file_name(file) {
            return Err(BackupError::Invalid(format!("bad file name `{file}`")));
        }
        let path = format!("/{file}");
        let mut dump = self
            .command(repo, &["dump", snapshot_id(snapshot)?, &path])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| BackupError::Engine(format!("cannot run restic: {e}")))?;
        let Some(pipe) = dump.stdout.take() else {
            let _ = dump.kill();
            return Err(BackupError::Engine(
                "no output pipe from restic dump".into(),
            ));
        };
        let consumer_status = consumer
            .stdin(Stdio::from(pipe))
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output();
        let mut stderr = String::new();
        if let Some(mut e) = dump.stderr.take() {
            let _ = e.read_to_string(&mut stderr);
        }
        let dump_status = dump
            .wait()
            .map_err(|e| BackupError::Engine(format!("waiting for restic: {e}")))?;
        let consumer = consumer_status
            .map_err(|e| BackupError::Failed(format!("cannot run the restore command: {e}")))?;
        if !dump_status.success() {
            return Err(failure(dump_status.code(), &stderr));
        }
        if !consumer.status.success() {
            let err = String::from_utf8_lossy(&consumer.stderr);
            let tail: String = err
                .chars()
                .rev()
                .take(300)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            return Err(BackupError::Failed(format!(
                "the restore command failed ({}): {}",
                consumer.status,
                tail.trim()
            )));
        }
        Ok(())
    }

    fn forget(&self, repo: &Repo, retention: &Retention) -> Result<usize, BackupError> {
        retention.validate()?;
        let mut cmd = self.command(
            repo,
            &[
                "forget",
                "--prune",
                "--host",
                &self.host,
                "--group-by",
                "host,tags",
            ],
        );
        cmd.args(retention.forget_args());
        let (out, _) = run(cmd, false)?;
        // The first line is the forget groups (a JSON array); prune's lines follow.
        let removed = out
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find_map(|v| v.as_array().cloned())
            .map(|groups| {
                groups
                    .iter()
                    .map(|g| g["remove"].as_array().map_or(0, Vec::len))
                    .sum()
            })
            .unwrap_or(0);
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_utc_seconds() {
        assert_eq!(restic_time(0), "1970-01-01 00:00:00");
        assert_eq!(restic_time(1_795_835_400), "2026-11-28 03:10:00");
    }

    #[test]
    fn errors_map_restic_exit_codes() {
        let stderr =
            "something\n{\"message_type\":\"exit_error\",\"code\":1,\"message\":\"Fatal: boom\"}\n";
        assert!(matches!(
            failure(Some(10), stderr),
            BackupError::NoRepository
        ));
        assert!(matches!(failure(Some(11), stderr), BackupError::Locked));
        assert!(matches!(failure(Some(12), stderr), BackupError::WrongKey));
        let e = failure(Some(1), stderr).to_string();
        assert!(e.contains("Fatal: boom") && e.contains("exit 1"), "{e}");
        let e = failure(None, "plain text\n").to_string();
        assert!(e.contains("plain text") && e.contains("signal"), "{e}");
    }

    #[test]
    fn summaries_and_snapshot_lists_parse() {
        let out = "{\"message_type\":\"status\"}\n{\"message_type\":\"summary\",\"data_added\":42,\"total_files_processed\":3,\"snapshot_id\":\"abcd\"}\n";
        let a = parse_summary(out, false).unwrap();
        assert_eq!(a.snapshot.as_deref(), Some("abcd"));
        assert_eq!((a.bytes_added, a.files), (42, 3));
        assert!(parse_summary("{}", false).is_err());

        let list = r#"[{"time":"2026-11-29T03:00:00Z","id":"bb","tags":["app:a","part:files"],"paths":["/x"]},
                      {"time":"2026-11-22T03:00:00.000000001+00:00","id":"aa"}]"#;
        let s = parse_snapshots(list).unwrap();
        assert_eq!(s[0].id, "aa");
        assert_eq!(s[1].time, 1_795_921_200);
        assert!(s[1].has_tags(&["app:a".into()]));
        assert!(!s[0].has_tags(&["app:a".into()]));
    }

    #[test]
    fn snapshot_ids_are_checked_before_they_reach_restic() {
        assert!(snapshot_id("abc123").is_ok());
        assert!(snapshot_id("--target=/").is_err());
        assert!(snapshot_id("").is_err());
    }
}
