//! Workspace ownership and agent scopes: an agent changes only the workspaces it looks after.
//!
//! The cluster model (docs/AGENT-MODEL.md): every **workspace** (today: an installed app) is owned by
//! exactly one **agent**; an agent may change only its own workspaces; only the **Lead agent** or the
//! **user** reassign workspaces. Every task has a [`Scope`] that says who it is. A tool declares
//! which argument names its workspace ([`Tool::target`]); the [`super::ToolRegistry`] checks the
//! scope **before** the label policy, for every tool, in one place ([`check`]):
//!
//! - an agent on another agent's (or an unassigned) workspace → refused outright
//!   (`ToolError::Refused("not yours: …")`, audited `tool.refused`), never a confirmation a
//!   tired human might click through;
//! - the lead agent on an agent's workspace → allowed; if the label policy asks the user anyway
//!   (destructive actions always do), the confirmation names the owning agent
//!   ([`ScopeDecision::Note`]);
//! - the lead agent on an unassigned workspace (from before agents existed) → a confirmation: only the
//!   user decides about those;
//! - otherwise → the usual label policy.
//!
//! Ownership is stored with the workspace (the `melissa.io/owner-agent` label on the app namespace)
//! and read through [`ResourceOwners`]. A workspace without that label is [`Owner::Unassigned`].
//!
//! [`Tool::target`]: super::Tool::target

use crate::{device::device_of, device::valid_agent_id, BoxFuture};

/// Kinds of resources with owners. New kinds (backups, agent-built apps, ...) add a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ResourceKind {
    /// A workspace: an installed app (`app-<name>` namespace) and its services.
    Workspace,
}

impl ResourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
        }
    }
}

/// What a call does to its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Names, phases, counts: any task may read any workspace (e.g. `app_status`).
    Read,
    /// Reads content from the workspace that the workspace controls (e.g. `app_logs`): its own agent,
    /// the lead agent and the user.
    ReadScoped,
    /// Creates, changes or deletes the workspace (install over, uninstall, update, restart,
    /// scale, stop).
    Change,
}

/// The workspace a tool works on, declared once per tool ([`super::Tool::target`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub kind: ResourceKind,
    /// Name of the (string) argument that names the workspace, e.g. `app`.
    pub arg: &'static str,
    pub access: Access,
    /// What the tool does to it, for messages: "uninstall", "install over", "read the logs of".
    pub verb: &'static str,
}

/// Who owns a workspace.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Owner {
    /// The agent with this id (`<device>.a<n>`).
    Agent(String),
    /// No agent: a workspace from before agents existed, or one the user installed without naming a
    /// agent. Only the lead agent (with the user's approval) or the user may change it.
    Unassigned,
}

impl Owner {
    /// From the `melissa.io/owner-agent` label: a valid agent id, else unassigned (no label,
    /// garbage, or the old `melissa.io/owner` scheme).
    pub fn from_label(owner_agent: Option<&str>) -> Self {
        match owner_agent {
            Some(id) if valid_agent_id(id) => Self::Agent(id.into()),
            _ => Self::Unassigned,
        }
    }

    /// The label value (None: no label).
    pub fn label_value(&self) -> Option<&str> {
        match self {
            Self::Agent(id) => Some(id),
            Self::Unassigned => None,
        }
    }
}

/// Agent names for messages and the `agent` argument of tools (implemented by the agent registry).
pub trait AgentNames: Send + Sync {
    /// The display name of agent `id`.
    fn name_of(&self, id: &str) -> Option<String>;
    /// The id of the agent called `name_or_id` (or with that id).
    fn resolve_id(&self, name_or_id: &str) -> Option<String>;
}

/// `agent Ada (d-0a1b2c3d.a1)`, or the id alone when the name is unknown.
pub fn describe_agent(names: Option<&dyn AgentNames>, id: &str) -> String {
    match names.and_then(|n| n.name_of(id)) {
        Some(name) => format!("agent {name} ({id})"),
        None => format!("agent {id}"),
    }
}

/// Who a task is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Scope {
    /// Nobody in particular (the default, e.g. a session nobody gave a scope): reads only.
    #[default]
    Nothing,
    /// The user in person (operator CLI and API): every workspace, no ownership confirmations.
    User,
    /// The lead agent of device `device`: every workspace on its device; destructive actions name the
    /// owning agent; unassigned workspaces need the user's approval. Workspaces on other devices are
    /// their own lead agent's.
    Lead { device: String },
    /// Agent `id`: only the workspaces it owns, and new workspaces (which then belong to it).
    Agent { id: String },
}

impl Scope {
    /// Who acted, for the workspace's "installed by" record.
    pub fn actor(&self, task: crate::TaskId, names: Option<&dyn AgentNames>) -> String {
        match self {
            Self::Nothing => format!("task {task}"),
            Self::User => "the user".into(),
            Self::Lead { .. } => format!("the lead agent (task {task})"),
            Self::Agent { id } => format!("{} (task {task})", describe_agent(names, id)),
        }
    }

    /// Does a call with `access` need the workspace's current owner to decide?
    pub fn needs_owner(&self, access: Access) -> bool {
        match (self, access) {
            (_, Access::Read) | (Self::User | Self::Nothing, _) => false,
            (Self::Lead { .. }, Access::ReadScoped) => false,
            (Self::Lead { .. } | Self::Agent { .. }, _) => true,
        }
    }

    /// The agent this scope is (agent sessions only).
    pub fn agent(&self) -> Option<&str> {
        match self {
            Self::Agent { id } => Some(id),
            _ => None,
        }
    }
}

/// The scope check's answer for one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeDecision {
    Allow,
    /// Allowed; if the label policy asks a human anyway, this goes first in the reason
    /// (it names the workspace's agent).
    Note(String),
    /// Allowed after the user approves; the reason says why.
    Confirm(String),
    /// Out of scope: refused without asking anyone.
    Refuse(String),
}

/// Decide whether a task with `scope` may do `target.access` to workspace `name`, owned by
/// `owner` (None = it does not exist; only looked up when [`Scope::needs_owner`]).
pub fn check(
    scope: &Scope,
    target: &Target,
    name: &str,
    owner: Option<&Owner>,
    names: Option<&dyn AgentNames>,
) -> ScopeDecision {
    let kind = target.kind.as_str();
    if target.access == Access::Read {
        return ScopeDecision::Allow;
    }
    match scope {
        Scope::User => ScopeDecision::Allow,
        Scope::Nothing => ScopeDecision::Refuse(format!(
            "not yours: this session may not change or read the {kind} `{name}`"
        )),
        Scope::Lead { device } => match (target.access, owner) {
            (Access::Change, Some(Owner::Agent(id))) => match device_of(id) {
                Some(c) if c != device => ScopeDecision::Refuse(format!(
                    "not yours: the {kind} `{name}` is on device {c}; ask that device's Lead"
                )),
                _ => ScopeDecision::Note(format!(
                    "`{name}` is looked after by {}",
                    describe_agent(names, id)
                )),
            },
            (Access::Change, Some(Owner::Unassigned)) => ScopeDecision::Confirm(format!(
                "`{name}` is not assigned to any agent (only you decide about it); the lead agent \
                 wants to {} it",
                target.verb
            )),
            _ => ScopeDecision::Allow,
        },
        Scope::Agent { id } => match owner {
            Some(Owner::Agent(o)) if o == id => ScopeDecision::Allow,
            // Creating is a change to a workspace that doesn't exist yet; it becomes the agent's.
            None => ScopeDecision::Allow,
            Some(Owner::Agent(other)) => ScopeDecision::Refuse(format!(
                "not yours: the {kind} `{name}` belongs to {}; ask the lead agent",
                describe_agent(names, other)
            )),
            Some(Owner::Unassigned) => ScopeDecision::Refuse(format!(
                "not yours: the {kind} `{name}` is not assigned to any agent; only the lead agent or \
                 the user may change it"
            )),
        },
    }
}

/// Adjust what a task sees of a targeted tool: `false` hides the tool; otherwise the target
/// argument's description says what is in scope.
pub fn present(scope: &Scope, target: &Target, params: &mut serde_json::Value) -> bool {
    if target.access == Access::Read {
        return true;
    }
    match scope {
        Scope::User | Scope::Lead { .. } => return true,
        Scope::Nothing => return false,
        Scope::Agent { .. } => {}
    }
    let note = format!(
        "You may only {} the {}s you look after and new ones you create (they become yours). \
         Others are refused.",
        target.verb,
        target.kind.as_str(),
    );
    if let Some(prop) = params
        .get_mut("properties")
        .and_then(|p| p.get_mut(target.arg))
        .and_then(|p| p.as_object_mut())
    {
        let desc = match prop.get("description").and_then(|d| d.as_str()) {
            Some(d) => format!("{d}. {note}"),
            None => note,
        };
        prop.insert("description".into(), desc.into());
    }
    true
}

/// Looks up who owns a workspace. `Ok(None)`: no such workspace. A workspace without an owner record must
/// be reported as [`Owner::Unassigned`].
pub trait ResourceOwners: Send + Sync {
    fn owner<'a>(
        &'a self,
        kind: ResourceKind,
        name: &'a str,
    ) -> BoxFuture<'a, Result<Option<Owner>, String>>;
}

/// One workspace as the workspace store sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceInfo {
    /// The workspace's name (its app).
    pub name: String,
    pub owner: Owner,
    /// The device it is on (None: not recorded yet, a workspace from before devices had ids).
    pub device: Option<String>,
    /// Who assigned it to its agent, and when (RFC 3339).
    pub assigned_by: Option<String>,
    pub assigned_at: Option<String>,
    /// The old `melissa.io/owner` value (`owner`, `job-<key>`) of a workspace not yet migrated.
    pub legacy_owner: Option<String>,
}

/// Where workspaces live (the cluster): list them and change their owner. Only the reassign path
/// (the lead agent's `reassign_workspace` after the user's approval, or the user's own
/// `melissa-server workspace reassign`) calls [`WorkspaceStore::assign`].
pub trait WorkspaceStore: Send + Sync {
    fn workspaces(&self) -> BoxFuture<'_, Result<Vec<WorkspaceInfo>, String>>;
    /// Make agent `agent` (an id) the owner of workspace `workspace`, recording `by`. Errors if the workspace
    /// does not exist.
    fn assign<'a>(
        &'a self,
        workspace: &'a str,
        agent: &'a str,
        by: &'a str,
    ) -> BoxFuture<'a, Result<(), String>>;
    /// Remove workspace `workspace` for good: its workloads, its namespace and all its data, waiting
    /// until it is gone (stuck objects are forced; a clear error after a timeout). Only the
    /// user's own force delete calls this; no agent tool reaches it. Returns what was
    /// removed, in words.
    fn force_delete<'a>(&'a self, workspace: &'a str)
        -> BoxFuture<'a, Result<Vec<String>, String>>;
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const UNINSTALL: Target = Target {
        kind: ResourceKind::Workspace,
        arg: "app",
        access: Access::Change,
        verb: "uninstall",
    };
    const LOGS: Target = Target {
        access: Access::ReadScoped,
        verb: "read the logs of",
        ..UNINSTALL
    };
    const STATUS: Target = Target {
        access: Access::Read,
        verb: "check",
        ..UNINSTALL
    };

    const ADA: &str = "d-0a1b2c3d.a1";
    const BO: &str = "d-0a1b2c3d.a2";

    struct Names;
    impl AgentNames for Names {
        fn name_of(&self, id: &str) -> Option<String> {
            match id {
                ADA => Some("Ada".into()),
                BO => Some("Bo".into()),
                _ => None,
            }
        }
        fn resolve_id(&self, _: &str) -> Option<String> {
            None
        }
    }

    fn agent(id: &str) -> Scope {
        Scope::Agent { id: id.into() }
    }

    fn lead() -> Scope {
        Scope::Lead {
            device: "d-0a1b2c3d".into(),
        }
    }

    fn check_(scope: &Scope, t: &Target, name: &str, owner: Option<&Owner>) -> ScopeDecision {
        check(scope, t, name, owner, Some(&Names))
    }

    #[test]
    fn owner_labels_default_to_unassigned() {
        assert_eq!(Owner::from_label(Some(ADA)), Owner::Agent(ADA.into()));
        assert_eq!(Owner::Agent(ADA.into()).label_value(), Some(ADA));
        assert_eq!(Owner::Unassigned.label_value(), None);
        // Unlabelled, the old scheme and garbage are unassigned.
        for v in [
            None,
            Some("owner"),
            Some("job-ab12"),
            Some("someone"),
            Some(""),
        ] {
            assert_eq!(Owner::from_label(v), Owner::Unassigned, "{v:?}");
        }
    }

    #[test]
    fn a_agent_may_not_touch_another_agents_workspace() {
        let d = check_(
            &agent(ADA),
            &UNINSTALL,
            "nextcloud",
            Some(&Owner::Agent(BO.into())),
        );
        assert_eq!(
            d,
            ScopeDecision::Refuse(
                "not yours: the workspace `nextcloud` belongs to agent Bo (d-0a1b2c3d.a2); ask the lead agent"
                    .into()
            )
        );
        assert!(matches!(
            check_(&agent(ADA), &UNINSTALL, "old", Some(&Owner::Unassigned)),
            ScopeDecision::Refuse(r) if r.contains("not assigned to any agent")
        ));
        assert!(matches!(
            check_(
                &agent(ADA),
                &LOGS,
                "nextcloud",
                Some(&Owner::Agent(BO.into()))
            ),
            ScopeDecision::Refuse(_)
        ));
        // Reads of names and phases stay open.
        assert_eq!(
            check_(
                &agent(ADA),
                &STATUS,
                "nextcloud",
                Some(&Owner::Agent(BO.into()))
            ),
            ScopeDecision::Allow
        );
    }

    #[test]
    fn a_agent_may_change_its_own_workspaces_and_create_new_ones() {
        assert_eq!(
            check_(
                &agent(ADA),
                &UNINSTALL,
                "blog",
                Some(&Owner::Agent(ADA.into()))
            ),
            ScopeDecision::Allow
        );
        assert_eq!(
            check_(&agent(ADA), &LOGS, "blog", Some(&Owner::Agent(ADA.into()))),
            ScopeDecision::Allow
        );
        assert_eq!(
            check_(&agent(ADA), &UNINSTALL, "new", None),
            ScopeDecision::Allow
        );
    }

    #[test]
    fn the_lead_names_the_agent_and_asks_for_unassigned_workspaces() {
        assert_eq!(
            check_(
                &lead(),
                &UNINSTALL,
                "nextcloud",
                Some(&Owner::Agent(ADA.into()))
            ),
            ScopeDecision::Note("`nextcloud` is looked after by agent Ada (d-0a1b2c3d.a1)".into())
        );
        assert_eq!(
            check_(&lead(), &UNINSTALL, "old", Some(&Owner::Unassigned)),
            ScopeDecision::Confirm(
                "`old` is not assigned to any agent (only you decide about it); the lead agent wants \
                 to uninstall it"
                    .into()
            )
        );
        assert_eq!(
            check_(&lead(), &UNINSTALL, "new", None),
            ScopeDecision::Allow
        );
        assert_eq!(
            check_(&lead(), &LOGS, "old", Some(&Owner::Unassigned)),
            ScopeDecision::Allow
        );
        // A workspace on another device is that device's lead agent's.
        assert!(matches!(
            check_(&lead(), &UNINSTALL, "x", Some(&Owner::Agent("d-ffffffff.a1".into()))),
            ScopeDecision::Refuse(r) if r.contains("device d-ffffffff")
        ));
        assert_eq!(
            check_(&Scope::User, &UNINSTALL, "old", Some(&Owner::Unassigned)),
            ScopeDecision::Allow
        );
        assert!(matches!(
            check_(&Scope::Nothing, &UNINSTALL, "x", None),
            ScopeDecision::Refuse(_)
        ));
        // Without names, the id alone.
        assert_eq!(
            check(
                &lead(),
                &UNINSTALL,
                "n",
                Some(&Owner::Agent(ADA.into())),
                None
            ),
            ScopeDecision::Note("`n` is looked after by agent d-0a1b2c3d.a1".into())
        );
    }

    #[test]
    fn actors_and_lookups() {
        assert_eq!(lead().actor(3, None), "the lead agent (task 3)");
        assert_eq!(
            agent(ADA).actor(7, Some(&Names)),
            "agent Ada (d-0a1b2c3d.a1) (task 7)"
        );
        assert_eq!(Scope::User.actor(0, None), "the user");
        assert!(!lead().needs_owner(Access::ReadScoped));
        assert!(lead().needs_owner(Access::Change));
        assert!(agent(ADA).needs_owner(Access::ReadScoped));
        assert!(!agent(ADA).needs_owner(Access::Read));
        assert!(!Scope::User.needs_owner(Access::Change));
        assert_eq!(agent(ADA).agent(), Some(ADA));
        assert_eq!(lead().agent(), None);
    }

    #[test]
    fn agents_are_told_their_limits_and_nobody_sees_nothing() {
        let params = || json!({"properties": {"app": {"type": "string"}}});
        let mut p = params();
        assert!(present(&agent(ADA), &UNINSTALL, &mut p));
        assert!(p["properties"]["app"]["description"]
            .as_str()
            .unwrap()
            .contains("you look after"));
        assert!(!present(&Scope::Nothing, &LOGS, &mut params()));
        assert!(present(&Scope::Nothing, &STATUS, &mut params()));
        let mut p = params();
        assert!(present(&lead(), &UNINSTALL, &mut p));
        assert_eq!(p, params());
    }
}
