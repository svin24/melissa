//! Settings → Permissions (the owner's "Always allow" rules) and No permission prompts
//! (Settings → Developer). Both are the owner's alone: agents have no tool for them, and every
//! change is audited by the registry (`permissions.*`).

use axum::{
    extract::{Form, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;

use super::pages::{self, Flash, RuleRow, DEVELOPER_URL, PERMISSIONS_URL};
use crate::{components::Tone, feed::rule_words, is_htmx, ui::Ui};

fn rows(ui: &Ui) -> Vec<RuleRow> {
    ui.registry()
        .permissions()
        .rules()
        .into_iter()
        .map(|r| {
            let (who, what) = rule_words(&r.key.principal, &r.verb, r.key.target.as_deref(), ui);
            RuleRow {
                id: r.id,
                who,
                what,
                tool: r.key.tool,
                created: r.created,
            }
        })
        .collect()
}

pub(super) async fn page(State(ui): State<Ui>) -> Response {
    let rows = rows(&ui);
    ui.framed(|f| pages::permissions_page(f, &rows, None))
        .await
        .into_response()
}

pub(super) async fn remove(
    State(ui): State<Ui>,
    Path(id): Path<u64>,
    headers: HeaderMap,
) -> Response {
    let flash = match ui.registry().remove_rule(id) {
        Ok(Some(_)) => Flash {
            text: "Removed. Melissa asks you again next time.".into(),
            tone: Tone::Ok,
        },
        Ok(None) => Flash {
            text: "That rule is already gone.".into(),
            tone: Tone::Muted,
        },
        Err(e) => Flash {
            text: format!("Couldn't remove it: {e}"),
            tone: Tone::Error,
        },
    };
    if !is_htmx(&headers) {
        return Redirect::to(PERMISSIONS_URL).into_response();
    }
    pages::permissions_region(&rows(&ui), Some(&flash)).into_response()
}

#[derive(Deserialize, Default)]
pub(super) struct BypassForm {
    #[serde(default)]
    enabled: String,
}

/// The switch: on asks first (the confirmation card), off applies at once.
pub(super) async fn bypass(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Form(form): Form<BypassForm>,
) -> Response {
    if form.enabled == "on" {
        if !is_htmx(&headers) {
            return Redirect::to(DEVELOPER_URL).into_response();
        }
        let on = ui.registry().permissions().bypass();
        return pages::bypass_region(on, !on, None).into_response();
    }
    let flash = ui.registry().set_bypass(false).err().map(|e| Flash {
        text: format!("Couldn't save that: {e}"),
        tone: Tone::Error,
    });
    bypass_reply(&ui, &headers, flash)
}

#[derive(Deserialize, Default)]
pub(super) struct ConfirmForm {
    #[serde(default)]
    confirm: String,
}

/// The confirmation card's yes: prompts go off, and whatever waits now is allowed.
pub(super) async fn bypass_on(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Form(form): Form<ConfirmForm>,
) -> Response {
    let flash = if form.confirm == "yes" {
        match ui.registry().set_bypass(true) {
            Ok(approved) if approved.is_empty() => None,
            Ok(approved) => Some(Flash {
                text: format!("Allowed {} waiting request(s).", approved.len()),
                tone: Tone::Info,
            }),
            Err(e) => Some(Flash {
                text: format!("Couldn't save that: {e}"),
                tone: Tone::Error,
            }),
        }
    } else {
        None
    };
    bypass_reply(&ui, &headers, flash)
}

fn bypass_reply(ui: &Ui, headers: &HeaderMap, flash: Option<Flash>) -> Response {
    if !is_htmx(headers) {
        return Redirect::to(DEVELOPER_URL).into_response();
    }
    pages::bypass_region(ui.registry().permissions().bypass(), false, flash.as_ref())
        .into_response()
}

// --- API ---------------------------------------------------------------------------------------

fn rule_json(ui: &Ui, r: &melissa_agent::tools::Rule) -> serde_json::Value {
    json!({
        "id": r.id,
        "principal": r.key.principal,
        "tool": r.key.tool,
        "target": r.key.target,
        "verb": r.verb,
        "created": r.created,
        "description": ui.registry().describe_rule(r),
    })
}

pub(super) async fn api_list(State(ui): State<Ui>) -> Response {
    let p = ui.registry().permissions();
    let rules: Vec<_> = p.rules().iter().map(|r| rule_json(&ui, r)).collect();
    Json(json!({"rules": rules, "bypass": p.bypass()})).into_response()
}

pub(super) async fn api_remove(State(ui): State<Ui>, Path(id): Path<u64>) -> Response {
    match ui.registry().remove_rule(id) {
        Ok(Some(r)) => Json(rule_json(&ui, &r)).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("no rule {id}")})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct ApiBypass {
    on: bool,
    /// Needed to switch it on.
    #[serde(default)]
    confirm: bool,
}

pub(super) async fn api_bypass(State(ui): State<Ui>, Json(body): Json<ApiBypass>) -> Response {
    if body.on && !body.confirm {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": "switching permission prompts off needs \"confirm\": true"})),
        )
            .into_response();
    }
    match ui.registry().set_bypass(body.on) {
        Ok(approved) => Json(json!({
            "bypass": body.on,
            "approved": approved.iter().map(|c| c.id).collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}
