#![forbid(unsafe_code)]
//! Melissa's backups (docs/design/BACKUPS-2026-11-28.md): Melissa's own state, a raw snapshot of
//! k3s, and every app's data, to a drive the owner picks, weekly by default.
//!
//! Who does what (the privilege split, [`request`]):
//! - The root unit `melissa-backup.service` (container image) runs everything here: it reads
//!   Melissa's state, k3s's datastore, every app's volumes and Secrets, and writes the drive.
//!   It is started by a path unit when the server leaves a request in [`REQUEST_DIR`], and by an
//!   hourly timer that runs the backup when the schedule says so.
//! - The server (user `melissa`) only creates fixed-name request files ([`request::Request`];
//!   one JSON file for settings, checked by root) and reads [`status::Status`] from
//!   [`STATUS_DIR`]. No agent tool reaches either, and nothing that reads a Secret value runs
//!   in the server.
//!
//! The engine is restic ([`engine::Restic`], a pinned static binary in the image, driven with
//! `--json`) behind [`engine::BackupEngine`], so rustic_core can replace it later on the same
//! repository format. Before anything writes to the target, [`marker::check_target`] checks
//! that it is the repository this device made (marker file + repository id): an empty
//! mountpoint of an unplugged drive is never written to.
//!
//! What is backed up (one restic snapshot per item, all of a run at the same time):
//! - `item:state`: Melissa's state ([`inventory`], the factory reset's list minus excludes);
//! - `item:k3s`: k3s's datastore (sqlite `VACUUM INTO`), token, TLS and secrets-encryption config,
//!   for a same-version rollback ([`k3s`]);
//! - `app:<name>`: per app its spec, labels, Secret values and volume data, made consistent by
//!   the spec's `backup` hook (dump, stop, live or skip; [`apps`]).

use std::path::{Path, PathBuf};

pub mod apps;
pub mod config;
pub mod engine;
mod fsutil;
pub mod inventory;
pub mod k3s;
pub mod key;
pub mod marker;
pub mod request;
pub mod run;
pub mod status;

/// Root's backup settings, the device's copy of the recovery key and the staging area
/// (root 0700, on the /var volume).
pub const CONFIG_DIR: &str = "/var/lib/melissa-container/backup";
/// What the server reads: `status.json`, and the recovery key until the owner has seen it
/// (root:melissa 0750, files 0640).
pub const STATUS_DIR: &str = "/var/lib/melissa-container/backup-status";
/// Where the server leaves requests (root:melissa 0770).
pub const REQUEST_DIR: &str = "/var/lib/melissa-container/backup-request";
/// Where the drive helper mounts backup drives, one directory per drive. A target the server
/// asks for must be below it; root may pick any directory by hand.
pub const DRIVES_DIR: &str = "/run/melissa-backup";
/// The root unit.
pub const UNIT: &str = "melissa-backup.service";
/// The pinned restic (packaging/container/restic.lock).
pub const RESTIC_BIN: &str = "/usr/bin/restic";
/// The sqlite3 CLI for k3s's datastore copy.
pub const SQLITE_BIN: &str = "/usr/bin/sqlite3";
/// kubectl (k3s's multi-call binary) and k3s's admin kubeconfig (root only).
pub const KUBECTL_BIN: &str = "/usr/bin/kubectl";
pub const ADMIN_KUBECONFIG: &str = "/etc/rancher/k3s/k3s.yaml";
/// restic's `--host` for every snapshot: the same on every device, so a restored device sees
/// the old one's snapshots as its own (and no real host name lands on the drive).
pub const SNAPSHOT_HOST: &str = "melissa";

/// `abs` (one of the constants) under `root` (`/` on the device, a temporary tree in tests).
pub fn at(root: &Path, abs: impl AsRef<Path>) -> PathBuf {
    let abs = abs.as_ref();
    root.join(abs.strip_prefix("/").unwrap_or(abs))
}

/// Errors. Messages are for the owner (status page, journal) and never hold a secret.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("{what}: {source}")]
    Io {
        what: String,
        #[source]
        source: std::io::Error,
    },
    #[error("no backup drive is set up")]
    NotConfigured,
    #[error("the backup drive is not set up for backups yet")]
    NoRepository,
    /// The target is missing, empty or has no marker (an unplugged drive's mountpoint).
    #[error("the backup drive is not connected: {0}")]
    DriveMissing(String),
    /// A marker that isn't this device's repository, or a repository that doesn't match it.
    #[error("this is not the backup drive Melissa set up: {0}")]
    WrongRepository(String),
    #[error("the recovery key does not open these backups")]
    WrongKey,
    #[error("the backups are in use by another run (locked)")]
    Locked,
    #[error("another backup or restore is running")]
    Busy,
    #[error("backup tool: {0}")]
    Engine(String),
    #[error("cluster: {0}")]
    Cluster(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Failed(String),
}

pub(crate) fn io(what: impl Into<String>) -> impl FnOnce(std::io::Error) -> BackupError {
    let what = what.into();
    move |source| BackupError::Io { what, source }
}

/// Unix seconds now.
pub fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU32, Ordering},
    };

    /// A fresh, empty directory under the system temp dir.
    pub fn scratch(name: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "melissa-backup-test-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }
}
