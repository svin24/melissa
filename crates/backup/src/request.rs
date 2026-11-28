//! The server's requests to the root unit, like the updater's: empty files with fixed names in
//! [`crate::REQUEST_DIR`] (root:melissa 0770), plus one JSON file for settings that root
//! checks itself ([`crate::config::ConfigRequest`]). Root acts on the names it knows and
//! removes every file it finds; it never takes a path or a command from a request (a target
//! must be below [`crate::DRIVES_DIR`]).
//!
//! | file | does |
//! |---|---|
//! | `config.json` | change target, encryption, schedule or retention |
//! | `init` | set up the target: a new repository in an empty directory (and a recovery key) |
//! | `ack-key` | the owner has the recovery key: the server's copy goes |
//! | `run` | back up now |
//! | `restore-<app>` / `restore-<app>@<unix time>` | restore one app (latest / that run) |

use std::path::{Path, PathBuf};

use melissa_agent::{AuditLog, AuditRecord};
use melissa_appspec::valid_app_name;

use crate::{
    config::{ConfigRequest, MAX_CONFIG_BYTES},
    fsutil, io,
    status::Status,
    BackupError,
};

pub const CONFIG_REQUEST: &str = "config.json";

/// One request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Config(ConfigRequest),
    Init,
    AckKey,
    RestoreApp { app: String, at: Option<i64> },
    Run,
}

impl Request {
    /// The file name that carries it.
    pub fn file_name(&self) -> String {
        match self {
            Self::Config(_) => CONFIG_REQUEST.into(),
            Self::Init => "init".into(),
            Self::AckKey => "ack-key".into(),
            Self::Run => "run".into(),
            Self::RestoreApp { app, at: None } => format!("restore-{app}"),
            Self::RestoreApp { app, at: Some(t) } => format!("restore-{app}@{t}"),
        }
    }

    /// A request named `name` (not `config.json`, which has contents).
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "init" => return Some(Self::Init),
            "ack-key" => return Some(Self::AckKey),
            "run" => return Some(Self::Run),
            _ => {}
        }
        let rest = name.strip_prefix("restore-")?;
        let (app, at) = match rest.split_once('@') {
            Some((app, t)) => {
                // Digits only (`+5` would parse too).
                if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                (app, Some(t.parse::<i64>().ok().filter(|t| *t > 0)?))
            }
            None => (rest, None),
        };
        valid_app_name(app).then(|| Self::RestoreApp {
            app: app.into(),
            at,
        })
    }

    /// Order of handling: settings first, the backup last.
    fn rank(&self) -> u8 {
        match self {
            Self::Config(_) => 0,
            Self::Init => 1,
            Self::AckKey => 2,
            Self::RestoreApp { .. } => 3,
            Self::Run => 4,
        }
    }

    /// For the audit log.
    pub fn describe(&self) -> String {
        match self {
            Self::Config(c) => format!(
                "change backup settings ({})",
                serde_json::to_string(c).unwrap_or_default()
            ),
            Self::Init => "set up the backup drive".into(),
            Self::AckKey => "recovery key kept by the owner".into(),
            Self::Run => "back up now".into(),
            Self::RestoreApp { app, at: None } => format!("restore {app} (latest backup)"),
            Self::RestoreApp { app, at: Some(t) } => format!("restore {app} (backup of {t})"),
        }
    }
}

// --- the server's side ---------------------------------------------------------------------

/// The server's side: leave requests, read the status. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Requester {
    dir: PathBuf,
    status_dir: PathBuf,
    audit: Option<AuditLog>,
}

impl Requester {
    pub fn new(
        dir: impl Into<PathBuf>,
        status_dir: impl Into<PathBuf>,
        audit: Option<AuditLog>,
    ) -> Self {
        Self {
            dir: dir.into(),
            status_dir: status_dir.into(),
            audit,
        }
    }

    /// This install: `Some` only where the root unit is (the container's request directory).
    pub fn system(audit: Option<AuditLog>) -> Option<Self> {
        Path::new(crate::REQUEST_DIR)
            .is_dir()
            .then(|| Self::new(crate::REQUEST_DIR, crate::STATUS_DIR, audit))
    }

    /// Leave `request` (audited `backup.requested`). Asking again while one waits changes
    /// nothing; a settings request replaces a waiting one.
    pub fn ask(&self, request: &Request, actor: &str) -> Result<(), BackupError> {
        let path = self.dir.join(request.file_name());
        match request {
            Request::Config(c) => {
                let text = serde_json::to_vec(c).map_err(|e| BackupError::Failed(e.to_string()))?;
                fsutil::write_atomic(&path, &text, 0o660, false)?;
            }
            _ => {
                use std::os::unix::fs::OpenOptionsExt;
                match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o660)
                    .open(&path)
                {
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(io(format!("writing {}", path.display()))(e)),
                }
            }
        }
        if let Some(log) = &self.audit {
            let record = AuditRecord::now("backup.requested", 0, "", request.describe()).by(actor);
            if let Err(e) = log.append(&record) {
                tracing::error!(path = %log.path().display(), "audit log write failed: {e}");
            }
        }
        Ok(())
    }

    /// Root's latest status.
    pub fn status(&self) -> Result<Status, BackupError> {
        Status::load(&self.status_dir)
    }

    /// The new recovery key, while the owner hasn't confirmed it.
    pub fn pending_key(&self) -> Result<Option<crate::key::RecoveryKey>, BackupError> {
        crate::status::pending_key(&self.status_dir)
    }
}

// --- root's side ---------------------------------------------------------------------------

/// Take every request in `dir`: each file is removed; unknown or unreadable ones become
/// problems. Returned in handling order, without duplicates.
pub fn take(dir: &Path) -> Result<(Vec<Request>, Vec<String>), BackupError> {
    let mut requests = Vec::new();
    let mut problems = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((requests, problems)),
        Err(e) => return Err(io(format!("reading {}", dir.display()))(e)),
    };
    for entry in entries {
        let entry = entry.map_err(io(format!("reading {}", dir.display())))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let meta = match path.symlink_metadata() {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io(format!("reading {}", path.display()))(e)),
        };
        // Requests are regular files: anything else (a directory, symlink, FIFO, socket) goes
        // at once, unread.
        if !meta.file_type().is_file() {
            fsutil::remove_any(&path)?;
            problems.push(format!("removed `{}`: not a request file", short(&name)));
            continue;
        }
        // A settings request being written (the server's temporary file; the path unit
        // ignores dot files). One left behind by a crash, or dated in the future, goes now.
        if name.starts_with('.') {
            let fresh = meta
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age.as_secs() <= TEMP_GRACE_SECS);
            if !fresh {
                fsutil::remove_any(&path)?;
            }
            continue;
        }
        let parsed = if name == CONFIG_REQUEST {
            match fsutil::read_small(&path, MAX_CONFIG_BYTES) {
                Ok(Some(text)) => serde_json::from_str::<ConfigRequest>(&text)
                    .map(Request::Config)
                    .map_err(|e| format!("settings request refused: {e}")),
                Ok(None) => continue,
                Err(e) => Err(format!("settings request refused: {e}")),
            }
        } else {
            Request::from_name(&name).ok_or_else(|| format!("unknown request `{}`", short(&name)))
        };
        fsutil::remove_any(&path)?;
        match parsed {
            Ok(r) if !requests.contains(&r) => requests.push(r),
            Ok(_) => {}
            Err(p) => problems.push(p),
        }
    }
    requests.sort_by_key(Request::rank);
    Ok((requests, problems))
}

/// How long the server's temporary file may sit in the request directory.
const TEMP_GRACE_SECS: u64 = 60;
/// Requests taken while another run held the lock, in root's config directory (outside the
/// watched directory, so they don't start the unit again and again).
pub const HELD_FILE: &str = "held-requests.json";

fn short(name: &str) -> String {
    name.chars()
        .take(80)
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Held {
    /// File names of the requests without contents.
    #[serde(default)]
    names: Vec<String>,
    #[serde(default)]
    config: Option<ConfigRequest>,
}

/// Keep `requests` in `config_dir` for the next run (added to any already held).
pub fn hold(config_dir: &Path, requests: &[Request]) -> Result<(), BackupError> {
    if requests.is_empty() {
        return Ok(());
    }
    let mut held = load_held(config_dir)?;
    for r in requests {
        match r {
            Request::Config(c) => held.config = Some(c.clone()),
            other => {
                let name = other.file_name();
                if !held.names.contains(&name) {
                    held.names.push(name);
                }
            }
        }
    }
    let text = serde_json::to_vec(&held).map_err(|e| BackupError::Failed(e.to_string()))?;
    fsutil::ensure_dir(config_dir, 0o700)?;
    fsutil::write_atomic(&config_dir.join(HELD_FILE), &text, 0o600, false)
}

fn load_held(config_dir: &Path) -> Result<Held, BackupError> {
    match fsutil::read_small(&config_dir.join(HELD_FILE), MAX_CONFIG_BYTES * 4) {
        Ok(Some(text)) => Ok(serde_json::from_str(&text).unwrap_or_default()),
        Ok(None) => Ok(Held::default()),
        Err(e) => Err(e),
    }
}

/// The requests [`hold`] kept (removed from there), checked like new ones.
pub fn take_held(config_dir: &Path) -> Result<Vec<Request>, BackupError> {
    let held = load_held(config_dir)?;
    fsutil::remove_any(&config_dir.join(HELD_FILE))?;
    let mut out: Vec<Request> = held.config.map(Request::Config).into_iter().collect();
    out.extend(held.names.iter().filter_map(|n| Request::from_name(n)));
    Ok(out)
}

/// Requests from both places, without duplicates, in handling order.
pub fn merge(mut a: Vec<Request>, b: Vec<Request>) -> Vec<Request> {
    for r in b {
        // A newer settings request replaces a held one.
        if let Request::Config(_) = r {
            a.retain(|x| !matches!(x, Request::Config(_)));
        }
        if !a.contains(&r) {
            a.push(r);
        }
    }
    a.sort_by_key(Request::rank);
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Schedule, test_support::scratch};
    use std::time::Duration;

    #[test]
    fn names_round_trip_and_nothing_else_parses() {
        for r in [
            Request::Init,
            Request::AckKey,
            Request::Run,
            Request::RestoreApp {
                app: "nextcloud".into(),
                at: None,
            },
            Request::RestoreApp {
                app: "a-b".into(),
                at: Some(1_795_921_200),
            },
        ] {
            assert_eq!(Request::from_name(&r.file_name()), Some(r.clone()), "{r:?}");
        }
        for bad in [
            "",
            "restore-",
            "restore-../etc",
            "restore-App",
            "restore-x@",
            "restore-x@-5",
            "restore-x@+5",
            "restore-x@5@6",
            "runs",
            "config.json",
        ] {
            assert_eq!(Request::from_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn requests_are_taken_once_in_order_and_strangers_are_reported() {
        let d = scratch("requests");
        let req = d.join("req");
        std::fs::create_dir(&req).unwrap();
        let r = Requester::new(&req, d.join("status"), None);
        r.ask(&Request::Run, "owner").unwrap();
        r.ask(&Request::Run, "owner").unwrap();
        r.ask(
            &Request::RestoreApp {
                app: "x".into(),
                at: None,
            },
            "owner",
        )
        .unwrap();
        r.ask(
            &Request::Config(ConfigRequest {
                schedule: Some(Schedule::Off),
                ..Default::default()
            }),
            "owner",
        )
        .unwrap();
        std::fs::write(req.join("rm -rf"), "").unwrap();
        std::fs::write(req.join(".config.json.1.tmp"), "half").unwrap();
        let (taken, problems) = take(&req).unwrap();
        assert!(matches!(taken[0], Request::Config(_)));
        assert!(matches!(taken[1], Request::RestoreApp { .. }));
        assert_eq!(taken[2], Request::Run);
        assert_eq!(taken.len(), 3);
        assert_eq!(problems.len(), 1, "{problems:?}");
        // Everything but the temporary file is gone.
        let left: Vec<_> = std::fs::read_dir(&req).unwrap().flatten().collect();
        assert_eq!(left.len(), 1);
        assert_eq!(take(&d.join("missing")).unwrap().0, vec![]);
    }

    #[test]
    fn nothing_in_the_directory_can_block_root_or_stay_behind() {
        let d = scratch("requests-odd");
        // A FIFO named like a request would block a plain open for reading.
        for name in ["config.json", "run"] {
            let made = std::process::Command::new("mkfifo")
                .arg(d.join(name))
                .status()
                .unwrap();
            assert!(made.success());
        }
        std::fs::create_dir(d.join("init")).unwrap();
        let outside = scratch("requests-odd-outside").join("kept");
        std::fs::write(&outside, "kept").unwrap();
        std::os::unix::fs::symlink(&outside, d.join("ack-key")).unwrap();
        // Temporary files: a fresh one stays (the server is writing it), an old one and one
        // dated in the future go.
        std::fs::write(d.join(".fresh.tmp"), "").unwrap();
        for (name, when) in [
            (
                ".old.tmp",
                std::time::SystemTime::now() - Duration::from_secs(3600),
            ),
            (
                ".future.tmp",
                std::time::SystemTime::now() + Duration::from_secs(86_400 * 365),
            ),
        ] {
            let f = std::fs::File::create(d.join(name)).unwrap();
            f.set_modified(when).unwrap();
        }
        let (taken, problems) = take(&d).unwrap();
        assert!(taken.is_empty(), "{taken:?}");
        assert_eq!(problems.len(), 4, "{problems:?}");
        let left: Vec<String> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, [".fresh.tmp"]);
        // The link went, not what it pointed at.
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "kept");
    }

    #[test]
    fn requests_held_during_a_run_come_back_once() {
        let d = scratch("requests-held");
        let restore = Request::RestoreApp {
            app: "notes".into(),
            at: None,
        };
        hold(&d, &[Request::Run, restore.clone()]).unwrap();
        hold(
            &d,
            &[
                Request::Run,
                Request::Config(ConfigRequest {
                    schedule: Some(Schedule::Off),
                    ..Default::default()
                }),
            ],
        )
        .unwrap();
        let held = take_held(&d).unwrap();
        assert_eq!(held.len(), 3, "{held:?}");
        assert!(take_held(&d).unwrap().is_empty());
        let all = merge(held, vec![Request::Run, Request::AckKey]);
        assert_eq!(all.len(), 4);
        assert!(matches!(all[0], Request::Config(_)));
        assert_eq!(all[1], Request::AckKey);
        assert_eq!(all[2], restore);
        assert_eq!(all[3], Request::Run);
    }

    #[test]
    fn a_bad_settings_request_is_a_problem_not_a_crash() {
        let d = scratch("requests-bad");
        std::fs::write(
            d.join(CONFIG_REQUEST),
            r#"{"target": "/x", "command": "sh"}"#,
        )
        .unwrap();
        let (taken, problems) = take(&d).unwrap();
        assert!(taken.is_empty());
        assert!(problems[0].contains("refused"), "{problems:?}");
        assert!(!d.join(CONFIG_REQUEST).exists());
    }
}
