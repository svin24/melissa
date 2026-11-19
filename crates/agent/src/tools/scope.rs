//! Cell ownership and agent scopes: a bee changes only the cells it looks after.
//!
//! The hive model (docs/HIVE-MODEL.md): every **cell** (today: an installed app) is owned by
//! exactly one **bee**; a bee may change only its own cells; only the **Queen** or the
//! **user** reassign cells. Every task has a [`Scope`] that says who it is. A tool declares
//! which argument names its cell ([`Tool::target`]); the [`super::ToolRegistry`] checks the
//! scope **before** the label policy, for every tool, in one place ([`check`]):
//!
//! - a bee on another bee's (or an unassigned) cell → refused outright
//!   (`ToolError::Refused("not yours: …")`, audited `tool.refused`), never a confirmation a
//!   tired human might click through;
//! - the Queen on a bee's cell → allowed; if the label policy asks the user anyway
//!   (destructive actions always do), the confirmation names the owning bee
//!   ([`ScopeDecision::Note`]);
//! - the Queen on an unassigned cell (from before bees existed) → a confirmation: only the
//!   user decides about those;
//! - otherwise → the usual label policy.
//!
//! Ownership is stored with the cell (the `melissa.io/owner-bee` label on the app namespace)
//! and read through [`ResourceOwners`]. A cell without that label is [`Owner::Unassigned`].
//!
//! [`Tool::target`]: super::Tool::target

use crate::{comb::comb_of, comb::valid_bee_id, BoxFuture};

/// Kinds of resources with owners. New kinds (backups, agent-built apps, ...) add a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ResourceKind {
    /// A cell: an installed app (`app-<name>` namespace) and its services.
    Cell,
}

impl ResourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cell => "cell",
        }
    }
}

/// What a call does to its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Names, phases, counts: any task may read any cell (e.g. `app_status`).
    Read,
    /// Reads content from the cell that the cell controls (e.g. `app_logs`): its own bee,
    /// the Queen and the user.
    ReadScoped,
    /// Creates, changes or deletes the cell (install over, uninstall, update, restart,
    /// scale, stop).
    Change,
}

/// The cell a tool works on, declared once per tool ([`super::Tool::target`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub kind: ResourceKind,
    /// Name of the (string) argument that names the cell, e.g. `app`.
    pub arg: &'static str,
    pub access: Access,
    /// What the tool does to it, for messages: "uninstall", "install over", "read the logs of".
    pub verb: &'static str,
}

/// Who owns a cell.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Owner {
    /// The bee with this id (`<comb>.b<n>`).
    Bee(String),
    /// No bee: a cell from before bees existed, or one the user installed without naming a
    /// bee. Only the Queen (with the user's approval) or the user may change it.
    Unassigned,
}

impl Owner {
    /// From the `melissa.io/owner-bee` label: a valid bee id, else unassigned (no label,
    /// garbage, or the old `melissa.io/owner` scheme).
    pub fn from_label(owner_bee: Option<&str>) -> Self {
        match owner_bee {
            Some(id) if valid_bee_id(id) => Self::Bee(id.into()),
            _ => Self::Unassigned,
        }
    }

    /// The label value (None: no label).
    pub fn label_value(&self) -> Option<&str> {
        match self {
            Self::Bee(id) => Some(id),
            Self::Unassigned => None,
        }
    }
}

/// Bee names for messages and the `bee` argument of tools (implemented by the bee registry).
pub trait BeeNames: Send + Sync {
    /// The display name of bee `id`.
    fn name_of(&self, id: &str) -> Option<String>;
    /// The id of the bee called `name_or_id` (or with that id).
    fn resolve_id(&self, name_or_id: &str) -> Option<String>;
}

/// `bee Ada (c-0a1b2c3d.b1)`, or the id alone when the name is unknown.
pub fn describe_bee(names: Option<&dyn BeeNames>, id: &str) -> String {
    match names.and_then(|n| n.name_of(id)) {
        Some(name) => format!("bee {name} ({id})"),
        None => format!("bee {id}"),
    }
}

/// Who a task is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Scope {
    /// Nobody in particular (the default, e.g. a session nobody gave a scope): reads only.
    #[default]
    Nothing,
    /// The user in person (operator CLI and API): every cell, no ownership confirmations.
    User,
    /// The Queen of comb `comb`: every cell on her comb; destructive actions name the
    /// owning bee; unassigned cells need the user's approval. Cells on other combs are
    /// their own Queen's.
    Queen { comb: String },
    /// Bee `id`: only the cells it owns, and new cells (which then belong to it).
    Bee { id: String },
}

impl Scope {
    /// Who acted, for the cell's "installed by" record.
    pub fn actor(&self, task: crate::TaskId, names: Option<&dyn BeeNames>) -> String {
        match self {
            Self::Nothing => format!("task {task}"),
            Self::User => "the user".into(),
            Self::Queen { .. } => format!("the Queen (task {task})"),
            Self::Bee { id } => format!("{} (task {task})", describe_bee(names, id)),
        }
    }

    /// Does a call with `access` need the cell's current owner to decide?
    pub fn needs_owner(&self, access: Access) -> bool {
        match (self, access) {
            (_, Access::Read) | (Self::User | Self::Nothing, _) => false,
            (Self::Queen { .. }, Access::ReadScoped) => false,
            (Self::Queen { .. } | Self::Bee { .. }, _) => true,
        }
    }

    /// The bee this scope is (bee sessions only).
    pub fn bee(&self) -> Option<&str> {
        match self {
            Self::Bee { id } => Some(id),
            _ => None,
        }
    }
}

/// The scope check's answer for one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeDecision {
    Allow,
    /// Allowed; if the label policy asks a human anyway, this goes first in the reason
    /// (it names the cell's bee).
    Note(String),
    /// Allowed after the user approves; the reason says why.
    Confirm(String),
    /// Out of scope: refused without asking anyone.
    Refuse(String),
}

/// Decide whether a task with `scope` may do `target.access` to cell `name`, owned by
/// `owner` (None = it does not exist; only looked up when [`Scope::needs_owner`]).
pub fn check(
    scope: &Scope,
    target: &Target,
    name: &str,
    owner: Option<&Owner>,
    names: Option<&dyn BeeNames>,
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
        Scope::Queen { comb } => match (target.access, owner) {
            (Access::Change, Some(Owner::Bee(id))) => match comb_of(id) {
                Some(c) if c != comb => ScopeDecision::Refuse(format!(
                    "not yours: the {kind} `{name}` is on comb {c}; ask that comb's Queen"
                )),
                _ => ScopeDecision::Note(format!(
                    "`{name}` is looked after by {}",
                    describe_bee(names, id)
                )),
            },
            (Access::Change, Some(Owner::Unassigned)) => ScopeDecision::Confirm(format!(
                "`{name}` is not assigned to any bee (only you decide about it); the Queen \
                 wants to {} it",
                target.verb
            )),
            _ => ScopeDecision::Allow,
        },
        Scope::Bee { id } => match owner {
            Some(Owner::Bee(o)) if o == id => ScopeDecision::Allow,
            // Creating is a change to a cell that doesn't exist yet; it becomes the bee's.
            None => ScopeDecision::Allow,
            Some(Owner::Bee(other)) => ScopeDecision::Refuse(format!(
                "not yours: the {kind} `{name}` belongs to {}; ask the Queen",
                describe_bee(names, other)
            )),
            Some(Owner::Unassigned) => ScopeDecision::Refuse(format!(
                "not yours: the {kind} `{name}` is not assigned to any bee; only the Queen or \
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
        Scope::User | Scope::Queen { .. } => return true,
        Scope::Nothing => return false,
        Scope::Bee { .. } => {}
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

/// Looks up who owns a cell. `Ok(None)`: no such cell. A cell without an owner record must
/// be reported as [`Owner::Unassigned`].
pub trait ResourceOwners: Send + Sync {
    fn owner<'a>(
        &'a self,
        kind: ResourceKind,
        name: &'a str,
    ) -> BoxFuture<'a, Result<Option<Owner>, String>>;
}

/// One cell as the cell store sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellInfo {
    /// The cell's name (its app).
    pub name: String,
    pub owner: Owner,
    /// The comb it is on (None: not recorded yet, a cell from before combs had ids).
    pub comb: Option<String>,
    /// Who assigned it to its bee, and when (RFC 3339).
    pub assigned_by: Option<String>,
    pub assigned_at: Option<String>,
    /// The old `melissa.io/owner` value (`owner`, `job-<key>`) of a cell not yet migrated.
    pub legacy_owner: Option<String>,
}

/// Where cells live (the cluster): list them and change their owner. Only the reassign path
/// (the Queen's `reassign_cell` after the user's approval, or the user's own
/// `melissa-queen cell reassign`) calls [`CellStore::assign`].
pub trait CellStore: Send + Sync {
    fn cells(&self) -> BoxFuture<'_, Result<Vec<CellInfo>, String>>;
    /// Make bee `bee` (an id) the owner of cell `cell`, recording `by`. Errors if the cell
    /// does not exist.
    fn assign<'a>(
        &'a self,
        cell: &'a str,
        bee: &'a str,
        by: &'a str,
    ) -> BoxFuture<'a, Result<(), String>>;
    /// Remove cell `cell` for good: its workloads, its namespace and all its data, waiting
    /// until it is gone (stuck objects are forced; a clear error after a timeout). Only the
    /// user's own force delete calls this; no agent tool reaches it. Returns what was
    /// removed, in words.
    fn force_delete<'a>(&'a self, cell: &'a str) -> BoxFuture<'a, Result<Vec<String>, String>>;
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const UNINSTALL: Target = Target {
        kind: ResourceKind::Cell,
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

    const ADA: &str = "c-0a1b2c3d.b1";
    const BO: &str = "c-0a1b2c3d.b2";

    struct Names;
    impl BeeNames for Names {
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

    fn bee(id: &str) -> Scope {
        Scope::Bee { id: id.into() }
    }

    fn queen() -> Scope {
        Scope::Queen {
            comb: "c-0a1b2c3d".into(),
        }
    }

    fn check_(scope: &Scope, t: &Target, name: &str, owner: Option<&Owner>) -> ScopeDecision {
        check(scope, t, name, owner, Some(&Names))
    }

    #[test]
    fn owner_labels_default_to_unassigned() {
        assert_eq!(Owner::from_label(Some(ADA)), Owner::Bee(ADA.into()));
        assert_eq!(Owner::Bee(ADA.into()).label_value(), Some(ADA));
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
    fn a_bee_may_not_touch_another_bees_cell() {
        let d = check_(
            &bee(ADA),
            &UNINSTALL,
            "nextcloud",
            Some(&Owner::Bee(BO.into())),
        );
        assert_eq!(
            d,
            ScopeDecision::Refuse(
                "not yours: the cell `nextcloud` belongs to bee Bo (c-0a1b2c3d.b2); ask the Queen"
                    .into()
            )
        );
        assert!(matches!(
            check_(&bee(ADA), &UNINSTALL, "old", Some(&Owner::Unassigned)),
            ScopeDecision::Refuse(r) if r.contains("not assigned to any bee")
        ));
        assert!(matches!(
            check_(&bee(ADA), &LOGS, "nextcloud", Some(&Owner::Bee(BO.into()))),
            ScopeDecision::Refuse(_)
        ));
        // Reads of names and phases stay open.
        assert_eq!(
            check_(
                &bee(ADA),
                &STATUS,
                "nextcloud",
                Some(&Owner::Bee(BO.into()))
            ),
            ScopeDecision::Allow
        );
    }

    #[test]
    fn a_bee_may_change_its_own_cells_and_create_new_ones() {
        assert_eq!(
            check_(&bee(ADA), &UNINSTALL, "blog", Some(&Owner::Bee(ADA.into()))),
            ScopeDecision::Allow
        );
        assert_eq!(
            check_(&bee(ADA), &LOGS, "blog", Some(&Owner::Bee(ADA.into()))),
            ScopeDecision::Allow
        );
        assert_eq!(
            check_(&bee(ADA), &UNINSTALL, "new", None),
            ScopeDecision::Allow
        );
    }

    #[test]
    fn the_queen_names_the_bee_and_asks_for_unassigned_cells() {
        assert_eq!(
            check_(
                &queen(),
                &UNINSTALL,
                "nextcloud",
                Some(&Owner::Bee(ADA.into()))
            ),
            ScopeDecision::Note("`nextcloud` is looked after by bee Ada (c-0a1b2c3d.b1)".into())
        );
        assert_eq!(
            check_(&queen(), &UNINSTALL, "old", Some(&Owner::Unassigned)),
            ScopeDecision::Confirm(
                "`old` is not assigned to any bee (only you decide about it); the Queen wants \
                 to uninstall it"
                    .into()
            )
        );
        assert_eq!(
            check_(&queen(), &UNINSTALL, "new", None),
            ScopeDecision::Allow
        );
        assert_eq!(
            check_(&queen(), &LOGS, "old", Some(&Owner::Unassigned)),
            ScopeDecision::Allow
        );
        // A cell on another comb is that comb's Queen's.
        assert!(matches!(
            check_(&queen(), &UNINSTALL, "x", Some(&Owner::Bee("c-ffffffff.b1".into()))),
            ScopeDecision::Refuse(r) if r.contains("comb c-ffffffff")
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
                &queen(),
                &UNINSTALL,
                "n",
                Some(&Owner::Bee(ADA.into())),
                None
            ),
            ScopeDecision::Note("`n` is looked after by bee c-0a1b2c3d.b1".into())
        );
    }

    #[test]
    fn actors_and_lookups() {
        assert_eq!(queen().actor(3, None), "the Queen (task 3)");
        assert_eq!(
            bee(ADA).actor(7, Some(&Names)),
            "bee Ada (c-0a1b2c3d.b1) (task 7)"
        );
        assert_eq!(Scope::User.actor(0, None), "the user");
        assert!(!queen().needs_owner(Access::ReadScoped));
        assert!(queen().needs_owner(Access::Change));
        assert!(bee(ADA).needs_owner(Access::ReadScoped));
        assert!(!bee(ADA).needs_owner(Access::Read));
        assert!(!Scope::User.needs_owner(Access::Change));
        assert_eq!(bee(ADA).bee(), Some(ADA));
        assert_eq!(queen().bee(), None);
    }

    #[test]
    fn bees_are_told_their_limits_and_nobody_sees_nothing() {
        let params = || json!({"properties": {"app": {"type": "string"}}});
        let mut p = params();
        assert!(present(&bee(ADA), &UNINSTALL, &mut p));
        assert!(p["properties"]["app"]["description"]
            .as_str()
            .unwrap()
            .contains("you look after"));
        assert!(!present(&Scope::Nothing, &LOGS, &mut params()));
        assert!(present(&Scope::Nothing, &STATUS, &mut params()));
        let mut p = params();
        assert!(present(&queen(), &UNINSTALL, &mut p));
        assert_eq!(p, params());
    }
}
