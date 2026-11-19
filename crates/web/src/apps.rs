//! App details: the dialog on "Apps and devices" and its JSON API.
//!
//! - `GET /ui/apps/{app}`: the dialog's content (status, logs region, actions).
//! - `GET /ui/apps/{app}/logs?container=`: the last [`LOG_LINES`] log lines (escaped text).
//! - `POST /ui/apps/{app}/stop|start`: the `stop_app`/`start_app` tools for the user (scope
//!   `User`, attended). A confirmation the policy asks for shows in the dialog; `POST
//!   /ui/apps/{app}/confirmations/{id}/approve|deny` answers it (the same inbox as the chat).
//! - `POST /ui/apps/{app}/force-delete` (form `confirm` = the app's name): the user's force
//!   delete ([`melissa_agent::lead::Lead::user_force_delete`]). Not a tool: no agent can
//!   reach it.
//! - JSON: `GET /api/apps/{app}` (`app_status`), `GET /api/apps/{app}/logs?lines=&container=`
//!   (`app_logs`), `POST /api/apps/{app}/force-delete` `{"confirm": "<app>"}` → the
//!   [`ForceDeleted`] report; 422 without the right `confirm`, a bad name or no such workspace,
//!   502 on a cluster failure.
//!
//! Reads and stop/start go through the tool registry, so they are policy-checked and audited
//! like the agents' calls. Mutating requests pass the same-origin check (`security`).

use std::time::Duration;

use axum::{
    extract::{Form, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use maud::Markup;
use melissa_agent::{
    lead::{agents::ForceDeleted, LeadError},
    tools::{Scope, TaskContext, ToolError, Verdict},
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    components::{app_confirm, app_details, app_gone, app_logs, AppDetails, AppLogs, Fact, Tone},
    feed::display_app,
    pages::format_uptime,
    state::{host_name_of, phase_words},
    ui::{host, Ui},
};

/// Log lines the dialog shows.
pub(crate) const LOG_LINES: u64 = 200;
/// Longest wait for a read.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// How long stop/start waits for the tool (or its confirmation) before answering.
const ACTION_WAIT: Duration = Duration::from_secs(5);

pub(crate) fn routes(ui: Ui) -> Router {
    Router::new()
        .route("/ui/apps/{app}", get(details))
        .route("/ui/apps/{app}/logs", get(logs))
        .route("/ui/apps/{app}/stop", post(stop))
        .route("/ui/apps/{app}/start", post(start))
        .route("/ui/apps/{app}/confirmations/{id}/{verdict}", post(answer))
        .route("/ui/apps/{app}/force-delete", post(force_delete))
        .route("/api/apps/{app}", get(api_details))
        .route("/api/apps/{app}/logs", get(api_logs))
        .route("/api/apps/{app}/force-delete", post(api_force_delete))
        .with_state(ui)
}

/// A catalog-style app name (the tools check again).
fn valid(app: &str) -> bool {
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

// ---- the dialog --------------------------------------------------------------------------

/// The facts for the dialog, from `app_status`. Pure, for tests.
pub(crate) struct DetailsView {
    pub name: String,
    pub phase: String,
    pub facts: Vec<(&'static str, String, Option<String>)>,
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
    facts.push(("Looked after by", keeper, None));
    if let Some(device) = status["device"].as_str() {
        facts.push(("Device", device.to_string(), None));
    }
    let port = status["port"].as_u64();
    let link = match (host.and_then(host_name_of), port) {
        (Some(h), Some(p)) => Some(format!("http://{h}:{p}")),
        _ => status["url"].as_str().map(str::to_string),
    };
    if let Some(link) = link.filter(|l| l.starts_with("http://") || l.starts_with("https://")) {
        facts.push(("Link", link.clone(), Some(link)));
    }
    if let Some(v) = status["version"].as_str() {
        facts.push(("Version", v.to_string(), None));
    }
    if let Some(i) = status["image"].as_str() {
        facts.push(("Image", i.to_string(), None));
    }
    if status["installed"] == true {
        let pods = status["pods"].as_array().cloned().unwrap_or_default();
        facts.push((
            "Pods",
            format!(
                "{}/{} ready",
                status["ready_replicas"].as_u64().unwrap_or(0),
                status["replicas"].as_u64().unwrap_or(0)
            ),
            None,
        ));
        let restarts: u64 = pods.iter().filter_map(|p| p["restarts"].as_u64()).sum();
        facts.push(("Restarts", restarts.to_string(), None));
        if let Some(age) = pods.iter().filter_map(|p| p["age_secs"].as_u64()).min() {
            facts.push(("Age", format_uptime(age), None));
        }
    }
    DetailsView {
        name: display_app(app),
        phase: status["phase"].as_str().unwrap_or("unknown").to_string(),
        facts,
    }
}

/// The dialog's content for `app`, with an optional message.
async fn render_details(
    ui: &Ui,
    app: &str,
    host: Option<&str>,
    message: Option<(&str, Tone)>,
) -> Markup {
    let status = match call(ui, "app_status", json!({ "app": app }), false).await {
        Ok(s) if s["k3s_reachable"] != false => s,
        Ok(_) | Err(_) => json!({"app": app, "phase": "unknown"}),
    };
    let view = details_view(&status, host);
    let (health, words) = phase_words(&view.phase);
    let facts: Vec<Fact<'_>> = view
        .facts
        .iter()
        .map(|(label, value, href)| Fact {
            label,
            value,
            href: href.as_deref(),
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
    let message = message.or_else(|| {
        (status["phase"] == "unknown").then_some(("Couldn't read the status.", Tone::Error))
    });
    app_details(&AppDetails {
        name: &view.name,
        health,
        status: words,
        facts: &facts,
        logs_url: &format!("/ui/apps/{app}/logs"),
        toggle,
        delete_url: &format!("/ui/apps/{app}/force-delete"),
        delete_word: app,
        message,
    })
}

async fn details(State(ui): State<Ui>, Path(app): Path<String>, headers: HeaderMap) -> Response {
    if !valid(&app) {
        return StatusCode::NOT_FOUND.into_response();
    }
    render_details(&ui, &app, host(&headers), None)
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
    if !valid(&app) {
        return StatusCode::NOT_FOUND.into_response();
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
    app_logs(&AppLogs {
        url: &url,
        containers: &containers,
        container: container.as_deref(),
        lines: &lines,
        empty,
    })
    .into_response()
}

// ---- stop / start ------------------------------------------------------------------------

async fn stop(State(ui): State<Ui>, Path(app): Path<String>, headers: HeaderMap) -> Response {
    toggle(ui, app, "stop_app", headers).await
}

async fn start(State(ui): State<Ui>, Path(app): Path<String>, headers: HeaderMap) -> Response {
    toggle(ui, app, "start_app", headers).await
}

/// The dialog content, telling the cluster region to refresh.
fn changed(markup: Markup) -> Response {
    ([("hx-trigger", "cluster-changed")], markup).into_response()
}

/// Run `tool` for `app` as the user. If the policy asks first (stop always does), the
/// dialog shows that confirmation; otherwise the new state.
async fn toggle(ui: Ui, app: String, tool: &'static str, headers: HeaderMap) -> Response {
    if !valid(&app) {
        return StatusCode::NOT_FOUND.into_response();
    }
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
            let msg = message.as_deref().map(|m| (m, Tone::Error));
            return changed(render_details(&ui, &app, host, msg).await);
        }
        if let Some(c) = confirmations
            .pending()
            .into_iter()
            .find(|c| c.tool == tool && c.args["app"] == app.as_str() && !before.contains(&c.id))
        {
            let name = display_app(&app);
            let (question, label) = if tool == "stop_app" {
                (format!("Stop {name}? Its data stays."), "Stop")
            } else {
                (format!("Start {name}?"), "Start")
            };
            let base = format!("/ui/apps/{app}/confirmations/{}", c.id);
            return app_confirm(
                &name,
                &question,
                label,
                &format!("{base}/approve"),
                &format!("{base}/deny"),
            )
            .into_response();
        }
        if tokio::time::Instant::now() >= deadline {
            // It goes on in the background; the dialog shows where it is now.
            return render_details(&ui, &app, host, Some(("Still working on it.", Tone::Info)))
                .await
                .into_response();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Answer the stop/start confirmation shown in the dialog.
async fn answer(
    State(ui): State<Ui>,
    Path((app, id, verdict)): Path<(String, u64, String)>,
    headers: HeaderMap,
) -> Response {
    let verdict = match verdict.as_str() {
        "approve" => Verdict::Approved,
        "deny" => Verdict::Denied,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    if !valid(&app) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let host = host(&headers);
    // Only the dialog's own question: stop/start of this app.
    let ours = ui.lead.confirmations().get(id).is_some_and(|c| {
        matches!(c.tool.as_str(), "stop_app" | "start_app") && c.args["app"] == app.as_str()
    });
    if !ours || ui.lead.resolve_confirmation(id, verdict).is_none() {
        let msg = Some(("That question was already answered.", Tone::Info));
        return render_details(&ui, &app, host, msg).await.into_response();
    }
    if verdict == Verdict::Approved {
        // The tool runs right after the approval; give it a moment to change the state.
        tokio::time::sleep(Duration::from_millis(600)).await;
    }
    ui.cluster.refresh_now().await;
    changed(render_details(&ui, &app, host, None).await)
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
    if !valid(&app) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let host = host(&headers);
    if !confirmed(&app, &form.confirm) {
        let msg = format!("Type {app} to confirm.");
        return render_details(&ui, &app, host, Some((&msg, Tone::Error)))
            .await
            .into_response();
    }
    let name = display_app(&app);
    match ui.lead.user_force_delete(&app).await {
        Ok(_) => {
            ui.cluster.refresh_now().await;
            changed(app_gone(&name, &format!("{name} and its data are gone.")))
        }
        Err(e) => {
            ui.cluster.refresh_now().await;
            let msg = format!("Couldn't delete it: {e}");
            changed(render_details(&ui, &app, host, Some((&msg, Tone::Error))).await)
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

async fn api_details(State(ui): State<Ui>, Path(app): Path<String>) -> Response {
    if !valid(&app) {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("`{app}` is not an app name"),
        );
    }
    tool_response(call(&ui, "app_status", json!({ "app": app }), false).await)
}

async fn api_logs(
    State(ui): State<Ui>,
    Path(app): Path<String>,
    Query(q): Query<LogsQuery>,
) -> Response {
    if !valid(&app) {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("`{app}` is not an app name"),
        );
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let get = |l: &str| v.facts.iter().find(|f| f.0 == l).map(|f| f.1.clone());
        assert_eq!(get("Looked after by").as_deref(), Some("Ada"));
        assert_eq!(get("Device").as_deref(), Some("d-0a1b2c3d"));
        assert_eq!(get("Link").as_deref(), Some("http://192.0.2.7:30000"));
        assert_eq!(get("Pods").as_deref(), Some("1/1 ready"));
        assert_eq!(get("Restarts").as_deref(), Some("3"));
        assert_eq!(get("Age").as_deref(), Some("Up 1 h 1 min"));
        // A hostile host header: the tool's own link.
        let v = details_view(&status, Some("<x>"));
        assert!(v.facts.iter().any(|f| f.1 == "http://203.0.113.10:30000"));
        // Nothing installed: no pod facts, nobody.
        let v = details_view(
            &json!({"app": "immich", "phase": "uninstalled_data_kept"}),
            None,
        );
        assert_eq!(v.facts.len(), 1);
        assert_eq!(v.facts[0].1, "Nobody");
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
}
