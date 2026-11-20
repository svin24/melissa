//! Settings → Agents: Melissa (the lead agent) and her helpers, what each runs on, and Delete
//! for a helper (in-page confirmation). Deleting is the agent layer's
//! [`melissa_agent::lead::agents::AgentPool::delete`]: refused while the helper looks after
//! apps ("Reassign or remove its apps first"), otherwise its chat, file box and schedules go
//! with it; audited `agent.delete`, actor `ui`. Melissa herself can't be deleted.

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};
use melissa_agent::lead::{backends::BackendView, LeadError};

use super::pages::{self, AgentRow, Flash, AGENTS_URL};
use crate::{agents::picker, components::Tone, feed::backend_label, is_htmx, ui::Ui};

/// "Claude · Opus 5.5 · High · Fast": the backend, and the model, level and speed when
/// chosen.
pub(crate) fn runs_on(views: &[BackendView], fallback: &str) -> String {
    let Some(current) = views.iter().find(|v| v.current) else {
        return backend_label(fallback).to_string();
    };
    let backend = backend_label(&current.name);
    let p = picker(views, String::new(), None, None);
    let chip = if p.fast {
        format!("{} · Fast", p.chip)
    } else {
        p.chip
    };
    if chip == backend {
        chip
    } else {
        format!("{backend} · {chip}")
    }
}

pub(crate) fn status_words(status: &str) -> (&'static str, Tone) {
    match status {
        "working" => ("Working", Tone::Info),
        "stopped" => ("Stopped", Tone::Muted),
        _ => ("Idle", Tone::Ok),
    }
}

fn rows(ui: &Ui) -> Vec<AgentRow> {
    let lead_views = ui.lead.backends();
    let mut rows = vec![AgentRow {
        id: None,
        name: "Melissa".into(),
        href: "/".into(),
        runs_on: runs_on(&lead_views, ""),
        status: status_words(if ui.lead.status().turn.is_some() {
            "working"
        } else {
            "idle"
        }),
    }];
    let pool = ui.lead.agents();
    rows.extend(pool.list().into_iter().map(|a| {
        let views = pool.backends_of(&a.id).unwrap_or_default();
        AgentRow {
            href: crate::pages::agent_href(&a.id),
            runs_on: runs_on(&views, &a.backend),
            status: status_words(a.status),
            name: a.name,
            id: Some(a.id),
        }
    }));
    rows
}

pub(super) async fn page(State(ui): State<Ui>) -> Response {
    let rows = rows(&ui);
    ui.framed(|f| pages::agents_page(f, &rows, None))
        .await
        .into_response()
}

pub(super) async fn delete(
    State(ui): State<Ui>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let flash = match ui.lead.agents().delete(&id, "ui").await {
        Ok(d) => Flash {
            text: format!("Deleted {}.", d.name),
            tone: Tone::Ok,
        },
        Err(LeadError::NoSuchAgent(_)) => Flash {
            text: "That helper is already gone.".into(),
            tone: Tone::Muted,
        },
        Err(e @ (LeadError::InUse(_) | LeadError::Invalid(_))) => Flash {
            text: e.to_string(),
            tone: Tone::Error,
        },
        Err(e) => Flash {
            text: format!("Couldn't delete it: {e}"),
            tone: Tone::Error,
        },
    };
    let rows = rows(&ui);
    if is_htmx(&headers) {
        return pages::agents_region(&rows, Some(&flash)).into_response();
    }
    if flash.tone == Tone::Error {
        return ui
            .framed(|f| pages::agents_page(f, &rows, Some(&flash)))
            .await
            .into_response();
    }
    Redirect::to(AGENTS_URL).into_response()
}
