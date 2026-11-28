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
//!
//! Under it, while connected, "Open Melissa from your other devices" (MVP #1; owner's Penpot
//! draft, 2026-11-28; `melissa_settings::tailscale::serve`): Off [Turn on] opens a form in
//! place (the name on the tailnet, with the certificate log notice, and the password again);
//! the tailnet has HTTPS off [Allow HTTPS in Tailscale] [Cancel] (the region polls, 204 until
//! it moves on); Turning on… (polls); On [Turn off] with the address and Copy. Turn off has no
//! confirmation (owner). When the computer runs its own Tailscale, the row shows the command
//! to run there instead (owner-approved exception to "no host commands").

use axum::{
    extract::{Extension, Form, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
};
use melissa_auth::AuthError;
use melissa_settings::tailscale::{
    valid_node_name, Login, Management, Serve, State as TsState, TailscaleControl, TailscaleError,
};
use serde::Deserialize;

use super::{
    pages::{self, Flash, ServeRow, TailscaleView, TAILSCALE_URL},
    setup::from_setup,
};
use crate::{
    auth::{
        pages::{blocking, say, src, Source},
        session_cookie, Viewer,
    },
    components::Tone,
    is_htmx,
    setup_password::{from_this_page, no_store},
    ui::Ui,
};

const REGION_URL: &str = "/ui/settings/tailscale";
/// Polled while a sign-in page is open: 204 (no swap) until the state moves on.
const WAITING_URL: &str = "/ui/settings/tailscale?waiting=1";
/// Polled while the tailnet has HTTPS off: 204 until it moves on.
const HTTPS_URL: &str = "/ui/settings/tailscale?https=1";
/// Polled while turning on: the answer that finds it on says so once.
const TURNING_URL: &str = "/ui/settings/tailscale?turning=1";
const TURNED_ON: &str = "Turned on. Melissa opens from your other devices.";

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
        details: None,
        compact: false,
        serve: None,
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

/// The address the form shows for the current name (`https://<MagicDNS name>`).
fn address(state: &TsState) -> Option<String> {
    match state {
        TsState::Connected(n) => n.dns_name.as_ref().map(|d| format!("https://{d}")),
        _ => None,
    }
}

/// What the HTTPS row should show. `fresh`: the answer to Turn on itself (the line says what
/// to do; later renders say Melissa waits).
async fn serve_row(c: &TailscaleControl, v: &mut TailscaleView, fresh: bool) -> Option<Serve> {
    if c.management() == Management::Host {
        v.serve = Some(ServeRow::Host {
            command: format!("sudo tailscale serve --bg http://127.0.0.1:{}", c.ui_port()),
        });
        return None;
    }
    let failure = c.take_serve_failure();
    let s = c.serve_state().await;
    if let Some(f) = &failure {
        v.flash = Some(Flash {
            text: "Couldn't turn it on: Tailscale refused it. Try again in a few minutes.".into(),
            tone: Tone::Caution,
        });
        v.details = Some(f.detail.clone());
    }
    v.serve = match &s {
        Serve::Unavailable => None,
        Serve::Off => Some(ServeRow::Off {
            failed: failure.is_some(),
        }),
        Serve::NeedsHttps(url) => {
            v.poll = v.poll.or(Some(HTTPS_URL));
            Some(ServeRow::NeedsHttps {
                url: url.as_ref().map(|u| u.expose().to_string()),
                waiting: !fresh,
            })
        }
        Serve::TurningOn => {
            v.poll = v.poll.or(Some(TURNING_URL));
            Some(ServeRow::TurningOn)
        }
        Serve::On { url } => Some(ServeRow::On { url: url.clone() }),
    };
    Some(s)
}

/// The whole view: Tailscale's row, and the HTTPS row while connected.
async fn full_view(ui: &Ui, flash: Option<Flash>) -> TailscaleView {
    let mut v = view(&state(ui).await, flash);
    if let Some(c) = control(ui) {
        serve_row(&c, &mut v, false).await;
    }
    v
}

#[derive(Deserialize, Default)]
pub(super) struct PageQuery {
    #[serde(default)]
    serve: Option<String>,
}

pub(super) async fn page(State(ui): State<Ui>, Query(q): Query<PageQuery>) -> Response {
    let mut v = full_view(&ui, None).await;
    // Turn on: the form in place of the row (without script too).
    if q.serve.is_some() && matches!(v.serve, Some(ServeRow::Off { .. })) {
        if let Some(c) = control(&ui) {
            v.serve = Some(ServeRow::Form {
                name: c.node_name(),
                address: address(&c.state().await),
                name_error: None,
                password_error: None,
            });
        }
    }
    let body = ui.framed(|f| pages::tailscale_page(f, &v)).await;
    // It may carry the page that allows HTTPS.
    no_store(StatusCode::OK, body)
}

#[derive(Deserialize)]
pub(super) struct RegionQuery {
    #[serde(default)]
    waiting: Option<String>,
    #[serde(default)]
    https: Option<String>,
    #[serde(default)]
    turning: Option<String>,
}

/// The region (htmx polling). `waiting`: a sign-in page is open in this browser; answer 204
/// until the sign-in is done, so the open button stays. `https`: the same while the page that
/// allows HTTPS is open. `turning`: say once that it is on.
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
    if let (Some(c), false) = (control(&ui), v.compact) {
        if q.https.is_some() && matches!(c.serve_state().await, Serve::NeedsHttps(_)) {
            return StatusCode::NO_CONTENT.into_response();
        }
        let now = serve_row(&c, &mut v, false).await;
        if q.turning.is_some() && matches!(now, Some(Serve::On { .. })) && v.flash.is_none() {
            v.flash = Some(Flash {
                text: TURNED_ON.into(),
                tone: Tone::Ok,
            });
        }
    }
    no_store(StatusCode::OK, pages::tailscale_region(&v))
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

const BAD_NAME: &str = "Use letters, digits and dashes.";

fn flash_for(e: &TailscaleError) -> Flash {
    let text = match e {
        TailscaleError::NotManaged => "Tailscale is not managed by Melissa on this system.",
        TailscaleError::Not(_) => "Not possible right now.",
        TailscaleError::BadName => BAD_NAME,
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
    let compact = from_setup(headers);
    let mut v = if compact {
        view(&state(ui).await, flash)
    } else {
        full_view(ui, flash).await
    };
    v.compact = compact;
    if is_htmx(headers) {
        return no_store(StatusCode::OK, pages::tailscale_region(&v));
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

#[derive(Deserialize)]
pub(super) struct ServeIn {
    #[serde(default)]
    name: String,
    #[serde(default)]
    password: String,
}

/// Who turned it on or off, for the audit: the signed-in person ("ui" without a session).
fn actor(viewer: Option<&Extension<Viewer>>) -> String {
    viewer.map_or_else(|| "ui".to_string(), |Extension(v)| v.name().to_string())
}

/// Turn on: the name (renamed first when it differs) and the password again, checked here
/// (it counts like any sign-in try and refreshes the 5-minute window).
pub(super) async fn serve_on(
    State(ui): State<Ui>,
    viewer: Option<Extension<Viewer>>,
    source: Source,
    headers: HeaderMap,
    Form(form): Form<ServeIn>,
) -> Response {
    if !from_this_page(&headers) {
        return no_store(StatusCode::FORBIDDEN, "Not from this page.");
    }
    let Some(c) = control(&ui) else {
        return no_store(StatusCode::NOT_FOUND, "Not available on this device.");
    };
    let form_again = |name_error: Option<String>, password_error: Option<String>| {
        let c = c.clone();
        let name = form.name.clone();
        async move {
            ServeRow::Form {
                name,
                address: address(&c.state().await),
                name_error,
                password_error,
            }
        }
    };
    let mut v = view(&c.state().await, None);
    let name = match valid_node_name(&form.name) {
        Some(n) => n,
        None => {
            v.serve = Some(form_again(Some(BAD_NAME.into()), None).await);
            return serve_answer(&ui, &headers, v).await;
        }
    };
    let checked = match session_cookie(&headers) {
        Some(token) => {
            let (auth, src, pw) = (ui.auth.clone(), src(&source), form.password.clone());
            blocking(move || auth.reauth(&token, &pw, &src)).await
        }
        None => Err(AuthError::WrongCredentials),
    };
    if let Err(e) = checked {
        let text = match e {
            AuthError::WrongCredentials => "That password is not right.".to_string(),
            e => say(&e),
        };
        v.serve = Some(form_again(None, Some(text)).await);
        return serve_answer(&ui, &headers, v).await;
    }
    match c.serve_on(&actor(viewer.as_ref()), Some(&name)).await {
        Ok(s) => {
            v = view(&c.state().await, None);
            serve_row(&c, &mut v, true).await;
            if matches!(s, Serve::On { .. }) && v.flash.is_none() {
                v.flash = Some(Flash {
                    text: TURNED_ON.into(),
                    tone: Tone::Ok,
                });
            }
        }
        Err(TailscaleError::BadName) => {
            v.serve = Some(form_again(Some(BAD_NAME.into()), None).await);
        }
        Err(e) => {
            tracing::warn!("tailscale serve: {e}");
            v = view(&c.state().await, Some(flash_for(&e)));
            serve_row(&c, &mut v, true).await;
        }
    }
    serve_answer(&ui, &headers, v).await
}

/// The region (htmx) or the whole page; never cached (it may carry the page that allows
/// HTTPS).
async fn serve_answer(ui: &Ui, headers: &HeaderMap, v: TailscaleView) -> Response {
    let body = if is_htmx(headers) {
        pages::tailscale_region(&v)
    } else {
        ui.framed(|f| pages::tailscale_page(f, &v)).await
    };
    no_store(StatusCode::OK, body)
}

/// Turn off (no confirmation: turning it on again is easy).
pub(super) async fn serve_off(
    State(ui): State<Ui>,
    viewer: Option<Extension<Viewer>>,
    headers: HeaderMap,
) -> Response {
    let Some(c) = control(&ui) else {
        return (StatusCode::NOT_FOUND, "Not available on this device.").into_response();
    };
    let flash = match c.serve_off(&actor(viewer.as_ref())).await {
        Ok(_) => None,
        Err(e) => {
            tracing::warn!("tailscale serve off: {e}");
            Some(flash_for(&e))
        }
    };
    answer(&ui, &headers, flash).await
}

/// Stop waiting for HTTPS to be allowed.
pub(super) async fn serve_cancel(State(ui): State<Ui>, headers: HeaderMap) -> Response {
    if let Some(c) = control(&ui) {
        c.serve_cancel();
    }
    answer(&ui, &headers, None).await
}
