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

/// Read at most `max` bytes of text from `path`; `None` when it doesn't exist. A symlink or a
/// bigger file is an error.
pub(crate) fn read_small(path: &Path, max: u64) -> Result<Option<String>, BackupError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nofollow())
        .open(path);
    let file = match file {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(format!("reading {}", path.display()))(e)),
    };
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

/// `<dst>.pre-restore`: where the replaced copy is kept until the next restore.
pub fn pre_restore_path(dst: &Path) -> PathBuf {
    let mut s = dst.as_os_str().to_owned();
    s.push(".pre-restore");
    PathBuf::from(s)
}

/// Put the restored `src` in place of `dst`, keeping what was there as `<dst>.pre-restore`
/// (an older one goes). `keep`: names directly inside `dst` that stay as they are on this
/// device (moved over into the restored copy first, when it has none). Both on one file
/// system: a rename, never a copy.
pub(crate) fn swap_in(src: &Path, dst: &Path, keep: &[&str]) -> Result<(), BackupError> {
    let pre = pre_restore_path(dst);
    remove_any(&pre)?;
    let dst_meta = dst.symlink_metadata();
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
        std::fs::rename(dst, &pre).map_err(io(format!("moving {} aside", dst.display())))?;
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(io(format!("creating {}", parent.display())))?;
    }
    std::fs::rename(src, dst).map_err(io(format!("putting {} in place", dst.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::scratch;

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
        std::fs::create_dir_all(pre_restore_path(&dst)).unwrap();
        swap_in(&src, &dst, &["models"]).unwrap();
        assert_eq!(std::fs::read_to_string(dst.join("data")).unwrap(), "old");
        assert_eq!(
            std::fs::read_to_string(dst.join("models/m")).unwrap(),
            "model"
        );
        let pre = pre_restore_path(&dst);
        assert_eq!(std::fs::read_to_string(pre.join("data")).unwrap(), "new");
        assert!(!src.exists());
        // A fresh destination is fine too.
        std::fs::create_dir_all(&src).unwrap();
        swap_in(&src, &d.join("new/place"), &[]).unwrap();
        assert!(d.join("new/place").is_dir());
    }
}
