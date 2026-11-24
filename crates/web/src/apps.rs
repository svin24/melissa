//! An app's details (2026-11-22, owner design A5: a page of the Apps panel) and its JSON API.
//!
//! - `GET /cluster/apps/{app}`: the details as a panel page (melissa.js opens it in the Apps
//!   modal; a plain page without script). `GET /ui/apps/{app}`: the details' region alone.
//! - `GET /ui/apps/{app}/logs?container=`: the last [`LOG_LINES`] log lines (escaped text).
//! - `POST /ui/apps/{app}/stop|start`: the `stop_app`/`start_app` tools for the user (scope
//!   `User`, attended). A confirmation the policy asks for shows in the details; `POST
//!   /ui/apps/{app}/confirmations/{id}/approve|deny` answers it (the same inbox as the chat).
//! - `POST /ui/apps/{app}/icon` (form `shape`, `color`): the app's icon in the UI (`ui.toml`).
//! - `POST /ui/apps/{app}/force-delete` (form `confirm` = the app's name; owner only): the
//!   user's force delete ([`melissa_agent::lead::Lead::user_force_delete`]). Not a tool: no
//!   agent can reach it.
//! - JSON: `GET /api/apps?by=` (the list as the person sees it, see [`crate::app_list`]),
//!   `GET /api/apps/{app}` (`app_status`), `GET /api/apps/{app}/logs?lines=&container=`
//!   (`app_logs`), `POST /api/apps/{app}/force-delete` `{"confirm": "<app>"}` → the
//!   [`ForceDeleted`] report; 422 without the right `confirm`, a bad name or no such workspace,
//!   502 on a cluster failure.
//! - Network permissions (2026-11-24): `GET /api/apps/{app}/network` → the `app_network` tool's
//!   view (`internet`, `lan`, `summary`, `links` [{app, port, ports: [{name, port, protocol}],
//!   active, problem}], `incoming` (links other apps have into this one, same shape),
//!   `link_options` [{app, ports}]) plus `pending`: agents' network requests waiting for the
//!   owner on this app [{id, tool, task, fields: ["internet"|"lan"|"links"], reason}].
//!   `POST /api/apps/{app}/network` (owner only, password entered recently) with
//!   `{"internet"?: bool, "lan"?: bool, "add_links"?: [{app, port?}], "remove_links"?: [{app,
//!   port?}]}` → the `set_app_network` result (`network`, `before`, `changed`, `warnings`): the
//!   owner's own change, in force at once, no confirmation, audited `app.network`. 403 for
//!   members, 422 bad change (unknown app or port, nothing to change).
//!
//! - The Network card (owner design N1–N6, 2026-11-24): `GET /ui/apps/{app}/network`
//!   (`?add=1&app=`: the add-link form open), `POST /ui/apps/{app}/network/internet|lan`
//!   (`enabled=on` or nothing), `.../network/links` (`app`, `port` = `<app>/<port>`) and
//!   `.../network/links/remove` (`app`, `port`?): the owner's change through
//!   `set_app_network`, with the password entered recently (`auth::sensitive`).
//!
//! **Who may**: the owner every app; a member only their own ([`crate::app_list`]). Any other
//! app answers 404, as if it did not exist, on every route here.
//!
//! Reads and stop/start go through the tool registry, so they are policy-checked and audited
//! like the agents' calls. Mutating requests pass the same-origin check (`security`).

use std::time::Duration;

use axum::{
    extract::{Form, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use maud::Markup;
use melissa_agent::{
    lead::{
        agents::ForceDeleted,
        icon::{AgentIcon, COLORS, SHAPES},
        LeadError,
    },
    tools::{Scope, TaskContext, ToolError, Verdict},
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    app_list::{icon_of, person_of, person_words, shown, visible, AppsCx},
    components::{
        app_details_region, app_head_icon_oob, app_icon_card, app_logs_region, network_card,
        AddLinkForm, AppAsk, AppDetailsView, AppFact, AppLogsView, NetworkCard, NetworkLink,
        NetworkSwitch, Tone,
    },
    feed::display_app,
    pages::{agent_href, entry_id, format_uptime},
    state::{host_name_of, phase_words, Apps},
    ui::{host, AppsQuery, Ui},
};

/// Log lines the details show.
pub(crate) const LOG_LINES: u64 = 200;
/// Longest wait for a read.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// How long stop/start waits for the tool (or its confirmation) before answering.
const ACTION_WAIT: Duration = Duration::from_secs(5);

pub(crate) fn routes(ui: Ui) -> Router {
    Router::new()
        .route("/cluster/apps/{app}", get(details_page))
        .route("/ui/apps/{app}", get(details))
        .route("/ui/apps/{app}/logs", get(logs))
        .route("/ui/apps/{app}/stop", post(stop))
        .route("/ui/apps/{app}/start", post(start))
        .route("/ui/apps/{app}/icon", post(set_icon))
        .route("/ui/apps/{app}/confirmations/{id}/{verdict}", post(answer))
        .route("/ui/apps/{app}/force-delete", post(force_delete))
        .route("/ui/apps/{app}/network", get(network_region))
        .route("/ui/apps/{app}/network/links", post(add_network_link))
        .route(
            "/ui/apps/{app}/network/links/remove",
            post(remove_network_link),
        )
        .route("/ui/apps/{app}/network/{field}", post(set_network_switch))
        .route("/api/apps", get(api_list))
        .route("/api/apps/{app}", get(api_details))
        .route("/api/apps/{app}/logs", get(api_logs))
        .route("/api/apps/{app}/force-delete", post(api_force_delete))
        .route(
            "/api/apps/{app}/network",
            get(api_network).post(api_set_network),
        )
        .with_state(ui)
}

/// A well-formed app name (the tools check again).
pub(crate) fn valid(app: &str) -> bool {
    !app.is_empty()
        && app.len() <= 63
        && app
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !app.starts_with('-')
        && !app.ends_with('-')
}

/// One tool call as the user (task 0, like the operator API).
async fn call(ui: &Ui, tool: &str, args: Value, attended: bool) -> Result<Value, ToolError> {
    #[cfg(feature = "fake-apps")]
    if let Some(v) = crate::fake_apps::answer(tool, &args) {
        return Ok(v);
    }
    let mut ctx = TaskContext {
        scope: Scope::User,
        attended,
        ..TaskContext::default()
    };
    let call = ui.lead.server().registry().call(0, &mut ctx, tool, args);
    match tokio::time::timeout(READ_TIMEOUT, call).await {
        Ok(r) => r,
        Err(_) => Err(ToolError::Failed("no answer in time".into())),
    }
}

// ---- who may -------------------------------------------------------------------------------

/// The person looking may see `app` (and act on it): the owner any app, a member only one of
/// theirs in the app list (see [`crate::app_list`]). Returns what the details need to know
/// about people; `None` answers 404.
async fn allowed(ui: &Ui, app: &str) -> Option<(AppsCx, Option<String>)> {
    if !valid(app) {
        return None;
    }
    let cx = ui.apps_cx(None);
    let snap = ui.cluster.get().await;
    let person = match &snap.apps {
        Apps::Known { installed } => installed
            .iter()
            .find(|a| a.name == app)
            .and_then(|a| person_of(a, &cx.owners)),
        _ => None,
    };
    cx.looker.may_see(person.as_deref()).then_some((cx, person))
}

fn not_found() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

// ---- the details ---------------------------------------------------------------------------

/// The facts for the details, from `app_status`. Pure, for tests.
pub(crate) struct DetailsView {
    pub name: String,
    pub phase: String,
    /// (label, lines).
    pub facts: Vec<(String, Vec<String>)>,
    /// The web link (http/https only).
    pub link: Option<String>,
}

pub(crate) fn details_view(status: &Value, host: Option<&str>) -> DetailsView {
    let app = status["app"].as_str().unwrap_or_default();
    let mut facts = Vec::new();
    let keeper = match (status["agent_name"].as_str(), status["agent"].as_str()) {
        (Some(name), _) => name.to_string(),
        (None, Some(id)) => id.to_string(),
        _ if status["legacy_owner"] == "owner" => "You".to_string(),
        _ => "Nobody".to_string(),
    };
    facts.push(("Looked after by".into(), vec![keeper]));
    if let Some(device) = status["device"].as_str() {
        facts.push(("Device".into(), vec![device.to_string()]));
    }
    let port = status["port"].as_u64();
    let link = match (host.and_then(host_name_of), port) {
        (Some(h), Some(p)) => Some(format!("http://{h}:{p}")),
        _ => status["url"].as_str().map(str::to_string),
    }
    .filter(|l| l.starts_with("http://") || l.starts_with("https://"));
    // Every address: the web link, then the other ports (`address:port (TCP, game)`), at the
    // address the visitor used.
    let mut addresses: Vec<String> = link
        .iter()
        .map(|l| {
            let bare = l
                .trim_start_matches("https://")
                .trim_start_matches("http://");
            format!("{bare} (HTTP)")
        })
        .collect();
    addresses.extend(port_addresses(status, host).into_iter().map(|(_, a)| a));
    if !addresses.is_empty() {
        facts.push(("Addresses".into(), addresses));
    }
    if let Some(v) = status["version"].as_str() {
        facts.push(("Version".into(), vec![v.to_string()]));
    }
    if let Some(i) = status["image"].as_str() {
        facts.push(("Image".into(), vec![i.to_string()]));
    }
    if status["installed"] == true {
        let pods = status["pods"].as_array().cloned().unwrap_or_default();
        facts.push((
            "Pods".into(),
            vec![format!(
                "{} of {} running",
                status["ready_replicas"].as_u64().unwrap_or(0),
                status["replicas"].as_u64().unwrap_or(0)
            )],
        ));
        let restarts: u64 = pods.iter().filter_map(|p| p["restarts"].as_u64()).sum();
        if restarts > 0 {
            facts.push(("Restarts".into(), vec![restarts.to_string()]));
        }
        if let Some(age) = pods.iter().filter_map(|p| p["age_secs"].as_u64()).min() {
            facts.push(("Age".into(), vec![format_uptime(age)]));
        }
    }
    DetailsView {
        name: display_app(app),
        phase: status["phase"].as_str().unwrap_or("unknown").to_string(),
        facts,
        link,
    }
}

/// The "Device" fact says the owner's name for this device (`this`: its device id), as the
/// rest of the UI does; other devices keep their ids.
fn name_this_device(facts: &mut [(String, Vec<String>)], this: &str, name: &str) {
    let lines = facts
        .iter_mut()
        .filter(|(label, _)| label == "Device")
        .flat_map(|(_, lines)| lines.iter_mut());
    for line in lines.filter(|l| l.as_str() == this) {
        *line = name.to_string();
    }
}

/// The app's ports on the device other than its web link, as (name, `host:port (TCP, name)`):
/// the address the visitor reached the UI at when usable, else the tool's address.
pub(crate) fn port_addresses(status: &Value, host: Option<&str>) -> Vec<(String, String)> {
    let host = host.and_then(host_name_of);
    status["ports"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["web"] != true)
        .filter_map(|p| {
            let name = p["name"].as_str()?;
            let port = p["port"].as_u64()?;
            let proto = match p["protocol"].as_str() {
                Some("udp") => "UDP",
                _ => "TCP",
            };
            let address = match host {
                Some(h) => format!("{h}:{port}"),
                None => p["address"].as_str()?.to_string(),
            };
            Some((
                name.to_string(),
                format!("{address} ({proto}, {})", display_app(name).to_lowercase()),
            ))
        })
        .collect()
}

/// What the details show around the facts: a message, or the tool's question.
#[derive(Default)]
struct Extra<'a> {
    message: Option<(&'a str, Tone)>,
    ask: Option<(u64, &'static str)>,
    gone: bool,
    /// The Network card's add-link form is open (for this app; empty: the first).
    add_link: Option<&'a str>,
}

/// The details' region for `app`.
async fn render_details(
    ui: &Ui,
    app: &str,
    host: Option<&str>,
    who: &(AppsCx, Option<String>),
    extra: Extra<'_>,
) -> Markup {
    let (cx, person) = who;
    let status = if extra.gone {
        json!({"app": app, "phase": "unknown"})
    } else {
        match call(ui, "app_status", json!({ "app": app }), false).await {
            Ok(s) if s["k3s_reachable"] != false => s,
            Ok(_) | Err(_) => json!({"app": app, "phase": "unknown"}),
        }
    };
    let mut view = details_view(&status, host);
    if let Some(name) = ui.settings.device_name() {
        name_this_device(
            &mut view.facts,
            &ui.lead.agents().registry().device(),
            &name,
        );
    }
    // The app asked for, whatever the answer names.
    view.name = display_app(app);
    let (health, words) = phase_words(&view.phase);
    let facts: Vec<AppFact<'_>> = view
        .facts
        .iter()
        .map(|(label, lines)| AppFact {
            label,
            lines: lines.iter().map(String::as_str).collect(),
        })
        .collect();
    let toggle_url;
    let toggle = match view.phase.as_str() {
        "ready" | "starting" | "failing" => {
            toggle_url = format!("/ui/apps/{app}/stop");
            Some(("Stop", toggle_url.as_str()))
        }
        "stopped" | "stopping" => {
            toggle_url = format!("/ui/apps/{app}/start");
            Some(("Start", toggle_url.as_str()))
        }
        _ => None,
    };
    let message = extra.message.or_else(|| {
        (status["phase"] == "unknown" && !extra.gone)
            .then_some(("Couldn't read the status.", Tone::Caution))
    });
    let icon = icon_of(app, &cx.prefs);
    let person_name = person
        .as_deref()
        .map(|p| person_words(p, &cx.names))
        .unwrap_or_else(|| "Unknown".into());
    let ask_texts = extra.ask.map(|(id, tool)| {
        let base = format!("/ui/apps/{app}/confirmations/{id}");
        let (question, detail, label) = if tool == "stop_app" {
            (
                format!("Stop {}?", view.name),
                "People using it are disconnected until you start it again. Its data stays.",
                "Stop app",
            )
        } else {
            (
                format!("Start {}?", view.name),
                "It starts again with the data it had.",
                "Start app",
            )
        };
        (
            question,
            detail,
            label,
            format!("{base}/approve"),
            format!("{base}/deny"),
        )
    });
    let network = if extra.gone {
        None
    } else {
        let show = NetShow {
            owner: cx.looker.owner,
            add: extra.add_link,
            message: None,
        };
        network_card_for(ui, app, &show).await
    };
    let icon_url = format!("/ui/apps/{app}/icon");
    let delete_url = format!("/ui/apps/{app}/force-delete");
    app_details_region(&AppDetailsView {
        app,
        name: &view.name,
        shape: &icon.shape,
        color: &icon.color,
        health,
        status: words,
        // Whose it is: for the owner, who sees everyone's.
        person: cx.looker.owner.then_some(person_name.as_str()),
        open: view.link.as_deref(),
        toggle,
        ask: ask_texts
            .as_ref()
            .map(|(question, detail, label, approve, deny)| AppAsk {
                question,
                detail,
                label,
                approve,
                deny,
            }),
        message,
        facts: &facts,
        network,
        icon: Some((SHAPES, COLORS, icon_url.as_str())),
        logs_url: &format!("/ui/apps/{app}/logs"),
        // Force delete is the owner's (the sign-in gate refuses members too).
        delete_url: cx.looker.owner.then_some(delete_url.as_str()),
        gone: extra.gone,
    })
}

async fn details(State(ui): State<Ui>, Path(app): Path<String>, headers: HeaderMap) -> Response {
    let Some(who) = allowed(&ui, &app).await else {
        return not_found();
    };
    render_details(&ui, &app, host(&headers), &who, Extra::default())
        .await
        .into_response()
}

/// The details as a page of the Apps panel.
#[derive(Deserialize)]
struct DetailsQuery {
    /// Open the Network card's add-link form (the plain link without script).
    #[serde(default)]
    add_link: Option<String>,
    /// The app to link to in that form.
    #[serde(default)]
    to: Option<String>,
}

async fn details_page(
    State(ui): State<Ui>,
    Path(app): Path<String>,
    Query(q): Query<DetailsQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(who) = allowed(&ui, &app).await else {
        return not_found();
    };
    let extra = Extra {
        add_link: q
            .add_link
            .is_some()
            .then(|| q.to.as_deref().unwrap_or_default()),
        ..Extra::default()
    };
    let region = render_details(&ui, &app, host(&headers), &who, extra).await;
    let name = display_app(&app);
    ui.framed_at(host(&headers), |f| {
        crate::pages::app_details_page(f, &name, region)
    })
    .await
    .into_response()
}

#[derive(Deserialize)]
struct LogsQuery {
    #[serde(default)]
    container: Option<String>,
    #[serde(default)]
    lines: Option<u64>,
}

fn logs_args(app: &str, q: &LogsQuery) -> Value {
    let mut args = json!({"app": app, "lines": q.lines.unwrap_or(LOG_LINES)});
    if let Some(c) = q.container.as_deref().filter(|c| !c.is_empty()) {
        args["container"] = json!(c);
    }
    args
}

async fn logs(
    State(ui): State<Ui>,
    Path(app): Path<String>,
    Query(q): Query<LogsQuery>,
) -> Response {
    if allowed(&ui, &app).await.is_none() {
        return not_found();
    }
    let q = LogsQuery {
        lines: Some(LOG_LINES),
        ..q
    };
    let url = format!("/ui/apps/{app}/logs");
    let result = call(&ui, "app_logs", logs_args(&app, &q), false).await;
    let (log, containers, container, empty) = match &result {
        Ok(v) => {
            let log = v["log"].as_str().unwrap_or_default().to_string();
            let containers: Vec<String> = v["containers"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c.as_str().map(str::to_string))
                .collect();
            let container = v["container"].as_str().map(str::to_string);
            let empty = log.trim().is_empty().then_some("No log lines yet.");
            (log, containers, container, empty)
        }
        Err(ToolError::Failed(m)) if m.contains("no pods") => (
            String::new(),
            Vec::new(),
            None,
            Some("Not running, so no logs."),
        ),
        // The container is still being created (the API server answers 400).
        Err(ToolError::Failed(m))
            if m.contains("waiting to start") || m.contains("ContainerCreating") =>
        {
            (String::new(), Vec::new(), None, Some("No logs yet."))
        }
        Err(_) => (
            String::new(),
            Vec::new(),
            None,
            Some("Couldn't read the logs."),
        ),
    };
    let lines: Vec<&str> = log.lines().collect();
    app_logs_region(&AppLogsView {
        url: &url,
        containers: &containers,
        container: container.as_deref(),
        lines: &lines,
        empty,
    })
    .into_response()
}

// ---- the icon ------------------------------------------------------------------------------

#[derive(Deserialize)]
struct IconForm {
    #[serde(default)]
    shape: String,
    #[serde(default)]
    color: String,
}

/// Save the app's icon (known names only): the icon card again, and the head's icon.
async fn set_icon(
    State(ui): State<Ui>,
    Path(app): Path<String>,
    headers: HeaderMap,
    Form(form): Form<IconForm>,
) -> Response {
    if allowed(&ui, &app).await.is_none() {
        return not_found();
    }
    let url = format!("/ui/apps/{app}/icon");
    let Some(icon) = AgentIcon::new(&form.shape, &form.color) else {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    };
    let saved = ui.settings.update_prefs(|p| {
        if icon == AgentIcon::for_app(&app) {
            p.app_icons.remove(&app);
        } else {
            p.app_icons.insert(
                app.clone(),
                melissa_settings::prefs::AppIconPref {
                    shape: icon.shape.clone(),
                    color: icon.color.clone(),
                },
            );
        }
    });
    if !crate::is_htmx(&headers) {
        return Redirect::to(&format!("/cluster/apps/{app}")).into_response();
    }
    let (shown_icon, note) = match saved {
        Ok(p) => (icon_of(&app, &p), "Saved."),
        Err(e) => {
            tracing::warn!("app icon not saved: {e}");
            (
                icon_of(&app, &ui.settings.prefs()),
                "Couldn't save the icon.",
            )
        }
    };
    let card = app_icon_card(
        SHAPES,
        COLORS,
        &shown_icon.shape,
        &shown_icon.color,
        &url,
        Some(note),
    );
    let head = app_head_icon_oob(&shown_icon.shape, &shown_icon.color);
    (
        [("hx-trigger", "cluster-changed")],
        maud::html! { (card) (head) },
    )
        .into_response()
}

// ---- stop / start ------------------------------------------------------------------------

async fn stop(State(ui): State<Ui>, Path(app): Path<String>, headers: HeaderMap) -> Response {
    toggle(ui, app, "stop_app", headers).await
}

async fn start(State(ui): State<Ui>, Path(app): Path<String>, headers: HeaderMap) -> Response {
    toggle(ui, app, "start_app", headers).await
}

/// The details' region, telling the list to refresh.
fn changed(markup: Markup) -> Response {
    ([("hx-trigger", "cluster-changed")], markup).into_response()
}

/// Run `tool` for `app` as the user. If the policy asks first (stop always does), the
/// details show that question; otherwise the new state.
async fn toggle(ui: Ui, app: String, tool: &'static str, headers: HeaderMap) -> Response {
    let Some(who) = allowed(&ui, &app).await else {
        return not_found();
    };
    let confirmations = ui.lead.confirmations().clone();
    let before: Vec<u64> = confirmations.pending().iter().map(|c| c.id).collect();
    let server = ui.lead.server().clone();
    let args = json!({ "app": app });
    let mut task = tokio::spawn(async move {
        let mut ctx = TaskContext {
            scope: Scope::User,
            attended: true,
            ..TaskContext::default()
        };
        server.registry().call(0, &mut ctx, tool, args).await
    });
    let deadline = tokio::time::Instant::now() + ACTION_WAIT;
    let host = host(&headers);
    loop {
        if task.is_finished() {
            let message = match (&mut task).await {
                Ok(Ok(_)) => None,
                Ok(Err(e)) => Some(e.to_string()),
                Err(e) => Some(e.to_string()),
            };
            ui.cluster.refresh_now().await;
            let extra = Extra {
                message: message.as_deref().map(|m| (m, Tone::Caution)),
                ..Extra::default()
            };
            return changed(render_details(&ui, &app, host, &who, extra).await);
        }
        if let Some(c) = confirmations
            .pending()
            .into_iter()
            .find(|c| c.tool == tool && c.args["app"] == app.as_str() && !before.contains(&c.id))
        {
            let extra = Extra {
                ask: Some((c.id, tool)),
                ..Extra::default()
            };
            return render_details(&ui, &app, host, &who, extra)
                .await
                .into_response();
        }
        if tokio::time::Instant::now() >= deadline {
            // It goes on in the background; the details show where it is now.
            let extra = Extra {
                message: Some(("Still working on it.", Tone::Info)),
                ..Extra::default()
            };
            return render_details(&ui, &app, host, &who, extra)
                .await
                .into_response();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Answer the stop/start question shown in the details.
async fn answer(
    State(ui): State<Ui>,
    Path((app, id, verdict)): Path<(String, u64, String)>,
    headers: HeaderMap,
) -> Response {
    let verdict = match verdict.as_str() {
        "approve" => Verdict::Approved,
        "deny" => Verdict::Denied,
        _ => return not_found(),
    };
    let Some(who) = allowed(&ui, &app).await else {
        return not_found();
    };
    let host = host(&headers);
    // Only the details' own question: stop/start of this app.
    let ours = ui.lead.confirmations().get(id).is_some_and(|c| {
        matches!(c.tool.as_str(), "stop_app" | "start_app") && c.args["app"] == app.as_str()
    });
    if !ours || ui.lead.resolve_confirmation(id, verdict).is_none() {
        let extra = Extra {
            message: Some(("That question was already answered.", Tone::Info)),
            ..Extra::default()
        };
        return render_details(&ui, &app, host, &who, extra)
            .await
            .into_response();
    }
    if verdict == Verdict::Approved {
        // The tool runs right after the approval; give it a moment to change the state.
        tokio::time::sleep(Duration::from_millis(600)).await;
    }
    ui.cluster.refresh_now().await;
    changed(render_details(&ui, &app, host, &who, Extra::default()).await)
}

// ---- force delete ------------------------------------------------------------------------

#[derive(Deserialize)]
struct Confirm {
    #[serde(default)]
    confirm: String,
}

/// The user typed the app's name.
fn confirmed(app: &str, typed: &str) -> bool {
    typed.trim() == app
}

async fn force_delete(
    State(ui): State<Ui>,
    Path(app): Path<String>,
    headers: HeaderMap,
    Form(form): Form<Confirm>,
) -> Response {
    let Some(who) = allowed(&ui, &app).await else {
        return not_found();
    };
    let host = host(&headers);
    if !confirmed(&app, &form.confirm) {
        let msg = format!("Type {app} to confirm.");
        let extra = Extra {
            message: Some((&msg, Tone::Caution)),
            ..Extra::default()
        };
        return render_details(&ui, &app, host, &who, extra)
            .await
            .into_response();
    }
    let name = display_app(&app);
    match ui.lead.user_force_delete(&app).await {
        Ok(_) => {
            ui.cluster.refresh_now().await;
            let msg = format!("{name} and its data are gone.");
            let extra = Extra {
                message: Some((&msg, Tone::Ok)),
                gone: true,
                ..Extra::default()
            };
            changed(render_details(&ui, &app, host, &who, extra).await)
        }
        Err(e) => {
            ui.cluster.refresh_now().await;
            let msg = format!("Couldn't delete it: {e}");
            let extra = Extra {
                message: Some((&msg, Tone::Caution)),
                ..Extra::default()
            };
            changed(render_details(&ui, &app, host, &who, extra).await)
        }
    }
}

// ---- JSON --------------------------------------------------------------------------------

fn error(status: StatusCode, message: impl std::fmt::Display) -> Response {
    (status, Json(json!({"error": message.to_string()}))).into_response()
}

fn tool_response(r: Result<Value, ToolError>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e @ ToolError::BadArgs(_)) => error(StatusCode::UNPROCESSABLE_ENTITY, e),
        Err(e @ (ToolError::Denied | ToolError::Refused(_))) => error(StatusCode::FORBIDDEN, e),
        Err(e @ ToolError::UnknownTool(_)) => error(StatusCode::NOT_FOUND, e),
        Err(e) => error(StatusCode::BAD_GATEWAY, e),
    }
}

/// The apps as the person sees them (a member: only their own; the owner: `?by=` as in the
/// list). `k3s`: `ready`, `not_set_up`, `unreachable` or `unknown`.
async fn api_list(State(ui): State<Ui>, Query(q): Query<AppsQuery>) -> Response {
    let cx = ui.apps_cx(q.by.as_deref());
    let snap = ui.cluster.get().await;
    let installed = match &snap.apps {
        Apps::Known { installed } => installed,
        Apps::NotSetUp => return Json(json!({"k3s": "not_set_up", "apps": []})).into_response(),
        Apps::Unreachable => {
            return Json(json!({"k3s": "unreachable", "apps": []})).into_response()
        }
        Apps::Unknown => return Json(json!({"k3s": "unknown", "apps": []})).into_response(),
    };
    let all = visible(installed, &cx.looker, &cx.owners, &cx.prefs);
    let apps: Vec<Value> = shown(&all, &cx.show)
        .iter()
        .map(|l| {
            json!({
                "app": l.app.name,
                "name": l.app.display_name(),
                "kind": l.kind.as_str(),
                "phase": l.app.phase,
                "installed_for": l.person.as_ref().map(|p| json!({
                    "id": p,
                    "name": person_words(p, &cx.names),
                })),
                "icon": {"shape": l.icon.shape, "color": l.icon.color},
                "url": l.app.url,
                "port": l.app.port,
            })
        })
        .collect();
    Json(json!({"k3s": "ready", "apps": apps})).into_response()
}

/// The refusal for an app the person may not see (404, the same answer as for no such app)
/// or a bad name (422); None: go ahead.
async fn api_allowed(ui: &Ui, app: &str) -> Option<Response> {
    if !valid(app) {
        return Some(error(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("`{app}` is not an app name"),
        ));
    }
    match allowed(ui, app).await {
        Some(_) => None,
        None => Some(error(StatusCode::NOT_FOUND, format!("no app `{app}`"))),
    }
}

async fn api_details(State(ui): State<Ui>, Path(app): Path<String>) -> Response {
    if let Some(r) = api_allowed(&ui, &app).await {
        return r;
    }
    tool_response(call(&ui, "app_status", json!({ "app": app }), false).await)
}

async fn api_logs(
    State(ui): State<Ui>,
    Path(app): Path<String>,
    Query(q): Query<LogsQuery>,
) -> Response {
    if let Some(r) = api_allowed(&ui, &app).await {
        return r;
    }
    tool_response(call(&ui, "app_logs", logs_args(&app, &q), false).await)
}

async fn api_force_delete(
    State(ui): State<Ui>,
    Path(app): Path<String>,
    body: Option<Json<Confirm>>,
) -> Response {
    if !valid(&app) {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("`{app}` is not an app name"),
        );
    }
    let typed = body.map(|Json(b)| b.confirm).unwrap_or_default();
    if !confirmed(&app, &typed) {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("force delete needs {{\"confirm\": \"{app}\"}}"),
        );
    }
    let result: Result<ForceDeleted, LeadError> = ui.lead.user_force_delete(&app).await;
    ui.cluster.refresh_now().await;
    match result {
        Ok(done) => Json(done).into_response(),
        Err(e @ LeadError::Failed(_)) => error(StatusCode::BAD_GATEWAY, e),
        Err(e) => error(StatusCode::UNPROCESSABLE_ENTITY, e),
    }
}

/// The parts of an app's network a pending request changes: `set_app_network` by its
/// arguments, a deploy or update by comparing its spec's network with `current`.
pub(crate) fn network_fields(tool: &str, args: &Value, current: &Value) -> Vec<&'static str> {
    let mut out = Vec::new();
    match tool {
        "set_app_network" => {
            for (key, field) in [
                ("internet", "internet"),
                ("lan", "lan"),
                ("add_links", "links"),
                ("remove_links", "links"),
            ] {
                if args.get(key).is_some_and(|v| !v.is_null()) && !out.contains(&field) {
                    out.push(field);
                }
            }
        }
        "deploy_app" | "update_app" => {
            let spec = match &args["spec"] {
                Value::String(t) => melissa_appspec::AppSpec::from_text(t).ok(),
                v => melissa_appspec::AppSpec::from_json(v).ok(),
            };
            let Some(n) = spec.map(|s| s.network_or_default()) else {
                return out;
            };
            if current["internet"].as_bool().unwrap_or(false) != n.internet {
                out.push("internet");
            }
            if current["lan"].as_bool().unwrap_or(false) != n.lan {
                out.push("lan");
            }
            let mut now: Vec<(String, Option<String>)> = current["links"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|l| {
                    Some((
                        l["app"].as_str()?.to_string(),
                        l["port"].as_str().map(str::to_string),
                    ))
                })
                .collect();
            let mut wanted: Vec<(String, Option<String>)> =
                n.links.into_iter().map(|l| (l.app, l.port)).collect();
            now.sort();
            wanted.sort();
            if now != wanted {
                out.push("links");
            }
        }
        _ => {}
    }
    out
}

/// Where confirmation `id` (of `task`) has its approval card: the chat it was asked in (the
/// same anchor the details' Waiting rows use), else Waiting for you.
fn approval_href(ui: &Ui, task: melissa_agent::TaskId, id: u64) -> String {
    let chat = if task == ui.lead.task_id() {
        "/".to_string()
    } else {
        match ui.lead.agents().agent_of_task(task) {
            Some(agent) => agent_href(&agent),
            None => "/approvals".to_string(),
        }
    };
    format!("{chat}#{}", entry_id(&format!("c{id}")))
}

/// `app`'s network (the `app_network` view) plus `pending`: agents' requests waiting for the
/// owner that change it.
async fn network_read(ui: &Ui, app: &str) -> Result<Value, ToolError> {
    let mut view = call(ui, "app_network", json!({ "app": app }), false).await?;
    #[allow(unused_mut)]
    let mut pending: Vec<Value> = ui
        .lead
        .confirmations()
        .pending()
        .into_iter()
        .filter(|c| c.args["app"].as_str() == Some(app))
        .filter_map(|c| {
            let fields = network_fields(&c.tool, &c.args, &view);
            (!fields.is_empty()).then(|| {
                json!({
                    "id": c.id,
                    "tool": c.tool,
                    "task": c.task,
                    "fields": fields,
                    "reason": c.reason,
                    "href": approval_href(ui, c.task, c.id),
                })
            })
        })
        .collect();
    // The screenshot harness's fixed answer may carry requests of its own.
    #[cfg(feature = "fake-apps")]
    pending.extend(view["pending"].as_array().cloned().unwrap_or_default());
    view["pending"] = json!(pending);
    Ok(view)
}

async fn api_network(State(ui): State<Ui>, Path(app): Path<String>) -> Response {
    if let Some(r) = api_allowed(&ui, &app).await {
        return r;
    }
    match network_read(&ui, &app).await {
        Ok(view) => Json(view).into_response(),
        Err(e) => tool_response(Err(e)),
    }
}

// ---- the Network card ----------------------------------------------------------------------

/// How the Network card shows.
#[derive(Default)]
struct NetShow<'a> {
    /// The owner: switches, Add link and remove. Otherwise read-only.
    owner: bool,
    /// The add-link form is open, for this app (empty: the first one).
    add: Option<&'a str>,
    message: Option<(&'a str, Tone)>,
}

/// A port as the card shows it: "db · 5432".
fn port_words(p: &Value) -> Option<String> {
    Some(format!("{} · {}", p["name"].as_str()?, p["port"].as_u64()?))
}

/// The Network card from the `network_read` view. Pure, for tests.
fn network_markup(app: &str, view: &Value, show: &NetShow<'_>) -> Markup {
    let base = format!("/ui/apps/{app}/network");
    // The approval card of the first waiting request that changes `field`.
    let asked = |field: &str| {
        view["pending"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|p| {
                p["fields"]
                    .as_array()
                    .is_some_and(|f| f.iter().any(|x| x == field))
            })
            .map(|p| match p["href"].as_str() {
                Some(h) => h.to_string(),
                None => format!(
                    "/approvals#{}",
                    entry_id(&format!("c{}", p["id"].as_u64().unwrap_or_default()))
                ),
            })
    };
    let (internet_asked, lan_asked, links_asked) =
        (asked("internet"), asked("lan"), asked("links"));
    let (internet_url, lan_url) = (format!("{base}/internet"), format!("{base}/lan"));
    let links_url = format!("{base}/links");
    let remove_url = format!("{base}/links/remove");
    // The links: (app, port name, display name, port words, inactive).
    let links: Vec<(String, Option<String>, String, String, bool)> = view["links"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| {
            let to = l["app"].as_str()?.to_string();
            let port = l["port"].as_str().map(str::to_string);
            let words = match &port {
                Some(name) => l["ports"]
                    .as_array()
                    .and_then(|ps| ps.first())
                    .and_then(port_words)
                    .unwrap_or_else(|| name.clone()),
                None => "all ports".to_string(),
            };
            Some((
                to.clone(),
                port,
                display_app(&to),
                words,
                l["active"] == false,
            ))
        })
        .collect();
    let rows: Vec<NetworkLink<'_>> = links
        .iter()
        .map(|(to, port, name, words, inactive)| NetworkLink {
            name,
            port: words,
            inactive: *inactive,
            remove: show
                .owner
                .then_some((remove_url.as_str(), to.as_str(), port.as_deref())),
        })
        .collect();
    // The picker: the other apps' named ports that aren't linked yet.
    let linked = |to: &str, port: &str| {
        links
            .iter()
            .any(|(a, p, ..)| a == to && p.as_deref().is_none_or(|p| p == port))
    };
    let options: Vec<(String, Vec<(String, String)>)> = view["link_options"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| {
            let to = o["app"].as_str()?;
            let ports: Vec<(String, String)> = o["ports"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|p| p["name"].as_str().is_some_and(|n| !linked(to, n)))
                .filter_map(|p| Some((format!("{to}/{}", p["name"].as_str()?), port_words(p)?)))
                .collect();
            (!ports.is_empty()).then(|| (to.to_string(), ports))
        })
        .collect();
    let app_names: Vec<(String, String)> = options
        .iter()
        .map(|(to, _)| (to.clone(), display_app(to)))
        .collect();
    let app_choices: Vec<(&str, &str)> = app_names
        .iter()
        .map(|(v, l)| (v.as_str(), l.as_str()))
        .collect();
    let open = show.add.filter(|_| show.owner).map(|want| {
        options
            .iter()
            .find(|(to, _)| to == want)
            .or(options.first())
    });
    let chosen = open.flatten();
    let no_ports = Vec::new();
    let refresh = format!("{base}?add=1");
    let details_url = format!("/cluster/apps/{app}");
    let add_plain = format!("{details_url}?add_link=1");
    let add_get = format!("{base}?add=1");
    let footer = format!("DNS and {}'s web ports always work.", display_app(app));
    network_card(&NetworkCard {
        internet: NetworkSwitch {
            label: "Internet",
            on: view["internet"] == true,
            action: show.owner.then_some(internet_url.as_str()),
            asked: internet_asked.as_deref(),
        },
        lan: NetworkSwitch {
            label: "Local network",
            on: view["lan"] == true,
            action: show.owner.then_some(lan_url.as_str()),
            asked: lan_asked.as_deref(),
        },
        links_asked: links_asked.as_deref(),
        add: (show.owner && open.is_none()).then_some((add_plain.as_str(), add_get.as_str())),
        form: open.map(|_| AddLinkForm {
            action: &links_url,
            cancel: (&details_url, &base),
            refresh: &refresh,
            apps: &app_choices,
            app: chosen.map(|(to, _)| to.as_str()).unwrap_or_default(),
            ports: chosen.map(|(_, p)| p).unwrap_or(&no_ports),
        }),
        links: &rows,
        footer: &footer,
        message: show.message,
    })
}

/// The Network card for `app`, or None when its network can't be read (not installed, no
/// app platform).
async fn network_card_for(ui: &Ui, app: &str, show: &NetShow<'_>) -> Option<Markup> {
    match network_read(ui, app).await {
        Ok(view) => Some(network_markup(app, &view, show)),
        Err(e) => {
            tracing::debug!("network of {app} not read: {e}");
            None
        }
    }
}

#[derive(Deserialize)]
struct NetQuery {
    #[serde(default)]
    add: Option<String>,
    #[serde(default)]
    app: Option<String>,
}

/// The Network card alone: `?add=1` opens the add-link form (`&app=` picks the app).
async fn network_region(
    State(ui): State<Ui>,
    Path(app): Path<String>,
    Query(q): Query<NetQuery>,
) -> Response {
    let Some((cx, _)) = allowed(&ui, &app).await else {
        return not_found();
    };
    let show = NetShow {
        owner: cx.looker.owner,
        add: q
            .add
            .is_some()
            .then(|| q.app.as_deref().unwrap_or_default()),
        message: None,
    };
    match network_card_for(&ui, &app, &show).await {
        Some(card) => card.into_response(),
        None => not_found(),
    }
}

#[derive(Deserialize)]
struct SwitchForm {
    #[serde(default)]
    enabled: Option<String>,
}

#[derive(Deserialize)]
struct LinkForm {
    #[serde(default)]
    app: String,
    /// `<app>/<port>` from the add form; a port name from a remove button.
    #[serde(default)]
    port: Option<String>,
}

/// Apply the owner's change to `app`'s network and answer with the card (htmx) or the details
/// page. The password step comes before this (the gate's `sensitive`).
async fn apply_network(
    ui: &Ui,
    app: &str,
    headers: &HeaderMap,
    change: Value,
    failed: &str,
) -> Response {
    let Some((cx, _)) = allowed(ui, app).await else {
        return not_found();
    };
    if !cx.looker.owner {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut args = change;
    args["app"] = json!(app);
    // The owner, watching: no confirmation (the tool asks only for agents' calls).
    let result = call(ui, "set_app_network", args, true).await;
    if let Err(e) = &result {
        tracing::warn!("network of {app} not changed: {e}");
    }
    ui.cluster.refresh_now().await;
    if !crate::is_htmx(headers) {
        return Redirect::to(&format!("/cluster/apps/{app}")).into_response();
    }
    let show = NetShow {
        owner: true,
        add: None,
        message: result.is_err().then_some((failed, Tone::Caution)),
    };
    match network_card_for(ui, app, &show).await {
        Some(card) => card.into_response(),
        None => not_found(),
    }
}

/// Internet or Local network on or off.
async fn set_network_switch(
    State(ui): State<Ui>,
    Path((app, field)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<SwitchForm>,
) -> Response {
    let key = match field.as_str() {
        "internet" | "lan" => field.as_str(),
        _ => return not_found(),
    };
    let on = form.enabled.is_some();
    let change = json!({ key: on });
    apply_network(
        &ui,
        &app,
        &headers,
        change,
        "Couldn't change it. Try again.",
    )
    .await
}

/// Add a link to one of another app's named ports.
async fn add_network_link(
    State(ui): State<Ui>,
    Path(app): Path<String>,
    headers: HeaderMap,
    Form(form): Form<LinkForm>,
) -> Response {
    let picked = form.port.as_deref().and_then(|p| p.split_once('/'));
    match picked {
        Some((to, port)) if to == form.app && !port.is_empty() => {
            let change = json!({ "add_links": [{ "app": to, "port": port }] });
            apply_network(&ui, &app, &headers, change, "Couldn't add that link.").await
        }
        // Another app was chosen without script (or nothing): show its ports.
        _ => {
            let Some((cx, _)) = allowed(&ui, &app).await else {
                return not_found();
            };
            if !crate::is_htmx(&headers) {
                let to = if valid(&form.app) {
                    form.app.as_str()
                } else {
                    ""
                };
                return Redirect::to(&format!("/cluster/apps/{app}?add_link=1&to={to}"))
                    .into_response();
            }
            let show = NetShow {
                owner: cx.looker.owner,
                add: Some(&form.app),
                message: None,
            };
            match network_card_for(&ui, &app, &show).await {
                Some(card) => card.into_response(),
                None => not_found(),
            }
        }
    }
}

/// Remove a link (at once, no confirmation: easy to add again).
async fn remove_network_link(
    State(ui): State<Ui>,
    Path(app): Path<String>,
    headers: HeaderMap,
    Form(form): Form<LinkForm>,
) -> Response {
    let mut link = json!({ "app": form.app });
    if let Some(port) = form.port.as_deref().filter(|p| !p.is_empty()) {
        link["port"] = json!(port);
    }
    let change = json!({ "remove_links": [link] });
    apply_network(&ui, &app, &headers, change, "Couldn't remove that link.").await
}

async fn api_set_network(
    State(ui): State<Ui>,
    Path(app): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    if !crate::auth::signed_in().is_some_and(|p| p.is_owner()) {
        return error(StatusCode::FORBIDDEN, "Only the owner can do this.");
    }
    if let Some(r) = api_allowed(&ui, &app).await {
        return r;
    }
    let mut change = match body {
        Some(Json(v @ Value::Object(_))) => v,
        _ => {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "send a JSON object with internet, lan, add_links or remove_links",
            )
        }
    };
    change["app"] = json!(app);
    // The owner, watching: no confirmation (the tool asks only for agents' calls).
    let r = call(&ui, "set_app_network", change, true).await;
    ui.cluster.refresh_now().await;
    tool_response(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_requests_name_the_network_fields_they_change() {
        let current =
            json!({"internet": false, "lan": true, "links": [{"app": "pg", "port": "db"}]});
        assert_eq!(
            network_fields(
                "set_app_network",
                &json!({"app": "x", "internet": true, "add_links": [], "remove_links": []}),
                &current
            ),
            ["internet", "links"]
        );
        let spec = |network: Value| {
            json!({"app": "x", "spec": {
                "description": "x",
                "containers": [{"name": "x", "image": "nginx:1.27"}],
                "network": network,
            }})
        };
        assert!(network_fields(
            "update_app",
            &spec(json!({"lan": true, "links": [{"app": "pg", "port": "db"}]})),
            &current
        )
        .is_empty());
        assert_eq!(
            network_fields("update_app", &spec(json!({"internet": true})), &current),
            ["internet", "lan", "links"]
        );
        assert!(network_fields("stop_app", &json!({"app": "x"}), &current).is_empty());
    }

    #[test]
    fn the_device_fact_uses_the_owners_name_for_this_device_only() {
        let fact = |d: &str| vec![("Device".to_string(), vec![d.to_string()])];
        let mut facts = fact("d-0a1b2c3d");
        name_this_device(&mut facts, "d-0a1b2c3d", "Living room");
        assert_eq!(facts, fact("Living room"));
        // Another device keeps its id.
        let mut facts = fact("d-ffffffff");
        name_this_device(&mut facts, "d-0a1b2c3d", "Living room");
        assert_eq!(facts, fact("d-ffffffff"));
    }

    #[test]
    fn details_read_the_status_and_build_the_link_from_the_host() {
        let status = json!({
            "app": "nextcloud", "phase": "ready", "agent": "d.a1", "agent_name": "Ada",
            "device": "d-0a1b2c3d", "port": 30000, "url": "http://203.0.113.10:30000",
            "version": "34.0.6", "image": "docker.io/library/nextcloud:34.0.6",
            "installed": true, "replicas": 1, "ready_replicas": 1,
            "pods": [{"restarts": 2, "age_secs": 3700}, {"restarts": 1, "age_secs": 90000}],
        });
        let v = details_view(&status, Some("192.0.2.7:8080"));
        assert_eq!(v.name, "Nextcloud");
        let get = |l: &str| v.facts.iter().find(|f| f.0 == l).map(|f| f.1.join(" | "));
        assert_eq!(get("Looked after by").as_deref(), Some("Ada"));
        assert_eq!(get("Device").as_deref(), Some("d-0a1b2c3d"));
        assert_eq!(get("Addresses").as_deref(), Some("192.0.2.7:30000 (HTTP)"));
        assert_eq!(v.link.as_deref(), Some("http://192.0.2.7:30000"));
        assert_eq!(get("Pods").as_deref(), Some("1 of 1 running"));
        assert_eq!(get("Restarts").as_deref(), Some("3"));
        assert_eq!(get("Age").as_deref(), Some("Up 1 h 1 min"));
        // A hostile host header: the tool's own link.
        let v = details_view(&status, Some("<x>"));
        assert_eq!(v.link.as_deref(), Some("http://203.0.113.10:30000"));
        // Nothing installed: no pod facts, nobody.
        let v = details_view(
            &json!({"app": "immich", "phase": "uninstalled_data_kept"}),
            None,
        );
        assert_eq!(v.facts.len(), 1);
        assert_eq!(v.facts[0].1, ["Nobody"]);
        assert_eq!(v.link, None);
    }

    #[test]
    fn details_show_other_ports_with_their_protocol() {
        let status = json!({
            "app": "minecraft", "phase": "ready",
            "ports": [
                {"name": "game", "protocol": "tcp", "port": 30001, "web": false,
                 "address": "203.0.113.10:30001"},
                {"name": "voice", "protocol": "udp", "port": 30002, "web": false,
                 "address": "203.0.113.10:30002"},
            ],
        });
        let addresses = |host: Option<&str>| {
            let v = details_view(&status, host);
            assert_eq!(v.link, None);
            v.facts
                .into_iter()
                .find(|f| f.0 == "Addresses")
                .map(|f| f.1)
                .unwrap_or_default()
        };
        assert_eq!(
            addresses(Some("100.64.0.10:8080")),
            [
                "100.64.0.10:30001 (TCP, game)",
                "100.64.0.10:30002 (UDP, voice)"
            ]
        );
        // No usable host: the tool's address.
        assert_eq!(addresses(None)[0], "203.0.113.10:30001 (TCP, game)");
        // The details keep the full MagicDNS name (the list shows the short one).
        assert_eq!(
            addresses(Some("melissa-box.example.ts.net:8080"))[0],
            "melissa-box.example.ts.net:30001 (TCP, game)"
        );
    }

    #[test]
    fn force_delete_needs_the_exact_name() {
        assert!(confirmed("nextcloud", " nextcloud "));
        assert!(!confirmed("nextcloud", ""));
        assert!(!confirmed("nextcloud", "Nextcloud"));
        assert!(!confirmed("nextcloud", "yes"));
        assert!(valid("nextcloud") && valid("home-assistant"));
        assert!(!valid("../x") && !valid("-a") && !valid("A") && !valid(""));
    }

    fn network_view() -> Value {
        json!({
            "internet": false, "lan": false,
            "links": [{"app": "postgres", "port": "db", "active": true,
                       "ports": [{"name": "db", "port": 5432, "protocol": "tcp"}]}],
            "link_options": [
                {"app": "ollama", "ports": [{"name": "api", "port": 11434, "protocol": "tcp"},
                                            {"name": "web", "port": 8080, "protocol": "tcp"}]},
                {"app": "postgres", "ports": [{"name": "db", "port": 5432, "protocol": "tcp"}]},
                {"app": "bare", "ports": []},
            ],
            "pending": [{"id": 4, "tool": "set_app_network", "fields": ["internet"]}],
        })
    }

    /// An agent's waiting request marks only the row it changes (owner decision 2).
    #[test]
    fn a_waiting_request_shows_agent_asked_on_its_row() {
        let html = network_markup("immich", &network_view(), &NetShow::default()).into_string();
        assert_eq!(html.matches("Agent asked").count(), 1);
        let internet = html.find(">Internet<").unwrap();
        let lan = html.find(">Local network<").unwrap();
        let badge = html.find("Agent asked").unwrap();
        assert!(internet < badge && badge < lan, "{html}");
        // It links to the request's approval card.
        assert!(html.contains("href=\"/approvals#f-c4\""), "{html}");
    }

    /// The picker offers the other apps' named ports not linked yet, and nothing to type
    /// (owner decision 3).
    #[test]
    fn the_link_picker_lists_only_named_ports() {
        let show = NetShow {
            owner: true,
            add: Some("ollama"),
            ..NetShow::default()
        };
        let html = network_markup("immich", &network_view(), &show).into_string();
        let values: Vec<&str> = html
            .split("<option value=\"")
            .skip(1)
            .filter_map(|o| o.split('"').next())
            .collect();
        // Apps: only those with a port left (postgres's one port is linked); then ollama's ports.
        assert_eq!(values, ["ollama", "ollama/api", "ollama/web"]);
        assert!(html.contains(">api · 11434<"));
        assert!(!html.contains("type=\"text\"") && !html.contains("type=\"number\""));
        // Open form: no second "Add link" button above it.
        assert_eq!(html.matches(">Add link<").count(), 1);
    }
}
