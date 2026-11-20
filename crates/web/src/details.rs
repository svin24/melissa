//! The details panel of a chat page (2026-11-20, owner decision): the agent whose chat it is
//! (Melissa on home, a helper on its page), its apps, its schedules and its file box (the
//! file box's own routes are in `crate::files`).
//!
//! `{key}` is `lead` (Melissa) or a helper's id (a name works too).
//!
//! UI (same-origin checked like every POST; htmx gets the changed section, a plain form post
//! a redirect back to the chat):
//! - `GET /ui/details/{key}/apps`: the apps section (it polls, like the app strip).
//! - `POST /ui/details/{key}/schedules/{id}/enabled` (`enabled` = `on`|`off`): the schedules
//!   section; only a schedule that runs this agent.
//! - `POST /ui/agents/{agent}/rename` (`name`): htmx gets `HX-Refresh` (the name shows in the
//!   rail, the title and the chat), or the agent section saying why not.
//! - `POST /ui/agents/{agent}/delete`: [`melissa_agent::lead::agents::AgentPool::delete`],
//!   like Settings → Agents; htmx gets `HX-Redirect: /`, or the agent section saying why not
//!   (it still looks after apps).
//!
//! API:
//! - `GET /api/details/{key}` → `{"agent": {key, name, role, backend, model, effort, status,
//!   created_by, created_at, can_delete, kept}, "apps": [{app, name, phase, agent,
//!   agent_name}], "schedules": [{id, name, when, enabled, next}], "files": <as
//!   GET /api/files/{key}>}`; 404 unknown. The rest of what the panel does has its own API:
//!   `/api/agents/{agent}/rename`, `DELETE /api/agents/{agent}`, `/api/schedules/{id}/enabled`
//!   and `/api/files/{key}/...` (upload, transfer, delete, retention).

use axum::{
    extract::{Form, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use melissa_agent::{
    files::box_key,
    lead::{agents::AgentView, schedules::describe, LeadError},
    tools::permissions::Principal,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    components::{
        agent_card, detail_apps, detail_schedules, AgentCard, DetailApp, DetailApps,
        DetailSchedule, DetailSchedules, DetailsPanel, FilePanel, Tone,
    },
    files::PanelData,
    is_htmx,
    pages::{agent_href, short_date},
    settings::agents::{runs_on, status_words},
    state::{phase_words, Apps, ClusterSnapshot, Keeper},
    ui::Ui,
};

/// The agent section's data (owned).
pub(crate) struct AgentData {
    key: String,
    name: String,
    lead: bool,
    runs_on: String,
    picker: bool,
    status: (&'static str, Tone),
    created: Option<String>,
    /// Apps it looks after (a helper with any can't be deleted).
    keeps: Vec<String>,
    error: Option<String>,
}

impl AgentData {
    fn rename_url(&self) -> Option<String> {
        (!self.lead).then(|| format!("/ui/agents/{}/rename", self.key))
    }

    fn delete_url(&self) -> Option<String> {
        (!self.lead).then(|| format!("/ui/agents/{}/delete", self.key))
    }

    fn kept(&self) -> Option<String> {
        (!self.lead && !self.keeps.is_empty()).then(|| {
            format!(
                "Looks after {}. Reassign or remove its apps before deleting it.",
                self.keeps.join(", ")
            )
        })
    }
}

/// Everything the details panel shows (owned; borrow with [`DetailsData::view`]).
pub(crate) struct DetailsData {
    agent: AgentData,
    rename: Option<String>,
    delete: Option<String>,
    kept: Option<String>,
    apps_url: String,
    apps: Vec<DetailApp>,
    apps_empty: String,
    schedules: Vec<DetailSchedule>,
    schedules_on: bool,
    pub(crate) files: PanelData,
}

impl DetailsData {
    /// The panel, with `files` (from `self.files.view()`).
    pub(crate) fn view<'a>(&'a self, files: &'a FilePanel<'a>) -> DetailsPanel<'a> {
        DetailsPanel {
            agent: card(&self.agent, &self.rename, &self.delete, &self.kept),
            apps: DetailApps {
                url: &self.apps_url,
                apps: &self.apps,
                empty: &self.apps_empty,
            },
            schedules: DetailSchedules {
                schedules: &self.schedules,
                available: self.schedules_on,
                flash: None,
            },
            files,
        }
    }
}

fn card<'a>(
    a: &'a AgentData,
    rename: &'a Option<String>,
    delete: &'a Option<String>,
    kept: &'a Option<String>,
) -> AgentCard<'a> {
    AgentCard {
        rename: rename.as_deref(),
        name: &a.name,
        role: if a.lead { "Lead agent" } else { "Helper" },
        runs_on: &a.runs_on,
        picker: a.picker,
        status: a.status,
        created: a.created.as_deref(),
        delete: delete.as_deref(),
        kept: kept.as_deref(),
        error: a.error.as_deref(),
    }
}

/// `the lead agent (task 3)` → `Melissa`, `the user` → `You`.
fn creator_words(created_by: &str) -> String {
    if created_by.starts_with("the lead agent") {
        "Melissa".into()
    } else if created_by == "the user" || created_by == "ui" {
        "You".into()
    } else {
        created_by.to_string()
    }
}

/// The apps `owner` looks after (Melissa: every app, each with who looks after it).
fn apps_of(snap: &ClusterSnapshot, owner: &Principal) -> Vec<DetailApp> {
    let Apps::Known { installed, .. } = &snap.apps else {
        return Vec::new();
    };
    let mut apps: Vec<DetailApp> = installed
        .iter()
        .filter(|a| match owner {
            Principal::Lead => true,
            Principal::Agent(id) => {
                matches!(&a.keeper, Some(Keeper::Agent { id: k, .. }) if k == id)
            }
        })
        .map(|a| {
            let (health, words) = phase_words(&a.phase);
            DetailApp {
                name: a.display_name(),
                details: format!("/ui/apps/{}", a.name),
                health,
                status: words.to_string(),
                keeper: match (owner, &a.keeper) {
                    (Principal::Lead, Some(Keeper::Agent { name, .. })) => Some(name.clone()),
                    (Principal::Lead, Some(Keeper::Owner)) => Some("You".into()),
                    (Principal::Lead, None) => Some("Nobody".into()),
                    _ => None,
                },
            }
        })
        .collect();
    // Melissa's list: grouped by who looks after each app.
    apps.sort_by(|a, b| a.keeper.cmp(&b.keeper).then_with(|| a.name.cmp(&b.name)));
    apps
}

fn apps_empty(snap: &ClusterSnapshot, owner: &Principal) -> String {
    match (&snap.apps, owner) {
        (Apps::Unknown, _) => "Checking your apps…".into(),
        (Apps::Unreachable, _) => "Can't reach the app platform right now.".into(),
        (_, Principal::Lead) => "No apps yet.".into(),
        (_, Principal::Agent(_)) => "None yet.".into(),
    }
}

impl Ui {
    fn agent_data(
        &self,
        owner: &Principal,
        view: Option<&AgentView>,
        snap: &ClusterSnapshot,
    ) -> AgentData {
        match (owner, view) {
            (Principal::Agent(id), Some(a)) => {
                let views = self.lead.agents().backends_of(id).unwrap_or_default();
                AgentData {
                    key: a.id.clone(),
                    name: a.name.clone(),
                    lead: false,
                    picker: !views.is_empty(),
                    runs_on: runs_on(&views, &a.backend),
                    status: status_words(a.status),
                    created: Some(format!(
                        "By {}, {}",
                        creator_words(&a.created_by),
                        short_date(a.created_at)
                    )),
                    keeps: apps_of(snap, owner).into_iter().map(|a| a.name).collect(),
                    error: None,
                }
            }
            _ => {
                let views = self.lead.backends();
                AgentData {
                    key: "lead".into(),
                    name: "Melissa".into(),
                    lead: true,
                    picker: !views.is_empty(),
                    runs_on: runs_on(&views, ""),
                    status: status_words(if self.lead.status().turn.is_some() {
                        "working"
                    } else {
                        "idle"
                    }),
                    created: None,
                    keeps: Vec::new(),
                    error: None,
                }
            }
        }
    }

    fn schedules_of(&self, owner: &Principal) -> (bool, Vec<DetailSchedule>) {
        let Some(sched) = self.lead.scheduler() else {
            return (false, Vec::new());
        };
        let s = sched.schedules();
        let key = box_key(owner).unwrap_or_default();
        let list = s
            .list()
            .into_iter()
            .filter(|sc| sc.agent == *owner)
            .map(|sc| DetailSchedule {
                when: describe(&sc.cron),
                next: match s.next_run(&sc) {
                    Some(t) => format!(
                        "Next {}",
                        s.local_time(u64::try_from(t.as_second()).unwrap_or(0))
                    ),
                    None => "Off".into(),
                },
                enabled: sc.enabled,
                toggle: format!("/ui/details/{key}/schedules/{}/enabled", sc.id),
                name: sc.name,
            })
            .collect();
        (true, list)
    }

    /// The details panel of `owner`'s chat (`view`: the helper, None for Melissa). Reads
    /// the file box on the blocking pool.
    pub(crate) async fn details_of(
        &self,
        owner: &Principal,
        view: Option<&AgentView>,
    ) -> Result<DetailsData, LeadError> {
        let snap = self.cluster.get().await;
        let files = {
            let ui = self.clone();
            let o = owner.clone();
            tokio::task::spawn_blocking(move || ui.panel_of_owner(&o))
                .await
                .map_err(|e| LeadError::Failed(e.to_string()))?
        };
        let agent = self.agent_data(owner, view, &snap);
        let (schedules_on, schedules) = self.schedules_of(owner);
        Ok(DetailsData {
            rename: agent.rename_url(),
            delete: agent.delete_url(),
            kept: agent.kept(),
            apps_url: format!("/ui/details/{}/apps", agent.key),
            apps: apps_of(&snap, owner),
            apps_empty: apps_empty(&snap, owner),
            schedules,
            schedules_on,
            agent,
            files,
        })
    }

    /// The owner and helper of a details `{key}`.
    fn details_target(&self, key: &str) -> Option<(Principal, Option<AgentView>)> {
        match self.box_named(key)? {
            Principal::Lead => Some((Principal::Lead, None)),
            Principal::Agent(id) => {
                let view = self.lead.agents().get(&id)?;
                Some((Principal::Agent(id), Some(view)))
            }
        }
    }
}

pub(crate) fn routes() -> Router<Ui> {
    Router::new()
        .route("/ui/details/{key}/apps", get(apps))
        .route(
            "/ui/details/{key}/schedules/{id}/enabled",
            post(schedule_enabled),
        )
        .route("/ui/agents/{agent}/rename", post(rename))
        .route("/ui/agents/{agent}/delete", post(delete))
}

pub(crate) fn api_routes() -> Router<Ui> {
    Router::new().route("/api/details/{key}", get(api_details))
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, crate::pages::not_found()).into_response()
}

fn chat_of(owner: &Principal) -> String {
    match owner {
        Principal::Lead => "/".into(),
        Principal::Agent(id) => agent_href(id),
    }
}

async fn apps(State(ui): State<Ui>, Path(key): Path<String>) -> Response {
    let Some((owner, _)) = ui.details_target(&key) else {
        return not_found();
    };
    let snap = ui.cluster.get().await;
    let list = apps_of(&snap, &owner);
    let url = format!("/ui/details/{}/apps", box_key(&owner).unwrap_or_default());
    detail_apps(&DetailApps {
        url: &url,
        apps: &list,
        empty: &apps_empty(&snap, &owner),
    })
    .into_response()
}

#[derive(Deserialize)]
struct EnabledForm {
    #[serde(default)]
    enabled: String,
}

async fn schedule_enabled(
    State(ui): State<Ui>,
    Path((key, id)): Path<(String, u64)>,
    headers: HeaderMap,
    Form(form): Form<EnabledForm>,
) -> Response {
    let Some((owner, _)) = ui.details_target(&key) else {
        return not_found();
    };
    let flash = match ui.lead.scheduler() {
        None => Some((
            "Schedules aren't running on this device.".to_string(),
            Tone::Error,
        )),
        Some(s) => {
            let store = s.schedules();
            match store.list().into_iter().find(|sc| sc.id == id) {
                // Only this agent's schedules from its panel.
                Some(sc) if sc.agent == owner => store
                    .set_enabled(
                        id,
                        form.enabled == "on",
                        &melissa_agent::lead::schedules::Creator::Owner,
                    )
                    .err()
                    .map(|e| (format!("Couldn't change it: {e}"), Tone::Error)),
                _ => Some(("That schedule is gone.".to_string(), Tone::Muted)),
            }
        }
    };
    if !is_htmx(&headers) {
        return Redirect::to(&chat_of(&owner)).into_response();
    }
    let (available, list) = ui.schedules_of(&owner);
    detail_schedules(&DetailSchedules {
        schedules: &list,
        available,
        flash: flash.as_ref().map(|(t, tone)| (t.as_str(), *tone)),
    })
    .into_response()
}

/// The agent section after a refused action, saying why.
async fn agent_section(
    ui: &Ui,
    owner: &Principal,
    view: Option<&AgentView>,
    error: String,
) -> Response {
    let snap = ui.cluster.get().await;
    let mut data = ui.agent_data(owner, view, &snap);
    data.error = Some(error);
    let (rename, delete, kept) = (data.rename_url(), data.delete_url(), data.kept());
    agent_card(&card(&data, &rename, &delete, &kept)).into_response()
}

fn hx(header: &'static str, value: &str) -> Response {
    let mut res = StatusCode::OK.into_response();
    if let Ok(v) = HeaderValue::from_str(value) {
        res.headers_mut().insert(header, v);
    }
    res
}

#[derive(Deserialize)]
struct RenameForm {
    name: String,
}

async fn rename(
    State(ui): State<Ui>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    Form(form): Form<RenameForm>,
) -> Response {
    let Some(view) = ui.lead.agents().get(&agent) else {
        return not_found();
    };
    let owner = Principal::Agent(view.id.clone());
    match ui.lead.agents().rename(&view.id, &form.name) {
        Ok(_) if is_htmx(&headers) => hx("hx-refresh", "true"),
        Ok(_) => Redirect::to(&agent_href(&view.id)).into_response(),
        Err(e) if is_htmx(&headers) => {
            agent_section(&ui, &owner, Some(&view), format!("Couldn't rename it: {e}")).await
        }
        Err(_) => Redirect::to(&agent_href(&view.id)).into_response(),
    }
}

async fn delete(State(ui): State<Ui>, Path(agent): Path<String>, headers: HeaderMap) -> Response {
    let Some(view) = ui.lead.agents().get(&agent) else {
        // Already gone: back to Melissa.
        return if is_htmx(&headers) {
            hx("hx-redirect", "/")
        } else {
            Redirect::to("/").into_response()
        };
    };
    let owner = Principal::Agent(view.id.clone());
    match ui.lead.agents().delete(&view.id, "ui").await {
        Ok(_) | Err(LeadError::NoSuchAgent(_)) if is_htmx(&headers) => hx("hx-redirect", "/"),
        Ok(_) | Err(LeadError::NoSuchAgent(_)) => Redirect::to("/").into_response(),
        Err(e) if is_htmx(&headers) => {
            let text = match e {
                LeadError::InUse(_) | LeadError::Invalid(_) => e.to_string(),
                other => format!("Couldn't delete it: {other}"),
            };
            agent_section(&ui, &owner, Some(&view), text).await
        }
        Err(_) => Redirect::to(&agent_href(&view.id)).into_response(),
    }
}

fn api_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}

async fn api_details(State(ui): State<Ui>, Path(key): Path<String>) -> Response {
    let Some((owner, view)) = ui.details_target(&key) else {
        return api_error(StatusCode::NOT_FOUND, "no such agent");
    };
    let snap = ui.cluster.get().await;
    let data = ui.agent_data(&owner, view.as_ref(), &snap);
    let agent = match &view {
        Some(a) => json!({
            "key": a.id,
            "name": a.name,
            "role": "helper",
            "backend": a.backend,
            "model": a.model,
            "effort": a.effort,
            "status": a.status,
            "created_by": a.created_by,
            "created_at": a.created_at,
            "can_delete": data.keeps.is_empty(),
            "runs_on": data.runs_on,
        }),
        None => json!({
            "key": "lead",
            "name": "Melissa",
            "role": "lead",
            "status": if ui.lead.status().turn.is_some() { "working" } else { "idle" },
            "can_delete": false,
            "runs_on": data.runs_on,
        }),
    };
    let apps: Vec<Value> = match &snap.apps {
        Apps::Known { installed, .. } => installed
            .iter()
            .filter_map(|a| {
                let (agent, agent_name) = match &a.keeper {
                    Some(Keeper::Agent { id, name }) => (Some(id.clone()), Some(name.clone())),
                    _ => (None, None),
                };
                let mine = match &owner {
                    Principal::Lead => true,
                    Principal::Agent(id) => agent.as_deref() == Some(id.as_str()),
                };
                mine.then(|| {
                    json!({
                        "app": a.name,
                        "name": a.display_name(),
                        "phase": a.phase,
                        "agent": agent,
                        "agent_name": agent_name,
                    })
                })
            })
            .collect(),
        _ => Vec::new(),
    };
    let schedules: Vec<Value> = match ui.lead.scheduler() {
        Some(s) => {
            let store = s.schedules();
            store
                .list()
                .into_iter()
                .filter(|sc| sc.agent == owner)
                .map(|sc| {
                    json!({
                        "id": sc.id,
                        "name": sc.name,
                        "when": describe(&sc.cron),
                        "enabled": sc.enabled,
                        "next": store.next_run(&sc).map(|t| t.as_second()),
                    })
                })
                .collect()
        }
        None => Vec::new(),
    };
    let ui2 = ui.clone();
    let o = owner.clone();
    let files = match tokio::task::spawn_blocking(move || crate::files::box_json(&ui2, &o)).await {
        Ok(v) => v,
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    Json(json!({
        "agent": agent,
        "apps": apps,
        "schedules": schedules,
        "files": files,
    }))
    .into_response()
}
