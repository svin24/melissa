//! What the server reads: `status.json` in [`crate::STATUS_DIR`] (root writes it, 0640
//! root:melissa, versioned), and the recovery key file while the owner hasn't confirmed it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    config::{Config, Retention, Schedule},
    fsutil,
    key::RecoveryKey,
    BackupError,
};

pub const STATUS_VERSION: u32 = 1;
pub const STATUS_FILE: &str = "status.json";
/// The new recovery key, until the owner confirms they keep it (`ack-key`).
pub const KEY_FILE: &str = "recovery-key.txt";
const MAX_STATUS_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub version: u32,
    /// The settings, as root has them.
    pub target: Option<PathBuf>,
    pub encrypted: bool,
    /// A repository is set up at the target.
    pub ready: bool,
    pub repo_id: Option<String>,
    pub schedule: Option<Schedule>,
    pub retention: Option<Retention>,
    /// Unix seconds of the next scheduled run.
    pub next_due: Option<i64>,
    /// Something runs now.
    pub running: Option<Running>,
    pub last_backup: Option<Report>,
    /// When the last backup finished with every item backed up.
    pub last_ok: Option<i64>,
    pub last_restore: Option<Report>,
    /// A new recovery key waits for the owner (shown once, then confirmed).
    pub recovery_key_pending: bool,
    /// The last request's problem (a refused setting, a drive that wasn't empty).
    pub problem: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Running {
    /// `backup`, `restore <app>`, `init`.
    pub action: String,
    pub started: i64,
    /// What it does now (`app nextcloud`).
    pub step: String,
}

/// One backup or restore run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// `scheduled`, `owner` (asked), `host` (by hand).
    pub trigger: String,
    pub started: i64,
    pub finished: i64,
    pub ok: bool,
    /// Why the whole run failed (drive missing, wrong key); item problems are in `items`.
    pub error: Option<String>,
    pub items: Vec<ItemResult>,
}

/// One item of a run: `state`, `k3s`, `app:<name>`, `retention`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemResult {
    pub item: String,
    pub ok: bool,
    /// For apps: `dump`, `stop`, `live`, `skip`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub snapshots: Vec<String>,
    #[serde(default)]
    pub bytes_added: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl ItemResult {
    pub fn ok(item: impl Into<String>) -> Self {
        Self {
            item: item.into(),
            ok: true,
            ..Self::default()
        }
    }

    pub fn failed(item: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            item: item.into(),
            ok: false,
            message: Some(message.into()),
            ..Self::default()
        }
    }
}

impl Status {
    /// The settings part from `config`.
    pub fn with_config(mut self, config: &Config) -> Self {
        self.version = STATUS_VERSION;
        self.target = config.target.clone();
        self.encrypted = config.encrypted;
        self.ready = config.target.is_some() && config.repo_id.is_some();
        self.repo_id = config.repo_id.clone();
        self.schedule = Some(config.schedule);
        self.retention = Some(config.retention);
        self
    }

    /// Read `status.json` in `dir`; the default when there is none.
    pub fn load(dir: &Path) -> Result<Self, BackupError> {
        let Some(text) = fsutil::read_small(&dir.join(STATUS_FILE), MAX_STATUS_BYTES)? else {
            return Ok(Self::default());
        };
        let s: Self = serde_json::from_str(&text)
            .map_err(|e| BackupError::Invalid(format!("backup status: {e}")))?;
        if s.version > STATUS_VERSION {
            return Err(BackupError::Invalid(format!(
                "backup status written by a newer Melissa (format {})",
                s.version
            )));
        }
        Ok(s)
    }

    /// Write atomically, readable by the directory's group (the server).
    pub fn save(&self, dir: &Path) -> Result<(), BackupError> {
        let mut s = self.clone();
        s.version = STATUS_VERSION;
        let text = serde_json::to_vec_pretty(&s).map_err(|e| BackupError::Failed(e.to_string()))?;
        fsutil::write_atomic(&dir.join(STATUS_FILE), &text, 0o640, true)
    }
}

/// Hand a new recovery key to the server (shown to the owner once).
pub fn offer_key(dir: &Path, key: &RecoveryKey) -> Result<(), BackupError> {
    fsutil::write_atomic(&dir.join(KEY_FILE), key.expose().as_bytes(), 0o640, true)
}

/// The key waiting for the owner, if any (the server's side).
pub fn pending_key(dir: &Path) -> Result<Option<RecoveryKey>, BackupError> {
    fsutil::read_small(&dir.join(KEY_FILE), 256)?
        .map(|t| RecoveryKey::parse(t.trim()))
        .transpose()
}

/// The owner has the key: the server's readable copy goes.
pub fn withdraw_key(dir: &Path) -> Result<(), BackupError> {
    fsutil::remove_any(&dir.join(KEY_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::scratch;

    #[test]
    fn status_round_trips_and_refuses_newer() {
        let d = scratch("status");
        assert_eq!(Status::load(&d).unwrap(), Status::default());
        let c = Config {
            target: Some("/run/melissa-backup/a".into()),
            repo_id: Some("r".into()),
            ..Config::default()
        };
        let mut s = Status::default().with_config(&c);
        s.last_backup = Some(Report {
            trigger: "scheduled".into(),
            ok: false,
            items: vec![ItemResult::ok("state"), ItemResult::failed("app:x", "boom")],
            ..Report::default()
        });
        s.save(&d).unwrap();
        let back = Status::load(&d).unwrap();
        assert!(back.ready);
        assert_eq!(back, s);
        std::fs::write(d.join(STATUS_FILE), r#"{"version": 9}"#).unwrap();
        assert!(Status::load(&d).is_err());
    }

    #[test]
    fn the_key_is_offered_once_and_withdrawn() {
        let d = scratch("status-key");
        assert_eq!(pending_key(&d).unwrap(), None);
        let k = RecoveryKey::from_entropy(&[7; 20]);
        offer_key(&d, &k).unwrap();
        assert_eq!(pending_key(&d).unwrap(), Some(k));
        withdraw_key(&d).unwrap();
        assert_eq!(pending_key(&d).unwrap(), None);
    }
}
