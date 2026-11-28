//! Melissa applies her own changes, so the owner never presses "restart".
//!
//! - An install or a sign-in needs nothing: the backends look for a missing program again
//!   (melissa-agent `ProgramPath`) and check the login file every time, so a new CLI or login
//!   counts at once.
//! - Saved provider settings (an API key, a program path, enabling a provider) are read when
//!   the lead agent starts. After such a change it restarts itself, but only once nothing runs
//!   that the restart would cut off ([`Ui::busy`]: a chat turn, a helper's job, an agent task)
//!   and the owner stopped changing things for a moment ([`QUIET`]). Until then the pages say
//!   "Ready after the current task".
//! - Where Melissa can't restart herself (a run from source), the page says how instead.
//!
//! Every automatic restart is audited (`settings.apply`).

use std::time::{Duration, Instant};

use melissa_settings::updates::UpdateAction;

use crate::ui::Ui;

/// How long after the last change Melissa waits before she restarts.
pub(super) const QUIET: Duration = Duration::from_secs(4);
/// How often she looks whether she is idle.
const TICK: Duration = Duration::from_secs(2);

/// Where an automatic apply stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ApplyState {
    /// Nothing waits (or Melissa can't restart herself).
    #[default]
    Idle,
    /// Saved changes wait for Melissa to be idle.
    Waiting,
    /// The restart was requested.
    Restarting,
}

#[derive(Default)]
pub(super) struct Apply {
    state: ApplyState,
    last_change: Option<Instant>,
}

/// What the pages show about saved changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pending {
    /// Waiting for a chat turn, a task or an install/sign-in to end.
    AfterTask,
    /// Restarting now (or in a moment).
    Now,
    /// Melissa can't restart herself here: the owner restarts her.
    Manual,
}

impl Pending {
    /// The short status word for a provider whose change waits.
    pub(crate) fn word(self) -> &'static str {
        match self {
            Self::AfterTask => "Ready after the current task",
            Self::Now => "Applying…",
            Self::Manual => "Ready after a restart",
        }
    }
}

/// Saved settings changed: restart once Melissa is idle (where she can restart herself).
pub(super) fn schedule(ui: &Ui) {
    let s = &ui.settings;
    if !s.parts().updates.can(UpdateAction::Restart) {
        return;
    }
    {
        let mut a = s.apply_lock();
        a.last_change = Some(Instant::now());
        if a.state != ApplyState::Idle {
            return;
        }
        a.state = ApplyState::Waiting;
    }
    let ui = ui.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(TICK).await;
            let s = &ui.settings;
            if !s.parts().store.restart_needed() {
                s.apply_lock().state = ApplyState::Idle;
                return;
            }
            let quiet = s
                .apply_lock()
                .last_change
                .is_none_or(|t| t.elapsed() >= QUIET);
            // Not while a check, an update or a rollback runs on the host either.
            let updating =
                s.parts().updates.helper_installed() && s.parts().updates.state().busy().is_some();
            if !quiet || updating || ui.busy() || s.jobs_running().await {
                continue;
            }
            s.apply_lock().state = ApplyState::Restarting;
            s.audit(
                "settings.apply",
                "lead",
                "restart to use saved provider settings (idle)",
                None,
            );
            if let Err(e) = s.parts().updates.request(UpdateAction::Restart).await {
                tracing::warn!("automatic restart failed: {e}");
                s.audit("settings.apply", "lead", "restart failed", None);
                s.apply_lock().state = ApplyState::Idle;
            }
            return;
        }
    });
}

/// Whether saved settings wait, and what for.
pub(crate) async fn pending(ui: &Ui) -> Option<Pending> {
    let s = &ui.settings;
    if !s.parts().store.restart_needed() {
        return None;
    }
    if !s.parts().updates.can(UpdateAction::Restart) {
        return Some(Pending::Manual);
    }
    let restarting = s.apply_lock().state == ApplyState::Restarting;
    Some(if restarting {
        Pending::Now
    } else if ui.busy() || s.jobs_running().await {
        Pending::AfterTask
    } else {
        Pending::Now
    })
}
