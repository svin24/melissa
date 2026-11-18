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

use std::time::{Duration, UNIX_EPOCH};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use melissa_agent::{
    queen::{Queen, QueenError},
    tools::Verdict,
};
use serde::Deserialize;
use serde_json::json;

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
