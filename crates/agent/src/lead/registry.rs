//! The agent registry: the persistent, named agents of this device.
//!
//! Stored as one JSON file, `$XDG_STATE_HOME/melissa/agents/registry.json` (mode 0600, written
//! atomically), next to one directory per agent (`agents/<id>/`: its agent session store,
//! `session.json` and transcript). Melissa's state directory, not the cluster: agents are this
//! device's agents, not workloads; they exist on devices without k3s too, and their transcripts
//! are private.
//!
//! What is **not** stored here: which workspaces an agent owns. That lives on the workspaces themselves
//! (the `melissa.io/owner-agent` label of the app namespace), the one source of truth the
//! scope check reads, so a reassignment can never leave two records that disagree.
//!
//! Ids are never reused: the next number is saved with the registry and only grows (it is
//! also kept above every id in the file, should the file be edited by hand).

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};

use crate::device::{agent_id, valid_agent_id, valid_device_id, write_private};
use crate::tools::AgentNames;

/// Longest agent name, in characters.
pub const MAX_NAME_CHARS: usize = 40;

/// One agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRecord {
    /// Stable id (`<device>.a<n>`), never reused.
    pub id: String,
    /// Display name (the lead agent chooses it; the user may rename the agent).
    pub name: String,
    /// Session backend its agent runs on.
    pub backend: String,
    /// Model the user chose for it (None: the backend's default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reasoning level the user chose for it (None: the agent's default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Tools it may use (None: the lead agent's tools without the lead agent-only ones).
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    /// Who created it (`the lead agent (task 3)`, `the user`).
    pub created_by: String,
    /// Unix seconds.
    pub created_at: u64,
    /// Stopped by the lead agent or the user: no session runs until its next job or message.
    #[serde(default)]
    pub stopped: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RegistryFile {
    device: String,
    /// Number for the next agent; only grows.
    next: u64,
    agents: Vec<AgentRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error("agent names are 1 to {MAX_NAME_CHARS} letters, digits, spaces, `-`, `_` or `.`, starting with a letter or digit")]
    BadName,
    #[error("there is already an agent called `{0}` on this device")]
    NameTaken(String),
    #[error("no agent `{0}` on this device")]
    NoSuchAgent(String),
    #[error("the agent registry {path} belongs to device {found}, not {expected}")]
    WrongDevice {
        path: String,
        found: String,
        expected: String,
    },
    #[error("agent registry {path}: {reason}")]
    Io { path: String, reason: String },
}

struct Inner {
    path: PathBuf,
    file: Mutex<RegistryFile>,
}

/// The agents of one device. Cheap to clone.
#[derive(Clone)]
pub struct AgentRegistry {
    inner: Arc<Inner>,
}

impl AgentRegistry {
    /// Open (or start) the registry in `dir` (`.../melissa/agents`) for device `device`.
    pub fn open(dir: &Path, device: &str) -> Result<Self, RegistryError> {
        let path = dir.join("registry.json");
        let shown = path.display().to_string();
        if !valid_device_id(device) {
            return Err(RegistryError::Io {
                path: shown,
                reason: format!("`{device}` is not a device id"),
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
                device: device.into(),
                next: 1,
                agents: Vec::new(),
            },
            Err(e) => {
                return Err(RegistryError::Io {
                    path: shown,
                    reason: e.to_string(),
                })
            }
        };
        if file.device != device {
            return Err(RegistryError::WrongDevice {
                path: shown,
                found: file.device,
                expected: device.into(),
            });
        }
        file.agents.retain(|b| valid_agent_id(&b.id));
        // Never hand out a number that is already in use.
        let highest = file
            .agents
            .iter()
            .filter_map(|b| {
                b.id.rsplit_once(".a")
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

    /// The device these agents live on.
    pub fn device(&self) -> String {
        lock(&self.inner.file).device.clone()
    }

    /// Directory for agent `id`'s own state (session store, transcript).
    pub fn agent_dir(&self, id: &str) -> PathBuf {
        self.inner
            .path
            .parent()
            .map_or_else(|| PathBuf::from(id), |d| d.join(id))
    }

    /// Every agent, oldest first.
    pub fn list(&self) -> Vec<AgentRecord> {
        lock(&self.inner.file).agents.clone()
    }

    pub fn get(&self, id: &str) -> Option<AgentRecord> {
        lock(&self.inner.file)
            .agents
            .iter()
            .find(|b| b.id == id)
            .cloned()
    }

    /// An agent by id or by name (names are matched ignoring case).
    pub fn resolve(&self, name_or_id: &str) -> Option<AgentRecord> {
        let key = name_or_id.trim();
        let file = lock(&self.inner.file);
        file.agents
            .iter()
            .find(|b| b.id == key)
            .or_else(|| {
                file.agents
                    .iter()
                    .find(|b| b.name.eq_ignore_ascii_case(key))
            })
            .cloned()
    }

    /// Make a new agent. Its id is saved before this returns.
    pub fn create(
        &self,
        name: &str,
        backend: &str,
        tools: Option<Vec<String>>,
        created_by: &str,
    ) -> Result<AgentRecord, RegistryError> {
        let name = check_name(name)?;
        let mut file = lock(&self.inner.file);
        if file
            .agents
            .iter()
            .any(|b| b.name.eq_ignore_ascii_case(&name))
        {
            return Err(RegistryError::NameTaken(name));
        }
        let record = AgentRecord {
            id: agent_id(&file.device, file.next),
            name,
            backend: backend.into(),
            model: None,
            effort: None,
            tools,
            created_by: created_by.into(),
            created_at: now(),
            stopped: false,
        };
        file.next += 1;
        file.agents.push(record.clone());
        self.save(&file)?;
        Ok(record)
    }

    /// Give an agent a new display name (the user's right; ids never change).
    pub fn rename(&self, name_or_id: &str, new_name: &str) -> Result<AgentRecord, RegistryError> {
        let new_name = check_name(new_name)?;
        let id = self
            .resolve(name_or_id)
            .ok_or_else(|| RegistryError::NoSuchAgent(name_or_id.into()))?
            .id;
        let mut file = lock(&self.inner.file);
        if file
            .agents
            .iter()
            .any(|b| b.id != id && b.name.eq_ignore_ascii_case(&new_name))
        {
            return Err(RegistryError::NameTaken(new_name));
        }
        let agent = file
            .agents
            .iter_mut()
            .find(|b| b.id == id)
            .ok_or_else(|| RegistryError::NoSuchAgent(name_or_id.into()))?;
        agent.name = new_name;
        let out = agent.clone();
        self.save(&file)?;
        Ok(out)
    }

    /// The user moves an agent to another backend (and model and reasoning level).
    pub fn set_backend(
        &self,
        id: &str,
        choice: &super::backends::BackendChoice,
    ) -> Result<AgentRecord, RegistryError> {
        let mut file = lock(&self.inner.file);
        let agent = file
            .agents
            .iter_mut()
            .find(|b| b.id == id)
            .ok_or_else(|| RegistryError::NoSuchAgent(id.into()))?;
        agent.backend.clone_from(&choice.backend);
        agent.model.clone_from(&choice.model);
        agent.effort.clone_from(&choice.effort);
        let out = agent.clone();
        self.save(&file)?;
        Ok(out)
    }

    /// Forget an agent (its id is never reused). Returns its record.
    pub fn remove(&self, id: &str) -> Result<AgentRecord, RegistryError> {
        let mut file = lock(&self.inner.file);
        let i = file
            .agents
            .iter()
            .position(|b| b.id == id)
            .ok_or_else(|| RegistryError::NoSuchAgent(id.into()))?;
        let mut next = RegistryFile {
            device: file.device.clone(),
            next: file.next,
            agents: file.agents.clone(),
        };
        let gone = next.agents.remove(i);
        self.save(&next)?;
        *file = next;
        Ok(gone)
    }

    /// Mark an agent stopped (or awake again).
    pub fn set_stopped(&self, id: &str, stopped: bool) -> Result<(), RegistryError> {
        let mut file = lock(&self.inner.file);
        let agent = file
            .agents
            .iter_mut()
            .find(|b| b.id == id)
            .ok_or_else(|| RegistryError::NoSuchAgent(id.into()))?;
        if agent.stopped == stopped {
            return Ok(());
        }
        agent.stopped = stopped;
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

impl AgentNames for AgentRegistry {
    fn name_of(&self, id: &str) -> Option<String> {
        self.get(id).map(|b| b.name)
    }

    fn resolve_id(&self, name_or_id: &str) -> Option<String> {
        self.resolve(name_or_id).map(|b| b.id)
    }
}

/// Agent names: what a person would call a helper. No control characters, nothing that looks
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

    const DEVICE: &str = "d-0a1b2c3d";

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("melissa-agents-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn agents_persist_and_ids_are_never_reused() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir("persist");
        let reg = AgentRegistry::open(&d, DEVICE).unwrap();
        let ada = reg
            .create("Ada", "grok-acp", None, "the lead agent (task 1)")
            .unwrap();
        let bo = reg
            .create(
                "Bo",
                "claude-session",
                Some(vec!["device_info".into()]),
                "the user",
            )
            .unwrap();
        assert_eq!(ada.id, "d-0a1b2c3d.a1");
        assert_eq!(bo.id, "d-0a1b2c3d.a2");
        assert_eq!(
            std::fs::metadata(d.join("registry.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        // A restarted daemon sees the same agents and keeps counting.
        let reg = AgentRegistry::open(&d, DEVICE).unwrap();
        assert_eq!(reg.list(), vec![ada.clone(), bo.clone()]);
        assert_eq!(reg.resolve("ada").unwrap().id, ada.id);
        assert_eq!(reg.resolve(&bo.id).unwrap().name, "Bo");
        let cy = reg.create("Cy", "grok-acp", None, "the user").unwrap();
        assert_eq!(cy.id, "d-0a1b2c3d.a3");

        // Even if an agent disappears from the file (hand edit) its number stays used, and a
        // stale `next` below an existing id is corrected.
        let mut file: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(d.join("registry.json")).unwrap())
                .unwrap();
        file["next"] = 1.into();
        file["agents"].as_array_mut().unwrap().remove(0);
        std::fs::write(d.join("registry.json"), file.to_string()).unwrap();
        let reg = AgentRegistry::open(&d, DEVICE).unwrap();
        let next = reg.create("Dee", "grok-acp", None, "the user").unwrap();
        assert_eq!(next.id, "d-0a1b2c3d.a4");
        assert!(reg
            .list()
            .iter()
            .all(|b| b.id != "d-0a1b2c3d.a1" || b.name == "Ada"));

        // Another device's registry is not ours.
        assert!(matches!(
            AgentRegistry::open(&d, "d-ffffffff"),
            Err(RegistryError::WrongDevice { .. })
        ));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn names_are_unique_valid_and_renamable() {
        let d = dir("names");
        let reg = AgentRegistry::open(&d, DEVICE).unwrap();
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
            AgentRegistry::open(&d, DEVICE)
                .unwrap()
                .get(&bo.id)
                .unwrap()
                .name,
            "Bea"
        );
        assert_eq!(AgentNames::name_of(&reg, &bo.id).as_deref(), Some("Bea"));
        assert_eq!(
            AgentNames::resolve_id(&reg, "BEA").as_deref(),
            Some(bo.id.as_str())
        );
        assert!(matches!(
            reg.rename("nobody", "X"),
            Err(RegistryError::NoSuchAgent(_))
        ));
        reg.set_stopped(&bo.id, true).unwrap();
        assert!(
            AgentRegistry::open(&d, DEVICE)
                .unwrap()
                .get(&bo.id)
                .unwrap()
                .stopped
        );
        std::fs::remove_dir_all(d).unwrap();
    }
}
