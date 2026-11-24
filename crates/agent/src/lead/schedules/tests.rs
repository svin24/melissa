//! Schedules: cron evaluation (with DST), the store, the scheduler's rules (grace, overlap,
//! time limit) on a fake clock and Tokio's paused time, and the `schedule_job` tool's scope.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use jiff::{civil::date, tz::TimeZone, SignedDuration, Timestamp};
use serde_json::{json, Value};
use tokio::sync::oneshot;

use super::*;
use crate::test_support::scratch;
use crate::{
    tools::{
        ActionKey, AgentNames, AlwaysAllowError, Permissions, RuleOffer, TaskContext, ToolError,
        ToolRegistry, Verdict,
    },
    AuditLog, BoxFuture,
};

const ADA: &str = "d-0a1b2c3d.a1";
const BO: &str = "d-0a1b2c3d.a2";

/// Europe/Athens rules (EET/EEST), without needing a tz database.
fn tz() -> TimeZone {
    TimeZone::posix("EET-2EEST,M3.5.0/3,M10.5.0/4").unwrap()
}

fn local(y: i16, mo: i8, d: i8, h: i8, mi: i8) -> Timestamp {
    date(y, mo, d)
        .at(h, mi, 0, 0)
        .to_zoned(tz())
        .unwrap()
        .timestamp()
}

fn show(t: Timestamp) -> String {
    t.to_zoned(tz()).strftime("%Y-%m-%d %H:%M %:z").to_string()
}

/// Wall-clock time that follows Tokio's (paused) clock from a settable start.
struct FakeClock(Mutex<(Timestamp, tokio::time::Instant)>);

impl FakeClock {
    fn at(t: Timestamp) -> Arc<Self> {
        Arc::new(Self(Mutex::new((t, tokio::time::Instant::now()))))
    }

    fn set(&self, t: Timestamp) {
        *self.0.lock().unwrap() = (t, tokio::time::Instant::now());
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        let (base, start) = *self.0.lock().unwrap();
        base + SignedDuration::try_from(start.elapsed()).unwrap()
    }
}

/// Starts numbered turns that end only when the test says so.
#[derive(Default)]
struct FakeRunner {
    next: AtomicU64,
    busy: AtomicBool,
    started: Mutex<Vec<(Principal, String, String)>>,
    ends: Mutex<HashMap<TaskId, oneshot::Sender<RunEnd>>>,
    waits: Mutex<HashMap<TaskId, oneshot::Receiver<RunEnd>>>,
    cancelled: Mutex<Vec<TaskId>>,
}

impl FakeRunner {
    fn finish(&self, turn: TaskId, end: RunEnd) {
        let tx = self.ends.lock().unwrap().remove(&turn).unwrap();
        let _ = tx.send(end);
    }

    fn started(&self) -> usize {
        self.started.lock().unwrap().len()
    }
}

impl Runner for FakeRunner {
    fn start(&self, agent: &Principal, name: &str, text: &str) -> Result<TaskId, StartError> {
        if self.busy.load(Ordering::SeqCst) {
            return Err(StartError::Busy);
        }
        let turn = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        self.started
            .lock()
            .unwrap()
            .push((agent.clone(), name.into(), text.into()));
        let (tx, rx) = oneshot::channel();
        self.ends.lock().unwrap().insert(turn, tx);
        self.waits.lock().unwrap().insert(turn, rx);
        Ok(turn)
    }

    fn wait(&self, _: &Principal, turn: TaskId) -> BoxFuture<'static, RunEnd> {
        let rx = self.waits.lock().unwrap().remove(&turn);
        Box::pin(async move {
            match rx {
                Some(rx) => rx.await.unwrap_or(RunEnd::Cancelled),
                None => RunEnd::Failed("no such turn".into()),
            }
        })
    }

    fn cancel(&self, _: &Principal, turn: TaskId) -> BoxFuture<'static, ()> {
        self.cancelled.lock().unwrap().push(turn);
        if let Some(tx) = self.ends.lock().unwrap().remove(&turn) {
            let _ = tx.send(RunEnd::Cancelled);
        }
        Box::pin(async {})
    }
}

fn input(name: &str, cron: &str) -> ScheduleInput {
    ScheduleInput {
        name: name.into(),
        agent: Principal::Agent(ADA.into()),
        instruction: "Update all apps and the system.".into(),
        cron: cron.into(),
        enabled: true,
        max_minutes: None,
    }
}

fn setup(start: Timestamp) -> (Schedules, Arc<FakeClock>, Arc<FakeRunner>, Scheduler) {
    let clock = FakeClock::at(start);
    let schedules = Schedules::in_memory(tz()).with_clock(clock.clone());
    let runner = Arc::new(FakeRunner::default());
    let scheduler = Scheduler::new(schedules.clone(), runner.clone(), DEFAULT_GRACE);
    (schedules, clock, runner, scheduler)
}

fn last(s: &Schedules, id: u64) -> LastRun {
    s.get(id).unwrap().last_run.expect("a last run")
}

/// Let spawned tasks (the run watchers) catch up.
async fn settle() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

// --- cron -------------------------------------------------------------------------------

#[test]
fn common_patterns_read_as_words_and_others_stay_cron() {
    for (cron, words) in [
        ("0 3 * * *", "Every day at 03:00"),
        ("5 * * * *", "Every hour at :05"),
        ("0 * * * *", "Every hour"),
        ("30 4 * * 1", "Every Monday at 04:30"),
        ("0 4 * * SUN", "Every Sunday at 04:00"),
        ("0 4 * * 7", "Every Sunday at 04:00"),
        ("*/15 * * * *", "Every 15 minutes"),
        ("* * * * *", "Every minute"),
        ("0 9 * * 1-5", "Weekdays at 09:00"),
        ("0 2 1 * *", "Every month on day 1 at 02:00"),
        ("0 3 * * 1,3", "0 3 * * 1,3"),
        ("0 3 1 1 *", "0 3 1 1 *"),
    ] {
        assert_eq!(describe(cron), words, "{cron}");
    }
    for p in [
        Preset::Hourly { minute: 15 },
        Preset::Daily { hour: 3, minute: 0 },
        Preset::Weekly {
            day: 1,
            hour: 4,
            minute: 30,
        },
    ] {
        assert_eq!(Preset::of(&p.cron()), Some(p));
        parse_cron(&p.cron()).unwrap();
    }
    assert_eq!(Preset::of("*/5 * * * *"), None);
    assert_eq!(Preset::of("0 3 1 * *"), None);
}

#[test]
fn only_five_field_cron_is_accepted() {
    for bad in [
        "",
        "0 3 * *",
        "0 0 3 * * *",
        "61 * * * *",
        "0 25 * * *",
        "every day",
    ] {
        assert!(
            matches!(parse_cron(bad), Err(ScheduleError::Invalid(_))),
            "{bad:?}"
        );
    }
    parse_cron("0 3 * * MON-FRI").unwrap();
    parse_cron(" 0  3 * *  * ").unwrap();
}

#[test]
fn next_run_is_in_local_time() {
    let cron = parse_cron("0 3 * * *").unwrap();
    let next = next_after(&cron, local(2026, 11, 20, 12, 0), &tz()).unwrap();
    assert_eq!(show(next), "2026-11-21 03:00 +02:00");
    // Exactly at a due time: the next one, not the same.
    let again = next_after(&cron, next, &tz()).unwrap();
    assert_eq!(show(again), "2026-11-22 03:00 +02:00");
    assert_eq!(
        last_at_or_before(&cron, local(2026, 11, 22, 3, 0), &tz()),
        Some(again)
    );
}

#[test]
fn a_daily_job_runs_once_on_both_dst_change_days() {
    let cron = parse_cron("30 3 * * *").unwrap();
    // 2026-03-29: clocks go 03:00 → 04:00, so 03:30 does not exist that day; the job still
    // runs once that day, right after the gap, and at 03:30 again the next day.
    let mut t = local(2026, 3, 28, 12, 0);
    let mut runs = Vec::new();
    for _ in 0..3 {
        t = next_after(&cron, t, &tz()).unwrap();
        runs.push(show(t));
    }
    assert_eq!(
        runs,
        [
            "2026-03-29 04:00 +03:00",
            "2026-03-30 03:30 +03:00",
            "2026-03-31 03:30 +03:00"
        ]
    );
    // 2026-10-25: clocks go 04:00 → 03:00, so 03:30 happens twice; the job runs once.
    let mut t = local(2026, 10, 24, 12, 0);
    let mut runs = Vec::new();
    for _ in 0..3 {
        t = next_after(&cron, t, &tz()).unwrap();
        runs.push(show(t));
    }
    assert_eq!(
        runs,
        [
            "2026-10-25 03:30 +03:00",
            "2026-10-26 03:30 +02:00",
            "2026-10-27 03:30 +02:00"
        ]
    );
}

// --- store ------------------------------------------------------------------------------

#[test]
fn schedules_are_saved_privately_and_come_back_after_a_restart() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("persist");
    let path = dir.join("schedules.json");
    let clock = FakeClock::at(local(2026, 11, 20, 12, 0));
    let s = Schedules::open(&path, tz())
        .unwrap()
        .with_clock(clock.clone());
    let a = s
        .create(&input("Nightly updates", "0 3 * * *"), &Creator::Owner)
        .unwrap();
    let b = s
        .create(
            &input("Hourly check", "15 * * * *"),
            &Creator::Agent(ADA.into()),
        )
        .unwrap();
    assert_eq!((a.id, b.id), (1, 2));
    assert_eq!(a.max_minutes, DEFAULT_MAX_MINUTES);
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    let again = Schedules::open(&path, tz())
        .unwrap()
        .with_clock(clock.clone());
    assert_eq!(again.list(), s.list());
    assert_eq!(
        again.next_run(&again.get(1).unwrap()).map(show).as_deref(),
        Some("2026-11-21 03:00 +02:00")
    );
    again.remove(1, &Creator::Owner).unwrap().unwrap();
    // Ids are never reused.
    let c = again
        .create(&input("Weekly", "0 4 * * 1"), &Creator::Owner)
        .unwrap();
    assert_eq!(c.id, 3);
    let third = Schedules::open(&path, tz()).unwrap();
    let ids: Vec<u64> = third.list().iter().map(|s| s.id).collect();
    assert_eq!(ids, [2, 3]);
    assert!(Schedules::open(dir.join("missing.json"), tz())
        .unwrap()
        .list()
        .is_empty());
    std::fs::write(dir.join("broken.json"), "{").unwrap();
    assert!(matches!(
        Schedules::open(dir.join("broken.json"), tz()),
        Err(ScheduleError::Parse { .. })
    ));
}

#[test]
fn the_wait_for_a_scheduled_runs_requests_is_the_owners_setting() {
    let dir = scratch("wait");
    let path = dir.join("schedules.json");
    let s = Schedules::open(&path, tz()).unwrap();
    // Default: 10 minutes, also for files saved before the setting existed.
    assert_eq!(s.approval_wait_minutes(), DEFAULT_APPROVAL_WAIT_MINUTES);
    assert_eq!(s.approval_wait(), std::time::Duration::from_secs(600));
    std::fs::write(&path, r#"{"next_id": 0, "schedules": []}"#).unwrap();
    assert_eq!(
        Schedules::open(&path, tz())
            .unwrap()
            .approval_wait_minutes(),
        10
    );
    // Bounds: 1 minute to a day.
    for bad in [0, MAX_APPROVAL_WAIT_MINUTES + 1] {
        assert!(matches!(
            s.set_approval_wait(bad, &Creator::Owner),
            Err(ScheduleError::Invalid(_))
        ));
    }
    s.set_approval_wait(1, &Creator::Owner).unwrap();
    s.set_approval_wait(MAX_APPROVAL_WAIT_MINUTES, &Creator::Owner)
        .unwrap();
    // Saved.
    let again = Schedules::open(&path, tz()).unwrap();
    assert_eq!(again.approval_wait_minutes(), 24 * 60);
    // A value edited by hand out of bounds is clamped.
    std::fs::write(&path, r#"{"approval_wait_minutes": 0}"#).unwrap();
    assert_eq!(
        Schedules::open(&path, tz())
            .unwrap()
            .approval_wait_minutes(),
        1
    );
}

#[test]
fn bad_input_is_refused_and_nothing_is_saved() {
    let s = Schedules::in_memory(tz());
    let mut cases = vec![
        input("", "0 3 * * *"),
        input("Nightly", "0 3 * *"),
        input(&"x".repeat(MAX_NAME_CHARS + 1), "0 3 * * *"),
    ];
    let mut no_text = input("Nightly", "0 3 * * *");
    no_text.instruction = "   ".into();
    cases.push(no_text);
    let mut no_time = input("Nightly", "0 3 * * *");
    no_time.max_minutes = Some(0);
    cases.push(no_time);
    for case in cases {
        assert!(
            matches!(
                s.create(&case, &Creator::Owner),
                Err(ScheduleError::Invalid(_))
            ),
            "{case:?}"
        );
    }
    assert!(s.list().is_empty());
    let ok = s
        .create(&input("  Nightly  ", "0  3 * * *"), &Creator::Owner)
        .unwrap();
    assert_eq!(
        (ok.name.as_str(), ok.cron.as_str()),
        ("Nightly", "0 3 * * *")
    );
}

#[test]
fn changes_are_audited_with_who_made_them() {
    let dir = scratch("audit");
    let log = dir.join("audit.jsonl");
    let s = Schedules::in_memory(tz()).with_audit(Some(AuditLog::new(&log)));
    let a = s
        .create(&input("Nightly", "0 3 * * *"), &Creator::Agent(ADA.into()))
        .unwrap();
    s.set_enabled(a.id, false, &Creator::Owner).unwrap();
    s.update(a.id, &input("Nightly", "0 4 * * *"), &Creator::Owner)
        .unwrap();
    s.remove(a.id, &Creator::Lead).unwrap();
    let lines: Vec<Value> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let got: Vec<(&str, &str)> = lines
        .iter()
        .map(|l| (l["action"].as_str().unwrap(), l["actor"].as_str().unwrap()))
        .collect();
    assert_eq!(
        got,
        [
            ("schedule.created", "agent"),
            ("schedule.changed", "ui"),
            ("schedule.changed", "ui"),
            ("schedule.removed", "lead"),
        ]
    );
    assert!(lines[0]["detail"].as_str().unwrap().contains(ADA));
}

// --- the scheduler ------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_due_job_runs_once_with_its_name_and_instruction() {
    let (s, clock, runner, scheduler) = setup(local(2026, 11, 20, 2, 0));
    let a = s
        .create(&input("Nightly updates", "0 3 * * *"), &Creator::Owner)
        .unwrap();
    scheduler.tick();
    assert_eq!(runner.started(), 0, "not due yet");
    clock.set(local(2026, 11, 20, 3, 0));
    scheduler.tick();
    scheduler.tick();
    assert_eq!(runner.started(), 1);
    assert_eq!(
        runner.started.lock().unwrap()[0],
        (
            Principal::Agent(ADA.into()),
            "Nightly updates".into(),
            "Update all apps and the system.".into()
        )
    );
    let run = last(&s, a.id);
    assert_eq!((run.outcome, run.turn), (Outcome::Running, Some(1)));
    assert!(s.is_running(a.id));
    runner.finish(1, RunEnd::Done);
    settle().await;
    let run = last(&s, a.id);
    assert_eq!(run.outcome, Outcome::Done);
    assert!(run.ended.is_some());
    assert!(!s.is_running(a.id));
    assert_eq!(
        s.next_run(&s.get(a.id).unwrap()).map(show).as_deref(),
        Some("2026-11-21 03:00 +02:00")
    );
}

#[tokio::test(start_paused = true)]
async fn a_missed_run_runs_late_within_the_grace_window_and_is_recorded_otherwise() {
    let (s, clock, runner, scheduler) = setup(local(2026, 11, 19, 12, 0));
    let late = s
        .create(&input("Late", "0 3 * * *"), &Creator::Owner)
        .unwrap();
    // Melissa comes back at 03:40: 40 minutes late, inside the hour.
    clock.set(local(2026, 11, 20, 3, 40));
    scheduler.tick();
    assert_eq!(runner.started(), 1);
    assert_eq!(last(&s, late.id).outcome, Outcome::Running);
    runner.finish(1, RunEnd::Done);
    settle().await;

    // Down for three days, back at 05:00: only the latest due time counts, and it is two
    // hours old, so it is recorded as missed and nothing runs.
    let (s, clock, runner, scheduler) = setup(local(2026, 11, 19, 12, 0));
    let gone = s
        .create(&input("Gone", "0 3 * * *"), &Creator::Owner)
        .unwrap();
    clock.set(local(2026, 11, 23, 5, 0));
    scheduler.tick();
    scheduler.tick();
    assert_eq!(runner.started(), 0);
    let run = last(&s, gone.id);
    assert_eq!(run.outcome, Outcome::Missed);
    assert_eq!(
        show(Timestamp::from_second(run.at as i64).unwrap()),
        "2026-11-23 03:00 +02:00"
    );
    // The next night runs as usual.
    clock.set(local(2026, 11, 24, 3, 0));
    scheduler.tick();
    assert_eq!(runner.started(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_new_or_changed_schedule_never_runs_for_a_time_that_has_passed() {
    let (s, clock, runner, scheduler) = setup(local(2026, 11, 20, 3, 10));
    // Made at 03:10: today's 03:00 is in the past.
    let a = s
        .create(&input("Nightly", "0 3 * * *"), &Creator::Owner)
        .unwrap();
    scheduler.tick();
    assert_eq!(runner.started(), 0);
    // Switched off over a due time and on again later: that due time does not run.
    s.set_enabled(a.id, false, &Creator::Owner).unwrap();
    clock.set(local(2026, 11, 21, 3, 20));
    scheduler.tick();
    s.set_enabled(a.id, true, &Creator::Owner).unwrap();
    scheduler.tick();
    assert_eq!(runner.started(), 0);
    // A new time already past today counts from tomorrow.
    s.update(a.id, &input("Nightly", "15 3 * * *"), &Creator::Owner)
        .unwrap();
    scheduler.tick();
    assert_eq!(runner.started(), 0);
    assert_eq!(
        s.next_run(&s.get(a.id).unwrap()).map(show).as_deref(),
        Some("2026-11-22 03:15 +02:00")
    );
}

#[tokio::test(start_paused = true)]
async fn a_run_still_going_or_a_busy_agent_skips_the_due_time() {
    let (s, clock, runner, scheduler) = setup(local(2026, 11, 20, 9, 30));
    let a = s
        .create(&input("Hourly", "0 * * * *"), &Creator::Owner)
        .unwrap();
    clock.set(local(2026, 11, 20, 10, 0));
    scheduler.tick();
    assert_eq!(runner.started(), 1);
    // 11:00 while the 10:00 run still works: skipped, not queued.
    clock.set(local(2026, 11, 20, 11, 0));
    scheduler.tick();
    assert_eq!(runner.started(), 1);
    let run = last(&s, a.id);
    assert_eq!(run.outcome, Outcome::Skipped);
    assert_eq!(
        run.detail.as_deref(),
        Some("the previous run was still going")
    );
    // "Run now" follows the same rule.
    assert_eq!(
        scheduler.run_now(a.id, &Creator::Owner).unwrap().outcome,
        Outcome::Skipped
    );
    runner.finish(1, RunEnd::Done);
    settle().await;
    // The finished run's outcome is what shows, and nothing ran late for 11:00.
    assert_eq!(last(&s, a.id).outcome, Outcome::Done);
    assert_eq!(runner.started(), 1);

    // The agent busy with something else (a chat, a job): skipped too.
    runner.busy.store(true, Ordering::SeqCst);
    clock.set(local(2026, 11, 20, 12, 0));
    scheduler.tick();
    let run = last(&s, a.id);
    assert_eq!(run.outcome, Outcome::Skipped);
    assert_eq!(run.detail.as_deref(), Some("the agent was busy"));
    runner.busy.store(false, Ordering::SeqCst);
    clock.set(local(2026, 11, 20, 13, 0));
    scheduler.tick();
    assert_eq!(runner.started(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_run_past_its_time_limit_is_cancelled_and_recorded_as_timed_out() {
    let (s, _clock, runner, scheduler) = setup(local(2026, 11, 20, 12, 0));
    let mut i = input("Slow", "0 3 * * *");
    i.max_minutes = Some(5);
    let a = s.create(&i, &Creator::Owner).unwrap();
    let run = scheduler.run_now(a.id, &Creator::Owner).unwrap();
    assert_eq!((run.outcome, run.manual), (Outcome::Running, true));
    // Waiting (say, on a confirmation nobody answers) is fine until the limit.
    tokio::time::sleep(Duration::from_secs(4 * 60)).await;
    assert!(runner.cancelled.lock().unwrap().is_empty());
    assert_eq!(last(&s, a.id).outcome, Outcome::Running);
    tokio::time::sleep(Duration::from_secs(2 * 60)).await;
    settle().await;
    assert_eq!(*runner.cancelled.lock().unwrap(), [1]);
    let run = last(&s, a.id);
    assert_eq!(run.outcome, Outcome::TimedOut);
    assert!(run.manual);
    assert!(!s.is_running(a.id));
}

#[tokio::test(start_paused = true)]
async fn the_loop_wakes_at_the_due_time_and_on_changes() {
    let (s, _clock, runner, scheduler) = setup(local(2026, 11, 20, 2, 58));
    scheduler.start();
    settle().await;
    // Made while the loop sleeps (nothing to wait for: a minute at most).
    let a = s
        .create(&input("Nightly", "0 3 * * *"), &Creator::Owner)
        .unwrap();
    tokio::time::sleep(Duration::from_secs(60)).await;
    settle().await;
    assert_eq!(runner.started(), 0);
    tokio::time::sleep(Duration::from_secs(61)).await;
    settle().await;
    assert_eq!(runner.started(), 1);
    assert_eq!(last(&s, a.id).outcome, Outcome::Running);
    scheduler.stop();
}

#[tokio::test(start_paused = true)]
async fn runs_going_when_melissa_stopped_are_marked_interrupted() {
    let dir = scratch("restart");
    let path = dir.join("schedules.json");
    let clock = FakeClock::at(local(2026, 11, 20, 12, 0));
    let s = Schedules::open(&path, tz())
        .unwrap()
        .with_clock(clock.clone());
    let a = s
        .create(&input("Nightly", "0 3 * * *"), &Creator::Owner)
        .unwrap();
    let runner = Arc::new(FakeRunner::default());
    let scheduler = Scheduler::new(s.clone(), runner.clone(), DEFAULT_GRACE);
    scheduler.run_now(a.id, &Creator::Owner).unwrap();
    assert_eq!(last(&s, a.id).outcome, Outcome::Running);
    // Melissa restarts.
    let s = Schedules::open(&path, tz()).unwrap().with_clock(clock);
    let _scheduler = Scheduler::new(s.clone(), Arc::new(FakeRunner::default()), DEFAULT_GRACE);
    let run = last(&s, a.id);
    assert_eq!((run.outcome, run.turn), (Outcome::Interrupted, Some(1)));
}

// --- the tool -----------------------------------------------------------------------------

struct Names;
impl AgentNames for Names {
    fn name_of(&self, id: &str) -> Option<String> {
        match id {
            ADA => Some("Ada".into()),
            BO => Some("Bo".into()),
            _ => None,
        }
    }
    fn resolve_id(&self, name_or_id: &str) -> Result<String, String> {
        match name_or_id {
            "Ada" | ADA => Ok(ADA.into()),
            "Bo" | BO => Ok(BO.into()),
            _ => Err(format!("no agent `{name_or_id}` on this device")),
        }
    }
}

fn registry(s: &Schedules, permissions: Permissions) -> ToolRegistry {
    ToolRegistry::new(
        vec![Arc::new(ScheduleJobTool::new(
            s.clone(),
            Some(Arc::new(Names)),
        ))],
        None,
    )
    .with_permissions(permissions)
    .with_confirm_timeout(Duration::from_secs(600))
}

fn agent(id: &str) -> TaskContext {
    TaskContext {
        scope: Scope::Agent { id: id.into() },
        ..TaskContext::default()
    }
}

fn lead() -> TaskContext {
    TaskContext {
        attended: true,
        delegate: true,
        scope: Scope::Lead {
            device: "d-0a1b2c3d".into(),
        },
        ..TaskContext::default()
    }
}

fn create(name: &str, agent: Option<&str>) -> Value {
    let mut args = json!({"action": "create", "name": name, "instruction": "Update all apps.",
        "schedule": "0 3 * * *"});
    if let Some(a) = agent {
        args["agent"] = json!(a);
    }
    args
}

/// Start a call in the background.
fn spawn(
    reg: &ToolRegistry,
    mut ctx: TaskContext,
    args: Value,
) -> tokio::task::JoinHandle<Result<Value, ToolError>> {
    let reg = reg.clone();
    tokio::spawn(async move { reg.call(7, &mut ctx, "schedule_job", args).await })
}

async fn one_pending(reg: &ToolRegistry) -> crate::tools::Confirmation {
    for _ in 0..10_000 {
        if let Some(c) = reg.confirmations().pending().first().cloned() {
            return c;
        }
        tokio::task::yield_now().await;
    }
    panic!("no confirmation");
}

#[tokio::test]
async fn an_agent_proposes_a_schedule_for_itself_and_the_owner_must_approve_it() {
    let s = Schedules::in_memory(tz());
    let reg = registry(&s, Permissions::in_memory());
    let call = spawn(&reg, agent(ADA), create("Nightly updates", None));
    let c = one_pending(&reg).await;
    assert!(
        c.reason.contains("only you can approve schedules"),
        "{}",
        c.reason
    );
    assert!(c.reason.contains("Every day at 03:00"), "{}", c.reason);
    // Nothing to remember: "Always allow" is not offered and cannot be forced.
    assert!(c.rule.is_none());
    assert!(matches!(
        reg.always_allow(c.id),
        Err(AlwaysAllowError::NotOffered)
    ));
    assert!(s.list().is_empty(), "nothing before the owner's yes");
    reg.confirmations().resolve(c.id, Verdict::Approved);
    let out = call.await.unwrap().unwrap();
    assert_eq!(out["created"]["agent"], format!("Ada ({ADA})"));
    let made = s.list().remove(0);
    assert_eq!(made.agent, Principal::Agent(ADA.into()));
    assert_eq!(made.created_by, Creator::Agent(ADA.into()));

    // Naming itself is fine too; denied means nothing is made.
    let call = spawn(&reg, agent(ADA), create("Again", Some("Ada")));
    let c = one_pending(&reg).await;
    reg.confirmations().resolve(c.id, Verdict::Denied);
    assert_eq!(call.await.unwrap(), Err(ToolError::Denied));
    assert_eq!(s.list().len(), 1);
}

#[tokio::test]
async fn an_agent_cannot_touch_another_agents_schedules() {
    let s = Schedules::in_memory(tz());
    let bo = s
        .create(
            &ScheduleInput {
                agent: Principal::Agent(BO.into()),
                ..input("Bo's", "0 3 * * *")
            },
            &Creator::Owner,
        )
        .unwrap();
    let reg = registry(&s, Permissions::in_memory());
    // Refused at once: nobody is asked.
    for args in [
        create("For Bo", Some("Bo")),
        create("For the lead", Some("lead")),
        json!({"action": "remove", "id": bo.id}),
    ] {
        let out = reg.call(7, &mut agent(ADA), "schedule_job", args).await;
        assert!(matches!(out, Err(ToolError::Refused(_))), "{out:?}");
        assert!(reg.confirmations().pending().is_empty());
    }
    // Ada's list shows only hers.
    let out = reg
        .call(
            7,
            &mut agent(ADA),
            "schedule_job",
            json!({"action": "list"}),
        )
        .await
        .unwrap();
    assert_eq!(out["schedules"], json!([]));
    assert_eq!(s.list().len(), 1);
}

#[tokio::test]
async fn the_lead_agent_may_schedule_any_agent_with_the_owners_yes() {
    let s = Schedules::in_memory(tz());
    let reg = registry(&s, Permissions::in_memory());
    // Attended and untainted, it still has to ask.
    let call = spawn(&reg, lead(), create("Bo's nightly", Some("Bo")));
    let c = one_pending(&reg).await;
    assert!(c.rule.is_none());
    reg.confirmations().resolve(c.id, Verdict::Approved);
    call.await.unwrap().unwrap();
    let made = s.list().remove(0);
    assert_eq!(
        (made.agent, made.created_by),
        (Principal::Agent(BO.into()), Creator::Lead)
    );
    let out = reg
        .call(7, &mut lead(), "schedule_job", json!({"action": "list"}))
        .await
        .unwrap();
    assert_eq!(out["schedules"][0]["when"], "Every day at 03:00");
    // Removing is the owner's call too.
    let call = spawn(&reg, lead(), json!({"action": "remove", "id": made.id}));
    let c = one_pending(&reg).await;
    reg.confirmations().resolve(c.id, Verdict::Approved);
    call.await.unwrap().unwrap();
    assert!(s.list().is_empty());
}

#[tokio::test]
async fn always_allow_rules_never_cover_schedules_but_no_permission_prompts_does() {
    let s = Schedules::in_memory(tz());
    let permissions = Permissions::in_memory();
    // A rule for exactly this agent and tool (only reachable by hand: the UI never offers it).
    permissions
        .add(&RuleOffer {
            key: ActionKey {
                principal: Principal::Agent(ADA.into()),
                tool: "schedule_job".into(),
                target: None,
            },
            verb: "use schedule_job".into(),
        })
        .unwrap();
    let reg = registry(&s, permissions);
    let call = spawn(&reg, agent(ADA), create("Nightly", None));
    let c = one_pending(&reg).await;
    reg.confirmations().resolve(c.id, Verdict::Denied);
    assert_eq!(call.await.unwrap(), Err(ToolError::Denied));

    // The developer's switch answers every confirmation, this one too.
    reg.set_bypass(true).unwrap();
    let out = tokio::time::timeout(
        Duration::from_secs(5),
        reg.call(7, &mut agent(ADA), "schedule_job", create("Nightly", None)),
    )
    .await
    .expect("no prompt with the bypass on");
    out.unwrap();
    assert_eq!(s.list().len(), 1);
}

#[tokio::test]
async fn bad_arguments_fail_before_anyone_is_asked() {
    let s = Schedules::in_memory(tz());
    let reg = registry(&s, Permissions::in_memory());
    let mut bad_cron = create("Nightly", None);
    bad_cron["schedule"] = json!("at three");
    for args in [
        bad_cron,
        json!({"action": "explode"}),
        json!({"action": "remove", "id": 99}),
        json!({"action": "create", "name": "x", "schedule": "0 3 * * *"}),
    ] {
        let out = reg.call(7, &mut agent(ADA), "schedule_job", args).await;
        assert!(matches!(out, Err(ToolError::BadArgs(_))), "{out:?}");
    }
    let out = reg
        .call(
            7,
            &mut lead(),
            "schedule_job",
            create("Nightly", Some("Nobody")),
        )
        .await;
    assert!(matches!(out, Err(ToolError::BadArgs(_))), "{out:?}");
    assert!(reg.confirmations().pending().is_empty());
}

#[test]
fn a_scheduled_runs_wait_stops_before_its_backends_tool_call_limit() {
    let min = |m: u64| std::time::Duration::from_secs(m * 60);
    let claude = Some(crate::claude::CLAUDE_TOOL_CALL_LIMIT);
    // Backends without a known limit (ACP agents, the echo backend): the owner's setting.
    assert_eq!(super::effective_wait(min(10), None), min(10));
    assert_eq!(super::effective_wait(min(1440), None), min(1440));
    // Claude Code stops waiting after 300 s: at most a minute before that.
    assert_eq!(super::effective_wait(min(10), claude), min(4));
    assert_eq!(super::effective_wait(min(1440), claude), min(4));
    // A shorter setting stays as it is.
    assert_eq!(super::effective_wait(min(1), claude), min(1));
    // A limit shorter than the margin still waits a moment.
    assert_eq!(
        super::effective_wait(min(10), Some(std::time::Duration::from_secs(30))),
        std::time::Duration::from_secs(1)
    );
}
