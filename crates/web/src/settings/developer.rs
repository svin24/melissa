//! Settings → Developer: the chat display preference (thinking and step timings). The page
//! itself (with the agent tasks) is `pages::dashboard`.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Form,
};

use super::pages::{self, DEVELOPER_URL};
use crate::{is_htmx, ui::Ui};

/// `POST /ui/settings/developer/steps` (`enabled=on` or nothing).
pub(super) async fn steps(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Form(form): Form<Vec<(String, String)>>,
) -> Response {
    let on = form.iter().any(|(k, v)| k == "enabled" && v == "on");
    let s = &ui.settings;
    let result = s.parts().prefs.update(|p| p.show_steps = on);
    let words = if on { "on" } else { "off" };
    match &result {
        Ok(_) => s.audit("settings.ui", "ui", &format!("show_steps: {words}"), None),
        Err(e) => tracing::warn!("cannot save the UI preferences: {e}"),
    }
    if !is_htmx(&headers) {
        return match result {
            Ok(_) => Redirect::to(DEVELOPER_URL).into_response(),
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Couldn't save.").into_response(),
        };
    }
    let saved = s.prefs().show_steps;
    let error = result.is_err().then_some("Couldn't save.");
    pages::chat_prefs_region(saved, error).into_response()
}
