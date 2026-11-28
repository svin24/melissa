//! What a backup of Melissa's own state holds. The lists live with the factory reset's
//! (`melissa_settings::factory_reset`: [`BACKUP_STATE`], [`BACKUP_EXCLUDE`],
//! [`BACKUP_ELSEWHERE`]), so a state file is either erased and backed up, or neither; this
//! module turns them into paths under a root.

use std::path::{Path, PathBuf};

pub use melissa_settings::factory_reset::{BACKUP_ELSEWHERE, BACKUP_EXCLUDE, BACKUP_STATE};

use crate::at;

/// The state directories that exist under `root`.
pub fn state_paths(root: &Path) -> Vec<PathBuf> {
    BACKUP_STATE
        .iter()
        .map(|p| at(root, p))
        .filter(|p| p.symlink_metadata().is_ok_and(|m| m.is_dir()))
        .collect()
}

/// The excludes under `root` (restic patterns: absolute paths without glob characters).
pub fn state_excludes(root: &Path) -> Vec<PathBuf> {
    BACKUP_EXCLUDE.iter().map(|p| at(root, p)).collect()
}

/// For a restore of state directory `dir` (one of [`BACKUP_STATE`]): the names directly in it
/// that stay as they are on this device (excluded from backups, so a backup has none).
pub fn kept_names(dir: &str) -> Vec<&'static str> {
    BACKUP_EXCLUDE
        .iter()
        .filter_map(|e: &&'static str| {
            let e: &'static Path = Path::new(*e);
            // Only direct children: deeper excludes (home/.cache) live inside a restored child
            // and are simply missing after a restore (caches).
            (e.parent() == Some(Path::new(dir)))
                .then(|| e.file_name()?.to_str())
                .flatten()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::scratch;

    #[test]
    fn state_is_the_reset_list_minus_excludes() {
        assert!(BACKUP_STATE.contains(&"/var/lib/melissa"));
        for e in [
            "/var/lib/melissa/tmp",
            "/var/lib/melissa/models",
            "/var/lib/melissa-container/etc-melissa/cluster-join.token",
        ] {
            assert!(BACKUP_EXCLUDE.contains(&e), "{e}");
        }
        // Tailscale's node key and k3s are never part of the state item.
        for p in ["/var/lib/melissa-tailscale", "/var/lib/rancher/k3s"] {
            assert!(BACKUP_ELSEWHERE.contains(&p));
            assert!(!BACKUP_STATE.iter().any(|s| Path::new(p).starts_with(s)));
        }
    }

    #[test]
    fn paths_are_rooted_and_only_existing_ones_count() {
        let root = scratch("inventory");
        assert!(state_paths(&root).is_empty());
        std::fs::create_dir_all(root.join("var/lib/melissa")).unwrap();
        assert_eq!(state_paths(&root), vec![root.join("var/lib/melissa")]);
        assert!(state_excludes(&root).contains(&root.join("var/lib/melissa/tmp")));
    }

    #[test]
    fn kept_names_are_the_direct_excludes() {
        let kept = kept_names("/var/lib/melissa");
        assert!(
            kept.contains(&"tmp") && kept.contains(&"models"),
            "{kept:?}"
        );
        assert!(!kept.contains(&".cache"), "{kept:?}");
        let etc = kept_names("/var/lib/melissa-container/etc-melissa");
        assert!(etc.contains(&"server.toml"), "{etc:?}");
    }
}
