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

use super::icon::AgentIcon;
use crate::device::{agent_id, valid_agent_id, valid_device_id, write_private};
use crate::tools::AgentNames;

/// Longest agent name, in characters.
pub const MAX_NAME_CHARS: usize = 40;

/// The lead agent's name until the user renames it (only the default display name: the
/// product stays "Melissa" whatever the lead agent is called).
pub const LEAD_DEFAULT_NAME: &str = "Melissa";

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
    /// Fast mode the user chose for it (only for a model that offers one).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fast: bool,
    /// Tools it may use (None: the lead agent's tools without the lead agent-only ones).
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    /// Who created it (`the lead agent (task 3)`, `the user`).
    pub created_by: String,
    /// The signed-in person (account id) it was made for: who asked for it in the UI, or whose
    /// message the lead agent was answering (2026-11-22; None before, and for agents nobody
    /// asked for). The apps it installs while nobody's message started the turn count as theirs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator: Option<String>,
    /// Unix seconds.
    pub created_at: u64,
    /// Stopped by the lead agent or the user: no session runs until its next job or message.
    #[serde(default)]
    pub stopped: bool,
    /// Its picture in the UI (2026-11-20; registries from before get the default for the id
    /// when they are opened).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<AgentIcon>,
    /// The user pinned it in the sidebar.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// Names it had before, oldest first (2026-11-21): a former name never silently reaches
    /// another agent that took it later (see [`AgentRegistry::lookup`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub former_names: Vec<String>,
}

impl AgentRecord {
    /// Its icon (the default for its id until one is set).
    pub fn icon(&self) -> AgentIcon {
        self.icon.clone().map_or_else(
            || AgentIcon::default_for(&self.id),
            |i| i.or_default(&self.id),
        )
    }

    /// `Ada (d-0a1b2c3d.a1)`: how agent-facing text names it, so models copy the id.
    pub fn label(&self) -> String {
        label(&self.name, &self.id)
    }
}

/// `Name (id)`, the one way agent-facing text names an agent (the lead agent's id is `lead`).
pub fn label(name: &str, id: &str) -> String {
    format!("{name} ({id})")
}

/// An agent that was deleted: its id and every name it had, so an old reference by name never
/// reaches a newer agent that took the name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DeletedAgent {
    id: String,
    names: Vec<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct RegistryFile {
    device: String,
    /// Number for the next agent; only grows.
    next: u64,
    agents: Vec<AgentRecord>,
    /// The lead agent's display name, once the user renamed it (None: [`LEAD_DEFAULT_NAME`]).
    /// Kept here, beside the helpers' names, so one lock keeps every name unique.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lead_name: Option<String>,
    /// The lead agent's earlier display names (see [`RegistryFile::lead_former`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    lead_former_names: Vec<String>,
    /// Agents that were deleted (their ids are never reused, their names never silently
    /// reach a newer agent).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    deleted: Vec<DeletedAgent>,
}

impl RegistryFile {
    fn lead_name(&self) -> &str {
        self.lead_name.as_deref().unwrap_or(LEAD_DEFAULT_NAME)
    }

    /// The lead agent's former names; its default name counts as one once it was renamed
    /// (registries from before 2026-11-21 did not record former names).
    fn lead_former(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self.lead_former_names.iter().map(String::as_str).collect();
        if self.lead_name.is_some()
            && !self.lead_name().eq_ignore_ascii_case(LEAD_DEFAULT_NAME)
            && !out
                .iter()
                .any(|n| n.eq_ignore_ascii_case(LEAD_DEFAULT_NAME))
        {
            out.insert(0, LEAD_DEFAULT_NAME);
        }
        out
    }

    /// `name` is already used by the lead agent or by a helper other than `except` (an id,
    /// or [`LEAD_KEY`]); `lead` itself always is (it names the lead agent).
    fn name_taken(&self, name: &str, except: Option<&str>) -> bool {
        name.eq_ignore_ascii_case(LEAD_KEY)
            || (except != Some(LEAD_KEY) && self.lead_name().eq_ignore_ascii_case(name))
            || self
                .agents
                .iter()
                .any(|b| Some(b.id.as_str()) != except && b.name.eq_ignore_ascii_case(name))
    }
}

/// How clients name the lead agent whatever it is called (`/api/agents/lead/...`).
pub const LEAD_KEY: &str = "lead";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error("agent names are 1 to {MAX_NAME_CHARS} letters, digits, spaces, `-`, `_` or `.`, starting with a letter or digit")]
    BadName,
    #[error("`{0}` looks like an agent id; pick a name that doesn't")]
    IdShapedName(String),
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
    /// Bumped after every saved change, and when an agent's status may have changed
    /// (see [`AgentRegistry::changes`]).
    changed: tokio::sync::watch::Sender<u64>,
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
                ..RegistryFile::default()
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
        // Agents from before icons get the default for their id, saved once.
        let mut migrated = false;
        for a in &mut file.agents {
            let icon = a.icon();
            if a.icon.as_ref() != Some(&icon) {
                a.icon = Some(icon);
                migrated = true;
            }
        }
        let registry = Self {
            inner: Arc::new(Inner {
                path,
                file: Mutex::new(file),
                changed: tokio::sync::watch::Sender::new(0),
            }),
        };
        if migrated {
            let file = lock(&registry.inner.file);
            registry.save(&file)?;
        }
        Ok(registry)
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

    /// A helper by id or by its exact current name (see [`Self::lookup`]); the lead agent is
    /// [`ResolveError::Lead`].
    pub fn resolve(&self, name_or_id: &str) -> Result<AgentRecord, ResolveError> {
        match self.lookup(name_or_id)?.who {
            Who::Agent(a) => Ok(*a),
            Who::Lead => Err(ResolveError::Lead(self.lead_name())),
        }
    }

    /// Who `key` names, strictly (2026-11-21): `lead`, an agent id, or a name that is the
    /// current name of exactly one agent (ignoring case) and nobody's former name. A former
    /// name is refused with the agent's new name and id ([`ResolveError::Renamed`]), the name
    /// of a deleted agent likewise ([`ResolveError::Deleted`]), and a name that points at more
    /// than one agent (one has it now, another had it before) is [`ResolveError::Ambiguous`]:
    /// never a silent pick.
    pub fn lookup(&self, key: &str) -> Result<Found, ResolveError> {
        let key = key.trim();
        let file = lock(&self.inner.file);
        if key.is_empty() {
            return Err(ResolveError::NotFound(key.into()));
        }
        if key.eq_ignore_ascii_case(LEAD_KEY) {
            return Ok(Found::new(Who::Lead, file.lead_name(), false));
        }
        if let Some(a) = file.agents.iter().find(|a| a.id == key) {
            return Ok(Found::new(Who::Agent(Box::new(a.clone())), &a.name, false));
        }
        if let Some(d) = file.deleted.iter().find(|d| d.id == key) {
            return Err(ResolveError::Deleted {
                key: key.into(),
                id: d.id.clone(),
            });
        }
        let same = |n: &str| n.eq_ignore_ascii_case(key);
        let mut current: Option<Who> = None;
        // Every other agent that had the name, by label.
        let mut former: Vec<String> = Vec::new();
        let mut deleted: Vec<String> = Vec::new();
        if same(file.lead_name()) {
            current = Some(Who::Lead);
        } else if file.lead_former().into_iter().any(same) {
            former.push(label(file.lead_name(), LEAD_KEY));
        }
        for a in &file.agents {
            if same(&a.name) {
                current = Some(Who::Agent(Box::new(a.clone())));
            } else if a.former_names.iter().any(|n| same(n)) {
                former.push(a.label());
            }
        }
        for d in &file.deleted {
            if d.names.iter().any(|n| same(n)) {
                deleted.push(d.id.clone());
            }
        }
        let current_label = |w: &Who| match w {
            Who::Lead => label(file.lead_name(), LEAD_KEY),
            Who::Agent(a) => a.label(),
        };
        match (current, former.len(), deleted.len()) {
            (Some(who), 0, 0) => {
                let name = match &who {
                    Who::Lead => file.lead_name().to_string(),
                    Who::Agent(a) => a.name.clone(),
                };
                Ok(Found::new(who, &name, true))
            }
            (None, 0, 0) => Err(ResolveError::NotFound(key.into())),
            (None, 1, 0) => Err(ResolveError::Renamed {
                key: key.into(),
                now: former.remove(0),
            }),
            (None, 0, 1) => Err(ResolveError::Deleted {
                key: key.into(),
                id: deleted.remove(0),
            }),
            (current, _, _) => {
                let mut candidates = Vec::new();
                if let Some(who) = &current {
                    candidates.push(format!("{} is called that now", current_label(who)));
                }
                for l in former {
                    candidates.push(format!("{l} was called that before"));
                }
                for id in deleted {
                    candidates.push(format!("a deleted agent ({id}) was called that"));
                }
                Err(ResolveError::Ambiguous {
                    key: key.into(),
                    candidates,
                })
            }
        }
    }

    /// `Ada (d-0a1b2c3d.a1)` for agent `id`, `Melissa (lead)` for the lead agent, the id alone
    /// for an agent that is gone.
    pub fn label_of(&self, id: &str) -> String {
        let file = lock(&self.inner.file);
        if id == LEAD_KEY {
            return label(file.lead_name(), LEAD_KEY);
        }
        file.agents
            .iter()
            .find(|a| a.id == id)
            .map_or_else(|| id.to_string(), AgentRecord::label)
    }

    /// Make a new agent. Its id is saved before this returns.
    pub fn create(
        &self,
        name: &str,
        backend: &str,
        tools: Option<Vec<String>>,
        created_by: &str,
    ) -> Result<AgentRecord, RegistryError> {
        self.create_with(name, backend, tools, created_by, None)
    }

    /// [`Self::create`] with the icon the user picked (None: the default for its id).
    pub fn create_with(
        &self,
        name: &str,
        backend: &str,
        tools: Option<Vec<String>>,
        created_by: &str,
        icon: Option<AgentIcon>,
    ) -> Result<AgentRecord, RegistryError> {
        let name = check_name(name)?;
        let mut file = lock(&self.inner.file);
        if file.name_taken(&name, None) {
            return Err(RegistryError::NameTaken(name));
        }
        let id = agent_id(&file.device, file.next);
        let icon = icon.unwrap_or_else(|| AgentIcon::default_for(&id));
        let record = AgentRecord {
            id,
            name,
            backend: backend.into(),
            model: None,
            effort: None,
            fast: false,
            tools,
            created_by: created_by.into(),
            creator: crate::audit::requester(),
            created_at: now(),
            stopped: false,
            icon: Some(icon),
            pinned: false,
            former_names: Vec::new(),
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
            .map_err(|_| RegistryError::NoSuchAgent(name_or_id.into()))?
            .id;
        let mut file = lock(&self.inner.file);
        if file.name_taken(&new_name, Some(&id)) {
            return Err(RegistryError::NameTaken(new_name));
        }
        let agent = file
            .agents
            .iter_mut()
            .find(|b| b.id == id)
            .ok_or_else(|| RegistryError::NoSuchAgent(name_or_id.into()))?;
        remember_former(&mut agent.former_names, &agent.name, &new_name);
        agent.name = new_name;
        let out = agent.clone();
        self.save(&file)?;
        Ok(out)
    }

    /// The lead agent's display name ([`LEAD_DEFAULT_NAME`] until the user renames it).
    pub fn lead_name(&self) -> String {
        lock(&self.inner.file).lead_name().to_string()
    }

    /// `key` names the lead agent ([`Self::lookup`]: `lead`, or its current name while it
    /// is nobody else's former name). Ignores case.
    pub fn is_lead(&self, key: &str) -> bool {
        matches!(self.lookup(key), Ok(Found { who: Who::Lead, .. }))
    }

    /// The user gives the lead agent a new display name (same rules as a helper's, unique
    /// among the lead agent and every helper). Returns the old and the new name.
    pub fn rename_lead(&self, new_name: &str) -> Result<(String, String), RegistryError> {
        let new_name = check_name(new_name)?;
        let mut file = lock(&self.inner.file);
        if file.name_taken(&new_name, Some(LEAD_KEY)) {
            return Err(RegistryError::NameTaken(new_name));
        }
        let old = file.lead_name().to_string();
        let mut next = file.clone();
        remember_former(&mut next.lead_former_names, &old, &new_name);
        next.lead_name = (new_name != LEAD_DEFAULT_NAME).then(|| new_name.clone());
        self.save(&next)?;
        std::mem::swap(&mut *file, &mut next);
        Ok((old, new_name))
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
        agent.fast = choice.fast;
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
        let mut next = file.clone();
        let gone = next.agents.remove(i);
        let mut names = gone.former_names.clone();
        names.push(gone.name.clone());
        next.deleted.push(DeletedAgent {
            id: gone.id.clone(),
            names,
        });
        self.save(&next)?;
        *file = next;
        Ok(gone)
    }

    /// The user gives an agent another icon.
    pub fn set_icon(&self, id: &str, icon: AgentIcon) -> Result<AgentRecord, RegistryError> {
        self.update(id, |a| a.icon = Some(icon))
    }

    /// The user pins an agent in the sidebar (or unpins it).
    pub fn set_pinned(&self, id: &str, pinned: bool) -> Result<AgentRecord, RegistryError> {
        self.update(id, |a| a.pinned = pinned)
    }

    fn update(
        &self,
        id: &str,
        change: impl FnOnce(&mut AgentRecord),
    ) -> Result<AgentRecord, RegistryError> {
        let mut file = lock(&self.inner.file);
        let agent = file
            .agents
            .iter_mut()
            .find(|b| b.id == id)
            .ok_or_else(|| RegistryError::NoSuchAgent(id.into()))?;
        change(agent);
        let out = agent.clone();
        self.save(&file)?;
        Ok(out)
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

    /// Wakes whenever an agent was created, renamed, moved, stopped or deleted, or its status
    /// may have changed ([`Self::touch`]): live views (the UI's rail) re-read the list then.
    /// Changes are seen once the change is complete (they are made under the registry lock).
    pub fn changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.changed.subscribe()
    }

    /// Wake [`Self::changes`] watchers (an agent's status may have changed).
    pub fn touch(&self) {
        self.inner.changed.send_modify(|v| *v = v.wrapping_add(1));
    }

    fn save(&self, file: &RegistryFile) -> Result<(), RegistryError> {
        let io = |reason: String| RegistryError::Io {
            path: self.inner.path.display().to_string(),
            reason,
        };
        let json = serde_json::to_vec_pretty(file).map_err(|e| io(e.to_string()))?;
        write_private(&self.inner.path, &json).map_err(|e| io(e.to_string()))?;
        self.touch();
        Ok(())
    }
}

impl AgentNames for AgentRegistry {
    fn name_of(&self, id: &str) -> Option<String> {
        self.get(id).map(|b| b.name)
    }

    fn resolve_id(&self, name_or_id: &str) -> Result<String, String> {
        self.resolve(name_or_id)
            .map(|b| b.id)
            .map_err(|e| e.to_string())
    }
}

/// What a reference to an agent names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Who {
    Lead,
    Agent(Box<AgentRecord>),
}

/// An agent found by [`AgentRegistry::lookup`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub who: Who,
    /// `Ada (d-0a1b2c3d.a1)`, or `Melissa (lead)`.
    pub label: String,
    /// Found by its name, not its id: tool results say which id was used.
    pub by_name: bool,
}

impl Found {
    fn new(who: Who, name: &str, by_name: bool) -> Self {
        let id = match &who {
            Who::Lead => LEAD_KEY,
            Who::Agent(a) => a.id.as_str(),
        };
        Self {
            label: label(name, id),
            who,
            by_name,
        }
    }

    /// `"iris" → Iris (d-0a1b2c3d.a3)` for a tool result when the agent was named by its
    /// name (None when named by its id).
    pub fn note(&self, key: &str) -> Option<String> {
        self.by_name
            .then(|| format!("\"{}\" → {}; next time use the id", key.trim(), self.label))
    }
}

/// Why a reference to an agent names nobody, or more than one agent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    #[error("no agent `{0}` on this device")]
    NotFound(String),
    /// A former name of one agent, which nobody has now.
    #[error("`{key}` is now called {now}; use the id")]
    Renamed { key: String, now: String },
    #[error("agent `{key}` was deleted (it was {id}); there is no agent by that name now")]
    Deleted { key: String, id: String },
    /// More than one agent has or had the name.
    #[error("`{key}` is ambiguous: {}; use the id", .candidates.join(", "))]
    Ambiguous {
        key: String,
        candidates: Vec<String>,
    },
    /// The lead agent, where only a helper will do.
    #[error("{0} is the lead agent (id `lead`), not a helper")]
    Lead(String),
}

/// After a rename from `old` to `new`: `old` joins the former names, `new` leaves them.
fn remember_former(former: &mut Vec<String>, old: &str, new: &str) {
    former.retain(|n| !n.eq_ignore_ascii_case(new));
    if !old.eq_ignore_ascii_case(new) && !former.iter().any(|n| n.eq_ignore_ascii_case(old)) {
        former.push(old.to_string());
    }
}

/// `name` looks like an agent or device id (`d-` and eight hex digits, any case): names and
/// ids must never be confused (`lead` itself is taken by the lead agent).
fn id_shaped(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.strip_prefix("d-").is_some_and(|rest| {
        rest.len() >= 8 && rest.as_bytes()[..8].iter().all(u8::is_ascii_hexdigit)
    })
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
    if !ok {
        Err(RegistryError::BadName)
    } else if id_shaped(name) {
        Err(RegistryError::IdShapedName(name.to_string()))
    } else {
        Ok(name.to_string())
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
            Ok(bo.id.as_str())
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

    #[test]
    fn the_lead_agent_has_a_name_unique_among_all_and_kept() {
        let dir = std::env::temp_dir().join(format!("melissa-lead-name-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let reg = AgentRegistry::open(&dir, "d-0a1b2c3d").unwrap();
        assert_eq!(reg.lead_name(), LEAD_DEFAULT_NAME);
        // Nobody else may take the lead agent's name, or `lead`.
        for taken in ["melissa", "Lead"] {
            assert!(matches!(
                reg.create(taken, "b", None, "the user"),
                Err(RegistryError::NameTaken(_))
            ));
        }
        reg.create("Bo", "b", None, "the user").unwrap();
        assert!(matches!(
            reg.rename_lead("bo"),
            Err(RegistryError::NameTaken(_))
        ));
        assert_eq!(reg.rename_lead("x!"), Err(RegistryError::BadName));
        assert_eq!(
            reg.rename_lead("Athena").unwrap(),
            ("Melissa".to_string(), "Athena".to_string())
        );
        // Its old name is free now; its new one is taken.
        assert!(matches!(
            reg.rename("Bo", "athena"),
            Err(RegistryError::NameTaken(_))
        ));
        // Its default name is a former name now: refused with its new name, never a guess.
        assert!(!reg.is_lead("Melissa"));
        assert_eq!(
            reg.lookup("melissa").unwrap_err().to_string(),
            "`melissa` is now called Athena (lead); use the id"
        );
        let mel = reg.create("Melissa", "b", None, "the user").unwrap();
        assert!(!reg.is_lead("melissa"));
        assert!(matches!(
            reg.lookup("Melissa"),
            Err(ResolveError::Ambiguous { .. })
        ));
        assert_eq!(reg.resolve(&mel.id).unwrap().name, "Melissa");
        assert!(reg.is_lead("athena") && reg.is_lead("LEAD"));
        assert_eq!(reg.label_of(LEAD_KEY), "Athena (lead)");
        // Its own name in another case is fine.
        assert_eq!(reg.rename_lead("ATHENA").unwrap().1, "ATHENA");

        let again = AgentRegistry::open(&dir, "d-0a1b2c3d").unwrap();
        assert_eq!(again.lead_name(), "ATHENA");
        assert_eq!(again.list().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The owner's resilience case (2026-11-21): Iris is renamed Ada, a new agent takes the
    /// name Iris; an old "message Iris" must not reach either one silently.
    #[test]
    fn a_reused_name_is_refused_not_misrouted() {
        let d = dir("reuse");
        let reg = AgentRegistry::open(&d, DEVICE).unwrap();
        let first = reg.create("Iris", "x", None, "the user").unwrap();
        let found = reg.lookup("iris").unwrap();
        assert_eq!(found.who, Who::Agent(Box::new(first.clone())));
        assert_eq!(
            found.note("iris").unwrap(),
            "\"iris\" → Iris (d-0a1b2c3d.a1); next time use the id"
        );
        assert_eq!(reg.lookup(&first.id).unwrap().note(&first.id), None);

        reg.rename(&first.id, "Ada").unwrap();
        // The old name, nobody's now: refused with the new name and id.
        let err = reg.lookup("Iris").unwrap_err();
        assert_eq!(
            err,
            ResolveError::Renamed {
                key: "Iris".into(),
                now: "Ada (d-0a1b2c3d.a1)".into()
            }
        );
        assert_eq!(
            err.to_string(),
            "`Iris` is now called Ada (d-0a1b2c3d.a1); use the id"
        );
        assert!(reg.resolve("Iris").is_err());
        assert_eq!(AgentNames::resolve_id(&reg, "ada").unwrap(), first.id);

        // A new Iris: the name is ambiguous, both are named with ids; the ids still work.
        let second = reg.create("Iris", "x", None, "the user").unwrap();
        let err = reg.lookup("iris").unwrap_err();
        assert_eq!(
            err.to_string(),
            "`iris` is ambiguous: Iris (d-0a1b2c3d.a2) is called that now, Ada (d-0a1b2c3d.a1) \
             was called that before; use the id"
        );
        assert!(AgentNames::resolve_id(&reg, "Iris").is_err());
        assert_eq!(reg.resolve(&second.id).unwrap().name, "Iris");
        assert_eq!(reg.resolve(&first.id).unwrap().name, "Ada");

        // Former names survive a restart.
        let reg = AgentRegistry::open(&d, DEVICE).unwrap();
        assert_eq!(reg.get(&first.id).unwrap().former_names, vec!["Iris"]);
        assert!(matches!(
            reg.lookup("Iris"),
            Err(ResolveError::Ambiguous { .. })
        ));

        // Renaming back makes it a current name again (and no longer a former one).
        reg.rename(&second.id, "Juno").unwrap();
        reg.rename(&first.id, "Iris").unwrap();
        assert!(reg.get(&first.id).unwrap().former_names == vec!["Ada"]);
        assert!(
            matches!(reg.lookup("Iris"), Err(ResolveError::Ambiguous { .. })),
            "the second agent was called Iris too"
        );

        // A deleted agent's names never reach a newer agent.
        reg.remove(&second.id).unwrap();
        assert_eq!(
            reg.lookup("juno").unwrap_err(),
            ResolveError::Deleted {
                key: "juno".into(),
                id: second.id.clone()
            }
        );
        assert!(matches!(
            reg.lookup(&second.id),
            Err(ResolveError::Deleted { .. })
        ));
        let third = reg.create("Juno", "x", None, "the user").unwrap();
        let err = reg.lookup("Juno").unwrap_err().to_string();
        assert!(
            err.contains(&third.id) && err.contains(&second.id) && err.contains("deleted"),
            "{err}"
        );
        assert!(reg.lookup("Nobody").unwrap_err() == ResolveError::NotFound("Nobody".into()));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn names_that_look_like_ids_are_refused() {
        let d = dir("idnames");
        let reg = AgentRegistry::open(&d, DEVICE).unwrap();
        for bad in [
            "d-0a1b2c3d.a1",
            "D-0A1B2C3D.A7",
            "d-0a1b2c3d",
            "d-ffffffff.a12 x",
        ] {
            assert_eq!(
                reg.create(bad, "x", None, "the user"),
                Err(RegistryError::IdShapedName(bad.into())),
                "{bad}"
            );
        }
        let ok = reg.create("d-day", "x", None, "the user").unwrap();
        assert_eq!(
            reg.rename(&ok.id, "d-12345678.a3"),
            Err(RegistryError::IdShapedName("d-12345678.a3".into()))
        );
        assert_eq!(
            reg.rename_lead("d-12345678"),
            Err(RegistryError::IdShapedName("d-12345678".into()))
        );
        std::fs::remove_dir_all(d).unwrap();
    }
}
