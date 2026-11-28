//! The drive's marker: `melissa-backup.json` next to the restic repository (`repo/`) in the
//! target directory. Nothing is written to a target unless its marker names the repository
//! this device expects ([`check_target`]), so an unplugged drive's empty mountpoint (or a
//! different drive mounted there) is never written to. A new repository is only made in an
//! empty directory ([`check_empty`]).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{fsutil, io, BackupError};

pub const MARKER_FILE: &str = "melissa-backup.json";
/// The restic repository inside the target.
pub const REPO_DIR: &str = "repo";
pub const MARKER_FORMAT: u32 = 1;
/// Entries a fresh file system may have (ext4's).
const FRESH_FS: &[&str] = &["lost+found"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub format: u32,
    /// restic's repository id.
    pub repo_id: String,
    /// Unix seconds.
    pub created_at: i64,
    pub encrypted: bool,
    /// What this is, for a person who finds the drive.
    #[serde(default)]
    pub about: String,
}

impl Marker {
    pub fn new(repo_id: &str, encrypted: bool) -> Self {
        Self {
            format: MARKER_FORMAT,
            repo_id: repo_id.into(),
            created_at: crate::now(),
            encrypted,
            about: "Melissa backups (restic repository in repo/). Encrypted backups need the \
                    recovery key."
                .into(),
        }
    }
}

pub fn repo_path(target: &Path) -> PathBuf {
    target.join(REPO_DIR)
}

/// The target is a real directory (not a symlink), or why not.
fn real_dir(target: &Path) -> Result<(), BackupError> {
    match target.symlink_metadata() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(BackupError::DriveMissing(
            format!("{} does not exist", target.display()),
        )),
        Err(e) => Err(io(format!("reading {}", target.display()))(e)),
        Ok(m) if m.file_type().is_symlink() => Err(BackupError::WrongRepository(format!(
            "{} is a symlink",
            target.display()
        ))),
        Ok(m) if !m.is_dir() => Err(BackupError::WrongRepository(format!(
            "{} is not a directory",
            target.display()
        ))),
        Ok(_) => Ok(()),
    }
}

/// Read the marker in `target`: `None` if there is none.
pub fn read(target: &Path) -> Result<Option<Marker>, BackupError> {
    real_dir(target)?;
    let path = target.join(MARKER_FILE);
    let Some(text) = fsutil::read_small(&path, 4096)? else {
        return Ok(None);
    };
    let m: Marker = serde_json::from_str(&text).map_err(|e| {
        BackupError::WrongRepository(format!("{} is not readable: {e}", path.display()))
    })?;
    if m.format > MARKER_FORMAT {
        return Err(BackupError::WrongRepository(format!(
            "made by a newer Melissa (marker format {})",
            m.format
        )));
    }
    Ok(Some(m))
}

/// Before every write: `target` holds the repository `expected` (marker and restic's
/// `config` file present). Returns the marker.
pub fn check_target(target: &Path, expected: &str) -> Result<Marker, BackupError> {
    let Some(m) = read(target)? else {
        return Err(BackupError::DriveMissing(format!(
            "no Melissa backups in {}",
            target.display()
        )));
    };
    if m.repo_id != expected {
        return Err(BackupError::WrongRepository(format!(
            "{} holds other backups (repository {}, expected {})",
            target.display(),
            short(&m.repo_id),
            short(expected)
        )));
    }
    if !repo_path(target).join("config").is_file() {
        return Err(BackupError::DriveMissing(format!(
            "{} has a marker but no repository",
            target.display()
        )));
    }
    Ok(m)
}

fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// Before making a repository: `target` is a real, empty directory (a fresh file system's
/// `lost+found` aside).
pub fn check_empty(target: &Path) -> Result<(), BackupError> {
    real_dir(target)?;
    let entries = std::fs::read_dir(target).map_err(io(format!("reading {}", target.display())))?;
    for e in entries {
        let e = e.map_err(io(format!("reading {}", target.display())))?;
        let name = e.file_name();
        if !FRESH_FS.iter().any(|f| name == *f) {
            return Err(BackupError::Invalid(format!(
                "{} is not empty ({} is there); pick an empty drive or folder",
                target.display(),
                name.to_string_lossy()
            )));
        }
    }
    Ok(())
}

/// Write the marker (0644: anyone who finds the drive may read what it is).
pub fn write(target: &Path, marker: &Marker) -> Result<(), BackupError> {
    let text = serde_json::to_vec_pretty(marker).map_err(|e| BackupError::Failed(e.to_string()))?;
    fsutil::write_atomic(&target.join(MARKER_FILE), &text, 0o644, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::scratch;

    fn repo(target: &Path, id: &str) {
        std::fs::create_dir_all(repo_path(target)).unwrap();
        std::fs::write(repo_path(target).join("config"), "x").unwrap();
        write(target, &Marker::new(id, true)).unwrap();
    }

    #[test]
    fn only_the_expected_repository_passes() {
        let d = scratch("marker");
        let t = d.join("usb1");
        // Missing mountpoint, empty mountpoint: the drive is not there.
        assert!(matches!(
            check_target(&t, "abc"),
            Err(BackupError::DriveMissing(_))
        ));
        std::fs::create_dir(&t).unwrap();
        assert!(matches!(
            check_target(&t, "abc"),
            Err(BackupError::DriveMissing(_))
        ));
        repo(&t, "abc");
        assert_eq!(check_target(&t, "abc").unwrap().repo_id, "abc");
        // Another drive in the same place.
        assert!(matches!(
            check_target(&t, "zzz"),
            Err(BackupError::WrongRepository(_))
        ));
        // A marker without the repository.
        std::fs::remove_dir_all(repo_path(&t)).unwrap();
        assert!(matches!(
            check_target(&t, "abc"),
            Err(BackupError::DriveMissing(_))
        ));
    }

    #[test]
    fn symlinks_garbage_and_newer_markers_are_refused() {
        let d = scratch("marker-bad");
        let real = d.join("real");
        std::fs::create_dir(&real).unwrap();
        repo(&real, "abc");
        std::os::unix::fs::symlink(&real, d.join("link")).unwrap();
        assert!(matches!(
            check_target(&d.join("link"), "abc"),
            Err(BackupError::WrongRepository(_))
        ));
        std::fs::write(real.join(MARKER_FILE), "not json").unwrap();
        assert!(matches!(
            check_target(&real, "abc"),
            Err(BackupError::WrongRepository(_))
        ));
        let mut m = Marker::new("abc", true);
        m.format = 9;
        write(&real, &m).unwrap();
        assert!(read(&real).is_err());
    }

    #[test]
    fn new_repositories_need_an_empty_directory() {
        let d = scratch("marker-empty");
        assert!(check_empty(&d.join("none")).is_err());
        let t = d.join("usb1");
        std::fs::create_dir_all(t.join("lost+found")).unwrap();
        check_empty(&t).unwrap();
        std::fs::write(t.join("holiday.jpg"), "x").unwrap();
        assert!(check_empty(&t)
            .unwrap_err()
            .to_string()
            .contains("not empty"));
    }
}
