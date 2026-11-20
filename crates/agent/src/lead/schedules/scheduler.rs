//! The scheduler: one Tokio task that sleeps until the next due time, then sends each due
//! schedule's instruction as a turn of its agent's conversation and watches the turn.
//!
//! Rules:
//! - **Overlap: skip.** A schedule whose previous run is still going, or whose agent is busy
//!   (a chat, a job), is not run; the due time is recorded as skipped ("still running", "the
//!   agent was busy"). Queueing was the alternative; it was not chosen because a queued run
//!   starts at a time nobody chose (a nightly update at noon, behind a stuck run), and runs
//!   pile up behind a hung one. A skip is visible in Settings → Schedules, and "Run now" is
//!   one click away.
//! - **Missed runs** (Melissa down or the box asleep when one was due): the latest one runs
//!   once on wake-up if it was due less than the grace window ago (default an hour);
//!   otherwise it is recorded as missed. Earlier ones in the same gap are not run.
//! - **Time limit:** a run still going after the schedule's `max_minutes` is cancelled and
//!   recorded as timed out. A confirmation it waits for stays in "Waiting for you" until then
//!   (no auto-deny), or until the confirmation itself times out.
//!
//! Wall-clock time comes from a [`Clock`] (tests use a fake one on Tokio's paused time); the
//! loop never sleeps longer than a minute, so a clock change or a suspend is noticed soon.

use std::{
    collections::hash_map::Entry,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use futures::future::BoxFuture;
use jiff::Timestamp;
use tokio::task::AbortHandle;

use super::{
    last_at_or_before, lock, parse_cron, Creator, LastRun, Outcome, Principal, Schedule,
    ScheduleError, Schedules,
};
use crate::{
    lead::{conversation::Conversation, LeadError},
    TaskId,
};

/// A due time is run if it is found less than this late (Melissa was down, the box asleep).
pub const DEFAULT_GRACE: Duration = Duration::from_secs(3600);
/// The loop looks at the clock at least this often.
const MAX_SLEEP: Duration = Duration::from_secs(60);

/// Wall-clock time.
pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}

/// The system clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// Why a run did not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// The agent is working on something else.
    Busy,
    /// The agent no longer exists.
    Gone(String),
    Failed(String),
}

/// How a run's turn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEnd {
    Done,
    Failed(String),
    Cancelled,
}

/// Starts and watches scheduled turns (the lead agent's conversations; a fake in tests).
pub trait Runner: Send + Sync {
    /// Start the turn: `text` is the instruction, `name` the schedule's (for the label).
    fn start(&self, agent: &Principal, name: &str, text: &str) -> Result<TaskId, StartError>;
    /// Resolves when the turn has ended.
    fn wait(&self, agent: &Principal, turn: TaskId) -> BoxFuture<'static, RunEnd>;
    /// Cancel the turn (it may take a moment to end).
    fn cancel(&self, agent: &Principal, turn: TaskId) -> BoxFuture<'static, ()>;
}

struct Inner {
    schedules: Schedules,
    runner: Arc<dyn Runner>,
    grace: Duration,
    task: Mutex<Option<AbortHandle>>,
}

/// Runs the schedules. Cheap to clone; clones share state.
#[derive(Clone)]
pub struct Scheduler {
    inner: Arc<Inner>,
}

fn secs(t: Timestamp) -> u64 {
    u64::try_from(t.as_second()).unwrap_or(0)
}

impl Scheduler {
    /// Runs recorded as going when Melissa last stopped are marked interrupted. Nothing runs
    /// until [`Scheduler::start`].
    pub fn new(schedules: Schedules, runner: Arc<dyn Runner>, grace: Duration) -> Self {
        schedules.recover_interrupted();
        Self {
            inner: Arc::new(Inner {
                schedules,
                runner,
                grace,
                task: Mutex::new(None),
            }),
        }
    }

    pub fn schedules(&self) -> &Schedules {
        &self.inner.schedules
    }

    /// Start the loop (once). Must be called inside a Tokio runtime.
    pub fn start(&self) {
        let mut task = lock(&self.inner.task);
        if task.is_some() {
            return;
        }
        let me = self.clone();
        *task = Some(tokio::spawn(me.run()).abort_handle());
    }

    /// Stop the loop (runs going on are left alone).
    pub fn stop(&self) {
        if let Some(t) = lock(&self.inner.task).take() {
            t.abort();
        }
    }

    async fn run(self) {
        loop {
            // Registered before looking, so a change made while looking wakes the next sleep.
            let changed = self.inner.schedules.shared.changed.notified();
            let wait = self.tick();
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = changed => {}
            }
        }
    }

    /// Deal with every due time up to now; returns how long to sleep before looking again.
    pub(crate) fn tick(&self) -> Duration {
        let schedules = &self.inner.schedules;
        let tz = schedules.time_zone().clone();
        let now = schedules.now();
        for s in schedules.list() {
            if !s.enabled {
                continue;
            }
            let Ok(cron) = parse_cron(&s.cron) else {
                continue;
            };
            let Some(due) = last_at_or_before(&cron, now, &tz) else {
                continue;
            };
            let at = secs(due);
            if at <= s.handled_until {
                continue;
            }
            schedules.mark_handled(s.id, at);
            let late = due.duration_until(now);
            if Duration::try_from(late).is_ok_and(|l| l > self.inner.grace) {
                schedules.record(
                    s.id,
                    LastRun {
                        at,
                        outcome: Outcome::Missed,
                        detail: Some("Melissa was not running when it was due".into()),
                        turn: None,
                        ended: None,
                        manual: false,
                    },
                );
                continue;
            }
            self.fire(&s, at, false);
        }
        let next = schedules
            .list()
            .iter()
            .filter_map(|s| schedules.next_run(s))
            .min();
        match next {
            Some(t) => Duration::try_from(now.duration_until(t))
                .unwrap_or(Duration::ZERO)
                .min(MAX_SLEEP),
            None => MAX_SLEEP,
        }
    }

    /// The owner's "Run now" (audited `schedule.run_now`): runs whether the schedule is on or
    /// not, with the same overlap rule. Returns what happened.
    pub fn run_now(&self, id: u64, by: &Creator) -> Result<LastRun, ScheduleError> {
        let schedules = &self.inner.schedules;
        let s = schedules.get(id).ok_or(ScheduleError::NotFound(id))?;
        schedules.audit(
            "schedule.run_now",
            by.actor(),
            &format!("schedule {} `{}`", s.id, s.name),
        );
        Ok(self.fire(&s, secs(schedules.now()), true))
    }

    /// Start a run of `s` due at `at`, unless one is going or the agent is busy.
    fn fire(&self, s: &Schedule, at: u64, manual: bool) -> LastRun {
        let schedules = &self.inner.schedules;
        let not_run = |outcome, detail: String| LastRun {
            at,
            outcome,
            detail: Some(detail),
            turn: None,
            ended: None,
            manual,
        };
        let run = {
            // Held across the start, so two triggers (the clock and "Run now") never both start.
            let mut running = lock(&schedules.shared.running);
            match running.entry(s.id) {
                Entry::Occupied(_) => {
                    not_run(Outcome::Skipped, "the previous run was still going".into())
                }
                Entry::Vacant(slot) => {
                    match self.inner.runner.start(&s.agent, &s.name, &s.instruction) {
                        Err(StartError::Busy) => {
                            not_run(Outcome::Skipped, "the agent was busy".into())
                        }
                        Err(StartError::Gone(e) | StartError::Failed(e)) => {
                            not_run(Outcome::Failed, e)
                        }
                        Ok(turn) => {
                            slot.insert((s.agent.clone(), turn));
                            LastRun {
                                at: secs(schedules.now()),
                                outcome: Outcome::Running,
                                detail: None,
                                turn: Some(turn),
                                ended: None,
                                manual,
                            }
                        }
                    }
                }
            }
        };
        schedules.record(s.id, run.clone());
        if let Some(turn) = run.turn {
            tokio::spawn(self.clone().watch(
                s.id,
                s.agent.clone(),
                turn,
                s.max_minutes,
                run.clone(),
            ));
        }
        run
    }

    /// Wait for a run's turn to end, at most `max_minutes`, and record how it ended.
    async fn watch(self, id: u64, agent: Principal, turn: TaskId, max_minutes: u32, run: LastRun) {
        let runner = self.inner.runner.clone();
        let limit = Duration::from_secs(u64::from(max_minutes) * 60);
        let (outcome, detail) = tokio::select! {
            end = runner.wait(&agent, turn) => match end {
                RunEnd::Done => (Outcome::Done, None),
                RunEnd::Failed(e) => (Outcome::Failed, Some(e)),
                RunEnd::Cancelled => (Outcome::Cancelled, None),
            },
            () = tokio::time::sleep(limit) => {
                runner.cancel(&agent, turn).await;
                (
                    Outcome::TimedOut,
                    Some(format!("still going after {max_minutes} min; cancelled")),
                )
            }
        };
        let schedules = &self.inner.schedules;
        lock(&schedules.shared.running).remove(&id);
        schedules.record(
            id,
            LastRun {
                outcome,
                detail,
                ended: Some(secs(schedules.now())),
                ..run
            },
        );
    }
}

/// Runs schedules in the lead agent's conversations: its own chat, and each agent's.
pub(in crate::lead) struct LeadRunner {
    pub(in crate::lead) lead: Weak<crate::lead::Inner>,
}

impl LeadRunner {
    fn conversation(&self, agent: &Principal) -> Option<Conversation> {
        let lead = self.lead.upgrade()?;
        match agent {
            Principal::Lead => Some(lead.conv.clone()),
            Principal::Agent(id) => lead.pool.conversation_of(id),
        }
    }
}

impl Runner for LeadRunner {
    fn start(&self, agent: &Principal, name: &str, text: &str) -> Result<TaskId, StartError> {
        let lead = self
            .lead
            .upgrade()
            .ok_or_else(|| StartError::Failed("Melissa is stopping".into()))?;
        let started = match agent {
            Principal::Lead => lead
                .conv
                .send_scheduled(name, text, "schedule")
                .map(|(t, _)| t),
            Principal::Agent(id) => lead.pool.scheduled_send(id, name, text),
        };
        started.map_err(|e| match e {
            LeadError::Busy => StartError::Busy,
            LeadError::NoSuchAgent(id) => StartError::Gone(format!("no agent `{id}` any more")),
            other => StartError::Failed(other.to_string()),
        })
    }

    fn wait(&self, agent: &Principal, turn: TaskId) -> BoxFuture<'static, RunEnd> {
        let conv = self.conversation(agent);
        Box::pin(async move {
            let Some(conv) = conv else {
                return RunEnd::Failed("the conversation is gone".into());
            };
            while !conv.wait_turn(turn, Duration::from_secs(3600)).await {}
            match conv.last_outcome() {
                Some(o) if o.turn == turn => match (o.stop_reason.as_str(), o.error) {
                    ("cancelled", _) => RunEnd::Cancelled,
                    (_, Some(e)) => RunEnd::Failed(e),
                    ("error", None) => RunEnd::Failed("the agent stopped with an error".into()),
                    _ => RunEnd::Done,
                },
                // Another turn ended since (rare): this one ended without an error we saw.
                _ => RunEnd::Done,
            }
        })
    }

    fn cancel(&self, agent: &Principal, turn: TaskId) -> BoxFuture<'static, ()> {
        let conv = self.conversation(agent);
        Box::pin(async move {
            if let Some(conv) = conv {
                conv.cancel(Some(turn), "schedule").await;
            }
        })
    }
}
