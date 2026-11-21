//! Settings → Developer: the chat preferences (thinking and step timings, Enter while an agent
//! works). The page
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
    let error = result.is_err().then_some("Couldn't save.");
    pages::chat_prefs_region(s.prefs(), error).into_response()
}

/// `POST /ui/settings/developer/enter` (`enter=queue` or `enter=steer`): what Enter does in
/// the composer while an agent works.
pub(super) async fn enter(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Form(form): Form<Vec<(String, String)>>,
) -> Response {
    let steers = match form
        .iter()
        .find(|(k, _)| k == "enter")
        .map(|(_, v)| v.as_str())
    {
        Some("steer") => true,
        Some("queue") => false,
        _ => return (StatusCode::UNPROCESSABLE_ENTITY, "Pick Queues or Steers.").into_response(),
    };
    let s = &ui.settings;
    let result = s.parts().prefs.update(|p| p.enter_steers = steers);
    let words = if steers { "steer" } else { "queue" };
    match &result {
        Ok(_) => s.audit(
            "settings.ui",
            "ui",
            &format!("enter_while_working: {words}"),
            None,
        ),
        Err(e) => tracing::warn!("cannot save the UI preferences: {e}"),
    }
    if !is_htmx(&headers) {
        return match result {
            Ok(_) => Redirect::to(DEVELOPER_URL).into_response(),
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Couldn't save.").into_response(),
        };
    }
    let error = result.is_err().then_some("Couldn't save.");
    pages::chat_prefs_region(s.prefs(), error).into_response()
}
