//! Tool labels and the policy that decides whether a tool call may run.
//!
//! Policy keys off labels, never tool names, so a new tool is safe by construction as
//! long as it is labelled honestly. Labels are part of the reviewed tool definition.

use std::collections::BTreeSet;

use super::scope::Scope;

/// What running the tool changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Effect {
    /// Read-only.
    None,
    /// Changes state but can be undone (scale, restart, install an app).
    Reversible,
    /// Takes a service offline without losing anything (stop an app); undone by its
    /// counterpart (start). Always confirmed, like a destructive action, but never shown
    /// as one: nothing is deleted.
    Disruptive,
    /// Can lose data or availability (remove an app, restore over data, delete a volume).
    Destructive,
    /// Changes who looks after a workspace (reassign). Always confirmed: only the user decides
    /// ownership, whoever asks.
    Ownership,
}

/// Security-relevant facts about a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Labels {
    /// Reads the user's private data (app data, backups, secrets by reference).
    pub reads_private: bool,
    /// Returns content an outsider can influence (web pages, app logs, mail).
    pub reads_untrusted: bool,
    /// Can send data off the device (network requests, notifications, uploads).
    pub egress: bool,
    pub effect: Effect,
}

impl Labels {
    pub const READ_ONLY: Self = Self {
        reads_private: false,
        reads_untrusted: false,
        egress: false,
        effect: Effect::None,
    };
}

/// Per-task facts the policy needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskContext {
    /// A human is watching this task and can answer confirmations quickly.
    pub attended: bool,
    /// The task has seen output from a `reads_untrusted` tool (possible prompt injection).
    pub tainted: bool,
    /// Tools the owner pre-approved for this task (reversible effects only).
    pub preapproved: BTreeSet<String>,
    /// May start agent agents (the lead agent only). Tools marked [`crate::tools::Tool::lead_only`]
    /// are hidden from, and refused for, every other session: agents cannot start agents.
    pub delegate: bool,
    /// What this task may change ([`super::scope`]); checked before the label policy.
    pub scope: Scope,
}

/// The outcome for one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// Run only after a human approves. The reason is shown to them.
    Confirm(String),
}

/// The confirmation reason for [`Effect::Disruptive`]. Deliberately free of the word
/// "destructive": the UI marks cards with that word as dangerous.
pub const STOPS_SERVICE: &str =
    "takes a running service offline (it can be started again; nothing is deleted)";

/// Decide whether `tool` (with `labels`) may run now in `ctx`.
pub fn decide(tool: &str, labels: &Labels, ctx: &TaskContext) -> Decision {
    if labels.effect == Effect::Destructive {
        return Decision::Confirm("destructive action".into());
    }
    if labels.effect == Effect::Disruptive {
        return Decision::Confirm(STOPS_SERVICE.into());
    }
    if labels.effect == Effect::Ownership {
        return Decision::Confirm(
            "changes who looks after a workspace (only you decide that)".into(),
        );
    }
    if ctx.tainted && (labels.egress || labels.effect != Effect::None) {
        return Decision::Confirm(
            "this task has read untrusted content; actions and outbound calls need approval".into(),
        );
    }
    if labels.egress && labels.reads_private && !ctx.attended {
        return Decision::Confirm("can send private data off the device".into());
    }
    if labels.effect == Effect::Reversible && !ctx.attended && !ctx.preapproved.contains(tool) {
        return Decision::Confirm("changes state while nobody is watching".into());
    }
    Decision::Allow
}

/// Reject tool sets that give an unattended agent private data, untrusted input and an
/// outbound channel at once (the classic prompt-injection exfiltration setup).
pub fn check_tool_set<'a>(
    tools: impl IntoIterator<Item = &'a Labels>,
    attended: bool,
) -> Result<(), String> {
    if attended {
        return Ok(());
    }
    let (mut private, mut untrusted, mut egress) = (false, false, false);
    for l in tools {
        private |= l.reads_private;
        untrusted |= l.reads_untrusted;
        egress |= l.egress;
    }
    if private && untrusted && egress {
        return Err(
            "an unattended task may not combine private data, untrusted input and outbound access"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const REVERSIBLE: Labels = Labels {
        effect: Effect::Reversible,
        ..Labels::READ_ONLY
    };
    const DESTRUCTIVE: Labels = Labels {
        effect: Effect::Destructive,
        ..Labels::READ_ONLY
    };
    const UNTRUSTED: Labels = Labels {
        reads_untrusted: true,
        ..Labels::READ_ONLY
    };
    const PRIVATE: Labels = Labels {
        reads_private: true,
        ..Labels::READ_ONLY
    };
    const EGRESS: Labels = Labels {
        egress: true,
        ..Labels::READ_ONLY
    };

    fn attended() -> TaskContext {
        TaskContext {
            attended: true,
            ..TaskContext::default()
        }
    }

    #[test]
    fn read_only_is_always_allowed() {
        assert_eq!(
            decide("t", &Labels::READ_ONLY, &TaskContext::default()),
            Decision::Allow
        );
        let tainted = TaskContext {
            tainted: true,
            ..TaskContext::default()
        };
        assert_eq!(decide("t", &UNTRUSTED, &tainted), Decision::Allow);
    }

    #[test]
    fn destructive_always_needs_confirmation() {
        let mut ctx = attended();
        ctx.preapproved.insert("remove_app".into());
        assert!(matches!(
            decide("remove_app", &DESTRUCTIVE, &ctx),
            Decision::Confirm(_)
        ));
    }

    #[test]
    fn disruptive_always_asks_but_is_not_called_destructive() {
        let disruptive = Labels {
            effect: Effect::Disruptive,
            ..Labels::READ_ONLY
        };
        let mut ctx = attended();
        ctx.preapproved.insert("stop_app".into());
        match decide("stop_app", &disruptive, &ctx) {
            Decision::Confirm(reason) => {
                assert_eq!(reason, STOPS_SERVICE);
                assert!(!reason.contains("destructive"));
            }
            Decision::Allow => panic!("a stop must ask"),
        }
    }

    #[test]
    fn reversible_needs_attendance_or_preapproval() {
        assert_eq!(decide("scale", &REVERSIBLE, &attended()), Decision::Allow);
        let mut ctx = TaskContext::default();
        assert!(matches!(
            decide("scale", &REVERSIBLE, &ctx),
            Decision::Confirm(_)
        ));
        ctx.preapproved.insert("scale".into());
        assert_eq!(decide("scale", &REVERSIBLE, &ctx), Decision::Allow);
    }

    #[test]
    fn taint_forces_confirmation_for_actions_and_egress() {
        let mut ctx = attended();
        ctx.tainted = true;
        ctx.preapproved.insert("scale".into());
        assert!(matches!(
            decide("scale", &REVERSIBLE, &ctx),
            Decision::Confirm(_)
        ));
        assert!(matches!(
            decide("notify", &EGRESS, &ctx),
            Decision::Confirm(_)
        ));
    }

    #[test]
    fn unattended_private_egress_needs_confirmation() {
        let both = Labels {
            reads_private: true,
            egress: true,
            ..Labels::READ_ONLY
        };
        assert!(matches!(
            decide("upload", &both, &TaskContext::default()),
            Decision::Confirm(_)
        ));
        assert_eq!(decide("upload", &both, &attended()), Decision::Allow);
    }

    #[test]
    fn dangerous_triad_is_rejected_for_unattended_tasks() {
        assert!(check_tool_set([&PRIVATE, &UNTRUSTED, &EGRESS], false).is_err());
        assert!(check_tool_set([&PRIVATE, &UNTRUSTED, &EGRESS], true).is_ok());
        assert!(check_tool_set([&PRIVATE, &UNTRUSTED], false).is_ok());
    }
}
