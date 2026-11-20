//! Settings → Schedules: the owner's scheduled jobs (list, switch, run now, remove, add and
//! edit) and their JSON API (`/api/schedules`, documented in `crate::api`). Changes made here
//! are the owner's own (audited `schedule.*`, actor `ui`) and need no confirmation; agents
//! propose schedules only through the `schedule_job` tool, which asks the owner every time.

use axum::{
    extract::{Form, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Json,
};
use melissa_agent::lead::schedules::{
    describe, Creator, LastRun, Outcome, Preset, Principal, Schedule, ScheduleError, ScheduleInput,
    Scheduler, DEFAULT_MAX_MINUTES,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::pages::{
    self, Flash, LastRunCell, ScheduleForm, ScheduleRow, SchedulesView, SCHEDULES_URL,
};
use crate::{components::Tone, is_htmx, ui::Ui};

const OWNER: Creator = Creator::Owner;

fn who(ui: &Ui, agent: &Principal) -> (String, String) {
    match agent {
        Principal::Lead => ("Melissa".into(), "/".into()),
        Principal::Agent(id) => (
            ui.lead
                .agents()
                .get(id)
                .map_or_else(|| format!("{id} (gone)"), |a| a.name),
            format!("/agents/{id}"),
        ),
    }
}

fn outcome_tone(o: Outcome) -> Tone {
    match o {
        Outcome::Done => Tone::Ok,
        Outcome::Running => Tone::Info,
        Outcome::Skipped | Outcome::Cancelled => Tone::Muted,
        Outcome::Missed | Outcome::Interrupted | Outcome::TimedOut => Tone::Warn,
        Outcome::Failed => Tone::Error,
    }
}

fn rows(ui: &Ui, sched: &Scheduler) -> Vec<ScheduleRow> {
    let s = sched.schedules();
    s.list()
        .into_iter()
        .map(|sc| {
            let (who, chat) = who(ui, &sc.agent);
            ScheduleRow {
                id: sc.id,
                who,
                chat,
                when: describe(&sc.cron),
                next: match s.next_run(&sc) {
                    Some(t) => s.local_time(u64::try_from(t.as_second()).unwrap_or(0)),
                    None => "Off".into(),
                },
                last: sc.last_run.as_ref().map(|r| LastRunCell {
                    words: r.outcome.words().into(),
                    tone: outcome_tone(r.outcome),
                    at: s.local_time(r.at),
                    detail: r.detail.clone(),
                }),
                enabled: sc.enabled,
                name: sc.name,
            }
        })
        .collect()
}

fn agent_choices(ui: &Ui) -> Vec<(String, String)> {
    std::iter::once(("lead".to_string(), "Melissa".to_string()))
        .chain(ui.lead.agents().list().into_iter().map(|a| (a.id, a.name)))
        .collect()
}

/// The form: empty (every day at 03:00, Melissa) or filled from schedule `edit`.
fn form(ui: &Ui, edit: Option<&Schedule>) -> ScheduleForm {
    let mut f = ScheduleForm {
        id: None,
        name: String::new(),
        agent: "lead".into(),
        agents: agent_choices(ui),
        instruction: String::new(),
        repeat: "daily".into(),
        day: "1".into(),
        time: "03:00".into(),
        cron: String::new(),
        max_minutes: DEFAULT_MAX_MINUTES.to_string(),
    };
    let Some(s) = edit else {
        return f;
    };
    f.id = Some(s.id);
    f.name = s.name.clone();
    f.agent = match &s.agent {
        Principal::Lead => "lead".into(),
        Principal::Agent(id) => id.clone(),
    };
    f.instruction = s.instruction.clone();
    f.cron = s.cron.clone();
    f.max_minutes = s.max_minutes.to_string();
    match Preset::of(&s.cron) {
        Some(Preset::Hourly { minute }) => {
            f.repeat = "hourly".into();
            f.time = format!("00:{minute:02}");
        }
        Some(Preset::Daily { hour, minute }) => f.time = format!("{hour:02}:{minute:02}"),
        Some(Preset::Weekly { day, hour, minute }) => {
            f.repeat = "weekly".into();
            f.day = day.to_string();
            f.time = format!("{hour:02}:{minute:02}");
        }
        None => f.repeat = "custom".into(),
    }
    f
}

fn view(ui: &Ui, form: Option<ScheduleForm>, flash: Option<Flash>) -> SchedulesView {
    let zone = |s: &Scheduler| {
        s.schedules()
            .time_zone()
            .iana_name()
            .unwrap_or("local")
            .to_string()
    };
    match ui.lead.scheduler() {
        Some(s) => SchedulesView {
            rows: rows(ui, s),
            form: form.unwrap_or_else(|| self::form(ui, None)),
            flash,
            available: true,
            zone: zone(s),
        },
        None => SchedulesView {
            rows: Vec::new(),
            form: self::form(ui, None),
            flash,
            available: false,
            zone: String::new(),
        },
    }
}

#[derive(Deserialize, Default)]
pub(super) struct PageQuery {
    edit: Option<u64>,
}

pub(super) async fn page(State(ui): State<Ui>, Query(q): Query<PageQuery>) -> Response {
    let edit = q
        .edit
        .and_then(|id| ui.lead.scheduler()?.schedules().get(id));
    let v = view(&ui, Some(form(&ui, edit.as_ref())), None);
    ui.framed(|f| pages::schedules_page(f, &v))
        .await
        .into_response()
}

/// The section after an action: the region for htmx, else the page (errors) or a redirect.
async fn reply(
    ui: &Ui,
    headers: &HeaderMap,
    form: Option<ScheduleForm>,
    flash: Option<Flash>,
) -> Response {
    let failed = flash.as_ref().is_some_and(|f| f.tone == Tone::Error);
    let v = view(ui, form, flash);
    if is_htmx(headers) {
        return pages::schedules_region(&v).into_response();
    }
    if failed {
        return ui
            .framed(|f| pages::schedules_page(f, &v))
            .await
            .into_response();
    }
    Redirect::to(SCHEDULES_URL).into_response()
}

fn error_flash(e: &ScheduleError) -> Flash {
    Flash {
        text: match e {
            ScheduleError::NotFound(_) => "That schedule is gone.".into(),
            other => format!("Couldn't save that: {other}"),
        },
        tone: Tone::Error,
    }
}

fn no_scheduler() -> Flash {
    Flash {
        text: "Schedules don't run on this device.".into(),
        tone: Tone::Error,
    }
}

/// The add/edit form as posted.
#[derive(Deserialize, Default)]
pub(super) struct FormIn {
    #[serde(default)]
    name: String,
    #[serde(default)]
    agent: String,
    #[serde(default)]
    instruction: String,
    #[serde(default)]
    repeat: String,
    #[serde(default)]
    day: String,
    #[serde(default)]
    time: String,
    #[serde(default)]
    cron: String,
    #[serde(default)]
    max_minutes: String,
}

/// `HH:MM` → (hour, minute).
fn hh_mm(t: &str) -> Option<(u8, u8)> {
    let (h, m) = t.trim().split_once(':')?;
    let h: u8 = h.parse().ok().filter(|h| *h < 24)?;
    let m: u8 = m.get(..2).unwrap_or(m).parse().ok().filter(|m| *m < 60)?;
    Some((h, m))
}

fn principal(ui: &Ui, agent: &str) -> Result<Principal, String> {
    let agent = agent.trim();
    if agent.is_empty() || agent.eq_ignore_ascii_case("lead") {
        return Ok(Principal::Lead);
    }
    ui.lead
        .agents()
        .get(agent)
        .map(|a| Principal::Agent(a.id))
        .ok_or_else(|| format!("no helper `{agent}`"))
}

impl FormIn {
    fn cron(&self) -> Result<String, String> {
        let time = || hh_mm(&self.time).ok_or_else(|| "pick a time".to_string());
        Ok(match self.repeat.as_str() {
            "hourly" => {
                let minute = hh_mm(&self.time).map_or(0, |(_, m)| m);
                Preset::Hourly { minute }.cron()
            }
            "daily" => {
                let (hour, minute) = time()?;
                Preset::Daily { hour, minute }.cron()
            }
            "weekly" => {
                let (hour, minute) = time()?;
                let day = self
                    .day
                    .parse::<u8>()
                    .ok()
                    .filter(|d| *d < 7)
                    .ok_or("pick a day")?;
                Preset::Weekly { day, hour, minute }.cron()
            }
            _ => self.cron.trim().to_string(),
        })
    }

    fn input(&self, ui: &Ui, enabled: bool) -> Result<ScheduleInput, String> {
        let max_minutes = match self.max_minutes.trim() {
            "" => None,
            m => Some(
                m.parse::<u32>()
                    .map_err(|_| "the time limit is a number of minutes".to_string())?,
            ),
        };
        Ok(ScheduleInput {
            name: self.name.clone(),
            agent: principal(ui, &self.agent)?,
            instruction: self.instruction.clone(),
            cron: self.cron()?,
            enabled,
            max_minutes,
        })
    }

    /// The form again with what was typed (after an error).
    fn echo(&self, ui: &Ui, id: Option<u64>) -> ScheduleForm {
        ScheduleForm {
            id,
            name: self.name.clone(),
            agent: self.agent.clone(),
            agents: agent_choices(ui),
            instruction: self.instruction.clone(),
            repeat: self.repeat.clone(),
            day: self.day.clone(),
            time: self.time.clone(),
            cron: self.cron.clone(),
            max_minutes: self.max_minutes.clone(),
        }
    }
}

/// Save the form: a new schedule (`id` None) or changes to one.
async fn save(ui: Ui, headers: HeaderMap, id: Option<u64>, form: FormIn) -> Response {
    let Some(sched) = ui.lead.scheduler() else {
        return reply(&ui, &headers, None, Some(no_scheduler())).await;
    };
    let enabled = id
        .and_then(|id| sched.schedules().get(id))
        .is_none_or(|s| s.enabled);
    let saved = form.input(&ui, enabled).and_then(|input| {
        match id {
            None => sched.schedules().create(&input, &OWNER),
            Some(id) => sched.schedules().update(id, &input, &OWNER),
        }
        .map_err(|e| e.to_string())
    });
    match saved {
        Ok(s) => {
            let text = match id {
                None => format!("Added. Next run: {}.", next_words(sched, &s)),
                Some(_) => "Saved.".into(),
            };
            reply(
                &ui,
                &headers,
                None,
                Some(Flash {
                    text,
                    tone: Tone::Ok,
                }),
            )
            .await
        }
        Err(text) => {
            let text = format!("Couldn't save that: {text}");
            let echo = form.echo(&ui, id);
            reply(
                &ui,
                &headers,
                Some(echo),
                Some(Flash {
                    text,
                    tone: Tone::Error,
                }),
            )
            .await
        }
    }
}

fn next_words(sched: &Scheduler, s: &Schedule) -> String {
    let st = sched.schedules();
    st.next_run(s).map_or_else(
        || "none".into(),
        |t| st.local_time(u64::try_from(t.as_second()).unwrap_or(0)),
    )
}

pub(super) async fn create(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Form(form): Form<FormIn>,
) -> Response {
    save(ui, headers, None, form).await
}

pub(super) async fn update(
    State(ui): State<Ui>,
    Path(id): Path<u64>,
    headers: HeaderMap,
    Form(form): Form<FormIn>,
) -> Response {
    save(ui, headers, Some(id), form).await
}

#[derive(Deserialize, Default)]
pub(super) struct EnabledForm {
    #[serde(default)]
    enabled: String,
}

pub(super) async fn enabled(
    State(ui): State<Ui>,
    Path(id): Path<u64>,
    headers: HeaderMap,
    Form(form): Form<EnabledForm>,
) -> Response {
    let flash = match ui.lead.scheduler() {
        None => Some(no_scheduler()),
        Some(s) => s
            .schedules()
            .set_enabled(id, form.enabled == "on", &OWNER)
            .err()
            .map(|e| error_flash(&e)),
    };
    reply(&ui, &headers, None, flash).await
}

fn run_words(run: &LastRun) -> Flash {
    match run.outcome {
        Outcome::Running => Flash {
            text: "Started.".into(),
            tone: Tone::Ok,
        },
        Outcome::Skipped => Flash {
            text: format!("Not started: {}.", run.detail.as_deref().unwrap_or("busy")),
            tone: Tone::Muted,
        },
        _ => Flash {
            text: format!(
                "Couldn't start it: {}.",
                run.detail.as_deref().unwrap_or("unknown error")
            ),
            tone: Tone::Error,
        },
    }
}

pub(super) async fn run(State(ui): State<Ui>, Path(id): Path<u64>, headers: HeaderMap) -> Response {
    let flash = match ui.lead.scheduler() {
        None => no_scheduler(),
        Some(s) => match s.run_now(id, &OWNER) {
            Ok(run) => run_words(&run),
            Err(e) => error_flash(&e),
        },
    };
    reply(&ui, &headers, None, Some(flash)).await
}

pub(super) async fn remove(
    State(ui): State<Ui>,
    Path(id): Path<u64>,
    headers: HeaderMap,
) -> Response {
    let flash = match ui.lead.scheduler() {
        None => no_scheduler(),
        Some(s) => match s.schedules().remove(id, &OWNER) {
            Ok(Some(_)) => Flash {
                text: "Removed.".into(),
                tone: Tone::Ok,
            },
            Ok(None) => Flash {
                text: "That schedule is already gone.".into(),
                tone: Tone::Muted,
            },
            Err(e) => error_flash(&e),
        },
    };
    reply(&ui, &headers, None, Some(flash)).await
}

// --- API ---------------------------------------------------------------------------------------

fn api_error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({"error": message.into()}))).into_response()
}

fn schedule_error(e: &ScheduleError) -> Response {
    let status = match e {
        ScheduleError::NotFound(_) => StatusCode::NOT_FOUND,
        ScheduleError::Invalid(_) | ScheduleError::TooMany => StatusCode::UNPROCESSABLE_ENTITY,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    api_error(status, e.to_string())
}

fn unavailable() -> Response {
    api_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "schedules don't run on this device",
    )
}

fn schedule_json(ui: &Ui, sched: &Scheduler, s: &Schedule) -> Value {
    let st = sched.schedules();
    let (agent_name, _) = who(ui, &s.agent);
    let next = st.next_run(s).map(|t| t.as_second());
    json!({
        "id": s.id,
        "name": s.name,
        "agent": match &s.agent { Principal::Lead => "lead", Principal::Agent(id) => id },
        "agent_name": agent_name,
        "instruction": s.instruction,
        "schedule": s.cron,
        "when": describe(&s.cron),
        "enabled": s.enabled,
        "max_minutes": s.max_minutes,
        "created_by": s.created_by,
        "created_at": s.created_at,
        "next_run": next,
        "next_run_local": next.map(|n| st.local_time(u64::try_from(n).unwrap_or(0))),
        "running": st.is_running(s.id),
        "last_run": s.last_run,
    })
}

pub(super) async fn api_list(State(ui): State<Ui>) -> Response {
    let Some(sched) = ui.lead.scheduler() else {
        return unavailable();
    };
    let list: Vec<Value> = sched
        .schedules()
        .list()
        .iter()
        .map(|s| schedule_json(&ui, sched, s))
        .collect();
    let zone = sched
        .schedules()
        .time_zone()
        .iana_name()
        .map(str::to_string);
    Json(json!({"schedules": list, "time_zone": zone})).into_response()
}

/// A schedule as the API takes it. On a change, missing fields keep their value.
#[derive(Deserialize, Default)]
pub(super) struct ApiIn {
    name: Option<String>,
    agent: Option<String>,
    instruction: Option<String>,
    schedule: Option<String>,
    enabled: Option<bool>,
    max_minutes: Option<u32>,
}

impl ApiIn {
    /// Err: an unknown agent.
    fn input(self, ui: &Ui, base: Option<&Schedule>) -> Result<ScheduleInput, String> {
        let agent = match (&self.agent, base) {
            (Some(a), _) => principal(ui, a)?,
            (None, Some(b)) => b.agent.clone(),
            (None, None) => Principal::Lead,
        };
        Ok(ScheduleInput {
            name: self
                .name
                .or_else(|| base.map(|b| b.name.clone()))
                .unwrap_or_default(),
            agent,
            instruction: self
                .instruction
                .or_else(|| base.map(|b| b.instruction.clone()))
                .unwrap_or_default(),
            cron: self
                .schedule
                .or_else(|| base.map(|b| b.cron.clone()))
                .unwrap_or_default(),
            enabled: self.enabled.or(base.map(|b| b.enabled)).unwrap_or(true),
            max_minutes: self.max_minutes.or(base.map(|b| b.max_minutes)),
        })
    }
}

pub(super) async fn api_create(State(ui): State<Ui>, Json(body): Json<ApiIn>) -> Response {
    let Some(sched) = ui.lead.scheduler() else {
        return unavailable();
    };
    let input = match body.input(&ui, None) {
        Ok(i) => i,
        Err(e) => return api_error(StatusCode::UNPROCESSABLE_ENTITY, e),
    };
    match sched.schedules().create(&input, &OWNER) {
        Ok(s) => (StatusCode::CREATED, Json(schedule_json(&ui, sched, &s))).into_response(),
        Err(e) => schedule_error(&e),
    }
}

pub(super) async fn api_update(
    State(ui): State<Ui>,
    Path(id): Path<u64>,
    Json(body): Json<ApiIn>,
) -> Response {
    let Some(sched) = ui.lead.scheduler() else {
        return unavailable();
    };
    let Some(base) = sched.schedules().get(id) else {
        return schedule_error(&ScheduleError::NotFound(id));
    };
    let input = match body.input(&ui, Some(&base)) {
        Ok(i) => i,
        Err(e) => return api_error(StatusCode::UNPROCESSABLE_ENTITY, e),
    };
    match sched.schedules().update(id, &input, &OWNER) {
        Ok(s) => Json(schedule_json(&ui, sched, &s)).into_response(),
        Err(e) => schedule_error(&e),
    }
}

#[derive(Deserialize)]
pub(super) struct OnIn {
    on: bool,
}

pub(super) async fn api_enabled(
    State(ui): State<Ui>,
    Path(id): Path<u64>,
    Json(body): Json<OnIn>,
) -> Response {
    let Some(sched) = ui.lead.scheduler() else {
        return unavailable();
    };
    match sched.schedules().set_enabled(id, body.on, &OWNER) {
        Ok(s) => Json(schedule_json(&ui, sched, &s)).into_response(),
        Err(e) => schedule_error(&e),
    }
}

pub(super) async fn api_run(State(ui): State<Ui>, Path(id): Path<u64>) -> Response {
    let Some(sched) = ui.lead.scheduler() else {
        return unavailable();
    };
    match sched.run_now(id, &OWNER) {
        Ok(run) => (StatusCode::ACCEPTED, Json(json!({"run": run}))).into_response(),
        Err(e) => schedule_error(&e),
    }
}

pub(super) async fn api_remove(State(ui): State<Ui>, Path(id): Path<u64>) -> Response {
    let Some(sched) = ui.lead.scheduler() else {
        return unavailable();
    };
    match sched.schedules().remove(id, &OWNER) {
        Ok(Some(s)) => Json(schedule_json(&ui, sched, &s)).into_response(),
        Ok(None) => schedule_error(&ScheduleError::NotFound(id)),
        Err(e) => schedule_error(&e),
    }
}
