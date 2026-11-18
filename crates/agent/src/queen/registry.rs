//! The bee registry: the persistent, named bees of this comb.
//!
//! Stored as one JSON file, `$XDG_STATE_HOME/melissa/bees/registry.json` (mode 0600, written
//! atomically), next to one directory per bee (`bees/<id>/`: its agent session store,
//! `session.json` and transcript). Melissa's state directory, not the cluster: bees are this
//! comb's agents, not workloads; they exist on combs without k3s too, and their transcripts
//! are private.
//!
//! What is **not** stored here: which cells a bee owns. That lives on the cells themselves
//! (the `melissa.io/owner-bee` label of the app namespace), the one source of truth the
//! scope check reads, so a reassignment can never leave two records that disagree.
//!
//! Ids are never reused: the next number is saved with the registry and only grows (it is
//! also kept above every id in the file, should the file be edited by hand).

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};

use crate::comb::{bee_id, valid_bee_id, valid_comb_id, write_private};
use crate::tools::BeeNames;

/// Longest bee name, in characters.
pub const MAX_NAME_CHARS: usize = 40;

/// One bee.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeeRecord {
    /// Stable id (`<comb>.b<n>`), never reused.
    pub id: String,
    /// Display name (the Queen chooses it; the user may rename the bee).
    pub name: String,
    /// Session backend its agent runs on.
    pub backend: String,
    /// Tools it may use (None: the Queen's tools without the Queen-only ones).
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    /// Who created it (`the Queen (task 3)`, `the user`).
    pub created_by: String,
    /// Unix seconds.
    pub created_at: u64,
    /// Stopped by the Queen or the user: no session runs until its next job or message.
    #[serde(default)]
    pub stopped: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RegistryFile {
    comb: String,
    /// Number for the next bee; only grows.
    next: u64,
    bees: Vec<BeeRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error("bee names are 1 to {MAX_NAME_CHARS} letters, digits, spaces, `-`, `_` or `.`, starting with a letter or digit")]
    BadName,
    #[error("there is already a bee called `{0}` on this comb")]
    NameTaken(String),
    #[error("no bee `{0}` on this comb")]
    NoSuchBee(String),
    #[error("the bee registry {path} belongs to comb {found}, not {expected}")]
    WrongComb {
        path: String,
        found: String,
        expected: String,
    },
    #[error("bee registry {path}: {reason}")]
    Io { path: String, reason: String },
}

struct Inner {
    path: PathBuf,
    file: Mutex<RegistryFile>,
}

/// The bees of one comb. Cheap to clone.
#[derive(Clone)]
pub struct BeeRegistry {
    inner: Arc<Inner>,
}

impl BeeRegistry {
    /// Open (or start) the registry in `dir` (`.../melissa/bees`) for comb `comb`.
    pub fn open(dir: &Path, comb: &str) -> Result<Self, RegistryError> {
        let path = dir.join("registry.json");
        let shown = path.display().to_string();
        if !valid_comb_id(comb) {
            return Err(RegistryError::Io {
                path: shown,
                reason: format!("`{comb}` is not a comb id"),
            });
        }
        let mut file = match std::fs::read_to_string(&path) {
            Ok(text) => {
                serde_json::from_str::<RegistryFile>(&text).map_err(|e| RegistryError::Io {
                    path: shown.clone(),
                    reason: e.to_string(),
                })?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => RegistryFile {
                comb: comb.into(),
                next: 1,
                bees: Vec::new(),
            },
            Err(e) => {
                return Err(RegistryError::Io {
                    path: shown,
                    reason: e.to_string(),
                })
            }
        };
        if file.comb != comb {
            return Err(RegistryError::WrongComb {
                path: shown,
                found: file.comb,
                expected: comb.into(),
            });
        }
        file.bees.retain(|b| valid_bee_id(&b.id));
        // Never hand out a number that is already in use.
        let highest = file
            .bees
            .iter()
            .filter_map(|b| {
                b.id.rsplit_once(".b")
                    .and_then(|(_, n)| n.parse::<u64>().ok())
            })
            .max()
            .unwrap_or(0);
        file.next = file.next.max(highest + 1).max(1);
        Ok(Self {
            inner: Arc::new(Inner {
                path,
                file: Mutex::new(file),
            }),
        })
    }

    /// The comb these bees live on.
    pub fn comb(&self) -> String {
        lock(&self.inner.file).comb.clone()
    }

    /// Directory for bee `id`'s own state (session store, transcript).
    pub fn bee_dir(&self, id: &str) -> PathBuf {
        self.inner
            .path
            .parent()
            .map_or_else(|| PathBuf::from(id), |d| d.join(id))
    }

    /// Every bee, oldest first.
    pub fn list(&self) -> Vec<BeeRecord> {
        lock(&self.inner.file).bees.clone()
    }

    pub fn get(&self, id: &str) -> Option<BeeRecord> {
        lock(&self.inner.file)
            .bees
            .iter()
            .find(|b| b.id == id)
            .cloned()
    }

    /// A bee by id or by name (names are matched ignoring case).
    pub fn resolve(&self, name_or_id: &str) -> Option<BeeRecord> {
        let key = name_or_id.trim();
        let file = lock(&self.inner.file);
        file.bees
            .iter()
            .find(|b| b.id == key)
            .or_else(|| file.bees.iter().find(|b| b.name.eq_ignore_ascii_case(key)))
            .cloned()
    }

    /// Make a new bee. Its id is saved before this returns.
    pub fn create(
        &self,
        name: &str,
        backend: &str,
        tools: Option<Vec<String>>,
        created_by: &str,
    ) -> Result<BeeRecord, RegistryError> {
        let name = check_name(name)?;
        let mut file = lock(&self.inner.file);
        if file.bees.iter().any(|b| b.name.eq_ignore_ascii_case(&name)) {
            return Err(RegistryError::NameTaken(name));
        }
        let record = BeeRecord {
            id: bee_id(&file.comb, file.next),
            name,
            backend: backend.into(),
            tools,
            created_by: created_by.into(),
            created_at: now(),
            stopped: false,
        };
        file.next += 1;
        file.bees.push(record.clone());
        self.save(&file)?;
        Ok(record)
    }

    /// Give a bee a new display name (the user's right; ids never change).
    pub fn rename(&self, name_or_id: &str, new_name: &str) -> Result<BeeRecord, RegistryError> {
        let new_name = check_name(new_name)?;
        let id = self
            .resolve(name_or_id)
            .ok_or_else(|| RegistryError::NoSuchBee(name_or_id.into()))?
            .id;
        let mut file = lock(&self.inner.file);
        if file
            .bees
            .iter()
            .any(|b| b.id != id && b.name.eq_ignore_ascii_case(&new_name))
        {
            return Err(RegistryError::NameTaken(new_name));
        }
        let bee = file
            .bees
            .iter_mut()
            .find(|b| b.id == id)
            .ok_or_else(|| RegistryError::NoSuchBee(name_or_id.into()))?;
        bee.name = new_name;
        let out = bee.clone();
        self.save(&file)?;
        Ok(out)
    }

    /// Mark a bee stopped (or awake again).
    pub fn set_stopped(&self, id: &str, stopped: bool) -> Result<(), RegistryError> {
        let mut file = lock(&self.inner.file);
        let bee = file
            .bees
            .iter_mut()
            .find(|b| b.id == id)
            .ok_or_else(|| RegistryError::NoSuchBee(id.into()))?;
        if bee.stopped == stopped {
            return Ok(());
        }
        bee.stopped = stopped;
        self.save(&file)
    }

    fn save(&self, file: &RegistryFile) -> Result<(), RegistryError> {
        let io = |reason: String| RegistryError::Io {
            path: self.inner.path.display().to_string(),
            reason,
        };
        let json = serde_json::to_vec_pretty(file).map_err(|e| io(e.to_string()))?;
        write_private(&self.inner.path, &json).map_err(|e| io(e.to_string()))
    }
}

impl BeeNames for BeeRegistry {
    fn name_of(&self, id: &str) -> Option<String> {
        self.get(id).map(|b| b.name)
    }

    fn resolve_id(&self, name_or_id: &str) -> Option<String> {
        self.resolve(name_or_id).map(|b| b.id)
    }
}

/// Bee names: what a person would call a helper. No control characters, nothing that looks
/// like markup, so names are safe in messages, confirmations and the transcript.
fn check_name(name: &str) -> Result<String, RegistryError> {
    let name = name.trim();
    let ok = (1..=MAX_NAME_CHARS).contains(&name.chars().count())
        && name.chars().next().is_some_and(char::is_alphanumeric)
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'));
    if ok {
        Ok(name.to_string())
    } else {
        Err(RegistryError::BadName)
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // Poisoning only means another thread panicked mid-update; the data is still usable.
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMB: &str = "c-0a1b2c3d";

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("melissa-bees-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn bees_persist_and_ids_are_never_reused() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir("persist");
        let reg = BeeRegistry::open(&d, COMB).unwrap();
        let ada = reg
            .create("Ada", "grok-acp", None, "the Queen (task 1)")
            .unwrap();
        let bo = reg
            .create(
                "Bo",
                "claude-session",
                Some(vec!["comb_info".into()]),
                "the user",
            )
            .unwrap();
        assert_eq!(ada.id, "c-0a1b2c3d.b1");
        assert_eq!(bo.id, "c-0a1b2c3d.b2");
        assert_eq!(
            std::fs::metadata(d.join("registry.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        // A restarted daemon sees the same bees and keeps counting.
        let reg = BeeRegistry::open(&d, COMB).unwrap();
        assert_eq!(reg.list(), vec![ada.clone(), bo.clone()]);
        assert_eq!(reg.resolve("ada").unwrap().id, ada.id);
        assert_eq!(reg.resolve(&bo.id).unwrap().name, "Bo");
        let cy = reg.create("Cy", "grok-acp", None, "the user").unwrap();
        assert_eq!(cy.id, "c-0a1b2c3d.b3");

        // Even if a bee disappears from the file (hand edit) its number stays used, and a
        // stale `next` below an existing id is corrected.
        let mut file: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(d.join("registry.json")).unwrap())
                .unwrap();
        file["next"] = 1.into();
        file["bees"].as_array_mut().unwrap().remove(0);
        std::fs::write(d.join("registry.json"), file.to_string()).unwrap();
        let reg = BeeRegistry::open(&d, COMB).unwrap();
        let next = reg.create("Dee", "grok-acp", None, "the user").unwrap();
        assert_eq!(next.id, "c-0a1b2c3d.b4");
        assert!(reg
            .list()
            .iter()
            .all(|b| b.id != "c-0a1b2c3d.b1" || b.name == "Ada"));

        // Another comb's registry is not ours.
        assert!(matches!(
            BeeRegistry::open(&d, "c-ffffffff"),
            Err(RegistryError::WrongComb { .. })
        ));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn names_are_unique_valid_and_renamable() {
        let d = dir("names");
        let reg = BeeRegistry::open(&d, COMB).unwrap();
        reg.create("Ada", "x", None, "the user").unwrap();
        assert_eq!(
            reg.create("ADA", "x", None, "the user"),
            Err(RegistryError::NameTaken("ADA".into()))
        );
        for bad in ["", " ", "<b>", "a\nb", "-x", &"x".repeat(41)] {
            assert_eq!(
                reg.create(bad, "x", None, "the user"),
                Err(RegistryError::BadName),
                "{bad:?}"
            );
        }
        let bo = reg.create("Bo", "x", None, "the user").unwrap();
        assert_eq!(
            reg.rename("bo", "ada"),
            Err(RegistryError::NameTaken("ada".into()))
        );
        let renamed = reg.rename("bo", "Bea").unwrap();
        assert_eq!(
            (renamed.id.as_str(), renamed.name.as_str()),
            (bo.id.as_str(), "Bea")
        );
        assert_eq!(
            BeeRegistry::open(&d, COMB)
                .unwrap()
                .get(&bo.id)
                .unwrap()
                .name,
            "Bea"
        );
        assert_eq!(BeeNames::name_of(&reg, &bo.id).as_deref(), Some("Bea"));
        assert_eq!(
            BeeNames::resolve_id(&reg, "BEA").as_deref(),
            Some(bo.id.as_str())
        );
        assert!(matches!(
            reg.rename("nobody", "X"),
            Err(RegistryError::NoSuchBee(_))
        ));
        reg.set_stopped(&bo.id, true).unwrap();
        assert!(
            BeeRegistry::open(&d, COMB)
                .unwrap()
                .get(&bo.id)
                .unwrap()
                .stopped
        );
        std::fs::remove_dir_all(d).unwrap();
    }
}
