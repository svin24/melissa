//! Standing permissions the owner grants: "Always allow" rules, the developer bypass
//! ("No permission prompts") and per-agent Full access.
//!
//! Full access (2026-11-20, owner request) is the bypass scoped to one agent (or the lead
//! agent): its *own* confirmations are approved without asking, audited as `bypass`. It needs
//! the developer option "Full access per agent" ([`Permissions::full_access_allowed`]);
//! switching that off puts every agent back on Ask. It never answers the user-only
//! confirmations (another agent's workspace), owner-only steps (schedules, moving
//! workspaces, deleting agents) or the lead agent's calls on its agents' workspaces: those
//! are not the agent's own ([`super::ToolRegistry`] decides which confirmations are).
//!
//! Both only answer *confirmations*: a call reaches them only after the scope check and the
//! argument check passed, so neither can turn a refusal ("not yours", bad arguments, a
//! permission-mode switch) into a yes. Nothing here is reachable from a tool: rules and the
//! bypass change only through the owner's UI and API ([`super::ToolRegistry::always_allow`],
//! [`super::ToolRegistry::remove_rule`], [`super::ToolRegistry::set_bypass`]).
//!
//! A rule is as narrow as a confirmation can be described: who asks ([`Principal`]: an agent
//! by id, so a rename keeps it, or the lead agent), which tool, and the resource the tool
//! works on (the workspace, for tools that have one). Saved as JSON (mode 0600) in the state
//! directory, never in the repo.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use super::scope::{Scope, Target};

/// Who asks for a confirmation, for rules.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Principal {
    /// The lead agent of this device.
    Lead,
    /// The agent with this id (`<device>.a<n>`; ids are never reused).
    Agent(String),
}

impl Principal {
    /// The principal of a task scope. The user and scope-less tasks get no rules.
    pub fn of(scope: &Scope) -> Option<Self> {
        match scope {
            Scope::Lead { .. } => Some(Self::Lead),
            Scope::Agent { id } => Some(Self::Agent(id.clone())),
            Scope::User | Scope::Nothing => None,
        }
    }
}

/// What a rule matches: exactly this principal, tool and target.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ActionKey {
    pub principal: Principal,
    /// The tool's registry name (e.g. `update_app`), or `agent:<kind>` for an agent CLI's own
    /// tools (shell, file edits, ...).
    pub tool: String,
    /// The resource the call works on (e.g. the workspace `minecraft`); None for tools
    /// without one, and then only calls without one match.
    pub target: Option<String>,
}

/// The rule an "Always allow" on a confirmation would save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleOffer {
    pub key: ActionKey,
    /// What the call does, for the rule's wording ("update", "run commands").
    pub verb: String,
}

impl RuleOffer {
    /// The offer for a confirmation of `tool` asked by a task with `scope`. `target`: the
    /// tool's declared target and the resource its arguments name. None for the user's own
    /// calls (nothing to remember: the user is asked only by the label policy).
    pub fn new(scope: &Scope, tool: &str, target: Option<(&Target, &str)>) -> Option<Self> {
        let principal = Principal::of(scope)?;
        let verb = match target {
            Some((t, _)) => t.verb.trim_end_matches(" over").to_string(),
            None => tool_verb(tool),
        };
        Some(Self {
            key: ActionKey {
                principal,
                tool: tool.to_string(),
                target: target.map(|(_, r)| r.to_string()),
            },
            verb,
        })
    }
}

/// Words for a tool without a target.
fn tool_verb(tool: &str) -> String {
    match tool {
        "agent:edit" => "edit files".into(),
        "agent:delete" => "delete files".into(),
        "agent:move" => "move files".into(),
        "agent:execute" => "run commands".into(),
        "agent:fetch" => "fetch from the web".into(),
        "agent:read" => "read files".into(),
        other => format!("use {other}"),
    }
}

/// A saved "Always allow" rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// Never reused.
    pub id: u64,
    #[serde(flatten)]
    pub key: ActionKey,
    pub verb: String,
    /// Unix seconds.
    pub created: u64,
}

/// Why a confirmation was answered without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grant {
    /// The rule with this id matched.
    Rule(u64),
    /// No permission prompts (developer mode) is on.
    Bypass,
    /// The asking agent is on Full access (its own confirmations only).
    Full,
}

impl Grant {
    /// The audit actor: `rule` or `bypass` (Full access is a bypass for one agent).
    pub fn actor(self) -> &'static str {
        match self {
            Self::Rule(_) => "rule",
            Self::Bypass | Self::Full => "bypass",
        }
    }

    /// For audit details: `rule 3`, `bypass` or `bypass (full access)`.
    pub fn describe(self) -> String {
        match self {
            Self::Rule(id) => format!("rule {id}"),
            Self::Bypass => "bypass".into(),
            Self::Full => "bypass (full access)".into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PermissionsError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("{path} is not valid: {message}")]
    Parse { path: String, message: String },
    #[error("cannot save {path}: {source}")]
    Write {
        path: String,
        source: std::io::Error,
    },
    #[error("Full access per agent is off (Settings → Developer)")]
    FullAccessOff,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    rules: Vec<Rule>,
    /// No permission prompts (developer mode).
    #[serde(default)]
    bypass: bool,
    /// Developer option: the owner may put single agents on Full access.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    full_access_allowed: bool,
    /// Who is on Full access (sorted, once each). Empty while the option is off.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    full_access: Vec<Principal>,
}

/// The owner's standing permissions. Cheap to clone; clones share state.
#[derive(Clone, Default)]
pub struct Permissions {
    state: Arc<Mutex<State>>,
    /// Where they are saved (None: in memory only, e.g. tests and one-shot operator calls).
    path: Option<PathBuf>,
}

impl Permissions {
    /// In memory only: no rules, bypass off.
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Load from `path` (a missing file: nothing granted). Changes are saved there.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, PermissionsError> {
        use melissa_statefile::FormatError;
        let path = path.into();
        let parse = |message: String| PermissionsError::Parse {
            path: path.display().to_string(),
            message,
        };
        let state = match melissa_statefile::read_json::<State>(&path, &crate::formats::PERMISSIONS)
        {
            // Grants a newer Melissa saved are not read leniently: a field this version does
            // not know may narrow them. The caller starts with none (fails closed).
            Ok(Some(l)) if l.read_only => {
                return Err(parse(format!(
                    "saved by a newer version of Melissa (format {}; this version reads up to \
                     {}), left as it is",
                    l.found,
                    crate::formats::PERMISSIONS.current
                )))
            }
            Ok(l) => l.map(|l| l.value).unwrap_or_default(),
            Err(FormatError::Read { source, .. }) => {
                return Err(PermissionsError::Read {
                    path: path.display().to_string(),
                    source,
                })
            }
            Err(e) => return Err(parse(e.to_string())),
        };
        Ok(Self {
            state: Arc::new(Mutex::new(state)),
            path: Some(path),
        })
    }

    /// Where the permissions are saved, if anywhere.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A poisoned lock only means a panic elsewhere; the state is still usable.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Save `next` and only then make it current, so memory never grants more than the file
    /// (a failed save changes nothing).
    fn commit(
        &self,
        guard: &mut MutexGuard<'_, State>,
        next: State,
    ) -> Result<(), PermissionsError> {
        if let Some(path) = &self.path {
            melissa_statefile::save_json(path, &crate::formats::PERMISSIONS, &next, true).map_err(
                |e| PermissionsError::Write {
                    path: path.display().to_string(),
                    source: std::io::Error::other(e),
                },
            )?;
        }
        **guard = next;
        Ok(())
    }

    /// Does a standing permission answer a confirmation for `key` (None: the call has no
    /// principal, only the bypass applies)? A matching rule wins over the bypass, so the audit
    /// names it.
    pub fn grant(&self, key: Option<&ActionKey>) -> Option<Grant> {
        let state = self.lock();
        if let Some(key) = key {
            if let Some(rule) = state.rules.iter().find(|r| &r.key == key) {
                return Some(Grant::Rule(rule.id));
            }
        }
        state.bypass.then_some(Grant::Bypass)
    }

    /// [`Self::grant`] for the asking agent's own confirmation: also its Full access. Needs
    /// `key` (who asks); a confirmation without one is never an agent's own.
    pub fn grant_own(&self, key: Option<&ActionKey>) -> Option<Grant> {
        self.grant(key).or_else(|| {
            let key = key?;
            self.has_full_access(&key.principal).then_some(Grant::Full)
        })
    }

    /// Is the developer option "Full access per agent" on?
    pub fn full_access_allowed(&self) -> bool {
        self.lock().full_access_allowed
    }

    /// Is `who` on Full access (and the option on)?
    pub fn has_full_access(&self, who: &Principal) -> bool {
        let state = self.lock();
        state.full_access_allowed && state.full_access.contains(who)
    }

    /// Everyone on Full access.
    pub fn full_access(&self) -> Vec<Principal> {
        let state = self.lock();
        if state.full_access_allowed {
            state.full_access.clone()
        } else {
            Vec::new()
        }
    }

    /// Switch the developer option. Off puts everyone back on Ask (so turning it on again
    /// never brings back an old Full access). Returns whether it changed.
    pub(crate) fn set_full_access_allowed(&self, on: bool) -> Result<bool, PermissionsError> {
        let mut guard = self.lock();
        if guard.full_access_allowed == on && (on || guard.full_access.is_empty()) {
            return Ok(false);
        }
        let mut next = guard.clone();
        next.full_access_allowed = on;
        if !on {
            next.full_access.clear();
        }
        self.commit(&mut guard, next)?;
        Ok(true)
    }

    /// Put `who` on Full access (`on`) or back on Ask. Full access needs the developer
    /// option. Returns whether it changed.
    pub(crate) fn set_full_access(
        &self,
        who: &Principal,
        on: bool,
    ) -> Result<bool, PermissionsError> {
        let mut guard = self.lock();
        if on && !guard.full_access_allowed {
            return Err(PermissionsError::FullAccessOff);
        }
        if guard.full_access.contains(who) == on {
            return Ok(false);
        }
        let mut next = guard.clone();
        if on {
            next.full_access.push(who.clone());
            next.full_access.sort();
        } else {
            next.full_access.retain(|p| p != who);
        }
        self.commit(&mut guard, next)?;
        Ok(true)
    }

    /// Every rule, oldest first.
    pub fn rules(&self) -> Vec<Rule> {
        self.lock().rules.clone()
    }

    /// Is No permission prompts on?
    pub fn bypass(&self) -> bool {
        self.lock().bypass
    }

    /// Save the rule `offer` describes. The same rule twice is saved once (the existing one
    /// is returned with `false`).
    pub(crate) fn add(&self, offer: &RuleOffer) -> Result<(Rule, bool), PermissionsError> {
        let mut guard = self.lock();
        if let Some(rule) = guard.rules.iter().find(|r| r.key == offer.key) {
            return Ok((rule.clone(), false));
        }
        let mut next = guard.clone();
        next.next_id += 1;
        let rule = Rule {
            id: next.next_id,
            key: offer.key.clone(),
            verb: offer.verb.clone(),
            created: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        };
        next.rules.push(rule.clone());
        self.commit(&mut guard, next)?;
        Ok((rule, true))
    }

    /// Remove rule `id`; None if there is no such rule.
    pub(crate) fn remove(&self, id: u64) -> Result<Option<Rule>, PermissionsError> {
        let mut guard = self.lock();
        let Some(i) = guard.rules.iter().position(|r| r.id == id) else {
            return Ok(None);
        };
        let mut next = guard.clone();
        let rule = next.rules.remove(i);
        self.commit(&mut guard, next)?;
        Ok(Some(rule))
    }

    /// Switch No permission prompts. Returns whether it changed.
    pub(crate) fn set_bypass(&self, on: bool) -> Result<bool, PermissionsError> {
        let mut guard = self.lock();
        if guard.bypass == on {
            return Ok(false);
        }
        let mut next = guard.clone();
        next.bypass = on;
        self.commit(&mut guard, next)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::scope::{Access, ResourceKind};

    const UPDATE: Target = Target {
        kind: ResourceKind::Workspace,
        arg: "app",
        access: Access::Change,
        verb: "update",
    };

    fn agent(id: &str) -> Scope {
        Scope::Agent { id: id.into() }
    }

    fn offer(scope: &Scope, tool: &str, target: Option<&str>) -> RuleOffer {
        RuleOffer::new(scope, tool, target.map(|t| (&UPDATE, t))).unwrap()
    }

    #[test]
    fn rules_match_only_their_agent_tool_and_target() {
        let p = Permissions::in_memory();
        let ada = agent("d-0a1b2c3d.a1");
        let (rule, new) = p
            .add(&offer(&ada, "update_app", Some("minecraft")))
            .unwrap();
        assert!(new);
        assert_eq!(rule.verb, "update");
        let grant = |s: &Scope, tool: &str, target: Option<&str>| {
            p.grant(Some(&offer(s, tool, target).key))
        };
        assert_eq!(
            grant(&ada, "update_app", Some("minecraft")),
            Some(Grant::Rule(rule.id))
        );
        // Another agent, app, tool, or the lead agent: no.
        assert_eq!(
            grant(&agent("d-0a1b2c3d.a2"), "update_app", Some("minecraft")),
            None
        );
        assert_eq!(grant(&ada, "update_app", Some("nextcloud")), None);
        assert_eq!(grant(&ada, "uninstall_app", Some("minecraft")), None);
        assert_eq!(grant(&ada, "update_app", None), None);
        let lead = Scope::Lead {
            device: "d-0a1b2c3d".into(),
        };
        assert_eq!(grant(&lead, "update_app", Some("minecraft")), None);
        // The user's calls have no principal: no rule, nothing to offer.
        assert!(RuleOffer::new(&Scope::User, "update_app", None).is_none());
        assert_eq!(p.grant(None), None);
        // The same rule again is not saved twice.
        let (again, new) = p
            .add(&offer(&ada, "update_app", Some("minecraft")))
            .unwrap();
        assert!(!new);
        assert_eq!(again.id, rule.id);
        assert_eq!(p.rules().len(), 1);
    }

    #[test]
    fn bypass_grants_everything_and_rules_win() {
        let p = Permissions::in_memory();
        let ada = agent("d-0a1b2c3d.a1");
        let key = offer(&ada, "agent:execute", None).key;
        assert_eq!(p.grant(Some(&key)), None);
        assert!(p.set_bypass(true).unwrap());
        assert!(!p.set_bypass(true).unwrap());
        assert_eq!(p.grant(Some(&key)), Some(Grant::Bypass));
        assert_eq!(p.grant(None), Some(Grant::Bypass));
        let (rule, _) = p.add(&offer(&ada, "agent:execute", None)).unwrap();
        assert_eq!(rule.verb, "run commands");
        assert_eq!(p.grant(Some(&key)), Some(Grant::Rule(rule.id)));
    }

    #[test]
    fn full_access_is_one_agents_own_and_needs_the_developer_option() {
        let p = Permissions::in_memory();
        let ada = Principal::Agent("d-0a1b2c3d.a1".into());
        let key = |who: &Principal| ActionKey {
            principal: who.clone(),
            tool: "agent:execute".into(),
            target: None,
        };
        // Off: Full access cannot be given.
        assert!(matches!(
            p.set_full_access(&ada, true),
            Err(PermissionsError::FullAccessOff)
        ));
        assert!(p.set_full_access_allowed(true).unwrap());
        assert!(p.set_full_access(&ada, true).unwrap());
        assert!(!p.set_full_access(&ada, true).unwrap());
        assert!(p.has_full_access(&ada));
        // Only Ada's own confirmations, and only through `grant_own`.
        assert_eq!(p.grant_own(Some(&key(&ada))), Some(Grant::Full));
        assert_eq!(p.grant(Some(&key(&ada))), None);
        assert_eq!(p.grant_own(None), None);
        let bo = Principal::Agent("d-0a1b2c3d.a2".into());
        assert_eq!(p.grant_own(Some(&key(&bo))), None);
        assert_eq!(p.grant_own(Some(&key(&Principal::Lead))), None);
        assert_eq!(Grant::Full.actor(), "bypass");
        // Switching the option off puts everyone back on Ask, for good.
        assert!(p.set_full_access_allowed(false).unwrap());
        assert_eq!(p.grant_own(Some(&key(&ada))), None);
        assert!(p.set_full_access_allowed(true).unwrap());
        assert!(!p.has_full_access(&ada));
        assert!(p.full_access().is_empty());
    }

    #[test]
    fn saved_across_restarts_and_ids_never_reused() {
        let dir = std::env::temp_dir().join(format!(
            "melissa-permissions-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = dir.join("permissions.json");
        let _ = std::fs::remove_dir_all(&dir);
        let ada = agent("d-0a1b2c3d.a1");
        {
            let p = Permissions::open(&path).unwrap();
            let (a, _) = p.add(&offer(&ada, "update_app", Some("a"))).unwrap();
            p.add(&offer(&ada, "update_app", Some("b"))).unwrap();
            p.remove(a.id).unwrap().unwrap();
            p.set_bypass(true).unwrap();
        }
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let p = Permissions::open(&path).unwrap();
        assert!(p.bypass());
        let rules = p.rules();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].key.target.as_deref(), Some("b"));
        assert_eq!(
            p.grant(Some(&offer(&ada, "update_app", Some("b")).key)),
            Some(Grant::Rule(rules[0].id))
        );
        // Ids keep counting after a restart.
        let (c, _) = p.add(&offer(&ada, "update_app", Some("c"))).unwrap();
        assert_eq!(c.id, 3);
        // A broken file is an error, not "nothing granted" silently overwritten.
        std::fs::write(&path, b"{not json").unwrap();
        assert!(matches!(
            Permissions::open(&path),
            Err(PermissionsError::Parse { .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
