//! Settings → Reset: erase Melissa's data on this device and start her again unclaimed
//! (`melissa_settings::factory_reset`; docs/DECISIONS.md "Factory reset").
//!
//! Only the owner, with the password entered within 5 minutes (the gate's `sensitive()` and
//! [`require_owner_reauth`] here), on a container install, and not while an update waits to
//! install. The typed word must match. Then the server writes the request (audited), answers
//! with a "Resetting…" card that polls the public `GET /reset-status`, and tidies up in the
//! background before the root unit stops it: scheduler paused, open requests denied, its own
//! Tailscale logged out, apps deleted, agent sessions closed (each step bounded).
//!
//! - `GET /settings/reset`; `POST /ui/settings/reset` (`confirm`): htmx gets the region back.
//! - `POST /api/settings/reset` `{"confirm": "<word>"}` → 202 `{"status": "resetting"}`; 422
//!   wrong word, 409 an update waits, 404 not available here. The host CLI
//!   (`melissa-server factory-reset --confirm <word>`) uses it over the host socket.
//! - `GET /reset-status`: 204 while the reset runs; `HX-Redirect: /claim` once Melissa is back
//!   unclaimed (`/` if no reset happened).

use std::time::Duration;

use axum::{
    extract::{Form, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use melissa_agent::tools::Verdict;
use melissa_settings::{
    factory_reset::{confirm_matches, Requester, CONFIRM_WORD},
    tailscale::Management,
};
use serde::Deserialize;
use serde_json::json;

use super::pages::{self, ResetView};
use crate::{auth::require_owner_reauth, auth::Viewer, is_htmx, ui::Ui};

/// How long the server's own part may take in all (the root unit waits a little longer).
const ONLINE_BUDGET: Duration = Duration::from_secs(75);

fn requester(ui: &Ui) -> Option<&Requester> {
    ui.settings.parts().factory_reset.as_ref()
}

/// This install can reset (else the Settings list says "Not available").
pub(super) fn available(ui: &Ui) -> bool {
    requester(ui).is_some()
}

/// Why a reset can't start now.
fn blocked(ui: &Ui) -> Option<&'static str> {
    let updates = &ui.settings.parts().updates;
    let staged =
        updates.helper_installed() && updates.state().status.is_some_and(|s| s.staged.is_some());
    staged.then_some("An update is waiting. Install it first.")
}

fn view(ui: &Ui, error: Option<String>) -> ResetView {
    let r = requester(ui);
    ResetView {
        available: r.is_some(),
        resetting: r.is_some_and(Requester::pending),
        blocked: blocked(ui),
        word: CONFIRM_WORD.into(),
        error,
    }
}

pub(super) async fn page(State(ui): State<Ui>) -> Response {
    let v = view(&ui, None);
    ui.framed(|f| pages::reset_page(f, &v))
        .await
        .into_response()
}

#[derive(Deserialize)]
pub(super) struct ResetForm {
    #[serde(default)]
    confirm: String,
}

/// What a request comes to.
enum Asked {
    Started,
    Refused(StatusCode, &'static str),
}

/// Check, write the request, start the server's part.
fn ask(ui: &Ui, typed: &str) -> Asked {
    let Some(r) = requester(ui) else {
        return Asked::Refused(StatusCode::NOT_FOUND, "Not available on this install.");
    };
    if r.pending() {
        return Asked::Started;
    }
    if let Some(b) = blocked(ui) {
        return Asked::Refused(StatusCode::CONFLICT, b);
    }
    if !confirm_matches(typed, CONFIRM_WORD) {
        return Asked::Refused(StatusCode::UNPROCESSABLE_ENTITY, "That's not the word.");
    }
    if let Err(e) = r.request("ui") {
        tracing::error!("factory reset: {e}");
        return Asked::Refused(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Couldn't start the reset.",
        );
    }
    tracing::warn!("factory reset requested; Melissa stops in a moment");
    tokio::spawn(tidy_up(ui.clone(), r.clone()));
    Asked::Started
}

/// The server's part while it still runs (best effort, bounded); then the root unit goes on.
async fn tidy_up(ui: Ui, r: Requester) {
    let work = async {
        let lead = &ui.lead;
        if let Some(s) = lead.scheduler() {
            s.stop();
        }
        for c in lead.confirmations().pending() {
            lead.resolve_confirmation(c.id, Verdict::Denied);
        }
        if let Some(t) = ui.settings.parts().tailscale.as_ref() {
            if t.management() == Management::Melissa && t.enabled() {
                match tokio::time::timeout(Duration::from_secs(30), t.disable("factory-reset"))
                    .await
                {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => tracing::warn!("factory reset: Tailscale: {e}"),
                    Err(_) => tracing::warn!("factory reset: Tailscale didn't answer in time"),
                }
            }
        }
        match tokio::time::timeout(Duration::from_secs(10), lead.agents().workspaces()).await {
            Ok(Ok(list)) => {
                for w in list {
                    let gone = tokio::time::timeout(
                        Duration::from_secs(20),
                        lead.user_force_delete(&w.workspace),
                    )
                    .await;
                    if !matches!(gone, Ok(Ok(_))) {
                        tracing::warn!(app = %w.workspace, "factory reset: app not deleted (k3s is wiped anyway)");
                    }
                }
            }
            _ => tracing::warn!("factory reset: apps not listed (k3s is wiped anyway)"),
        }
        if tokio::time::timeout(Duration::from_secs(15), lead.close())
            .await
            .is_err()
        {
            tracing::warn!("factory reset: agent sessions didn't close in time");
        }
    };
    if tokio::time::timeout(ONLINE_BUDGET, work).await.is_err() {
        tracing::warn!("factory reset: the server's part ran out of time");
    }
    if let Err(e) = r.online_done() {
        tracing::error!("factory reset: {e}");
    }
}

pub(super) async fn ui_reset(
    State(ui): State<Ui>,
    headers: HeaderMap,
    viewer: Option<Extension<Viewer>>,
    Form(form): Form<ResetForm>,
) -> Response {
    if let Err(r) = require_owner_reauth(viewer.as_ref().map(|v| &v.0), &headers) {
        return *r;
    }
    let (status, error) = match ask(&ui, &form.confirm) {
        Asked::Started => (StatusCode::OK, None),
        Asked::Refused(status, why) => (status, Some(why.to_string())),
    };
    let v = view(&ui, error);
    let body = if is_htmx(&headers) {
        pages::reset_region(&v)
    } else {
        ui.framed(|f| pages::reset_page(f, &v)).await
    };
    // htmx swaps only 2xx answers: a refusal comes back as 200 with the reason in the card.
    let status = if is_htmx(&headers) {
        StatusCode::OK
    } else {
        status
    };
    (status, body).into_response()
}

#[derive(Deserialize)]
pub(super) struct ResetBody {
    #[serde(default)]
    confirm: String,
}

pub(super) async fn api_reset(
    State(ui): State<Ui>,
    headers: HeaderMap,
    viewer: Option<Extension<Viewer>>,
    Json(body): Json<ResetBody>,
) -> Response {
    if let Err(r) = require_owner_reauth(viewer.as_ref().map(|v| &v.0), &headers) {
        return *r;
    }
    match ask(&ui, &body.confirm) {
        Asked::Started => {
            (StatusCode::ACCEPTED, Json(json!({"status": "resetting"}))).into_response()
        }
        Asked::Refused(status, why) => (status, Json(json!({"error": why}))).into_response(),
    }
}

/// Polled by the "Resetting…" card (public: the session ends with the reset).
pub(crate) async fn status(State(ui): State<Ui>) -> Response {
    let to = if !ui.auth.has_owner() {
        "/claim"
    } else if requester(&ui).is_some_and(Requester::pending) {
        return StatusCode::NO_CONTENT.into_response();
    } else {
        "/"
    };
    let mut res = StatusCode::OK.into_response();
    res.headers_mut()
        .insert("hx-redirect", HeaderValue::from_static(to));
    res
}
