//! Settings → Tailscale: Melissa's own Tailscale, off by default
//! (`melissa_settings::tailscale`). States: Not managed by Melissa | Off [Enable] | Starting |
//! Sign in needed [Sign in] | Waiting for approval | Connected · name · address [Disable] |
//! Error (one line).
//!
//! Sign in answers with the sign-in page's URL for this browser only: positive same-origin
//! evidence required (like the setup password), `Cache-Control: no-store`, never in a URL, the
//! page's first HTML, a log or anything an agent sees. Audited `tailscale.login_started`.
//! Disable asks once in the page (it logs the node out). Enable and Disable are audited
//! `settings.tailscale`, actor `ui`.

use axum::{
    extract::{Form, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
};
use melissa_settings::tailscale::{Login, State as TsState, TailscaleControl, TailscaleError};
use serde::Deserialize;

use super::{
    pages::{self, Flash, TailscaleView, TAILSCALE_URL},
    setup::from_setup,
};
use crate::{
    components::Tone,
    is_htmx,
    setup_password::{from_this_page, no_store},
    ui::Ui,
};

const REGION_URL: &str = "/ui/settings/tailscale";
/// Polled while a sign-in page is open: 204 (no swap) until the state moves on.
const WAITING_URL: &str = "/ui/settings/tailscale?waiting=1";

fn control(ui: &Ui) -> Option<TailscaleControl> {
    ui.settings.parts().tailscale.clone()
}

async fn state(ui: &Ui) -> TsState {
    match control(ui) {
        Some(c) => c.state().await,
        None => TsState::NotManaged,
    }
}

/// The page's view of a state.
fn view(state: &TsState, flash: Option<Flash>) -> TailscaleView {
    let mut v = TailscaleView {
        sentence: state.sentence(),
        enable: None,
        sign_in: false,
        disable: false,
        login_url: None,
        poll: None,
        flash,
        compact: false,
    };
    match state {
        TsState::NotManaged => {}
        TsState::Off => v.enable = Some("Enable"),
        TsState::Starting => v.poll = Some(REGION_URL),
        TsState::SignInNeeded => {
            v.sign_in = true;
            v.disable = true;
        }
        TsState::NeedsApproval => {
            v.disable = true;
            v.poll = Some(REGION_URL);
        }
        TsState::Connected(_) => v.disable = true,
        TsState::Error(_) => {
            v.enable = Some("Restart Tailscale");
            v.disable = true;
        }
    }
    v
}

pub(super) async fn page(State(ui): State<Ui>) -> Response {
    let v = view(&state(&ui).await, None);
    ui.framed(|f| pages::tailscale_page(f, &v))
        .await
        .into_response()
}

#[derive(Deserialize)]
pub(super) struct RegionQuery {
    #[serde(default)]
    waiting: Option<String>,
}

/// The region (htmx polling). `waiting`: a sign-in page is open in this browser; answer 204
/// until the sign-in is done, so the open button stays.
pub(super) async fn region(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Query(q): Query<RegionQuery>,
) -> Response {
    let s = state(&ui).await;
    if q.waiting.is_some() && s == TsState::SignInNeeded {
        return StatusCode::NO_CONTENT.into_response();
    }
    let mut v = view(&s, None);
    v.compact = from_setup(&headers);
    pages::tailscale_region(&v).into_response()
}

/// The setup page's view of Tailscale (first-run setup, Extras).
pub(super) async fn setup_view(ui: &Ui) -> TailscaleView {
    let mut v = view(&state(ui).await, None);
    v.compact = true;
    v
}

/// Whether Melissa manages Tailscale on this system at all (setup's Extras leaves it out
/// when not).
pub(super) async fn managed(ui: &Ui) -> bool {
    !matches!(state(ui).await, TsState::NotManaged)
}

/// Whether Melissa's own Tailscale is on (setup's Extras counts as done then).
pub(super) async fn is_on(ui: &Ui) -> bool {
    !matches!(state(ui).await, TsState::NotManaged | TsState::Off)
}

fn flash_for(e: &TailscaleError) -> Flash {
    let text = match e {
        TailscaleError::NotManaged => "Tailscale is not managed by Melissa on this system.",
        TailscaleError::Not(_) => "Not possible right now.",
        TailscaleError::Save(_) => "Couldn't save that.",
        TailscaleError::Failed(_) => "Couldn't reach Tailscale. Try again.",
    };
    Flash {
        text: text.into(),
        tone: Tone::Caution,
    }
}

async fn answer(ui: &Ui, headers: &HeaderMap, flash: Option<Flash>) -> Response {
    let failed = flash.is_some();
    let mut v = view(&state(ui).await, flash);
    v.compact = from_setup(headers);
    if is_htmx(headers) {
        return pages::tailscale_region(&v).into_response();
    }
    if v.compact && !failed {
        return Redirect::to(super::setup::EXTRAS_URL).into_response();
    }
    if failed {
        return ui
            .framed(|f| pages::tailscale_page(f, &v))
            .await
            .into_response();
    }
    Redirect::to(TAILSCALE_URL).into_response()
}

pub(super) async fn enable(State(ui): State<Ui>, headers: HeaderMap) -> Response {
    let Some(c) = control(&ui) else {
        return (StatusCode::NOT_FOUND, "Not available on this device.").into_response();
    };
    let flash = match c.enable("ui").await {
        Ok(_) => None,
        Err(e) => {
            tracing::warn!("tailscale enable: {e}");
            Some(flash_for(&e))
        }
    };
    answer(&ui, &headers, flash).await
}

pub(super) async fn disable(State(ui): State<Ui>, headers: HeaderMap) -> Response {
    let Some(c) = control(&ui) else {
        return (StatusCode::NOT_FOUND, "Not available on this device.").into_response();
    };
    let flash = match c.disable("ui").await {
        Ok(_) => None,
        Err(e) => {
            tracing::warn!("tailscale disable: {e}");
            Some(flash_for(&e))
        }
    };
    answer(&ui, &headers, flash).await
}

/// The switch in first-run setup (`enabled=on` or nothing): Enable or Disable.
pub(super) async fn switch(
    state: State<Ui>,
    headers: HeaderMap,
    Form(form): Form<Vec<(String, String)>>,
) -> Response {
    if form.iter().any(|(k, v)| k == "enabled" && v == "on") {
        enable(state, headers).await
    } else {
        disable(state, headers).await
    }
}

/// Start a sign-in and give this browser the sign-in page (never logged).
pub(super) async fn sign_in(State(ui): State<Ui>, headers: HeaderMap) -> Response {
    if !from_this_page(&headers) {
        return no_store(StatusCode::FORBIDDEN, "Not from this page.");
    }
    let Some(c) = control(&ui) else {
        return no_store(StatusCode::NOT_FOUND, "Not available on this device.");
    };
    let (url, flash) = match c.start_login("ui").await {
        Ok(Login::Url(url)) => (Some(url), None),
        Ok(Login::Connected) => (None, None),
        Err(e) => {
            tracing::warn!("tailscale sign-in: {e}");
            (None, Some(flash_for(&e)))
        }
    };
    let mut v = view(&c.state().await, flash);
    v.compact = from_setup(&headers);
    if let Some(url) = url {
        v.login_url = Some(url.expose().to_string());
        v.sign_in = false;
        v.poll = Some(WAITING_URL);
    }
    let body = if is_htmx(&headers) {
        pages::tailscale_region(&v)
    } else {
        ui.framed(|f| pages::tailscale_page(f, &v)).await
    };
    no_store(StatusCode::OK, body)
}
