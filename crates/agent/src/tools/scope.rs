//! Resource ownership and agent scopes: agents change only what belongs to them.
//!
//! Every resource a tool changes (today: apps) has an [`Owner`]: the human owner, or the
//! worker job that created it. Every task has a [`Scope`]: the set of resources it may
//! change. A tool declares which argument names its resource ([`Tool::target`]); the
//! [`super::ToolRegistry`] checks the scope **before** the label policy, for every tool, in
//! one place ([`check`]):
//!
//! - out of scope → refused outright (`ToolError::Refused`, audited `tool.refused`), never a
//!   confirmation a tired human might click through;
//! - in scope but owned by the human while an agent acts for them (the Queen) → a
//!   confirmation that names the owner ("`nextcloud` belongs to you; the Queen wants to
//!   uninstall it");
//! - otherwise → the usual label policy.
//!
//! Ownership is stored with the resource (for apps: the `melissa.io/owner` label on the
//! namespace) and read through [`ResourceOwners`]. A resource without an owner record
//! counts as the human's (safe default for apps installed before ownership existed).
//!
//! [`Tool::target`]: super::Tool::target

use std::collections::BTreeSet;

use crate::BoxFuture;

/// Kinds of resources with owners. New kinds (backups, agent-built apps, ...) add a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ResourceKind {
    App,
}

impl ResourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
        }
    }
}

/// What a call does to its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Names, phases, counts: any task may read any resource (e.g. `app_status`).
    Read,
    /// Reads content from the resource that the resource controls (e.g. `app_logs`): only
    /// in scope, except for agents acting for the owner (the Queen).
    ReadScoped,
    /// Creates, changes or deletes the resource (install over, uninstall, update, restart,
    /// scale, stop).
    Change,
}

/// The resource a tool works on, declared once per tool ([`super::Tool::target`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub kind: ResourceKind,
    /// Name of the (string) argument that names the resource, e.g. `app`.
    pub arg: &'static str,
    pub access: Access,
    /// What the tool does to it, for messages: "uninstall", "install over", "read the logs of".
    pub verb: &'static str,
}

/// Who owns a resource.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Owner {
    /// The human who owns this comb.
    Human,
    /// The worker job with this key (a worker created it for its job).
    Job(String),
}

/// Label value for [`Owner::Human`].
pub const OWNER_HUMAN: &str = "owner";
/// Prefix of the label value for [`Owner::Job`].
pub const OWNER_JOB_PREFIX: &str = "job-";

impl Owner {
    /// Value for a Kubernetes label (`owner` or `job-<key>`).
    pub fn label_value(&self) -> String {
        match self {
            Self::Human => OWNER_HUMAN.into(),
            Self::Job(key) => format!("{OWNER_JOB_PREFIX}{key}"),
        }
    }

    /// Parse a label value. `None` (no label) and anything unrecognised mean the human:
    /// when in doubt, a resource is the owner's.
    pub fn from_label(value: Option<&str>) -> Self {
        match value.and_then(|v| v.strip_prefix(OWNER_JOB_PREFIX)) {
            Some(key) if valid_job_key(key) => Self::Job(key.into()),
            _ => Self::Human,
        }
    }

    /// For messages to the human.
    pub fn describe(&self) -> String {
        match self {
            Self::Human => "the owner".into(),
            Self::Job(key) => format!("worker job {key}"),
        }
    }
}

/// Job keys: 1-40 of `[a-z0-9]` (fits a label value with the prefix).
pub fn valid_job_key(key: &str) -> bool {
    (1..=40).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// A worker job's scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobScope {
    /// The job's key, recorded as owner of what it creates.
    pub key: String,
    /// Existing apps the Queen granted (each granted under the Queen's own rules).
    pub apps: BTreeSet<String>,
    /// May create new apps (owned by this job, or by the human if `for_owner`).
    pub create: bool,
    /// The human approved that what this job creates belongs to them.
    pub for_owner: bool,
}

/// What a task may change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Scope {
    /// Nothing (the default, e.g. a session nobody gave a scope).
    #[default]
    Nothing,
    /// The human themself (operator CLI and API): everything, no ownership confirmations.
    Owner,
    /// An agent acting for the human in an attended session (the Queen, tasks the human
    /// starts in the UI): every resource; changing one the human owns needs a confirmation
    /// that names them. What it creates belongs to the human.
    ForOwner {
        /// Who acts, for messages ("the Queen").
        agent: String,
    },
    /// A worker: only granted resources and those its job creates.
    Job(JobScope),
}

impl Scope {
    /// Owner recorded on a resource this scope creates (None: may not create).
    pub fn creator(&self) -> Option<Owner> {
        match self {
            Self::Nothing => None,
            Self::Owner | Self::ForOwner { .. } => Some(Owner::Human),
            Self::Job(j) if j.for_owner => Some(Owner::Human),
            Self::Job(j) => Some(Owner::Job(j.key.clone())),
        }
    }

    /// Who acted, for the resource's "installed by" record.
    pub fn actor(&self, task: crate::TaskId) -> String {
        match self {
            Self::Nothing => format!("task {task}"),
            Self::Owner => "owner".into(),
            Self::ForOwner { agent } => format!("{agent} (task {task})"),
            Self::Job(j) => format!("worker task {task}, job {}", j.key),
        }
    }

    /// Does a call with `access` to resource `name` need its current owner to decide?
    pub fn needs_owner(&self, access: Access, name: &str) -> bool {
        match (self, access) {
            (_, Access::Read) | (Self::Owner | Self::Nothing, _) => false,
            (Self::ForOwner { .. }, Access::ReadScoped) => false,
            (Self::ForOwner { .. }, Access::Change) => true,
            (Self::Job(j), _) => !j.apps.contains(name),
        }
    }
}

/// The scope check's answer for one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeDecision {
    Allow,
    /// Allowed after a human approves; the reason names the owner.
    Confirm(String),
    /// Out of scope: refused without asking anyone.
    Refuse(String),
}

/// Decide whether a task with `scope` may do `target.access` to resource `name`, owned by
/// `owner` (None = it does not exist; only looked up when [`Scope::needs_owner`]).
pub fn check(scope: &Scope, target: &Target, name: &str, owner: Option<&Owner>) -> ScopeDecision {
    let kind = target.kind.as_str();
    if target.access == Access::Read {
        return ScopeDecision::Allow;
    }
    match scope {
        Scope::Owner => ScopeDecision::Allow,
        Scope::Nothing => ScopeDecision::Refuse(format!(
            "not yours: this session may not change or read the {kind} `{name}`"
        )),
        Scope::ForOwner { agent } => match (target.access, owner) {
            (Access::Change, Some(Owner::Human)) => ScopeDecision::Confirm(format!(
                "`{name}` belongs to you; {agent} wants to {} it",
                target.verb
            )),
            _ => ScopeDecision::Allow,
        },
        Scope::Job(j) => {
            if j.apps.contains(name) {
                return ScopeDecision::Allow;
            }
            match owner {
                Some(Owner::Job(key)) if *key == j.key => ScopeDecision::Allow,
                // Creating is a change to a resource that doesn't exist yet.
                None if j.create => ScopeDecision::Allow,
                None => ScopeDecision::Refuse(format!(
                    "not yours: this job may only use the {kind}s it was given ({})",
                    list(&j.apps)
                )),
                Some(other) => ScopeDecision::Refuse(format!(
                    "not yours: the {kind} `{name}` belongs to {}; ask the Queen",
                    other.describe()
                )),
            }
        }
    }
}

fn list(apps: &BTreeSet<String>) -> String {
    if apps.is_empty() {
        "none".into()
    } else {
        apps.iter().cloned().collect::<Vec<_>>().join(", ")
    }
}

/// Adjust what a task sees of a targeted tool: `false` hides the tool; otherwise the target
/// argument's description (and, if the set is closed, an `enum`) lists what is in scope.
pub fn present(scope: &Scope, target: &Target, params: &mut serde_json::Value) -> bool {
    if target.access == Access::Read {
        return true;
    }
    let (apps, create) = match scope {
        Scope::Owner | Scope::ForOwner { .. } => return true,
        Scope::Nothing => return false,
        Scope::Job(j) => (&j.apps, j.create),
    };
    if apps.is_empty() && !create {
        return false;
    }
    let mut note = format!(
        "You may only {} these {}s: {}",
        target.verb,
        target.kind.as_str(),
        list(apps)
    );
    if create {
        note.push_str(", and those you create in this job");
    }
    note.push_str(". Others are refused.");
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
        if !create {
            prop.insert(
                "enum".into(),
                apps.iter().cloned().collect::<Vec<_>>().into(),
            );
        }
    }
    true
}

/// Looks up who owns a resource. `Ok(None)`: no such resource. A resource without an owner
/// record must be reported as [`Owner::Human`].
pub trait ResourceOwners: Send + Sync {
    fn owner<'a>(
        &'a self,
        kind: ResourceKind,
        name: &'a str,
    ) -> BoxFuture<'a, Result<Option<Owner>, String>>;
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const UNINSTALL: Target = Target {
        kind: ResourceKind::App,
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

    fn job(apps: &[&str], create: bool) -> Scope {
        Scope::Job(JobScope {
            key: "k1".into(),
            apps: apps.iter().map(|a| a.to_string()).collect(),
            create,
            for_owner: false,
        })
    }

    fn queen() -> Scope {
        Scope::ForOwner {
            agent: "the Queen".into(),
        }
    }

    #[test]
    fn owner_labels_round_trip_and_default_to_the_human() {
        assert_eq!(Owner::Human.label_value(), "owner");
        assert_eq!(Owner::Job("ab12".into()).label_value(), "job-ab12");
        assert_eq!(
            Owner::from_label(Some("job-ab12")),
            Owner::Job("ab12".into())
        );
        assert_eq!(Owner::from_label(Some("owner")), Owner::Human);
        // Unlabelled (legacy) and garbage count as the human's.
        assert_eq!(Owner::from_label(None), Owner::Human);
        assert_eq!(Owner::from_label(Some("job-")), Owner::Human);
        assert_eq!(Owner::from_label(Some("job-UPPER")), Owner::Human);
        assert_eq!(Owner::from_label(Some("someone")), Owner::Human);
    }

    #[test]
    fn a_worker_may_not_touch_the_owners_app() {
        let d = check(
            &job(&[], true),
            &UNINSTALL,
            "nextcloud",
            Some(&Owner::Human),
        );
        assert!(
            matches!(&d, ScopeDecision::Refuse(r) if r.contains("not yours") && r.contains("belongs to the owner")),
            "{d:?}"
        );
        let other = Owner::Job("k2".into());
        assert!(matches!(
            check(&job(&[], true), &UNINSTALL, "x", Some(&other)),
            ScopeDecision::Refuse(_)
        ));
        assert!(matches!(
            check(&job(&[], true), &LOGS, "nextcloud", Some(&Owner::Human)),
            ScopeDecision::Refuse(_)
        ));
        // Reads of names and phases stay open.
        assert_eq!(
            check(&job(&[], false), &STATUS, "nextcloud", Some(&Owner::Human)),
            ScopeDecision::Allow
        );
    }

    #[test]
    fn a_worker_may_change_what_its_job_created_or_was_granted() {
        let mine = Owner::Job("k1".into());
        assert_eq!(
            check(&job(&[], false), &UNINSTALL, "blog", Some(&mine)),
            ScopeDecision::Allow
        );
        assert_eq!(
            check(&job(&["nextcloud"], false), &UNINSTALL, "nextcloud", None),
            ScopeDecision::Allow
        );
        // Creating needs the create right.
        assert_eq!(
            check(&job(&[], true), &UNINSTALL, "new", None),
            ScopeDecision::Allow
        );
        assert!(matches!(
            check(&job(&["a"], false), &UNINSTALL, "new", None),
            ScopeDecision::Refuse(_)
        ));
    }

    #[test]
    fn the_queen_needs_a_confirmation_naming_the_owner() {
        assert_eq!(
            check(&queen(), &UNINSTALL, "nextcloud", Some(&Owner::Human)),
            ScopeDecision::Confirm(
                "`nextcloud` belongs to you; the Queen wants to uninstall it".into()
            )
        );
        assert_eq!(
            check(&queen(), &UNINSTALL, "blog", Some(&Owner::Job("k1".into()))),
            ScopeDecision::Allow
        );
        assert_eq!(
            check(&queen(), &UNINSTALL, "new", None),
            ScopeDecision::Allow
        );
        assert_eq!(
            check(&queen(), &LOGS, "nextcloud", Some(&Owner::Human)),
            ScopeDecision::Allow
        );
        assert_eq!(
            check(&Scope::Owner, &UNINSTALL, "nextcloud", Some(&Owner::Human)),
            ScopeDecision::Allow
        );
        assert!(matches!(
            check(&Scope::Nothing, &UNINSTALL, "x", None),
            ScopeDecision::Refuse(_)
        ));
    }

    #[test]
    fn creators_and_lookups() {
        assert_eq!(queen().creator(), Some(Owner::Human));
        assert_eq!(job(&[], true).creator(), Some(Owner::Job("k1".into())));
        let mut j = job(&[], true);
        if let Scope::Job(s) = &mut j {
            s.for_owner = true;
        }
        assert_eq!(j.creator(), Some(Owner::Human));
        assert_eq!(Scope::Nothing.creator(), None);
        assert!(!queen().needs_owner(Access::ReadScoped, "x"));
        assert!(queen().needs_owner(Access::Change, "x"));
        assert!(!job(&["x"], false).needs_owner(Access::Change, "x"));
        assert!(job(&["x"], false).needs_owner(Access::Change, "y"));
        assert!(!Scope::Owner.needs_owner(Access::Change, "x"));
    }

    #[test]
    fn workers_see_only_their_apps() {
        let params = || json!({"properties": {"app": {"type": "string"}}});
        let mut p = params();
        assert!(present(&job(&["blog"], false), &UNINSTALL, &mut p));
        assert_eq!(p["properties"]["app"]["enum"], json!(["blog"]));
        assert!(p["properties"]["app"]["description"]
            .as_str()
            .unwrap()
            .contains("blog"));
        let mut p = params();
        assert!(present(&job(&[], true), &UNINSTALL, &mut p));
        assert!(p["properties"]["app"].get("enum").is_none());
        assert!(!present(&job(&[], false), &UNINSTALL, &mut params()));
        assert!(!present(&Scope::Nothing, &LOGS, &mut params()));
        assert!(present(&Scope::Nothing, &STATUS, &mut params()));
        let mut p = params();
        assert!(present(&queen(), &UNINSTALL, &mut p));
        assert_eq!(p, params());
    }
}
