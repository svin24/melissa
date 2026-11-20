//! JSON API for the lead agent chat and the confirmation inbox (no pages: the CLI uses it today,
//! the web UI later). Mutating requests pass the same same-origin check as the UI.
//!
//! - `POST /api/lead/messages` `{"text": "..."}` → 202 `{"turn": n, "since": seq}`; 409 while
//!   a turn runs; 422 for an empty or too long message.
//! - `GET /api/lead/events?since=<seq>&wait=<secs>&limit=<n>` → `{"events": [...], "next":
//!   seq, "missed": bool}`; waits up to `wait` (max 30) seconds for the first new event.
//! - `POST /api/lead/cancel` → `{"cancelled": bool}`.
//! - `GET /api/lead/status` → `{"turn": n|null, "backend": "..."|null, "last_seq": seq}`.
//! - `GET /api/lead/backends` → `{"backends": [{name, label, available, reason?, current,
//!   model?, models?: [{id, label}]}]}`: the session backends the lead agent can switch to
//!   (`[lead] backends`); `models` only once the agent has reported them.
//!   Listing also starts a model probe in the background for usable backends whose list is
//!   missing or stale (rate-limited; see `melissa_agent::lead::backends`).
//! - `POST /api/lead/backends/refresh` → `{"probes": [{backend, result, models, error?}]}`:
//!   ask every usable session backend for its models now (no prompt, up to 15 s each, in
//!   parallel); `result` is `refreshed`, `no_models`, `skipped` (one is running) or `failed`.
//! - `POST /api/lead/backend` `{"name": "...", "model": "..."?}` → `{backend, model?,
//!   changed, after_turn}`: switch from the next turn on (saved, audited `lead.backend`);
//!   422 unknown backend or model, or a backend that cannot run here (not installed, not
//!   logged in).
//! - `GET /api/agents/{agent}/backends`, `POST /api/agents/{agent}/backend`: the same for an agent
//!   (any session backend; saved in the agent registry, audited `agent.backend`).
//! - `GET /api/agents` → `{"agents": [...]}`: this device's agents (id, name, backend, status, job).
//! - `GET /api/agents/{agent}` (name or id) → one agent; 404 if there is none.
//! - The user talks to an agent directly, same shapes as the lead agent's: `POST
//!   /api/agents/{agent}/messages`, `GET /api/agents/{agent}/events`, `POST /api/agents/{agent}/cancel`,
//!   `GET /api/agents/{agent}/status` (409 while the agent works on a job or another message).
//! - `POST /api/agents/{agent}/rename` `{"name": "..."}` → the agent (the user renames; 422 for a
//!   bad or taken name).
//! - `GET /api/workspaces` → `{"workspaces": [...]}`: every workspace and its owning agent (502 if k3s is
//!   unreachable).
//! - `POST /api/workspaces/{workspace}/reassign` `{"to": "<agent>"}` → the workspace: the user's own
//!   reassignment (no confirmation; audited `workspace.reassign`, actor ui). 404 no such agent,
//!   422 no such workspace.
//! - `GET /api/confirmations`; `POST /api/confirmations/{id}/approve|deny` → 200 or 404;
//!   `POST /api/confirmations/{id}/always` approves and saves its "Always allow" rule (the
//!   asking agent + tool + target) → 200 with the rule and every confirmation it approved,
//!   404 gone, 409 nothing to remember (the user's own request).
//! - `POST /api/tools/{name}` (JSON arguments) → 200 `{"result": ...}`: runs one tool for an
//!   operator (`melissa-server tool --wait`) through the daemon's registry, as an unattended
//!   task (task 0): the policy applies and confirmations land in the same inbox, so the call
//!   waits for `approve`/`deny` (up to the confirmation timeout). Lead-only tools are not
//!   reachable. The caller is the user (scope `User`: no ownership checks). Errors: 404 unknown
//!   tool, 422 bad arguments, 403 denied or refused, 408 confirmation
//!   timeout, 502 tool failure.
//! - Scheduled jobs (the owner's own changes: no confirmation; audited `schedule.*`, actor `ui`;
//!   handlers in `settings/schedules.rs`). 503 when this device runs no scheduler.
//!   - `GET /api/schedules` → `{"schedules": [{id, name, agent ("lead" or an agent id),
//!     agent_name, instruction, schedule (5-field cron, local time), when, enabled,
//!     max_minutes, created_by, created_at, next_run (Unix s)|null, next_run_local, running,
//!     last_run: {at, outcome, detail?, turn?, ended?, manual?}|null}], "time_zone": "..."}`.
//!   - `POST /api/schedules` `{"name", "agent"? (name, id or "lead"; default lead),
//!     "instruction", "schedule", "enabled"? (true), "max_minutes"? (60)}` → 201 the schedule;
//!     422 invalid (bad cron, empty name or instruction, unknown agent).
//!   - `POST /api/schedules/{id}` (same fields; missing ones keep their value) → the schedule;
//!     404 unknown, 422 invalid. A new timing counts from now (no catch-up runs).
//!   - `POST /api/schedules/{id}/enabled` `{"on": bool}` → the schedule.
//!   - `POST /api/schedules/{id}/run` → 202 `{"run": last_run}`: run now (outcome `running`,
//!     or `skipped` while the previous run or the agent is busy, or `failed`).
//!   - `POST /api/schedules/{id}/remove` → the removed schedule; 404 unknown.
//! - `GET /api/apps/{app}`, `GET /api/apps/{app}/logs`, `POST /api/apps/{app}/force-delete`: see
//!   [`crate::apps`].

use std::time::{Duration, UNIX_EPOCH};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use melissa_agent::{
    lead::{backends::BackendView, Lead, LeadError},
    mcp::McpServer,
    tools::{AlwaysAllowError, Scope, TaskContext, ToolError, Verdict},
    AuditRecord,
};
use serde::Deserialize;

use crate::feed::backend_label;
use serde_json::{json, Value};

/// Longest long-poll wait for events.
const MAX_WAIT_SECS: u64 = 30;

pub(crate) fn routes(lead: Lead) -> Router {
    Router::new()
        .route("/api/lead/messages", post(send_message))
        .route("/api/lead/events", get(events))
        .route("/api/lead/cancel", post(cancel))
        .route("/api/lead/status", get(status))
        .route("/api/lead/new-chat", post(new_chat))
        .route("/api/lead/chats", get(past_chats))
        .route("/api/lead/backends", get(backends))
        .route("/api/lead/backends/refresh", post(refresh_models))
        .route("/api/lead/backend", post(use_backend))
        .route("/api/agents", get(agents))
        .route("/api/agents/{agent}", get(agent).delete(agent_delete))
        .route("/api/agents/{agent}/messages", post(agent_message))
        .route("/api/agents/{agent}/events", get(agent_events))
        .route("/api/agents/{agent}/cancel", post(agent_cancel))
        .route("/api/agents/{agent}/status", get(agent_status))
        .route("/api/agents/{agent}/rename", post(agent_rename))
        .route("/api/agents/{agent}/backends", get(agent_backends))
        .route("/api/agents/{agent}/backend", post(agent_use_backend))
        .route("/api/workspaces", get(workspaces))
        .route("/api/workspaces/{workspace}/reassign", post(reassign))
        .route("/api/confirmations", get(confirmations))
        .route("/api/confirmations/{id}/{verdict}", post(resolve))
        .with_state(lead)
}

pub(crate) fn tool_routes(mcp: McpServer) -> Router {
    Router::new()
        .route("/api/tools/{name}", post(call_tool))
        .with_state(mcp)
}

/// One operator tool call through the daemon's registry (see the module docs).
async fn call_tool(
    State(mcp): State<McpServer>,
    Path(name): Path<String>,
    Json(args): Json<Value>,
) -> Response {
    let registry = mcp.registry();
    if let Some(log) = registry.audit_log() {
        let record =
            AuditRecord::now("operator.tool", 0, "", "tool call from the local API").by("operator");
        if let Err(e) = log.append(&record.with_tool(&name, "")) {
            tracing::error!(path = %log.path().display(), "audit log write failed: {e}");
        }
    }
    // Unattended, untainted, no delegation; the caller is the user (local API), so the
    // scope is theirs: no ownership checks, but the label policy still asks for confirmations.
    let mut ctx = TaskContext {
        scope: Scope::User,
        ..TaskContext::default()
    };
    match registry.call(0, &mut ctx, &name, args).await {
        Ok(result) => Json(json!({"result": result})).into_response(),
        Err(e @ ToolError::UnknownTool(_)) => error(StatusCode::NOT_FOUND, e),
        Err(e @ ToolError::BadArgs(_)) => error(StatusCode::UNPROCESSABLE_ENTITY, e),
        Err(e @ (ToolError::Denied | ToolError::Refused(_))) => error(StatusCode::FORBIDDEN, e),
        Err(e @ (ToolError::ConfirmationTimeout | ToolError::Withdrawn)) => {
            error(StatusCode::REQUEST_TIMEOUT, e)
        }
        Err(e @ ToolError::Failed(_)) => error(StatusCode::BAD_GATEWAY, e),
    }
}

fn error(status: StatusCode, message: impl std::fmt::Display) -> Response {
    (status, Json(json!({"error": message.to_string()}))).into_response()
}

#[derive(Deserialize)]
struct Message {
    text: String,
}

async fn send_message(State(lead): State<Lead>, Json(msg): Json<Message>) -> Response {
    match lead.send(&msg.text) {
        Ok((turn, since)) => (
            StatusCode::ACCEPTED,
            Json(json!({"turn": turn, "since": since})),
        )
            .into_response(),
        Err(e @ LeadError::Busy) => error(StatusCode::CONFLICT, e),
        Err(e) => error(StatusCode::UNPROCESSABLE_ENTITY, e),
    }
}

#[derive(Deserialize)]
struct EventsQuery {
    #[serde(default)]
    since: u64,
    #[serde(default)]
    wait: u64,
    #[serde(default)]
    limit: Option<usize>,
}

async fn events(State(lead): State<Lead>, Query(q): Query<EventsQuery>) -> Response {
    let limit = q.limit.unwrap_or(500).clamp(1, 1000);
    let wait = Duration::from_secs(q.wait.min(MAX_WAIT_SECS));
    Json(lead.events().wait_since(q.since, limit, wait).await).into_response()
}

async fn cancel(State(lead): State<Lead>) -> Response {
    Json(json!({"cancelled": lead.cancel().await})).into_response()
}

/// A new chat with the lead agent: `{"archived": <id or null>}`.
async fn new_chat(State(lead): State<Lead>) -> Response {
    match lead.new_chat().await {
        Ok(archived) => Json(json!({ "archived": archived })).into_response(),
        Err(e) => lead_error(e),
    }
}

/// Archived chats, newest first.
async fn past_chats(State(lead): State<Lead>) -> Response {
    Json(json!({ "chats": lead.past_chats() })).into_response()
}

/// Backend views with their display names.
fn backend_list(list: Vec<BackendView>) -> Response {
    let list: Vec<Value> = list
        .into_iter()
        .map(|b| {
            let mut v = json!(b);
            v["label"] = json!(backend_label(&b.name));
            v
        })
        .collect();
    Json(json!({ "backends": list })).into_response()
}

#[derive(Deserialize)]
struct UseBackend {
    name: String,
    #[serde(default)]
    model: Option<String>,
    /// Reasoning level (one the model offers); absent: the agent's default.
    #[serde(default)]
    effort: Option<String>,
    /// Fast mode (only for a model that offers one); absent: standard speed.
    #[serde(default)]
    fast: bool,
}

async fn backends(State(lead): State<Lead>) -> Response {
    lead.refresh_models_soon();
    backend_list(lead.backends())
}

async fn refresh_models(State(lead): State<Lead>) -> Response {
    let probes = lead.refresh_models(true).await;
    Json(json!({ "probes": probes })).into_response()
}

async fn use_backend(State(lead): State<Lead>, Json(body): Json<UseBackend>) -> Response {
    match lead.use_backend_with(
        &body.name,
        body.model.as_deref(),
        body.effort.as_deref(),
        body.fast,
    ) {
        Ok(s) => Json(s).into_response(),
        Err(e) => lead_error(e),
    }
}

async fn agent_backends(State(lead): State<Lead>, Path(agent): Path<String>) -> Response {
    match lead.agents().backends_of(&agent) {
        Ok(list) => backend_list(list),
        Err(e) => lead_error(e),
    }
}

async fn agent_use_backend(
    State(lead): State<Lead>,
    Path(agent): Path<String>,
    Json(body): Json<UseBackend>,
) -> Response {
    match lead.agents().user_use_backend_with(
        &agent,
        &body.name,
        body.model.as_deref(),
        body.effort.as_deref(),
        body.fast,
    ) {
        Ok(s) => Json(s).into_response(),
        Err(e) => lead_error(e),
    }
}

async fn status(State(lead): State<Lead>) -> Response {
    Json(lead.status()).into_response()
}

fn lead_error(e: LeadError) -> Response {
    match e {
        LeadError::Busy | LeadError::InUse(_) => error(StatusCode::CONFLICT, e),
        LeadError::NoSuchAgent(_) => error(StatusCode::NOT_FOUND, e),
        LeadError::Failed(_) => error(StatusCode::BAD_GATEWAY, e),
        _ => error(StatusCode::UNPROCESSABLE_ENTITY, e),
    }
}

async fn agents(State(lead): State<Lead>) -> Response {
    Json(json!({"agents": lead.agents().list()})).into_response()
}

async fn agent(State(lead): State<Lead>, Path(agent): Path<String>) -> Response {
    match lead.agents().get(&agent) {
        Some(b) => Json(b).into_response(),
        None => lead_error(LeadError::NoSuchAgent(agent)),
    }
}

/// Delete an agent (see [`melissa_agent::lead::agents::AgentPool::delete`]): 409 while it
/// looks after apps, 422 for the lead agent.
async fn agent_delete(State(lead): State<Lead>, Path(agent): Path<String>) -> Response {
    match lead.agents().delete(&agent, "ui").await {
        Ok(d) => Json(d).into_response(),
        Err(e) => lead_error(e),
    }
}

async fn agent_message(
    State(lead): State<Lead>,
    Path(agent): Path<String>,
    Json(msg): Json<Message>,
) -> Response {
    match lead.agents().user_send(&agent, &msg.text) {
        Ok((turn, since)) => (
            StatusCode::ACCEPTED,
            Json(json!({"turn": turn, "since": since})),
        )
            .into_response(),
        Err(e) => lead_error(e),
    }
}

async fn agent_events(
    State(lead): State<Lead>,
    Path(agent): Path<String>,
    Query(q): Query<EventsQuery>,
) -> Response {
    let events = match lead.agents().events_of(&agent) {
        Ok(e) => e,
        Err(e) => return lead_error(e),
    };
    let limit = q.limit.unwrap_or(500).clamp(1, 1000);
    let wait = Duration::from_secs(q.wait.min(MAX_WAIT_SECS));
    Json(events.wait_since(q.since, limit, wait).await).into_response()
}

async fn agent_cancel(State(lead): State<Lead>, Path(agent): Path<String>) -> Response {
    match lead.agents().user_cancel(&agent).await {
        Ok(cancelled) => Json(json!({"cancelled": cancelled})).into_response(),
        Err(e) => lead_error(e),
    }
}

async fn agent_status(State(lead): State<Lead>, Path(agent): Path<String>) -> Response {
    match lead.agents().conversation_status(&agent) {
        Ok(s) => Json(s).into_response(),
        Err(e) => lead_error(e),
    }
}

#[derive(Deserialize)]
struct Rename {
    name: String,
}

async fn agent_rename(
    State(lead): State<Lead>,
    Path(agent): Path<String>,
    Json(body): Json<Rename>,
) -> Response {
    match lead.agents().rename(&agent, &body.name) {
        Ok(b) => Json(b).into_response(),
        Err(e) => lead_error(e),
    }
}

async fn workspaces(State(lead): State<Lead>) -> Response {
    match lead.agents().workspaces().await {
        Ok(list) => Json(json!({"workspaces": list})).into_response(),
        Err(e) => lead_error(e),
    }
}

#[derive(Deserialize)]
struct Reassign {
    to: String,
}

async fn reassign(
    State(lead): State<Lead>,
    Path(workspace): Path<String>,
    Json(body): Json<Reassign>,
) -> Response {
    match lead.agents().user_reassign(&workspace, &body.to).await {
        Ok(c) => Json(c).into_response(),
        Err(e) => lead_error(e),
    }
}

async fn confirmations(State(lead): State<Lead>) -> Response {
    let list: Vec<_> = lead
        .confirmations()
        .pending()
        .into_iter()
        .map(|c| {
            json!({
                "id": c.id,
                "task": c.task,
                "tool": c.tool,
                "reason": c.reason,
                "args": c.args,
                "always": c.rule.as_ref().map(|o| json!({
                    "principal": o.key.principal,
                    "tool": o.key.tool,
                    "target": o.key.target,
                    "verb": o.verb,
                })),
                // Another agent's workspace: only the user's own yes, for this call.
                "user_only": c.user_only,
                "created": c.created.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            })
        })
        .collect();
    Json(json!({"confirmations": list})).into_response()
}

async fn resolve(State(lead): State<Lead>, Path((id, verdict)): Path<(u64, String)>) -> Response {
    let verdict = match verdict.as_str() {
        "approve" => Verdict::Approved,
        "deny" => Verdict::Denied,
        "always" => return always(&lead, id),
        _ => return error(StatusCode::NOT_FOUND, "use approve, deny or always"),
    };
    match lead.resolve_confirmation(id, verdict) {
        Some(c) => {
            Json(json!({"id": c.id, "tool": c.tool, "verdict": verdict.as_str()})).into_response()
        }
        None => error(
            StatusCode::NOT_FOUND,
            format!("no pending confirmation {id}"),
        ),
    }
}

/// "Always allow" on confirmation `id`: approve it and save its rule.
fn always(lead: &Lead, id: u64) -> Response {
    let registry = lead.server().registry();
    match registry.always_allow(id) {
        Ok(done) => Json(json!({
            "rule": {
                "id": done.rule.id,
                "principal": done.rule.key.principal,
                "tool": done.rule.key.tool,
                "target": done.rule.key.target,
                "verb": done.rule.verb,
                "description": registry.describe_rule(&done.rule),
            },
            "approved": done.approved.iter().map(|c| c.id).collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(AlwaysAllowError::Gone(_)) => error(
            StatusCode::NOT_FOUND,
            format!("no pending confirmation {id}"),
        ),
        Err(e @ AlwaysAllowError::NotOffered) => error(StatusCode::CONFLICT, e.to_string()),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}
