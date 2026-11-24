//! The details panel of a chat page, "Agent details" (2026-11-20, owner design): the agent's
//! on-device services (its apps and this device; Melissa: every app, grouped by who looks
//! after it), its uploaded files (`crate::files` has their routes), its permissions (approvals
//! waiting and its "Always allow" rules), its recent activity and its schedules.
//!
//! `{key}` is `lead` (Melissa) or a helper's id (a name works too).
//!
//! UI (same-origin checked like every POST; htmx gets the changed section, a plain form post
//! a redirect back to the chat):
//! - `GET /ui/details/{key}/apps`: the services section (it polls, for app status).
//!
//! Live: the chat's stream sends a section again when what it shows changed
//! ([`DetailsLive`], [`details_update`]): after the chat's own events, Melissa's (an app
//! handed over, a helper made or deleted), a save in Settings, or a new read of the cluster,
//! which those events ask for at once (`ClusterCache::refresh_soon`).
//! - `POST /ui/details/{key}/schedules/{id}/enabled` (`enabled` = `on`|`off`): the schedules
//!   section; only a schedule that runs this agent.
//! - `POST /ui/details/{key}/rules/{id}/remove`: remove one of this agent's "Always allow"
//!   rules (the owner's own change, audited by the registry); the permissions section.
//!
//! Rename, Change icon, Pin, Fresh start and Delete are in the agent's menu and on its page in
//! Settings → Agents (`crate::settings::agents`).
//!
//! API:
//! - `GET /api/details/{key}` → `{"agent": {key, name, role, backend, model, effort, status,
//!   created_by, created_at, can_delete, runs_on, icon?, pinned}, "apps": [{app, name, phase,
//!   agent, agent_name}], "schedules": [{id, name, when, enabled, next}], "files": <as
//!   GET /api/files/{key}>}`; 404 unknown. The rest of what the panel does has its own API:
//!   `/api/agents/{agent}/rename|icon|pin|fresh-start`, `DELETE /api/agents/{agent}`,
//!   `/api/schedules/{id}/enabled`, `/api/settings/permissions/...` and `/api/files/{key}/...`.

use axum::{
    extract::{Form, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use maud::{html, Markup};
use melissa_agent::{
    files::box_key,
    lead::{agents::AgentView, schedules::describe, LeadError},
    tools::permissions::Principal,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    app_list::Looker,
    components::{
        detail_activity_inner, detail_apps, detail_apps_inner, detail_conversations,
        detail_permissions, detail_permissions_inner, detail_schedules, detail_schedules_inner,
        oob_inner, ActivityLine, Avatar, ConversationRow, DetailApp, DetailApps, DetailDevice,
        DetailPermissions, DetailRule, DetailSchedule, DetailSchedules, DetailWaiting,
        DetailsPanel, FilePanel, Health, ThreadLink, Tone, DETAILS_ACTIVITY_ID, DETAILS_APPS_ID,
        DETAILS_PERMISSIONS_ID, DETAILS_SCHEDULES_ID,
    },
    feed::{approval_words, rule_words, Clock},
    files::PanelData,
    is_htmx,
    pages::{agent_href, disk_warning, entry_id},
    settings::agents::runs_on,
    state::{phase_words, Apps, ClusterSnapshot, Keeper},
    threads::ConvData,
    ui::Ui,
};

/// Everything the details panel shows (owned; borrow with [`DetailsData::view`]).
pub(crate) struct DetailsData {
    apps_url: String,
    live: DetailsLive,
    pub(crate) files: PanelData,
}

impl DetailsData {
    /// The Conversations rows (borrow for [`DetailsData::view`]).
    pub(crate) fn conversation_rows(&self) -> Vec<ConversationRow<'_>> {
        conversation_rows(&self.live.conversations)
    }

    /// The panel, with `files` (from `self.files.view()`) and `rows` (from
    /// [`DetailsData::conversation_rows`]).
    pub(crate) fn view<'a>(
        &'a self,
        files: &'a FilePanel<'a>,
        rows: &'a [ConversationRow<'a>],
    ) -> DetailsPanel<'a> {
        let l = &self.live;
        DetailsPanel {
            apps: l.apps_view(&self.apps_url),
            files,
            permissions: DetailPermissions {
                waiting: &l.waiting,
                rules: &l.rules,
                flash: None,
            },
            activity: &l.activity,
            schedules: DetailSchedules {
                schedules: &l.schedules,
                available: l.schedules_on,
                flash: None,
            },
            conversations: rows,
            thread: None,
        }
    }
}

/// Conversations rows from their owned data.
fn conversation_rows(rows: &[ConvData]) -> Vec<ConversationRow<'_>> {
    rows.iter()
        .map(|r| ConversationRow {
            avatar: match &r.icon {
                Some((shape, color)) => Avatar::Agent { shape, color },
                None => Avatar::Melissa,
            },
            name: &r.name,
            last: &r.last,
            when: &r.when,
            link: ThreadLink {
                href: &r.href,
                fetch: &r.fetch,
            },
        })
        .collect()
}

/// The parts of the details panel that change while it is open: its services (apps handed
/// over, helpers gone, app status), permissions, recent activity and schedules. The chat's
/// stream keeps the one it last sent and sends the sections that changed
/// ([`details_update`]); the files have their own live update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DetailsLive {
    apps: Vec<DetailApp>,
    apps_empty: String,
    device: DetailDevice,
    waiting: Vec<DetailWaiting>,
    rules: Vec<DetailRule>,
    activity: Vec<ActivityLine>,
    schedules: Vec<DetailSchedule>,
    schedules_on: bool,
    /// The agents this one talked with (agent threads; the owner's only).
    conversations: Vec<ConvData>,
}

impl DetailsLive {
    fn apps_view<'a>(&'a self, url: &'a str) -> DetailApps<'a> {
        DetailApps {
            url,
            apps: &self.apps,
            empty: &self.apps_empty,
            device: Some(&self.device),
        }
    }
}

/// Out-of-band updates for the sections that changed from `old` to `new`; empty if none.
pub(crate) fn details_update(old: &DetailsLive, new: &DetailsLive) -> Markup {
    let apps =
        (&old.apps, &old.apps_empty, &old.device) != (&new.apps, &new.apps_empty, &new.device);
    html! {
        @if apps {
            // The section's own poll URL stays on the element; only its content changes.
            (oob_inner(DETAILS_APPS_ID, detail_apps_inner(&new.apps_view(""))))
        }
        @if (&old.waiting, &old.rules) != (&new.waiting, &new.rules) {
            (oob_inner(DETAILS_PERMISSIONS_ID, detail_permissions_inner(&DetailPermissions {
                waiting: &new.waiting,
                rules: &new.rules,
                flash: None,
            })))
        }
        @if old.activity != new.activity {
            (oob_inner(DETAILS_ACTIVITY_ID, detail_activity_inner(&new.activity)))
        }
        @if old.conversations != new.conversations {
            (detail_conversations(&conversation_rows(&new.conversations), true))
        }
        @if (&old.schedules, old.schedules_on) != (&new.schedules, new.schedules_on) {
            (oob_inner(DETAILS_SCHEDULES_ID, detail_schedules_inner(&DetailSchedules {
                schedules: &new.schedules,
                available: new.schedules_on,
                flash: None,
            })))
        }
    }
}

/// An app's key facts: the port people use ("TCP 25565") or its web port.
fn app_facts(a: &crate::state::App) -> Option<String> {
    match (a.other_port, a.port) {
        (Some((port, proto)), _) => Some(format!("{proto} {port}")),
        (None, Some(port)) => Some(format!("Web · port {port}")),
        _ => None,
    }
}

/// The apps `owner` looks after (Melissa: every app, each with who looks after it), of those
/// the person looking may see (`cx`; None: all, to tell whether the agent has any).
fn apps_of(
    snap: &ClusterSnapshot,
    owner: &Principal,
    cx: Option<&crate::app_list::AppsCx>,
) -> Vec<DetailApp> {
    let Apps::Known { installed, .. } = &snap.apps else {
        return Vec::new();
    };
    let mut apps: Vec<DetailApp> = installed
        .iter()
        .filter(|a| {
            cx.is_none_or(|cx| {
                cx.looker
                    .may_see(crate::app_list::person_of(a, &cx.owners).as_deref())
            })
        })
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
                details: format!("/cluster/apps/{}", a.name),
                health,
                status: words.to_string(),
                facts: app_facts(a),
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
        (Apps::NotSetUp, _) => "The app platform isn't set up on this device.".into(),
        (_, Principal::Lead) => "No apps yet.".into(),
        (_, Principal::Agent(_)) => "No apps yet.".into(),
    }
}

/// This device as the services section shows it; `name`: the owner's name for it, if any.
fn device_of(snap: &ClusterSnapshot, name: Option<String>) -> DetailDevice {
    match snap.devices.first() {
        Some(d) => {
            let (health, status) = match d.ready {
                Some(true) => (Health::Ok, "Online"),
                Some(false) => (Health::Problem, "Not ready"),
                None => (Health::Idle, "Status unknown"),
            };
            let os =
                d.os.as_deref()
                    .map(|o| o.split_whitespace().take(2).collect::<Vec<_>>().join(" "))
                    .filter(|o| !o.is_empty())
                    .unwrap_or_else(|| "Linux".into());
            DetailDevice {
                name: name.unwrap_or_else(|| d.name.clone()),
                sub: format!("{os} · This device"),
                health,
                status: status.into(),
                warning: disk_warning(snap),
            }
        }
        None => DetailDevice {
            name: "This device".into(),
            sub: "Checking…".into(),
            health: Health::Idle,
            status: "Checking".into(),
            warning: None,
        },
    }
}

impl Ui {
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

    /// The approvals `owner` waits for, and its "Always allow" rules.
    fn permissions_of(&self, owner: &Principal) -> (Vec<DetailWaiting>, Vec<DetailRule>) {
        let lead_task = self.lead.task_id();
        let agents = self.lead.agents();
        let key = box_key(owner).unwrap_or_default();
        let chat = match owner {
            Principal::Lead => "/".to_string(),
            Principal::Agent(id) => agent_href(id),
        };
        let waiting = self
            .lead
            .confirmations()
            .pending()
            .into_iter()
            .filter(|c| match owner {
                Principal::Lead => c.task == lead_task,
                Principal::Agent(id) => agents.agent_of_task(c.task).as_deref() == Some(id),
            })
            .map(|c| {
                let a = approval_words(c.id, &c.tool, &c.reason, Some(&c.args), self);
                let sub = match &a.detail {
                    Some(d) => format!("{d} · waiting for you"),
                    None => "Waiting for you".into(),
                };
                DetailWaiting {
                    kind: a.kind.to_string(),
                    sub,
                    href: format!("{chat}#{}", entry_id(&format!("c{}", c.id))),
                }
            })
            .collect();
        let rules = self
            .registry()
            .permissions()
            .rules()
            .into_iter()
            .filter(|r| r.key.principal == *owner)
            .map(|r| {
                let (_, what) =
                    rule_words(&r.key.principal, &r.verb, r.key.target.as_deref(), self);
                let mut c = what.chars();
                let what = match c.next() {
                    Some(f) => f.to_uppercase().chain(c).collect(),
                    None => what,
                };
                DetailRule {
                    what,
                    remove: format!("/ui/details/{key}/rules/{}/remove", r.id),
                }
            })
            .collect();
        (waiting, rules)
    }

    /// The details panel of `owner`'s chat (`view`: the helper, None for Melissa). Reads
    /// the file box on the blocking pool.
    pub(crate) async fn details_of(
        &self,
        owner: &Principal,
        _view: Option<&AgentView>,
    ) -> Result<DetailsData, LeadError> {
        let snap = self.cluster.get().await;
        let files = {
            let ui = self.clone();
            let o = owner.clone();
            tokio::task::spawn_blocking(move || ui.panel_of_owner(&o))
                .await
                .map_err(|e| LeadError::Failed(e.to_string()))?
        };
        let key = box_key(owner).unwrap_or_default();
        Ok(DetailsData {
            apps_url: format!("/ui/details/{key}/apps"),
            live: self.details_live(owner, &snap, &Looker::current()),
            files,
        })
    }

    /// What `owner`'s details panel shows now, but its files (`snap`: the cluster as last
    /// read), to `looker`: only the apps they may see. The chat's stream passes the person
    /// it captured when it opened, as it runs outside the request's sign-in scope.
    pub(crate) fn details_live(
        &self,
        owner: &Principal,
        snap: &ClusterSnapshot,
        looker: &Looker,
    ) -> DetailsLive {
        let (schedules_on, schedules) = self.schedules_of(owner);
        let (waiting, rules) = self.permissions_of(owner);
        let cx = self.apps_cx_for(None, looker.clone());
        // Activity names apps only the looker may see (a handover in Melissa's shared chat).
        let seen: std::collections::HashSet<String> = match &snap.apps {
            Apps::Known { installed, .. } => installed
                .iter()
                .filter(|a| {
                    cx.looker
                        .may_see(crate::app_list::person_of(a, &cx.owners).as_deref())
                })
                .map(|a| a.name.clone())
                .collect(),
            _ => Default::default(),
        };
        let may_see = |app: &str| cx.looker.owner || seen.contains(app);
        let activity = match owner {
            Principal::Lead => {
                self.recent_activity(owner, self.lead.events(), &Clock::default(), &may_see)
            }
            Principal::Agent(id) => match self.lead.agents().events_of(id) {
                Ok(log) => self.recent_activity(owner, &log, &Clock::default(), &may_see),
                Err(_) => Vec::new(),
            },
        };
        DetailsLive {
            apps: apps_of(snap, owner, Some(&cx)),
            apps_empty: apps_empty(snap, owner),
            device: device_of(snap, self.settings.device_name()),
            waiting,
            rules,
            activity,
            schedules,
            schedules_on,
            conversations: self.conversations_of(owner, looker.owner),
        }
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
        .route("/ui/details/{key}/rules/{id}/remove", post(rule_remove))
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
    let list = apps_of(&snap, &owner, Some(&ui.apps_cx(None)));
    let url = format!("/ui/details/{}/apps", box_key(&owner).unwrap_or_default());
    let device = device_of(&snap, ui.settings.device_name());
    detail_apps(&DetailApps {
        url: &url,
        apps: &list,
        empty: &apps_empty(&snap, &owner),
        device: Some(&device),
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
            Tone::Caution,
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
                    .map(|e| (format!("Couldn't change it: {e}"), Tone::Caution)),
                _ => Some(("That schedule is gone.".to_string(), Tone::Neutral)),
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

/// Remove one of this agent's "Always allow" rules (only its own from its panel).
async fn rule_remove(
    State(ui): State<Ui>,
    Path((key, id)): Path<(String, u64)>,
    headers: HeaderMap,
) -> Response {
    let Some((owner, _)) = ui.details_target(&key) else {
        return not_found();
    };
    let mine = ui
        .registry()
        .permissions()
        .rules()
        .into_iter()
        .any(|r| r.id == id && r.key.principal == owner);
    let flash = if !mine {
        ("That rule is already gone.".to_string(), Tone::Neutral)
    } else {
        match ui.registry().remove_rule(id) {
            Ok(Some(_)) => (
                "Removed. It asks you again next time.".to_string(),
                Tone::Ok,
            ),
            Ok(None) => ("That rule is already gone.".to_string(), Tone::Neutral),
            Err(e) => {
                tracing::warn!("cannot remove rule {id}: {e}");
                ("Couldn't remove it.".to_string(), Tone::Caution)
            }
        }
    };
    if !is_htmx(&headers) {
        return Redirect::to(&chat_of(&owner)).into_response();
    }
    let (waiting, rules) = ui.permissions_of(&owner);
    detail_permissions(&DetailPermissions {
        waiting: &waiting,
        rules: &rules,
        flash: Some((&flash.0, flash.1)),
    })
    .into_response()
}

fn api_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}

async fn api_details(State(ui): State<Ui>, Path(key): Path<String>) -> Response {
    let Some((owner, view)) = ui.details_target(&key) else {
        return api_error(StatusCode::NOT_FOUND, "no such agent");
    };
    let snap = ui.cluster.get().await;
    let agent = match &view {
        Some(a) => {
            let views = ui.lead.agents().backends_of(&a.id).unwrap_or_default();
            json!({
                "key": a.id,
                "name": a.name,
                "role": "helper",
                "backend": a.backend,
                "model": a.model,
                "effort": a.effort,
                "status": a.status,
                "created_by": a.created_by,
                "created_at": a.created_at,
                "can_delete": apps_of(&snap, &owner, None).is_empty(),
                "runs_on": runs_on(&views, &a.backend),
                "icon": a.icon,
                "pinned": a.pinned,
            })
        }
        None => json!({
            "key": "lead",
            "name": ui.lead.agents().lead_name(),
            "role": "lead",
            "status": if ui.lead.status().turn.is_some() { "working" } else { "idle" },
            "can_delete": false,
            "runs_on": runs_on(&ui.lead.backends(), ""),
            "pinned": true,
        }),
    };
    let cx = ui.apps_cx(None);
    let apps: Vec<Value> = match &snap.apps {
        Apps::Known { installed, .. } => installed
            .iter()
            // Only apps the person may see (a member: their own).
            .filter(|a| {
                cx.looker
                    .may_see(crate::app_list::person_of(a, &cx.owners).as_deref())
            })
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
