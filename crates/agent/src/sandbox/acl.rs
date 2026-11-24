//! POSIX ACLs, just enough to let one more user into a file or directory (agent users,
//! [`super::users`]): read, change and write the `system.posix_acl_access` and
//! `system.posix_acl_default` extended attributes (the kernel's binary format, so no `setfacl`
//! and no C library).
//!
//! An access ACL gives named users rights next to the owner/group/other bits; a directory's
//! default ACL is copied to everything created in it. The kernel caps every named entry with
//! the mask, which also shows as the group bits of the mode; the owning group's own entry is
//! never widened here.

use std::{io, path::Path};

use rustix::fs::XattrFlags;

const ACCESS: &str = "system.posix_acl_access";
const DEFAULT: &str = "system.posix_acl_default";
const VERSION: u32 = 2;

const USER_OBJ: u16 = 0x01;
const USER: u16 = 0x02;
const GROUP_OBJ: u16 = 0x04;
const GROUP: u16 = 0x08;
const MASK: u16 = 0x10;
const OTHER: u16 = 0x20;
/// The id of entries that name nobody (owner, owning group, mask, other).
const NO_ID: u32 = u32::MAX;

/// Read, write, execute (search, for a directory).
pub const R: u16 = 4;
pub const W: u16 = 2;
pub const X: u16 = 1;
pub const RWX: u16 = R | W | X;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Entry {
    tag: u16,
    id: u32,
    perm: u16,
}

/// An ACL as the kernel stores it, entries in the kernel's order (tag, then id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Acl {
    entries: Vec<Entry>,
}

impl Acl {
    /// The ACL a file without one has: its mode bits.
    pub fn from_mode(mode: u32) -> Self {
        let bits = |shift: u32| ((mode >> shift) & 7) as u16;
        Self {
            entries: vec![
                entry(USER_OBJ, NO_ID, bits(6)),
                entry(GROUP_OBJ, NO_ID, bits(3)),
                entry(OTHER, NO_ID, bits(0)),
            ],
        }
    }

    /// A default ACL for a shared directory: the creator (owner) and each of `users` get
    /// everything, the owning group and others nothing.
    pub fn shared_default(users: &[u32]) -> Self {
        let mut acl = Self {
            entries: vec![
                entry(USER_OBJ, NO_ID, RWX),
                entry(GROUP_OBJ, NO_ID, 0),
                entry(OTHER, NO_ID, 0),
            ],
        };
        for uid in users {
            acl.set_user(*uid, RWX);
        }
        acl
    }

    /// Parse the kernel's binary form.
    pub fn parse(bytes: &[u8]) -> io::Result<Self> {
        let bad = || io::Error::new(io::ErrorKind::InvalidData, "malformed POSIX ACL");
        let (head, rest) = bytes.split_first_chunk::<4>().ok_or_else(bad)?;
        let (chunks, tail) = rest.as_chunks::<8>();
        if u32::from_le_bytes(*head) != VERSION || !tail.is_empty() {
            return Err(bad());
        }
        let entries = chunks
            .iter()
            .map(|&[t0, t1, p0, p1, i0, i1, i2, i3]| Entry {
                tag: u16::from_le_bytes([t0, t1]),
                perm: u16::from_le_bytes([p0, p1]),
                id: u32::from_le_bytes([i0, i1, i2, i3]),
            })
            .collect();
        Ok(Self { entries })
    }

    /// The kernel's binary form, entries sorted as it requires.
    pub fn encode(&self) -> Vec<u8> {
        let mut entries = self.entries.clone();
        entries.sort_by_key(|e| (e.tag, e.id));
        let mut out = Vec::with_capacity(4 + 8 * entries.len());
        out.extend_from_slice(&VERSION.to_le_bytes());
        for e in entries {
            out.extend_from_slice(&e.tag.to_le_bytes());
            out.extend_from_slice(&e.perm.to_le_bytes());
            out.extend_from_slice(&e.id.to_le_bytes());
        }
        out
    }

    /// The rights named user `uid` has in this ACL (before the mask), if it is named.
    pub fn user(&self, uid: u32) -> Option<u16> {
        self.entries
            .iter()
            .find(|e| e.tag == USER && e.id == uid)
            .map(|e| e.perm)
    }

    /// Whether `uid` is named with at least `perm`, and the mask lets that through.
    pub fn grants(&self, uid: u32, perm: u16) -> bool {
        let mask = self
            .entries
            .iter()
            .find(|e| e.tag == MASK)
            .map_or(RWX, |e| e.perm);
        self.user(uid).is_some_and(|p| p & mask & perm == perm)
    }

    /// Name `uid` with exactly `perm`, and widen the mask to the union of the group class
    /// (named users and groups, the owning group), as `setfacl` does. The owning group's own
    /// entry is unchanged, so it gains nothing.
    pub fn set_user(&mut self, uid: u32, perm: u16) {
        match self
            .entries
            .iter_mut()
            .find(|e| e.tag == USER && e.id == uid)
        {
            Some(e) => e.perm = perm,
            None => self.entries.push(entry(USER, uid, perm)),
        }
        let class = self
            .entries
            .iter()
            .filter(|e| matches!(e.tag, USER | GROUP | GROUP_OBJ))
            .fold(0, |acc, e| acc | e.perm);
        match self.entries.iter_mut().find(|e| e.tag == MASK) {
            Some(m) => m.perm = class,
            None => self.entries.push(entry(MASK, NO_ID, class)),
        }
    }
}

fn entry(tag: u16, id: u32, perm: u16) -> Entry {
    Entry { tag, id, perm }
}

/// Whether to follow a symlink at the path itself (never for paths an agent could have
/// replaced).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Follow {
    Yes,
    No,
}

fn get(path: &Path, name: &str, follow: Follow) -> io::Result<Option<Acl>> {
    let mut buf = [0u8; 8192];
    let res = match follow {
        Follow::Yes => rustix::fs::getxattr(path, name, &mut buf[..]),
        Follow::No => rustix::fs::lgetxattr(path, name, &mut buf[..]),
    };
    match res {
        Ok(n) => Acl::parse(&buf[..n]).map(Some),
        Err(rustix::io::Errno::NODATA) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn set(path: &Path, name: &str, acl: &Acl, follow: Follow) -> io::Result<()> {
    let bytes = acl.encode();
    let res = match follow {
        Follow::Yes => rustix::fs::setxattr(path, name, &bytes, XattrFlags::empty()),
        Follow::No => rustix::fs::lsetxattr(path, name, &bytes, XattrFlags::empty()),
    };
    res.map_err(io::Error::from)
}

/// The access ACL of `path` (its mode bits if it has none).
pub fn access(path: &Path, follow: Follow) -> io::Result<Acl> {
    use std::os::unix::fs::MetadataExt;
    if let Some(acl) = get(path, ACCESS, follow)? {
        return Ok(acl);
    }
    let meta = match follow {
        Follow::Yes => std::fs::metadata(path)?,
        Follow::No => std::fs::symlink_metadata(path)?,
    };
    Ok(Acl::from_mode(meta.mode()))
}

/// Give `uid` exactly `perm` on `path` (only the owner may). Writes nothing when it already
/// has that. Returns whether it changed anything.
pub fn grant(path: &Path, uid: u32, perm: u16, follow: Follow) -> io::Result<bool> {
    let mut acl = access(path, follow)?;
    if acl.user(uid) == Some(perm) && acl.grants(uid, perm) {
        return Ok(false);
    }
    acl.set_user(uid, perm);
    set(path, ACCESS, &acl, follow)?;
    Ok(true)
}

/// Set the default ACL of directory `dir` (what new files and directories in it get).
pub fn set_default(dir: &Path, acl: &Acl, follow: Follow) -> io::Result<()> {
    if get(dir, DEFAULT, follow)?.as_ref() == Some(acl) {
        return Ok(());
    }
    set(dir, DEFAULT, acl, follow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_acl_round_trips_through_the_kernel_format() {
        let acl = Acl::from_mode(0o750);
        assert_eq!(Acl::parse(&acl.encode()).unwrap(), acl);
        assert_eq!(acl.encode().len(), 4 + 3 * 8);
        assert!(Acl::parse(&[1, 0, 0, 0]).is_err());
        assert!(Acl::parse(&[2, 0, 0, 0, 1]).is_err());
    }

    #[test]
    fn a_named_user_widens_the_mask_but_not_the_owning_group() {
        let mut acl = Acl::from_mode(0o600);
        assert!(!acl.grants(1234, R));
        acl.set_user(1234, R | W);
        assert!(acl.grants(1234, R | W));
        assert!(!acl.grants(1234, X));
        assert!(!acl.grants(99, R));
        let parsed = Acl::parse(&acl.encode()).unwrap();
        // The owning group stays without rights; the mask is exactly the named user's.
        assert!(parsed.entries.contains(&entry(GROUP_OBJ, NO_ID, 0)));
        assert!(parsed.entries.contains(&entry(MASK, NO_ID, R | W)));
        // Sorted as the kernel wants: owner, named users by id, owning group, mask, other.
        acl.set_user(7, R);
        let tags: Vec<(u16, u32)> = Acl::parse(&acl.encode())
            .unwrap()
            .entries
            .iter()
            .map(|e| (e.tag, e.id))
            .collect();
        assert_eq!(
            tags,
            vec![
                (USER_OBJ, NO_ID),
                (USER, 7),
                (USER, 1234),
                (GROUP_OBJ, NO_ID),
                (MASK, NO_ID),
                (OTHER, NO_ID)
            ]
        );
    }

    #[test]
    fn a_mask_below_the_entry_means_no_grant() {
        let mut acl = Acl::from_mode(0o700);
        acl.set_user(5, RWX);
        for e in &mut acl.entries {
            if e.tag == MASK {
                e.perm = R;
            }
        }
        assert!(acl.grants(5, R));
        assert!(!acl.grants(5, W));
    }

    #[test]
    fn shared_default_names_each_user_and_nobody_else() {
        let acl = Acl::shared_default(&[10, 20]);
        assert_eq!(acl.user(10), Some(RWX));
        assert_eq!(acl.user(20), Some(RWX));
        assert!(acl.entries.contains(&entry(OTHER, NO_ID, 0)));
        assert!(acl.entries.contains(&entry(GROUP_OBJ, NO_ID, 0)));
    }

    /// On the real file system (skipped where the test directory has no ACLs).
    #[test]
    fn grant_and_default_on_disk() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::test_support::fresh_path("acl");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let file = dir.join("f");
        std::fs::write(&file, "x").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        match grant(&file, 4_000_000, R, Follow::No) {
            Err(e) if e.raw_os_error() == Some(95) => {
                eprintln!("SKIP: no POSIX ACLs in {}", dir.display());
                let _ = std::fs::remove_dir_all(&dir);
                return;
            }
            other => assert!(other.unwrap()),
        }
        assert!(
            !grant(&file, 4_000_000, R, Follow::No).unwrap(),
            "idempotent"
        );
        assert!(access(&file, Follow::No).unwrap().grants(4_000_000, R));
        let default = Acl::shared_default(&[4_000_000]);
        set_default(&dir, &default, Follow::No).unwrap();
        let new = dir.join("new");
        std::fs::write(&new, "y").unwrap();
        assert!(access(&new, Follow::No).unwrap().grants(4_000_000, R | W));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
