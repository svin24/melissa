//! Small file helpers: atomic writes, bounded reads that never follow a symlink, tree copies
//! and the swap that puts a restored directory in place.

use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use crate::{io, BackupError};

fn nofollow() -> i32 {
    rustix::fs::OFlags::NOFOLLOW.bits() as i32
}

/// Opening a FIFO for reading blocks until a writer comes; this makes it return at once (the
/// file type is checked right after).
fn nonblock() -> i32 {
    rustix::fs::OFlags::NONBLOCK.bits() as i32
}

/// How long a restore's safety copy of the replaced data is kept (owner decision 2026-11-28).
pub const PRE_RESTORE_DAYS: i64 = 7;
const PRE_RESTORE: &str = ".pre-restore-";
/// `20261128T031000Z`: a safety copy's time, in its name.
const STAMP: &str = "%Y%m%dT%H%M%SZ";

/// Write `data` to `path` atomically: a new temporary file with `mode` (never through a
/// symlink), fsync, rename, fsync the directory. `group_of_dir`: the file gets its directory's
/// group (status files the server reads).
pub(crate) fn write_atomic(
    path: &Path,
    data: &[u8],
    mode: u32,
    group_of_dir: bool,
) -> Result<(), BackupError> {
    let dir = path
        .parent()
        .ok_or_else(|| BackupError::Failed(format!("{} has no directory", path.display())))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let write = || -> std::io::Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .custom_flags(nofollow())
            .open(&tmp)?;
        // The umask may have narrowed it.
        f.set_permissions(std::fs::Permissions::from_mode(mode))?;
        if group_of_dir {
            let gid = dir.metadata()?.gid();
            std::os::unix::fs::fchown(&f, None, Some(gid))?;
        }
        f.write_all(data)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        File::open(dir)?.sync_all()
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io(format!("writing {}", path.display()))(e)
    })
}

/// Read at most `max` bytes of text from `path`; `None` when it doesn't exist. A symlink, a
/// FIFO, a device or anything else that is not a regular file, or a bigger file, is an error
/// (and never blocks).
pub(crate) fn read_small(path: &Path, max: u64) -> Result<Option<String>, BackupError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nofollow() | nonblock())
        .open(path);
    let file = match file {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(format!("reading {}", path.display()))(e)),
    };
    let meta = file
        .metadata()
        .map_err(io(format!("reading {}", path.display())))?;
    if !meta.file_type().is_file() {
        return Err(BackupError::Invalid(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let mut text = String::new();
    file.take(max + 1)
        .read_to_string(&mut text)
        .map_err(io(format!("reading {}", path.display())))?;
    if text.len() as u64 > max {
        return Err(BackupError::Invalid(format!(
            "{} is larger than {max} bytes",
            path.display()
        )));
    }
    Ok(Some(text))
}

/// Make `dir` (and its parents) with `mode` on the last part.
pub(crate) fn ensure_dir(dir: &Path, mode: u32) -> Result<(), BackupError> {
    std::fs::create_dir_all(dir).map_err(io(format!("creating {}", dir.display())))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode))
        .map_err(io(format!("setting the mode of {}", dir.display())))
}

/// Remove `path` (a directory tree or a file) if it is there; never follows a symlink.
pub(crate) fn remove_any(path: &Path) -> Result<(), BackupError> {
    match path.symlink_metadata() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io(format!("reading {}", path.display()))(e)),
        Ok(m) if m.is_dir() => {
            std::fs::remove_dir_all(path).map_err(io(format!("removing {}", path.display())))
        }
        Ok(_) => std::fs::remove_file(path).map_err(io(format!("removing {}", path.display()))),
    }
}

/// A fresh, empty staging directory (root only).
pub(crate) fn fresh_dir(dir: &Path) -> Result<(), BackupError> {
    remove_any(dir)?;
    ensure_dir(dir, 0o700)
}

/// Copy `src` to `dst` (a file, a symlink as a symlink, or a directory tree), keeping modes.
/// `dst` must not exist.
pub(crate) fn copy_tree(src: &Path, dst: &Path) -> Result<(), BackupError> {
    let meta = src
        .symlink_metadata()
        .map_err(io(format!("reading {}", src.display())))?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        let target = std::fs::read_link(src).map_err(io(format!("reading {}", src.display())))?;
        std::os::unix::fs::symlink(&target, dst).map_err(io(format!("linking {}", dst.display())))
    } else if ft.is_dir() {
        std::fs::create_dir(dst).map_err(io(format!("creating {}", dst.display())))?;
        let entries = std::fs::read_dir(src).map_err(io(format!("reading {}", src.display())))?;
        for e in entries {
            let e = e.map_err(io(format!("reading {}", src.display())))?;
            copy_tree(&e.path(), &dst.join(e.file_name()))?;
        }
        std::fs::set_permissions(dst, meta.permissions())
            .map_err(io(format!("setting the mode of {}", dst.display())))
    } else if ft.is_file() {
        std::fs::copy(src, dst).map_err(io(format!("copying {}", src.display())))?;
        std::fs::set_permissions(dst, meta.permissions())
            .map_err(io(format!("setting the mode of {}", dst.display())))
    } else {
        // Sockets, fifos and devices are not data.
        Ok(())
    }
}

/// The UTC stamp for a safety copy made at `unix` seconds.
pub fn stamp(unix: i64) -> String {
    jiff::Timestamp::from_second(unix)
        .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
        .strftime(STAMP)
        .to_string()
}

/// `<dst>.pre-restore-<stamp>`: where the data a restore replaced is kept for
/// [`PRE_RESTORE_DAYS`]. Never an existing path: a second restore in the same second gets
/// `-2`, `-3`, ...
pub fn pre_restore_path(dst: &Path, stamp: &str) -> PathBuf {
    let base = {
        let mut s = dst.as_os_str().to_owned();
        s.push(PRE_RESTORE);
        s.push(stamp);
        s
    };
    let mut candidate = PathBuf::from(&base);
    let mut n = 2;
    while candidate.symlink_metadata().is_ok() {
        let mut s = base.clone();
        s.push(format!("-{n}"));
        candidate = PathBuf::from(s);
        n += 1;
    }
    candidate
}

/// When the safety copy `name` was made (from its name), if it is one.
fn pre_restore_time(name: &str) -> Option<i64> {
    let (_, rest) = name.rsplit_once(PRE_RESTORE)?;
    let stamp = rest.get(..16)?;
    let civil = jiff::civil::DateTime::strptime("%Y%m%dT%H%M%S", stamp.strip_suffix('Z')?).ok()?;
    civil
        .to_zoned(jiff::tz::TimeZone::UTC)
        .ok()
        .map(|z| z.timestamp().as_second())
}

/// Remove the safety copies directly in `dir` older than [`PRE_RESTORE_DAYS`] at `now`.
/// Returns what went; a missing `dir` is nothing to do.
pub(crate) fn prune_pre_restore(dir: &Path, now: i64) -> Result<Vec<PathBuf>, BackupError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io(format!("reading {}", dir.display()))(e)),
    };
    let mut removed = Vec::new();
    for e in entries {
        let e = e.map_err(io(format!("reading {}", dir.display())))?;
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(made) = pre_restore_time(&name) else {
            continue;
        };
        if now - made > PRE_RESTORE_DAYS * 86_400 {
            remove_any(&e.path())?;
            removed.push(e.path());
        }
    }
    Ok(removed)
}

/// `path` exists, is not a symlink, and resolves to a place inside `base` (no symlink on the
/// way leads out). For anything read from a restored snapshot before it is used.
pub(crate) fn inside(base: &Path, path: &Path) -> Result<(), BackupError> {
    let refuse = |why: &str| {
        Err(BackupError::Invalid(format!(
            "{} in the backup {why}; not using it",
            path.display()
        )))
    };
    match path.symlink_metadata() {
        Ok(m) if m.file_type().is_symlink() => return refuse("is a symlink"),
        Ok(_) => {}
        Err(e) => return Err(io(format!("reading {}", path.display()))(e)),
    }
    let base = base
        .canonicalize()
        .map_err(io(format!("reading {}", base.display())))?;
    let real = path
        .canonicalize()
        .map_err(io(format!("reading {}", path.display())))?;
    if real.starts_with(&base) && real != base {
        Ok(())
    } else {
        refuse("leads outside the restored copy")
    }
}

/// Is `path` a mount point (another file system than its parent's)? A drive that isn't
/// mounted leaves an empty directory on the parent's file system (e.g. tmpfs `/run`).
pub(crate) fn is_mount_point(path: &Path) -> Result<bool, BackupError> {
    let meta = path
        .symlink_metadata()
        .map_err(io(format!("reading {}", path.display())))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Ok(false);
    }
    let parent = path.join("..");
    let up = parent
        .metadata()
        .map_err(io(format!("reading {}", parent.display())))?;
    Ok(meta.dev() != up.dev())
}

/// Put the restored `src` in place of `dst`, keeping what was there as a safety copy
/// ([`pre_restore_path`] with `stamp`; never removing an older one). Returns the safety copy,
/// when there was something to keep. `keep`: names directly inside `dst` that stay as they are
/// on this device (moved over into the restored copy first, when it has none). Both on one
/// file system: a rename, never a copy.
pub(crate) fn swap_in(
    src: &Path,
    dst: &Path,
    keep: &[&str],
    stamp: &str,
) -> Result<Option<PathBuf>, BackupError> {
    let dst_meta = dst.symlink_metadata();
    let mut kept_copy = None;
    if let Ok(m) = &dst_meta {
        if m.file_type().is_symlink() {
            return Err(BackupError::Invalid(format!(
                "{} is a symlink; not replacing it",
                dst.display()
            )));
        }
        if m.is_dir() && src.is_dir() {
            for k in keep {
                let (from, to) = (dst.join(k), src.join(k));
                if from.symlink_metadata().is_ok() && to.symlink_metadata().is_err() {
                    std::fs::rename(&from, &to)
                        .map_err(io(format!("keeping {}", from.display())))?;
                }
            }
        }
        let pre = pre_restore_path(dst, stamp);
        std::fs::rename(dst, &pre).map_err(io(format!("moving {} aside", dst.display())))?;
        kept_copy = Some(pre);
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(io(format!("creating {}", parent.display())))?;
    }
    if let Err(e) = std::fs::rename(src, dst) {
        // Put the live data back rather than leave nothing in its place.
        if let Some(pre) = &kept_copy {
            let _ = std::fs::rename(pre, dst);
        }
        return Err(io(format!("putting {} in place", dst.display()))(e));
    }
    Ok(kept_copy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::scratch;
    use std::process::Command;

    #[test]
    fn atomic_writes_replace_and_never_follow_a_symlink() {
        let d = scratch("atomic");
        let p = d.join("a.json");
        write_atomic(&p, b"one", 0o600, false).unwrap();
        write_atomic(&p, b"two", 0o640, true).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "two");
        assert_eq!(p.metadata().unwrap().mode() & 0o777, 0o640);
        assert_eq!(read_small(&p, 10).unwrap().as_deref(), Some("two"));
        assert!(read_small(&p, 2).is_err());
        assert_eq!(read_small(&d.join("none"), 10).unwrap(), None);
        std::os::unix::fs::symlink(&p, d.join("link")).unwrap();
        assert!(read_small(&d.join("link"), 10).is_err());
    }

    #[test]
    fn copies_keep_modes_and_links() {
        let d = scratch("copy");
        let src = d.join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("sub/key"), "k").unwrap();
        std::fs::set_permissions(src.join("sub/key"), std::fs::Permissions::from_mode(0o600))
            .unwrap();
        std::os::unix::fs::symlink("sub/key", src.join("link")).unwrap();
        copy_tree(&src, &d.join("dst")).unwrap();
        let key = d.join("dst/sub/key");
        assert_eq!(std::fs::read_to_string(&key).unwrap(), "k");
        assert_eq!(key.metadata().unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            std::fs::read_link(d.join("dst/link")).unwrap(),
            Path::new("sub/key")
        );
    }

    #[test]
    fn a_swap_keeps_the_old_copy_and_the_kept_names() {
        let d = scratch("swap");
        let (src, dst) = (d.join("restored"), d.join("live"));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(dst.join("models")).unwrap();
        std::fs::write(src.join("data"), "old").unwrap();
        std::fs::write(dst.join("data"), "new").unwrap();
        std::fs::write(dst.join("models/m"), "model").unwrap();
        let s = stamp(1_795_835_400);
        assert_eq!(s, "20261128T031000Z");
        // An earlier restore's copy from the same second is never touched.
        let earlier = pre_restore_path(&dst, &s);
        std::fs::create_dir_all(&earlier).unwrap();
        std::fs::write(earlier.join("data"), "earlier").unwrap();
        let pre = swap_in(&src, &dst, &["models"], &s).unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(dst.join("data")).unwrap(), "old");
        assert_eq!(
            std::fs::read_to_string(dst.join("models/m")).unwrap(),
            "model"
        );
        assert!(pre
            .to_string_lossy()
            .ends_with(".pre-restore-20261128T031000Z-2"));
        assert_eq!(std::fs::read_to_string(pre.join("data")).unwrap(), "new");
        assert_eq!(
            std::fs::read_to_string(earlier.join("data")).unwrap(),
            "earlier"
        );
        assert!(!src.exists());
        // A fresh destination is fine too, and keeps nothing.
        std::fs::create_dir_all(&src).unwrap();
        assert_eq!(swap_in(&src, &d.join("new/place"), &[], &s).unwrap(), None);
        assert!(d.join("new/place").is_dir());
    }

    #[test]
    fn safety_copies_go_after_seven_days_only() {
        let d = scratch("prune");
        let made = 1_795_835_400;
        let copy = pre_restore_path(&d.join("vol"), &stamp(made));
        std::fs::create_dir_all(copy.join("x")).unwrap();
        std::fs::create_dir_all(d.join("vol")).unwrap();
        std::fs::create_dir_all(d.join("other.pre-restore-garbage")).unwrap();
        assert!(prune_pre_restore(&d, made + 6 * 86_400).unwrap().is_empty());
        assert_eq!(
            prune_pre_restore(&d, made + 8 * 86_400).unwrap(),
            vec![copy.clone()]
        );
        assert!(!copy.exists());
        assert!(d.join("vol").is_dir());
        assert!(d.join("other.pre-restore-garbage").is_dir());
        assert!(prune_pre_restore(&d.join("none"), made).unwrap().is_empty());
    }

    #[test]
    fn restored_paths_must_stay_inside_and_fifos_are_not_read() {
        let d = scratch("inside");
        let staging = d.join("staging");
        std::fs::create_dir_all(staging.join("var/lib/x")).unwrap();
        inside(&staging, &staging.join("var/lib/x")).unwrap();
        // A symlink on the way out of the staging area.
        std::fs::create_dir_all(d.join("elsewhere/x")).unwrap();
        std::os::unix::fs::symlink(d.join("elsewhere"), staging.join("var/lib/y")).unwrap();
        assert!(inside(&staging, &staging.join("var/lib/y/x")).is_err());
        assert!(inside(&staging, &staging.join("var/lib/y")).is_err());
        assert!(inside(&staging, &staging).is_err());
        assert!(inside(&staging, &staging.join("missing")).is_err());

        let fifo = d.join("fifo");
        let made = Command::new("mkfifo").arg(&fifo).status().unwrap();
        assert!(made.success());
        assert!(read_small(&fifo, 10).is_err());
    }

    #[test]
    fn mount_points_are_other_file_systems() {
        let d = scratch("mount");
        assert!(!is_mount_point(&d).unwrap());
        // /proc is its own file system wherever these tests run.
        assert!(is_mount_point(Path::new("/proc")).unwrap());
        assert!(is_mount_point(&d.join("missing")).is_err());
    }
}
