//! JSON API for the Queen chat and the confirmation inbox (no pages: the CLI uses it today,
//! the web UI later). Mutating requests pass the same same-origin check as the UI.
//!
//! - `POST /api/queen/messages` `{"text": "..."}` → 202 `{"turn": n, "since": seq}`; 409 while
//!   a turn runs; 422 for an empty or too long message.
//! - `GET /api/queen/events?since=<seq>&wait=<secs>&limit=<n>` → `{"events": [...], "next":
//!   seq, "missed": bool}`; waits up to `wait` (max 30) seconds for the first new event.
//! - `POST /api/queen/cancel` → `{"cancelled": bool}`.
//! - `GET /api/queen/status` → `{"turn": n|null, "backend": "..."|null, "last_seq": seq}`.
//! - `GET /api/workers` → the Queen's workers, newest first.
//! - `GET /api/confirmations`; `POST /api/confirmations/{id}/approve|deny` → 200 or 404.
//! - `POST /api/tools/{name}` (JSON arguments) → 200 `{"result": ...}`: runs one tool for an
//!   operator (`melissa-queen tool --wait`) through the daemon's registry, as an unattended
//!   task (task 0): the policy applies and confirmations land in the same inbox, so the call
//!   waits for `approve`/`deny` (up to the confirmation timeout). Queen-only tools are not
//!   reachable. The caller is the owner (scope `Owner`: no ownership checks). Errors: 404 unknown
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
    queen::{Queen, QueenError},
    tools::{Scope, TaskContext, ToolError, Verdict},
    AuditRecord,
};
use serde::Deserialize;
use serde_json::{json, Value};

/// Longest long-poll wait for events.
const MAX_WAIT_SECS: u64 = 30;

pub(crate) fn routes(queen: Queen) -> Router {
    Router::new()
        .route("/api/queen/messages", post(send_message))
        .route("/api/queen/events", get(events))
        .route("/api/queen/cancel", post(cancel))
        .route("/api/queen/status", get(status))
        .route("/api/workers", get(workers))
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
    // Unattended, untainted, no delegation; the caller is the owner (local API), so the
    // scope is theirs: no ownership checks, but the label policy still asks for confirmations.
    let mut ctx = TaskContext {
        scope: Scope::Owner,
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

async fn status(State(queen): State<Queen>) -> Response {
    Json(queen.status()).into_response()
}

async fn workers(State(queen): State<Queen>) -> Response {
    let list: Vec<_> = queen
        .workers()
        .into_iter()
        .map(|t| {
            json!({
                "worker": t.id,
                "backend": t.backend,
                "status": t.status.as_str(),
                "elapsed_secs": t.elapsed.as_secs(),
                "goal": queen.worker_goal(t.id),
                "error": t.error,
            })
        })
        .collect();
    Json(json!({"workers": list})).into_response()
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
