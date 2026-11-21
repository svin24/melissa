//! Scheduled jobs: "Ada: update all apps and the system, nightly at 3:00".
//!
//! A [`Schedule`] gives one agent (by id, so a rename keeps it, or the lead agent) a
//! recurring instruction on a 5-field cron expression, evaluated in the box's local time
//! zone. The [`Scheduler`] sends the instruction as a turn of that agent's conversation when
//! it is due (labelled "Scheduled: <name>" in the feed); the turn runs unattended, so what it
//! may do on its own is what the owner's "Always allow" rules cover, and anything else waits
//! in "Waiting for you" as usual.
//!
//! Who changes schedules: the owner (Settings → Schedules and the JSON API) directly; agents
//! only through the `schedule_job` tool ([`ScheduleJobTool`]), for themselves (the lead agent
//! for any agent), and every create or remove there needs the owner's yes, which no
//! "Always allow" rule can give (see [`crate::tools::Tool::owner_approval`]).
//!
//! Saved as JSON (mode 0600) in the state directory (`schedules.json`), never in the repo.
//! Every change and every run is audited (`schedule.*`).

mod scheduler;
#[cfg(test)]
mod tests;
mod tool;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use croner::{
    parser::{CronParser, Seconds, Year},
    Cron,
};
pub use jiff::{tz::TimeZone, Timestamp};
use serde::{Deserialize, Serialize};

pub use crate::tools::permissions::Principal;
use crate::{
    tools::{AgentNames, Scope},
    AuditLog, AuditRecord, TaskId,
};
pub(in crate::lead) use scheduler::LeadRunner;
pub use scheduler::{Clock, RunEnd, Runner, Scheduler, StartError, SystemClock, DEFAULT_GRACE};
pub use tool::ScheduleJobTool;

/// At most this many schedules on a device.
pub const MAX_SCHEDULES: usize = 100;
/// Longest schedule name, in characters.
pub const MAX_NAME_CHARS: usize = 60;
/// Longest instruction, in bytes.
pub const MAX_INSTRUCTION_BYTES: usize = 4000;
/// A run's time limit when none is given, in minutes.
pub const DEFAULT_MAX_MINUTES: u32 = 60;
/// The longest time limit, in minutes (a day).
pub const MAX_MAX_MINUTES: u32 = 24 * 60;
/// How long a scheduled run's confirmation waits for the owner by default, in minutes.
pub const DEFAULT_APPROVAL_WAIT_MINUTES: u32 = 10;
/// The shortest and longest wait the owner can set, in minutes (1 minute to a day).
pub const MIN_APPROVAL_WAIT_MINUTES: u32 = 1;
pub const MAX_APPROVAL_WAIT_MINUTES: u32 = 24 * 60;
/// How much sooner than its agent's own tool-call limit a scheduled run's confirmation gives
/// up, so it is withdrawn before the agent stops waiting for it.
pub const TOOL_CALL_MARGIN: std::time::Duration = std::time::Duration::from_secs(60);

/// How long a scheduled run's confirmation really waits: the owner's `setting`, but never
/// past the backend's own tool-call `limit` (less [`TOOL_CALL_MARGIN`]), if it has one; at
/// least a second.
pub fn effective_wait(
    setting: std::time::Duration,
    limit: Option<std::time::Duration>,
) -> std::time::Duration {
    match limit {
        Some(limit) => setting
            .min(limit.saturating_sub(TOOL_CALL_MARGIN))
            .max(std::time::Duration::from_secs(1)),
        None => setting,
    }
}

/// Who made a schedule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Creator {
    /// The owner, in the UI or the API (also the operator's `melissa-server tool`).
    Owner,
    /// The lead agent, approved by the owner.
    Lead,
    /// The agent with this id, approved by the owner.
    Agent(String),
}

impl Creator {
    /// Who a tool call's scope acts as. None: a task with no scope (it may not schedule).
    pub fn of(scope: &Scope) -> Option<Self> {
        match scope {
            Scope::User => Some(Self::Owner),
            Scope::Lead { .. } => Some(Self::Lead),
            Scope::Agent { id } => Some(Self::Agent(id.clone())),
            Scope::Nothing => None,
        }
    }

    /// The audit actor: `ui`, `lead` or `agent` (the agent's id goes in the detail).
    pub fn actor(&self) -> &'static str {
        match self {
            Self::Owner => "ui",
            Self::Lead => "lead",
            Self::Agent(_) => "agent",
        }
    }

    fn audit_prefix(&self) -> String {
        match self {
            Self::Agent(id) => format!("by agent {id}: "),
            _ => String::new(),
        }
    }
}

/// How a run ended (or why there was none).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The turn is still going.
    Running,
    /// The turn finished.
    Done,
    /// The turn could not start or ended with an error.
    Failed,
    /// Someone cancelled the turn.
    Cancelled,
    /// The turn ran past the schedule's time limit and was cancelled.
    TimedOut,
    /// Not run: the previous run was still going, or the agent was busy.
    Skipped,
    /// Not run: Melissa was down (or asleep) when it was due, longer ago than the grace window.
    Missed,
    /// Melissa stopped while the run was going.
    Interrupted,
}

impl Outcome {
    /// Short words for the UI.
    pub fn words(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Done => "Done",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
            Self::TimedOut => "Timed out",
            Self::Skipped => "Skipped",
            Self::Missed => "Missed",
            Self::Interrupted => "Interrupted",
        }
    }

    fn audit_action(self) -> &'static str {
        match self {
            Self::Running => "schedule.run_started",
            Self::Skipped => "schedule.run_skipped",
            Self::Missed => "schedule.run_missed",
            _ => "schedule.run_finished",
        }
    }
}

/// The last run of a schedule (or the last due time it did not run).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastRun {
    /// When it started, or when it was due (skipped, missed). Unix seconds.
    pub at: u64,
    pub outcome: Outcome,
    /// Why, in words ("still running", an error), if there is more to say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The turn in the agent's conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<TaskId>,
    /// When the turn ended. Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<u64>,
    /// Started with "Run now" rather than by the clock.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub manual: bool,
}

/// A saved schedule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    /// Never reused.
    pub id: u64,
    pub name: String,
    /// Whose conversation runs it.
    pub agent: Principal,
    /// What the agent is asked to do, sent as the turn's message.
    pub instruction: String,
    /// 5-field cron expression (minute hour day-of-month month day-of-week), local time.
    pub cron: String,
    pub enabled: bool,
    /// A run still going after this many minutes is cancelled (recorded as timed out).
    pub max_minutes: u32,
    pub created_by: Creator,
    /// Unix seconds.
    pub created_at: u64,
    /// Due times at or before this (Unix seconds) are dealt with: run, skipped or missed.
    /// Set to "now" on creation and whenever the timing or the switch changes, so a change
    /// never runs a job for a time that has passed.
    pub handled_until: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<LastRun>,
}

/// What the owner (or an agent's tool call) gives to make or change a schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleInput {
    pub name: String,
    pub agent: Principal,
    pub instruction: String,
    pub cron: String,
    pub enabled: bool,
    /// None: [`DEFAULT_MAX_MINUTES`].
    pub max_minutes: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScheduleError {
    /// Bad input (name, instruction, cron expression, time limit).
    #[error("{0}")]
    Invalid(String),
    #[error("no schedule {0}")]
    NotFound(u64),
    #[error("at most {MAX_SCHEDULES} schedules")]
    TooMany,
    #[error("cannot read {path}: {message}")]
    Read { path: String, message: String },
    #[error("{path} is not valid: {message}")]
    Parse { path: String, message: String },
    #[error("cannot save {path}: {message}")]
    Write { path: String, message: String },
}

/// Parse a 5-field cron expression (minute hour day-of-month month day-of-week; names like
/// `MON` and `JAN`, ranges, lists and steps; day-of-month and day-of-week match if either
/// does, as in Vixie cron). No seconds or years.
pub fn parse_cron(expr: &str) -> Result<Cron, ScheduleError> {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(ScheduleError::Invalid(
            "a schedule has five fields: minute hour day-of-month month day-of-week \
             (e.g. `0 3 * * *` for every day at 03:00)"
                .into(),
        ));
    }
    CronParser::builder()
        .seconds(Seconds::Disallowed)
        .year(Year::Disallowed)
        .build()
        .parse(&fields.join(" "))
        .map_err(|e| ScheduleError::Invalid(format!("not a valid schedule: {e}")))
}

/// The first due time strictly after `after`, in time zone `tz`.
pub fn next_after(cron: &Cron, after: Timestamp, tz: &TimeZone) -> Option<Timestamp> {
    cron.find_next_occurrence(&after.to_zoned(tz.clone()), false)
        .ok()
        .map(|z| z.timestamp())
}

/// The last due time at or before `at` (whole seconds), in time zone `tz`.
pub fn last_at_or_before(cron: &Cron, at: Timestamp, tz: &TimeZone) -> Option<Timestamp> {
    let at = Timestamp::from_second(at.as_second()).ok()?;
    cron.find_previous_occurrence(&at.to_zoned(tz.clone()), true)
        .ok()
        .map(|z| z.timestamp())
}

/// The schedule presets the UI offers, and what a cron expression is in their terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// Every hour at this minute.
    Hourly { minute: u8 },
    /// Every day at this time.
    Daily { hour: u8, minute: u8 },
    /// Every week on this day (0 = Sunday … 6 = Saturday) at this time.
    Weekly { day: u8, hour: u8, minute: u8 },
}

/// Day names, Sunday first (cron day 0 and 7).
pub const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

impl Preset {
    /// The cron expression.
    pub fn cron(self) -> String {
        match self {
            Self::Hourly { minute } => format!("{minute} * * * *"),
            Self::Daily { hour, minute } => format!("{minute} {hour} * * *"),
            Self::Weekly { day, hour, minute } => format!("{minute} {hour} * * {day}"),
        }
    }

    /// The preset a cron expression is, if it is one.
    pub fn of(cron: &str) -> Option<Self> {
        let f: Vec<&str> = cron.split_whitespace().collect();
        let [m, h, dom, mon, dow] = f.as_slice() else {
            return None;
        };
        let minute = number(m, 59)?;
        if (*dom, *mon) != ("*", "*") {
            return None;
        }
        if *h == "*" {
            return (*dow == "*").then_some(Self::Hourly { minute });
        }
        let hour = number(h, 23)?;
        if *dow == "*" {
            return Some(Self::Daily { hour, minute });
        }
        let day = weekday(dow)?;
        Some(Self::Weekly { day, hour, minute })
    }
}

fn number(field: &str, max: u8) -> Option<u8> {
    if field.is_empty() || !field.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    field.parse::<u8>().ok().filter(|n| *n <= max)
}

/// One day of the week as cron writes it (`0`–`7`, `SUN`…`SAT`), Sunday = 0.
fn weekday(field: &str) -> Option<u8> {
    if let Some(n) = number(field, 7) {
        return Some(n % 7);
    }
    let up = field.to_ascii_uppercase();
    ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"]
        .iter()
        .position(|d| *d == up)
        .and_then(|i| u8::try_from(i).ok())
}

/// A schedule in words for the common patterns ("Every day at 03:00"), else the expression.
pub fn describe(cron: &str) -> String {
    if let Some(p) = Preset::of(cron) {
        return match p {
            Preset::Hourly { minute: 0 } => "Every hour".into(),
            Preset::Hourly { minute } => format!("Every hour at :{minute:02}"),
            Preset::Daily { hour, minute } => format!("Every day at {hour:02}:{minute:02}"),
            Preset::Weekly { day, hour, minute } => {
                format!("Every {} at {hour:02}:{minute:02}", DAYS[usize::from(day)])
            }
        };
    }
    let f: Vec<&str> = cron.split_whitespace().collect();
    match f.as_slice() {
        ["*", "*", "*", "*", "*"] => "Every minute".into(),
        [m, "*", "*", "*", "*"] if m.starts_with("*/") => match number(&m[2..], 59) {
            Some(n) if n > 0 => format!("Every {n} minutes"),
            _ => cron.trim().to_string(),
        },
        [m, h, "*", "*", dow] => match (number(m, 59), number(h, 23)) {
            (Some(m), Some(h))
                if dow.eq_ignore_ascii_case("1-5") || dow.eq_ignore_ascii_case("MON-FRI") =>
            {
                format!("Weekdays at {h:02}:{m:02}")
            }
            _ => cron.trim().to_string(),
        },
        [m, h, dom, "*", "*"] => match (number(m, 59), number(h, 23), number(dom, 31)) {
            (Some(m), Some(h), Some(d)) if d > 0 => {
                format!("Every month on day {d} at {h:02}:{m:02}")
            }
            _ => cron.trim().to_string(),
        },
        _ => cron.trim().to_string(),
    }
}

/// Check and tidy an input (trimmed name and instruction, single-spaced cron).
fn validate(input: &ScheduleInput) -> Result<ScheduleInput, ScheduleError> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(ScheduleError::Invalid("give the schedule a name".into()));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(ScheduleError::Invalid(format!(
            "the name is longer than {MAX_NAME_CHARS} characters"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(ScheduleError::Invalid(
            "the name has control characters".into(),
        ));
    }
    let instruction = input.instruction.trim();
    if instruction.is_empty() {
        return Err(ScheduleError::Invalid(
            "say what the agent should do".into(),
        ));
    }
    if instruction.len() > MAX_INSTRUCTION_BYTES {
        return Err(ScheduleError::Invalid(format!(
            "the instruction is longer than {MAX_INSTRUCTION_BYTES} bytes"
        )));
    }
    let cron = input.cron.split_whitespace().collect::<Vec<_>>().join(" ");
    parse_cron(&cron)?;
    let max = input.max_minutes.unwrap_or(DEFAULT_MAX_MINUTES);
    if !(1..=MAX_MAX_MINUTES).contains(&max) {
        return Err(ScheduleError::Invalid(format!(
            "the time limit is 1 to {MAX_MAX_MINUTES} minutes"
        )));
    }
    Ok(ScheduleInput {
        name: name.to_string(),
        agent: input.agent.clone(),
        instruction: instruction.to_string(),
        cron,
        enabled: input.enabled,
        max_minutes: Some(max),
    })
}

/// Check an input without saving it (the tool checks before the owner is asked).
pub fn check_input(input: &ScheduleInput) -> Result<(), ScheduleError> {
    validate(input).map(|_| ())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    schedules: Vec<Schedule>,
    /// How long a scheduled run's confirmation waits for the owner before it is withdrawn
    /// (minutes; the owner's setting in Settings → Schedules).
    #[serde(default = "default_approval_wait")]
    approval_wait_minutes: u32,
}

fn default_approval_wait() -> u32 {
    DEFAULT_APPROVAL_WAIT_MINUTES
}

impl Default for State {
    fn default() -> Self {
        Self {
            next_id: 0,
            schedules: Vec::new(),
            approval_wait_minutes: DEFAULT_APPROVAL_WAIT_MINUTES,
        }
    }
}

struct Shared {
    state: Mutex<State>,
    /// Runs going now: schedule id → (agent, turn). In memory only.
    running: Mutex<HashMap<u64, (Principal, TaskId)>>,
    /// Woken on every change, so the scheduler looks again.
    changed: tokio::sync::Notify,
}

/// The device's schedules. Cheap to clone; clones share state.
#[derive(Clone)]
pub struct Schedules {
    shared: Arc<Shared>,
    /// Where they are saved (None: in memory only, e.g. tests).
    path: Option<PathBuf>,
    tz: TimeZone,
    clock: Arc<dyn Clock>,
    audit: Option<AuditLog>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned lock only means a panic elsewhere; the data is still usable.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Schedules {
    fn with_state(state: State, path: Option<PathBuf>, tz: TimeZone) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(state),
                running: Mutex::new(HashMap::new()),
                changed: tokio::sync::Notify::new(),
            }),
            path,
            tz,
            clock: Arc::new(SystemClock),
            audit: None,
        }
    }

    /// In memory only, no schedules.
    pub fn in_memory(tz: TimeZone) -> Self {
        Self::with_state(State::default(), None, tz)
    }

    /// Load from `path` (a missing file: no schedules). Changes are saved there. Due times
    /// are evaluated in `tz` (the box's: [`TimeZone::system`]).
    pub fn open(path: impl Into<PathBuf>, tz: TimeZone) -> Result<Self, ScheduleError> {
        let path = path.into();
        let shown = path.display().to_string();
        let state = match std::fs::read(&path) {
            Ok(data) => serde_json::from_slice(&data).map_err(|e| ScheduleError::Parse {
                path: shown,
                message: e.to_string(),
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => {
                return Err(ScheduleError::Read {
                    path: shown,
                    message: e.to_string(),
                })
            }
        };
        Ok(Self::with_state(state, Some(path), tz))
    }

    /// Use `clock` for "now" (tests).
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Audit changes and runs to `audit`.
    pub fn with_audit(mut self, audit: Option<AuditLog>) -> Self {
        self.audit = audit;
        self
    }

    /// Where the schedules are saved, if anywhere.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The time zone due times are evaluated in.
    pub fn time_zone(&self) -> &TimeZone {
        &self.tz
    }

    /// A time (Unix seconds) in the schedules' time zone, for people: `2026-11-21 03:00`.
    pub fn local_time(&self, secs: u64) -> String {
        i64::try_from(secs)
            .ok()
            .and_then(|s| Timestamp::from_second(s).ok())
            .map(|t| {
                t.to_zoned(self.tz.clone())
                    .strftime("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_default()
    }

    /// Now, by the schedules' clock.
    pub fn now(&self) -> Timestamp {
        self.clock.now()
    }

    fn now_secs(&self) -> u64 {
        u64::try_from(self.clock.now().as_second()).unwrap_or(0)
    }

    /// How long a scheduled run's confirmation waits for the owner, in minutes. A saved value
    /// outside the bounds (edited by hand) is clamped.
    pub fn approval_wait_minutes(&self) -> u32 {
        lock(&self.shared.state)
            .approval_wait_minutes
            .clamp(MIN_APPROVAL_WAIT_MINUTES, MAX_APPROVAL_WAIT_MINUTES)
    }

    /// [`Schedules::approval_wait_minutes`] as a duration.
    pub fn approval_wait(&self) -> std::time::Duration {
        std::time::Duration::from_secs(u64::from(self.approval_wait_minutes()) * 60)
    }

    /// Set how long a scheduled run's confirmation waits for the owner (audited
    /// `schedule.settings`). Applies to runs that start afterwards.
    pub fn set_approval_wait(&self, minutes: u32, by: &Creator) -> Result<(), ScheduleError> {
        if !(MIN_APPROVAL_WAIT_MINUTES..=MAX_APPROVAL_WAIT_MINUTES).contains(&minutes) {
            return Err(ScheduleError::Invalid(format!(
                "the wait is {MIN_APPROVAL_WAIT_MINUTES} to {MAX_APPROVAL_WAIT_MINUTES} minutes"
            )));
        }
        let mut guard = lock(&self.shared.state);
        if guard.approval_wait_minutes == minutes {
            return Ok(());
        }
        let mut next = guard.clone();
        next.approval_wait_minutes = minutes;
        self.commit(&mut guard, next)?;
        drop(guard);
        self.audit(
            "schedule.settings",
            by.actor(),
            &format!(
                "{}a scheduled run's request waits {minutes} min for your answer",
                by.audit_prefix()
            ),
        );
        Ok(())
    }

    /// Every schedule, oldest first.
    pub fn list(&self) -> Vec<Schedule> {
        lock(&self.shared.state).schedules.clone()
    }

    pub fn get(&self, id: u64) -> Option<Schedule> {
        lock(&self.shared.state)
            .schedules
            .iter()
            .find(|s| s.id == id)
            .cloned()
    }

    /// When `s` runs next (None: switched off, or it never matches again).
    pub fn next_run(&self, s: &Schedule) -> Option<Timestamp> {
        if !s.enabled {
            return None;
        }
        let cron = parse_cron(&s.cron).ok()?;
        let handled = Timestamp::from_second(i64::try_from(s.handled_until).ok()?).ok()?;
        next_after(&cron, self.clock.now().max(handled), &self.tz)
    }

    /// Is a run of schedule `id` going now?
    pub fn is_running(&self, id: u64) -> bool {
        lock(&self.shared.running).contains_key(&id)
    }

    /// Save `next` and only then make it current (a failed save changes nothing).
    fn commit(&self, guard: &mut MutexGuard<'_, State>, next: State) -> Result<(), ScheduleError> {
        if let Some(path) = &self.path {
            let shown = || path.display().to_string();
            // Serializing plain data cannot fail; an error would only mean a bug, so it is
            // reported as a write error rather than unwrapped.
            let data = serde_json::to_vec_pretty(&next).map_err(|e| ScheduleError::Write {
                path: shown(),
                message: e.to_string(),
            })?;
            crate::device::write_private(path, &data).map_err(|e| ScheduleError::Write {
                path: shown(),
                message: e.to_string(),
            })?;
        }
        **guard = next;
        self.shared.changed.notify_one();
        Ok(())
    }

    /// Change schedule `id` with `f` and save.
    fn change<R>(
        &self,
        id: u64,
        f: impl FnOnce(&mut Schedule) -> R,
    ) -> Result<(Schedule, R), ScheduleError> {
        let mut guard = lock(&self.shared.state);
        let mut next = guard.clone();
        let s = next
            .schedules
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(ScheduleError::NotFound(id))?;
        let r = f(s);
        let s = s.clone();
        self.commit(&mut guard, next)?;
        Ok((s, r))
    }

    /// Make a schedule (audited `schedule.created`).
    pub fn create(&self, input: &ScheduleInput, by: &Creator) -> Result<Schedule, ScheduleError> {
        let input = validate(input)?;
        let now = self.now_secs();
        let mut guard = lock(&self.shared.state);
        if guard.schedules.len() >= MAX_SCHEDULES {
            return Err(ScheduleError::TooMany);
        }
        let mut next = guard.clone();
        next.next_id += 1;
        let s = Schedule {
            id: next.next_id,
            name: input.name,
            agent: input.agent,
            instruction: input.instruction,
            cron: input.cron,
            enabled: input.enabled,
            max_minutes: input.max_minutes.unwrap_or(DEFAULT_MAX_MINUTES),
            created_by: by.clone(),
            created_at: now,
            handled_until: now,
            last_run: None,
        };
        next.schedules.push(s.clone());
        self.commit(&mut guard, next)?;
        drop(guard);
        self.audit(
            "schedule.created",
            by.actor(),
            &format!("{}{}", by.audit_prefix(), summary(&s)),
        );
        Ok(s)
    }

    /// Change a schedule (the owner's edit; audited `schedule.changed`). A new timing or
    /// switching it on counts from now: nothing runs for a time that has passed.
    pub fn update(
        &self,
        id: u64,
        input: &ScheduleInput,
        by: &Creator,
    ) -> Result<Schedule, ScheduleError> {
        let input = validate(input)?;
        let now = self.now_secs();
        let (s, ()) = self.change(id, |s| {
            if s.cron != input.cron || (input.enabled && !s.enabled) {
                s.handled_until = now;
            }
            s.name = input.name;
            s.agent = input.agent;
            s.instruction = input.instruction;
            s.cron = input.cron;
            s.enabled = input.enabled;
            s.max_minutes = input.max_minutes.unwrap_or(DEFAULT_MAX_MINUTES);
        })?;
        self.audit(
            "schedule.changed",
            by.actor(),
            &format!("{}{}", by.audit_prefix(), summary(&s)),
        );
        Ok(s)
    }

    /// Switch a schedule on or off (audited `schedule.changed`). On counts from now.
    pub fn set_enabled(&self, id: u64, on: bool, by: &Creator) -> Result<Schedule, ScheduleError> {
        let now = self.now_secs();
        let (s, changed) = self.change(id, |s| {
            let changed = s.enabled != on;
            if on && !s.enabled {
                s.handled_until = now;
            }
            s.enabled = on;
            changed
        })?;
        if changed {
            let word = if on { "on" } else { "off" };
            self.audit(
                "schedule.changed",
                by.actor(),
                &format!(
                    "{}schedule {} `{}` switched {word}",
                    by.audit_prefix(),
                    s.id,
                    s.name
                ),
            );
        }
        Ok(s)
    }

    /// Remove a schedule (audited `schedule.removed`); None if there is no such schedule. A
    /// run going now goes on.
    pub fn remove(&self, id: u64, by: &Creator) -> Result<Option<Schedule>, ScheduleError> {
        let mut guard = lock(&self.shared.state);
        let Some(i) = guard.schedules.iter().position(|s| s.id == id) else {
            return Ok(None);
        };
        let mut next = guard.clone();
        let s = next.schedules.remove(i);
        self.commit(&mut guard, next)?;
        drop(guard);
        self.audit(
            "schedule.removed",
            by.actor(),
            &format!("{}{}", by.audit_prefix(), summary(&s)),
        );
        Ok(Some(s))
    }

    /// Record a run (or a due time without one) as schedule `id`'s last run, and audit it.
    fn record(&self, id: u64, run: LastRun) {
        let action = run.outcome.audit_action();
        let mut detail = format!("schedule {id}: {}", run.outcome.words().to_lowercase());
        if let Some(d) = &run.detail {
            detail.push_str(&format!(" ({d})"));
        }
        if let Some(t) = run.turn {
            detail.push_str(&format!(", turn {t}"));
        }
        if run.manual {
            detail.push_str(", run now");
        }
        if let Err(e) = self.change(id, |s| s.last_run = Some(run)) {
            // A schedule removed meanwhile has no last run to keep.
            if !matches!(e, ScheduleError::NotFound(_)) {
                tracing::error!("saving a schedule's last run: {e}");
            }
        }
        self.audit(action, "schedule", &detail);
    }

    /// Mark due times up to `until` (Unix seconds) as dealt with.
    fn mark_handled(&self, id: u64, until: u64) {
        if let Err(e) = self.change(id, |s| s.handled_until = s.handled_until.max(until)) {
            if !matches!(e, ScheduleError::NotFound(_)) {
                tracing::error!("saving a schedule: {e}");
            }
        }
    }

    /// Runs recorded as going when Melissa stopped: interrupted.
    fn recover_interrupted(&self) {
        let ids: Vec<(u64, LastRun)> = self
            .list()
            .into_iter()
            .filter_map(|s| {
                let run = s.last_run?;
                (run.outcome == Outcome::Running).then_some((s.id, run))
            })
            .collect();
        for (id, run) in ids {
            self.record(
                id,
                LastRun {
                    outcome: Outcome::Interrupted,
                    detail: Some("Melissa stopped during the run".into()),
                    ..run
                },
            );
        }
    }

    fn audit(&self, action: &str, actor: &str, detail: &str) {
        if let Some(log) = &self.audit {
            if let Err(e) = log.append(&AuditRecord::now(action, 0, "", detail).by(actor)) {
                tracing::error!(path = %log.path().display(), "audit log write failed: {e}");
            }
        }
    }
}

/// For audit lines: `schedule 3 `Nightly updates` for agent d.a1, 0 3 * * *, on`.
fn summary(s: &Schedule) -> String {
    let who = match &s.agent {
        Principal::Lead => "the lead agent".to_string(),
        Principal::Agent(id) => format!("agent {id}"),
    };
    let on = if s.enabled { "on" } else { "off" };
    format!(
        "schedule {} `{}` for {who}, `{}`, {on}, limit {} min",
        s.id, s.name, s.cron, s.max_minutes
    )
}

/// Who a schedule is for, in words: "the lead agent (lead)" or "Ada (d-0a1b2c3d.a1)" (the
/// id too: names can change, see [`crate::lead::registry::AgentRegistry::lookup`]).
pub fn agent_words(agent: &Principal, names: Option<&dyn AgentNames>) -> String {
    match agent {
        Principal::Lead => "the lead agent (lead)".into(),
        Principal::Agent(id) => names
            .and_then(|n| n.name_of(id))
            .map_or_else(|| id.clone(), |name| format!("{name} ({id})")),
    }
}
