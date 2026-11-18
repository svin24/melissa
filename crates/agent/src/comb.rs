//! Ids of the hive model (docs/HIVE-MODEL.md): combs (devices) and the bees on them.
//!
//! - A **comb id** is `c-` plus 8 random hex characters, made once per device and kept in
//!   `$XDG_STATE_HOME/melissa/comb.json` ([`CombIdentity`]). Random, so two combs that
//!   later join one hive don't collide.
//! - A **bee id** is `<comb id>.b<n>`: the comb it was born on and a number the comb's bee
//!   registry hands out once (never reused, also after a bee is gone). So a bee id is unique
//!   across the hive and says where the bee lives, which is what cross-comb takeovers need.
//! - A **cell** is named by its app (`nextcloud`); across combs it is `<comb id>/<name>`.
//!
//! Every id fits a Kubernetes label value (`[A-Za-z0-9._-]`, at most 63, alphanumeric at
//! both ends), because cells record their comb and owning bee as namespace labels.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// `c-` + 8 lowercase hex characters.
pub fn valid_comb_id(id: &str) -> bool {
    id.strip_prefix("c-")
        .is_some_and(|h| h.len() == 8 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

/// `<comb id>.b<n>`, `n` from 1 to 999999999 without leading zeros.
pub fn valid_bee_id(id: &str) -> bool {
    let Some((comb, n)) = id.split_once(".b") else {
        return false;
    };
    valid_comb_id(comb)
        && (1..=9).contains(&n.len())
        && !n.starts_with('0')
        && n.bytes().all(|b| b.is_ascii_digit())
}

/// The bee id for number `n` on `comb`.
pub fn bee_id(comb: &str, n: u64) -> String {
    format!("{comb}.b{n}")
}

/// The comb a bee id belongs to (None for an invalid id).
pub fn comb_of(bee: &str) -> Option<&str> {
    valid_bee_id(bee).then(|| bee.split_once(".b").map_or("", |(c, _)| c))
}

/// This device's comb: stable id and display name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CombIdentity {
    pub id: String,
    /// Shown to the user; they may rename the comb (not used anywhere else yet).
    pub name: String,
}

#[derive(Debug, thiserror::Error)]
pub enum CombError {
    #[error("reading {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("{path} is not a valid comb file: {reason}")]
    Invalid { path: String, reason: String },
    #[error("writing {path}: {source}")]
    Write {
        path: String,
        source: std::io::Error,
    },
}

impl CombIdentity {
    /// Load the comb identity from `path`, or make a new one and save it (mode 0600).
    pub fn load_or_create(path: &Path) -> Result<Self, CombError> {
        let shown = path.display().to_string();
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let comb: Self = serde_json::from_str(&text).map_err(|e| CombError::Invalid {
                    path: shown.clone(),
                    reason: e.to_string(),
                })?;
                if !valid_comb_id(&comb.id) {
                    return Err(CombError::Invalid {
                        path: shown,
                        reason: format!("`{}` is not a comb id", comb.id),
                    });
                }
                Ok(comb)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let id = format!(
                    "c-{}",
                    random_hex(4).map_err(|source| CombError::Write {
                        path: shown.clone(),
                        source,
                    })?
                );
                let comb = Self {
                    name: "this comb".into(),
                    id,
                };
                let json = serde_json::to_vec_pretty(&comb).map_err(|e| CombError::Write {
                    path: shown.clone(),
                    source: std::io::Error::other(e),
                })?;
                write_private(path, &json).map_err(|source| CombError::Write {
                    path: shown,
                    source,
                })?;
                Ok(comb)
            }
            Err(source) => Err(CombError::Read {
                path: shown,
                source,
            }),
        }
    }
}

/// `n` random bytes from the kernel CSPRNG as lowercase hex.
pub(crate) fn random_hex(n: usize) -> std::io::Result<String> {
    use std::io::Read;
    let mut bytes = vec![0u8; n];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Write `data` to `path` atomically (temp file + rename), mode 0600, creating the parent.
pub(crate) fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_have_the_documented_shape() {
        assert!(valid_comb_id("c-0a1b2c3d"));
        for bad in ["c-0A1B2C3D", "c-0a1b2c3", "x-0a1b2c3d", "c-0a1b2c3d4", ""] {
            assert!(!valid_comb_id(bad), "{bad}");
        }
        assert!(valid_bee_id("c-0a1b2c3d.b1"));
        assert!(valid_bee_id("c-0a1b2c3d.b123456789"));
        for bad in [
            "c-0a1b2c3d.b0",
            "c-0a1b2c3d.b01",
            "c-0a1b2c3d.b",
            "c-0a1b2c3d.bx",
            "c-0a1b2c3d.b1234567890",
            "c-0a1b2c3.b1",
            "job-abc",
            "owner",
        ] {
            assert!(!valid_bee_id(bad), "{bad}");
        }
        assert_eq!(bee_id("c-0a1b2c3d", 7), "c-0a1b2c3d.b7");
        assert_eq!(comb_of("c-0a1b2c3d.b7"), Some("c-0a1b2c3d"));
        assert_eq!(comb_of("nope"), None);
    }

    #[test]
    fn the_comb_identity_is_made_once_and_kept() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("melissa-comb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("comb.json");
        let a = CombIdentity::load_or_create(&path).unwrap();
        assert!(valid_comb_id(&a.id), "{}", a.id);
        let b = CombIdentity::load_or_create(&path).unwrap();
        assert_eq!(a, b);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::write(&path, r#"{"id": "bogus", "name": "x"}"#).unwrap();
        assert!(CombIdentity::load_or_create(&path).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
