//! Workspace ownership and agent scopes: an agent acts only on the workspaces it looks after.
//!
//! The cluster model (docs/AGENT-MODEL.md): every **workspace** (today: an installed app) is owned by
//! exactly one **agent**; an agent may act only on its own workspaces; only the **lead agent** or
//! the **user** reassign workspaces. Every task has a [`Scope`] that says who it is. A tool declares
//! which argument names its workspace ([`Tool::target`]); the [`super::ToolRegistry`] checks the
//! scope **before** the label policy, for every tool, in one place ([`check`]):
//!
//! - an agent on another agent's (or an unassigned) workspace, for anything (reads included) →
//!   never on its own: a confirmation only the user answers, for that one call
//!   ([`ScopeDecision::AskUser`]: no "Always allow" rule and not No permission prompts either,
//!   since ownership is a safety boundary, not a convenience prompt);
//! - the lead agent on an agent's workspace → allowed; if the label policy asks the user anyway
//!   (destructive actions always do), the confirmation names the owning agent
//!   ([`ScopeDecision::Note`]);
//! - the lead agent on an unassigned workspace (from before agents existed) → a confirmation: only the
//!   user decides about those;
//! - the lead agent on another device's workspace, or a session without a scope reading into or
//!   changing a workspace → refused outright (`ToolError::Refused("not yours: …")`);
//! - otherwise → the usual label policy.
//!
//! Every cross-owner attempt (asked, approved, denied, refused, and the lead agent's override) is
//! audited `tool.cross_owner` naming both sides ([`crossing`]).
//!
//! Ownership is stored with the workspace (the `melissa.io/owner-agent` label on the app namespace)
//! and read through [`ResourceOwners`] at every call, so a reassignment takes effect at once.
//! A workspace without that label is [`Owner::Unassigned`].
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
    /// Names, phases, counts, addresses (e.g. `app_status`): the user, the lead agent, unscoped
    /// readers (the UI) and the owning agent. Another agent needs the user's yes, each time.
    Read,
    /// Reads content from the workspace that the workspace controls (e.g. `app_logs`): its own agent,
    /// the lead agent and the user.
    ReadScoped,
    /// Changes or deletes the workspace (install over, uninstall, update, restart, scale,
    /// stop).
    Change,
    /// Creates a new workspace, or (when it exists) changes it like [`Access::Change`]
    /// (`deploy_app`). New workspaces come only from agents (they become theirs) or the user:
    /// the lead agent hands creation to an agent ([`LEAD_HANDS_OFF`]).
    Create,
}

/// Why the lead agent may not create a workspace itself: every new app gets an agent owner
/// from the start, so the lead hands the job to one.
pub const LEAD_HANDS_OFF: &str = "the lead agent does not set up new apps itself: hand this to a \
    helper. create_agent with `job` (the goal, `user_request`: the user's words, `research`: what \
    you found out) creates one and starts it at once; or give_agent_job to a helper that already \
    looks after related apps. The helper deploys the app and looks after it; you keep watching \
    (agent_status) and can still stop, update or uninstall it.";

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
    /// The id of the agent with id `name_or_id`, or whose exact current name it is. Err: why
    /// not (unknown, a former name, ambiguous: never a guess), for the caller to pass on.
    fn resolve_id(&self, name_or_id: &str) -> Result<String, String>;
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
    /// their own lead agent's. It creates no new workspaces: agents do ([`Access::Create`]).
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
            (Self::User | Self::Nothing, _) => false,
            (Self::Lead { .. }, Access::Read | Access::ReadScoped) => false,
            (Self::Lead { .. }, Access::Change | Access::Create) | (Self::Agent { .. }, _) => true,
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
    /// Another agent's (or an unassigned) workspace: only the user may allow it, for this one
    /// call. No "Always allow" rule and not No permission prompts can answer it.
    AskUser(String),
    /// Out of scope: refused without asking anyone.
    Refuse(String),
}

/// `Ada`, or `agent <id>` when the name is unknown: for short confirmation texts.
fn short_agent(names: Option<&dyn AgentNames>, id: &str) -> String {
    names
        .and_then(|n| n.name_of(id))
        .unwrap_or_else(|| format!("agent {id}"))
}

/// Who acts on whose workspace, for the `tool.cross_owner` audit line, when `scope` acts on a
/// workspace owned by someone else (another agent, or nobody): e.g. "agent Pip (d-….a3) → stop
/// `minecraft` of agent Ada (d-….a1)". None: its own workspace, a new one, the user's call, or
/// the owner was not looked up.
pub fn crossing(
    scope: &Scope,
    target: &Target,
    name: &str,
    owner: Option<&Owner>,
    names: Option<&dyn AgentNames>,
) -> Option<String> {
    let who = match scope {
        Scope::Agent { id } => {
            if owner == Some(&Owner::Agent(id.clone())) {
                return None;
            }
            describe_agent(names, id)
        }
        Scope::Lead { device } => format!("the lead agent of {device}"),
        Scope::User | Scope::Nothing => return None,
    };
    let whose = match owner? {
        Owner::Agent(o) => describe_agent(names, o),
        Owner::Unassigned => "nobody (unassigned)".into(),
    };
    Some(format!("{who} → {} `{name}` of {whose}", target.verb))
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
    if target.access == Access::Read && !matches!(scope, Scope::Agent { .. }) {
        return ScopeDecision::Allow;
    }
    match scope {
        Scope::User => ScopeDecision::Allow,
        Scope::Nothing => ScopeDecision::Refuse(format!(
            "not yours: this session may not change or read the {kind} `{name}`"
        )),
        Scope::Lead { device } => match (target.access, owner) {
            // A new workspace: an agent creates it (and owns it from the start).
            (Access::Create, None) => ScopeDecision::Refuse(LEAD_HANDS_OFF.into()),
            (Access::Change | Access::Create, Some(Owner::Agent(id))) => match device_of(id) {
                Some(c) if c != device => ScopeDecision::Refuse(format!(
                    "not yours: the {kind} `{name}` is on device {c}; ask that device's Lead"
                )),
                _ => ScopeDecision::Note(format!(
                    "`{name}` is looked after by {}",
                    describe_agent(names, id)
                )),
            },
            (Access::Change | Access::Create, Some(Owner::Unassigned)) => {
                ScopeDecision::Confirm(format!(
                "`{name}` is not assigned to any agent (only you decide about it); the lead agent \
                 wants to {} it",
                target.verb
            ))
            }
            _ => ScopeDecision::Allow,
        },
        Scope::Agent { id } => {
            let me = short_agent(names, id);
            let only = "Only you can allow this, and only this once";
            match owner {
                Some(Owner::Agent(o)) if o == id => ScopeDecision::Allow,
                // Creating is a change to a workspace that doesn't exist yet; it becomes the
                // agent's. Reading one that doesn't exist reads nothing of anyone's.
                None => ScopeDecision::Allow,
                Some(Owner::Agent(other)) => ScopeDecision::AskUser(format!(
                    "{me} wants to {} {}'s `{name}` (not {me}'s). {only}",
                    target.verb,
                    short_agent(names, other),
                )),
                Some(Owner::Unassigned) => ScopeDecision::AskUser(format!(
                    "{me} wants to {} `{name}`, which no agent looks after. {only}",
                    target.verb,
                )),
            }
        }
    }
}

/// Adjust what a task sees of a targeted tool: `false` hides the tool; otherwise the target
/// argument's description says what is in scope.
pub fn present(scope: &Scope, target: &Target, params: &mut serde_json::Value) -> bool {
    match scope {
        Scope::User | Scope::Lead { .. } => return true,
        Scope::Nothing => return target.access == Access::Read,
        Scope::Agent { .. } => {}
    }
    let note = format!(
        "Only the {}s you look after (my_apps lists them) and new ones you create (they become \
         yours). Another agent's needs the user's yes, each time.",
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
    /// It is being deleted (its namespace is terminating): it and its owner record go away on
    /// their own, so it no longer ties its agent down.
    pub deleting: bool,
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
    const DEPLOY: Target = Target {
        access: Access::Create,
        verb: "deploy over",
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
        fn resolve_id(&self, n: &str) -> Result<String, String> {
            Err(format!("no agent `{n}`"))
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
    fn a_agent_never_touches_another_agents_workspace_on_its_own() {
        // Changes, scoped reads and plain reads alike: only the user's yes, for one call.
        for t in [&UNINSTALL, &LOGS, &STATUS] {
            let d = check_(&agent(ADA), t, "nextcloud", Some(&Owner::Agent(BO.into())));
            assert_eq!(
                d,
                ScopeDecision::AskUser(format!(
                    "Ada wants to {} Bo's `nextcloud` (not Ada's). Only you can allow this, and \
                     only this once",
                    t.verb
                ))
            );
        }
        assert_eq!(
            check_(&agent(ADA), &UNINSTALL, "old", Some(&Owner::Unassigned)),
            ScopeDecision::AskUser(
                "Ada wants to uninstall `old`, which no agent looks after. Only you can allow \
                 this, and only this once"
                    .into()
            )
        );
        // Without names, the ids.
        assert!(matches!(
            check(&agent(ADA), &STATUS, "n", Some(&Owner::Agent(BO.into())), None),
            ScopeDecision::AskUser(r) if r.starts_with("agent d-0a1b2c3d.a1 wants to check agent d-0a1b2c3d.a2's `n`")
        ));
    }

    #[test]
    fn crossings_name_both_sides() {
        let bo = Owner::Agent(BO.into());
        assert_eq!(
            crossing(
                &agent(ADA),
                &UNINSTALL,
                "nextcloud",
                Some(&bo),
                Some(&Names)
            )
            .as_deref(),
            Some("agent Ada (d-0a1b2c3d.a1) → uninstall `nextcloud` of agent Bo (d-0a1b2c3d.a2)")
        );
        assert_eq!(
            crossing(
                &lead(),
                &UNINSTALL,
                "old",
                Some(&Owner::Unassigned),
                Some(&Names)
            )
            .as_deref(),
            Some("the lead agent of d-0a1b2c3d → uninstall `old` of nobody (unassigned)")
        );
        // Its own, a new one, the user's: no crossing.
        let ada = Owner::Agent(ADA.into());
        assert_eq!(
            crossing(&agent(ADA), &UNINSTALL, "n", Some(&ada), None),
            None
        );
        assert_eq!(crossing(&agent(ADA), &UNINSTALL, "n", None, None), None);
        assert_eq!(
            crossing(&Scope::User, &UNINSTALL, "n", Some(&bo), None),
            None
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
    fn new_workspaces_come_from_agents_never_the_lead() {
        // The lead agent hands creation to an agent: refused before anyone is asked.
        assert_eq!(
            check_(&lead(), &DEPLOY, "minecraft", None),
            ScopeDecision::Refuse(LEAD_HANDS_OFF.into())
        );
        assert!(
            LEAD_HANDS_OFF.contains("create_agent") && LEAD_HANDS_OFF.contains("give_agent_job")
        );
        // An existing workspace (uninstalled, data kept): the lead's override, as for a change.
        assert_eq!(
            check_(
                &lead(),
                &DEPLOY,
                "minecraft",
                Some(&Owner::Agent(ADA.into()))
            ),
            ScopeDecision::Note("`minecraft` is looked after by agent Ada (d-0a1b2c3d.a1)".into())
        );
        assert!(matches!(
            check_(&lead(), &DEPLOY, "old", Some(&Owner::Unassigned)),
            ScopeDecision::Confirm(_)
        ));
        assert!(lead().needs_owner(Access::Create));
        // An agent creates (the workspace becomes its own); the user may too.
        assert_eq!(
            check_(&agent(ADA), &DEPLOY, "minecraft", None),
            ScopeDecision::Allow
        );
        assert!(matches!(
            check_(
                &agent(ADA),
                &DEPLOY,
                "minecraft",
                Some(&Owner::Agent(BO.into()))
            ),
            ScopeDecision::AskUser(_)
        ));
        assert_eq!(
            check_(&Scope::User, &DEPLOY, "minecraft", None),
            ScopeDecision::Allow
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
        assert!(agent(ADA).needs_owner(Access::Read));
        assert!(!lead().needs_owner(Access::Read));
        assert!(!Scope::Nothing.needs_owner(Access::Read));
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
