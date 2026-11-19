//! JSON API for the Queen chat and the confirmation inbox (no pages: the CLI uses it today,
//! the web UI later). Mutating requests pass the same same-origin check as the UI.
//!
//! - `POST /api/queen/messages` `{"text": "..."}` → 202 `{"turn": n, "since": seq}`; 409 while
//!   a turn runs; 422 for an empty or too long message.
//! - `GET /api/queen/events?since=<seq>&wait=<secs>&limit=<n>` → `{"events": [...], "next":
//!   seq, "missed": bool}`; waits up to `wait` (max 30) seconds for the first new event.
//! - `POST /api/queen/cancel` → `{"cancelled": bool}`.
//! - `GET /api/queen/status` → `{"turn": n|null, "backend": "..."|null, "last_seq": seq}`.
//! - `GET /api/queen/backends` → `{"backends": [{name, label, available, reason?, current,
//!   model?, models?: [{id, label}]}]}`: the session backends the Queen can switch to
//!   (`[queen] backends`); `models` only once the agent has reported them.
//! - `POST /api/queen/backend` `{"name": "...", "model": "..."?}` → `{backend, model?,
//!   changed, after_turn}`: switch from the next turn on (saved, audited `queen.backend`);
//!   422 unknown backend or model, or a backend that cannot run here (not installed, not
//!   logged in).
//! - `GET /api/bees/{bee}/backends`, `POST /api/bees/{bee}/backend`: the same for a bee
//!   (any session backend; saved in the bee registry, audited `bee.backend`).
//! - `GET /api/bees` → `{"bees": [...]}`: this comb's bees (id, name, backend, status, job).
//! - `GET /api/bees/{bee}` (name or id) → one bee; 404 if there is none.
//! - The user talks to a bee directly, same shapes as the Queen's: `POST
//!   /api/bees/{bee}/messages`, `GET /api/bees/{bee}/events`, `POST /api/bees/{bee}/cancel`,
//!   `GET /api/bees/{bee}/status` (409 while the bee works on a job or another message).
//! - `POST /api/bees/{bee}/rename` `{"name": "..."}` → the bee (the user renames; 422 for a
//!   bad or taken name).
//! - `GET /api/cells` → `{"cells": [...]}`: every cell and its owning bee (502 if k3s is
//!   unreachable).
//! - `POST /api/cells/{cell}/reassign` `{"to": "<bee>"}` → the cell: the user's own
//!   reassignment (no confirmation; audited `cell.reassign`, actor ui). 404 no such bee,
//!   422 no such cell.
//! - `GET /api/confirmations`; `POST /api/confirmations/{id}/approve|deny` → 200 or 404.
//! - `POST /api/tools/{name}` (JSON arguments) → 200 `{"result": ...}`: runs one tool for an
//!   operator (`melissa-queen tool --wait`) through the daemon's registry, as an unattended
//!   task (task 0): the policy applies and confirmations land in the same inbox, so the call
//!   waits for `approve`/`deny` (up to the confirmation timeout). Queen-only tools are not
//!   reachable. The caller is the user (scope `User`: no ownership checks). Errors: 404 unknown
//!   tool, 422 bad arguments, 403 denied or refused, 408 confirmation
//!   timeout, 502 tool failure.

use std::time::{Duration, UNIX_EPOCH};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use melissa_agent::{
    mcp::McpServer,
    queen::{backends::BackendView, Queen, QueenError},
    tools::{Scope, TaskContext, ToolError, Verdict},
    AuditRecord,
};
use serde::Deserialize;

use crate::feed::backend_label;
use serde_json::{json, Value};

/// Longest long-poll wait for events.
const MAX_WAIT_SECS: u64 = 30;

pub(crate) fn routes(queen: Queen) -> Router {
    Router::new()
        .route("/api/queen/messages", post(send_message))
        .route("/api/queen/events", get(events))
        .route("/api/queen/cancel", post(cancel))
        .route("/api/queen/status", get(status))
        .route("/api/queen/new-chat", post(new_chat))
        .route("/api/queen/chats", get(past_chats))
        .route("/api/queen/backends", get(backends))
        .route("/api/queen/backend", post(use_backend))
        .route("/api/bees", get(bees))
        .route("/api/bees/{bee}", get(bee))
        .route("/api/bees/{bee}/messages", post(bee_message))
        .route("/api/bees/{bee}/events", get(bee_events))
        .route("/api/bees/{bee}/cancel", post(bee_cancel))
        .route("/api/bees/{bee}/status", get(bee_status))
        .route("/api/bees/{bee}/rename", post(bee_rename))
        .route("/api/bees/{bee}/backends", get(bee_backends))
        .route("/api/bees/{bee}/backend", post(bee_use_backend))
        .route("/api/cells", get(cells))
        .route("/api/cells/{cell}/reassign", post(reassign))
        .route("/api/confirmations", get(confirmations))
        .route("/api/confirmations/{id}/{verdict}", post(resolve))
        .with_state(queen)
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
        Err(e @ ToolError::ConfirmationTimeout) => error(StatusCode::REQUEST_TIMEOUT, e),
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

async fn send_message(State(queen): State<Queen>, Json(msg): Json<Message>) -> Response {
    match queen.send(&msg.text) {
        Ok((turn, since)) => (
            StatusCode::ACCEPTED,
            Json(json!({"turn": turn, "since": since})),
        )
            .into_response(),
        Err(e @ QueenError::Busy) => error(StatusCode::CONFLICT, e),
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

async fn events(State(queen): State<Queen>, Query(q): Query<EventsQuery>) -> Response {
    let limit = q.limit.unwrap_or(500).clamp(1, 1000);
    let wait = Duration::from_secs(q.wait.min(MAX_WAIT_SECS));
    Json(queen.events().wait_since(q.since, limit, wait).await).into_response()
}

async fn cancel(State(queen): State<Queen>) -> Response {
    Json(json!({"cancelled": queen.cancel().await})).into_response()
}

/// A new chat with the Queen: `{"archived": <id or null>}`.
async fn new_chat(State(queen): State<Queen>) -> Response {
    match queen.new_chat().await {
        Ok(archived) => Json(json!({ "archived": archived })).into_response(),
        Err(e) => queen_error(e),
    }
}

/// Archived chats, newest first.
async fn past_chats(State(queen): State<Queen>) -> Response {
    Json(json!({ "chats": queen.past_chats() })).into_response()
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
}

async fn backends(State(queen): State<Queen>) -> Response {
    backend_list(queen.backends())
}

async fn use_backend(State(queen): State<Queen>, Json(body): Json<UseBackend>) -> Response {
    match queen.use_backend(&body.name, body.model.as_deref()) {
        Ok(s) => Json(s).into_response(),
        Err(e) => queen_error(e),
    }
}

async fn bee_backends(State(queen): State<Queen>, Path(bee): Path<String>) -> Response {
    match queen.bees().backends_of(&bee) {
        Ok(list) => backend_list(list),
        Err(e) => queen_error(e),
    }
}

async fn bee_use_backend(
    State(queen): State<Queen>,
    Path(bee): Path<String>,
    Json(body): Json<UseBackend>,
) -> Response {
    match queen
        .bees()
        .user_use_backend(&bee, &body.name, body.model.as_deref())
    {
        Ok(s) => Json(s).into_response(),
        Err(e) => queen_error(e),
    }
}

async fn status(State(queen): State<Queen>) -> Response {
    Json(queen.status()).into_response()
}

fn queen_error(e: QueenError) -> Response {
    match e {
        QueenError::Busy => error(StatusCode::CONFLICT, e),
        QueenError::NoSuchBee(_) => error(StatusCode::NOT_FOUND, e),
        QueenError::Failed(_) => error(StatusCode::BAD_GATEWAY, e),
        _ => error(StatusCode::UNPROCESSABLE_ENTITY, e),
    }
}

async fn bees(State(queen): State<Queen>) -> Response {
    Json(json!({"bees": queen.bees().list()})).into_response()
}

async fn bee(State(queen): State<Queen>, Path(bee): Path<String>) -> Response {
    match queen.bees().get(&bee) {
        Some(b) => Json(b).into_response(),
        None => queen_error(QueenError::NoSuchBee(bee)),
    }
}

async fn bee_message(
    State(queen): State<Queen>,
    Path(bee): Path<String>,
    Json(msg): Json<Message>,
) -> Response {
    match queen.bees().user_send(&bee, &msg.text) {
        Ok((turn, since)) => (
            StatusCode::ACCEPTED,
            Json(json!({"turn": turn, "since": since})),
        )
            .into_response(),
        Err(e) => queen_error(e),
    }
}

async fn bee_events(
    State(queen): State<Queen>,
    Path(bee): Path<String>,
    Query(q): Query<EventsQuery>,
) -> Response {
    let events = match queen.bees().events_of(&bee) {
        Ok(e) => e,
        Err(e) => return queen_error(e),
    };
    let limit = q.limit.unwrap_or(500).clamp(1, 1000);
    let wait = Duration::from_secs(q.wait.min(MAX_WAIT_SECS));
    Json(events.wait_since(q.since, limit, wait).await).into_response()
}

async fn bee_cancel(State(queen): State<Queen>, Path(bee): Path<String>) -> Response {
    match queen.bees().user_cancel(&bee).await {
        Ok(cancelled) => Json(json!({"cancelled": cancelled})).into_response(),
        Err(e) => queen_error(e),
    }
}

async fn bee_status(State(queen): State<Queen>, Path(bee): Path<String>) -> Response {
    match queen.bees().conversation_status(&bee) {
        Ok(s) => Json(s).into_response(),
        Err(e) => queen_error(e),
    }
}

#[derive(Deserialize)]
struct Rename {
    name: String,
}

async fn bee_rename(
    State(queen): State<Queen>,
    Path(bee): Path<String>,
    Json(body): Json<Rename>,
) -> Response {
    match queen.bees().rename(&bee, &body.name) {
        Ok(b) => Json(b).into_response(),
        Err(e) => queen_error(e),
    }
}

async fn cells(State(queen): State<Queen>) -> Response {
    match queen.bees().cells().await {
        Ok(list) => Json(json!({"cells": list})).into_response(),
        Err(e) => queen_error(e),
    }
}

#[derive(Deserialize)]
struct Reassign {
    to: String,
}

async fn reassign(
    State(queen): State<Queen>,
    Path(cell): Path<String>,
    Json(body): Json<Reassign>,
) -> Response {
    match queen.bees().user_reassign(&cell, &body.to).await {
        Ok(c) => Json(c).into_response(),
        Err(e) => queen_error(e),
    }
}

async fn confirmations(State(queen): State<Queen>) -> Response {
    let list: Vec<_> = queen
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
                "created": c.created.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            })
        })
        .collect();
    Json(json!({"confirmations": list})).into_response()
}

async fn resolve(State(queen): State<Queen>, Path((id, verdict)): Path<(u64, String)>) -> Response {
    let verdict = match verdict.as_str() {
        "approve" => Verdict::Approved,
        "deny" => Verdict::Denied,
        _ => return error(StatusCode::NOT_FOUND, "use approve or deny"),
    };
    match queen.resolve_confirmation(id, verdict) {
        Some(c) => {
            Json(json!({"id": c.id, "tool": c.tool, "verdict": verdict.as_str()})).into_response()
        }
        None => error(
            StatusCode::NOT_FOUND,
            format!("no pending confirmation {id}"),
        ),
    }
}
