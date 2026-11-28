//! A raw snapshot of k3s's server state, for a same-version rollback of the whole cluster
//! (PLAN Q23): the datastore (`server/db/state.db`, copied consistently with sqlite's
//! `VACUUM INTO` while k3s runs), the join token, the cluster's TLS keys, and `server/cred`
//! (with `encryption-config.json`, the key of Secrets encrypted at rest). Without that last one
//! a restored datastore's Secrets can't be read.
//!
//! It is not how single apps come back (that is the per-app export, [`crate::apps`]): putting a
//! datastore back replaces every app's objects and the cluster's identity, and only works with
//! the same k3s version. [`stage`] records the version next to the copy and [`place`] refuses
//! another one.

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use crate::{fsutil, io, BackupError};

/// k3s's server directory.
pub const SERVER_DIR: &str = "/var/lib/rancher/k3s/server";
/// The datastore, relative to [`SERVER_DIR`].
pub const DB: &str = "db/state.db";
/// The datastore's directory (with its `-wal`/`-shm` while k3s runs), swapped as a whole.
pub const DB_DIR: &str = "db";
/// Copied as they are (relative to [`SERVER_DIR`]); `cred` and `tls` are directories.
pub const FILES: &[&str] = &["token", "tls", "cred"];
/// The k3s version this image ships (`K3S_VERSION=`).
pub const K3S_LOCK: &str = "/usr/lib/melissa/k3s.lock";
/// Written next to the copy.
pub const VERSION_FILE: &str = "k3s-version";

/// The `K3S_VERSION=` line of a k3s.lock.
pub fn lock_version(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix("K3S_VERSION="))
        .map(|v| v.trim().trim_matches('"').to_string())
        .filter(|v| !v.is_empty())
}

/// Is there a k3s server datastore here (an agent device has none)?
pub fn present(server: &Path) -> bool {
    server.join(DB).is_file()
}

/// Copy k3s's server state from `server` into a fresh `staging` (root 0700) with the sqlite3
/// CLI at `sqlite`. `version`: the running k3s's version, recorded with the copy.
pub fn stage(
    server: &Path,
    staging: &Path,
    sqlite: &Path,
    version: Option<&str>,
) -> Result<(), BackupError> {
    if !present(server) {
        return Err(BackupError::Invalid(format!(
            "no k3s datastore at {}",
            server.join(DB).display()
        )));
    }
    fsutil::fresh_dir(staging)?;
    let db_out = staging.join(DB);
    fsutil::ensure_dir(db_out.parent().unwrap_or(staging), 0o700)?;
    vacuum_into(sqlite, &server.join(DB), &db_out)?;
    for f in FILES {
        let src = server.join(f);
        if src.symlink_metadata().is_ok() {
            fsutil::copy_tree(&src, &staging.join(f))?;
        }
    }
    if let Some(v) = version {
        std::fs::write(staging.join(VERSION_FILE), format!("{v}\n"))
            .map_err(io("writing the k3s version"))?;
    }
    Ok(())
}

/// `VACUUM INTO`: a consistent, compact copy of a live SQLite database (WAL included), made
/// through SQLite itself rather than by copying its files.
fn vacuum_into(sqlite: &Path, db: &Path, out: &Path) -> Result<(), BackupError> {
    let quoted = out.to_string_lossy().replace('\'', "''");
    let result = Command::new(sqlite)
        .env_clear()
        .args(["-bail", "-cmd", ".timeout 30000"])
        .arg(db)
        .arg(format!("VACUUM INTO '{quoted}'"))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| BackupError::Failed(format!("cannot run {}: {e}", sqlite.display())))?;
    if !result.status.success() || !out.is_file() {
        return Err(BackupError::Failed(format!(
            "copying k3s's datastore failed ({}): {}",
            result.status,
            String::from_utf8_lossy(&result.stderr).trim()
        )));
    }
    Ok(())
}

/// Put a restored copy (`staged`, as [`stage`] made it) in place of `server`'s state, for a
/// same-version rollback. k3s must be stopped. What was there is kept as a safety copy
/// (`*.pre-restore-<stamp>`). The whole `db/` directory is swapped, so the live datastore's
/// write-ahead log (`-wal`, `-shm`) goes into the safety copy with it, never away.
/// `version`: this device's k3s version; a copy from another version is refused.
pub fn place(
    staged: &Path,
    server: &Path,
    version: Option<&str>,
    stamp: &str,
) -> Result<Vec<PathBuf>, BackupError> {
    let recorded = fsutil::read_small(&staged.join(VERSION_FILE), 256)
        .ok()
        .flatten()
        .map(|v| v.trim().to_string());
    match (recorded.as_deref(), version) {
        (Some(a), Some(b)) if a == b => {}
        (a, b) => {
            return Err(BackupError::Invalid(format!(
                "the k3s snapshot is from k3s {}, this device runs {}: a raw rollback needs the \
                 same version",
                a.unwrap_or("?"),
                b.unwrap_or("?")
            )))
        }
    }
    let db = staged.join(DB);
    if db.symlink_metadata().map(|m| m.is_file()).unwrap_or(false) {
        fsutil::inside(staged, &db)?;
    } else {
        return Err(BackupError::Invalid("the snapshot has no datastore".into()));
    }
    // Everything checked before anything moves.
    let mut parts = Vec::new();
    for rel in std::iter::once(DB_DIR).chain(FILES.iter().copied()) {
        let src = staged.join(rel);
        if src.symlink_metadata().is_err() {
            continue;
        }
        fsutil::inside(staged, &src)?;
        parts.push((src, server.join(rel)));
    }
    let mut placed = Vec::new();
    for (src, dst) in parts {
        fsutil::swap_in(&src, &dst, &[], stamp)?;
        placed.push(dst);
    }
    Ok(placed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::scratch;
    use std::os::unix::fs::PermissionsExt;

    /// A stand-in for sqlite3: copies the database to the path in `VACUUM INTO '<path>'`.
    fn fake_sqlite(dir: &Path) -> PathBuf {
        let p = dir.join("sqlite3");
        std::fs::write(
            &p,
            "#!/bin/sh\n# args: -bail -cmd .timeout db sql\nout=$(printf '%s' \"$5\" | sed \"s/^VACUUM INTO '\\(.*\\)'$/\\1/\")\nexec cp \"$4\" \"$out\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn server(root: &Path, db: &str) -> PathBuf {
        let s = root.join("server");
        std::fs::create_dir_all(s.join("db")).unwrap();
        std::fs::create_dir_all(s.join("tls/etcd")).unwrap();
        std::fs::create_dir_all(s.join("cred")).unwrap();
        std::fs::write(s.join(DB), db).unwrap();
        std::fs::write(s.join("db/state.db-wal"), "wal").unwrap();
        std::fs::write(s.join("token"), "K10::server:x").unwrap();
        std::fs::write(s.join("tls/server-ca.key"), "ca").unwrap();
        std::fs::write(s.join("cred/encryption-config.json"), "{}").unwrap();
        s
    }

    #[test]
    fn the_lock_names_the_version() {
        assert_eq!(
            lock_version("# x\nK3S_VERSION=v1.37.2+k3s1\nK3S_SHA256=a\n").as_deref(),
            Some("v1.37.2+k3s1")
        );
        assert_eq!(lock_version("nothing"), None);
    }

    #[test]
    fn a_snapshot_is_staged_and_rolled_back_with_the_same_version_only() {
        let d = scratch("k3s");
        let s = server(&d, "datastore-1");
        let staging = d.join("staging");
        stage(&s, &staging, &fake_sqlite(&d), Some("v1")).unwrap();
        assert_eq!(
            std::fs::read_to_string(staging.join(DB)).unwrap(),
            "datastore-1"
        );
        assert!(staging.join("cred/encryption-config.json").is_file());
        assert!(staging.join("tls/server-ca.key").is_file());
        assert!(!staging.join("db/state.db-wal").exists());

        std::fs::write(s.join(DB), "datastore-2").unwrap();
        let st = "20261128T031000Z";
        assert!(place(&staging, &s, Some("v2"), st).is_err());
        assert!(place(&staging, &s, None, st).is_err());
        let placed = place(&staging, &s, Some("v1"), st).unwrap();
        assert_eq!(placed.len(), 4);
        assert_eq!(std::fs::read_to_string(s.join(DB)).unwrap(), "datastore-1");
        assert!(!s.join("db/state.db-wal").exists());
        // The live datastore and its write-ahead log are kept together.
        let pre = s.join(format!("db.pre-restore-{st}"));
        assert_eq!(
            std::fs::read_to_string(pre.join("state.db")).unwrap(),
            "datastore-2"
        );
        assert_eq!(
            std::fs::read_to_string(pre.join("state.db-wal")).unwrap(),
            "wal"
        );
    }

    #[test]
    fn a_tampered_snapshot_cannot_point_outside() {
        let d = scratch("k3s-evil");
        let s = server(&d, "live");
        let staged = d.join("staged");
        std::fs::create_dir_all(staged.join("db")).unwrap();
        std::fs::write(staged.join(DB), "x").unwrap();
        std::fs::write(staged.join(VERSION_FILE), "v1\n").unwrap();
        std::fs::create_dir_all(d.join("secret-elsewhere")).unwrap();
        std::os::unix::fs::symlink(d.join("secret-elsewhere"), staged.join("tls")).unwrap();
        assert!(place(&staged, &s, Some("v1"), "20261128T031000Z").is_err());
        // Nothing moved.
        assert_eq!(std::fs::read_to_string(s.join(DB)).unwrap(), "live");
        assert!(s.join("tls/server-ca.key").is_file());
    }

    #[test]
    fn no_datastore_no_snapshot() {
        let d = scratch("k3s-none");
        assert!(!present(&d.join("server")));
        assert!(stage(&d.join("server"), &d.join("st"), &fake_sqlite(&d), None).is_err());
    }

    #[test]
    fn a_failing_sqlite_is_an_error() {
        let d = scratch("k3s-fail");
        let s = server(&d, "x");
        assert!(stage(&s, &d.join("st"), Path::new("/bin/false"), None).is_err());
    }
}
