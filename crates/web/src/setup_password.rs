//! The setup password card's routes (owner design 2026-11-21; docs/DECISIONS.md).
//!
//! An install that generates an app's password puts a card in the chat
//! ([`melissa_agent::lead::LeadEvent::SetupPassword`]: the app and login name only). The card
//! asks the server for everything else, between the server and the owner's browser only:
//!
//! - `GET /ui/apps/{app}/setup-password[?confirm=1]`: the card in its current state (masked,
//!   dismissed, gone); `confirm` asks "Forget it?". Reads no password.
//! - `POST /ui/apps/{app}/setup-password/reveal`: the password, for Show (htmx: the card with
//!   the password in place) or Copy (`Accept: text/plain`: the bare value). Audited
//!   `app.credentials_viewed` (actor `ui`, the app; never the value). 410 once dismissed.
//! - `POST /ui/apps/{app}/setup-password/dismiss`: Melissa forgets it for good (the app keeps
//!   its own copy). Audited `app.credentials_dismissed`.
//!
//! Every response is `Cache-Control: no-store`. The password is never in a URL, the page's
//! first HTML, a feed event or a log. Both POSTs need positive same-origin evidence
//! (`Sec-Fetch-Site` or `Origin`), stricter than the router's check. Agents can't reach these
//! routes: a sandbox only reaches `POST /mcp/<token>`, and no tool calls them.

use std::{fmt, sync::Arc};

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use maud::Markup;
use melissa_agent::BoxFuture;
use serde::Deserialize;

use crate::{
    apps::valid,
    components::{setup_password_card, SetupPasswordCard, SetupPasswordState},
    feed::display_app,
    ui::Ui,
};

/// What the card can show for an app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupState {
    /// Melissa can show the password. `login`: the app's login name, if any.
    Available { login: Option<String> },
    /// The owner dismissed it.
    Dismissed,
    /// No such app, or no generated password.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupError {
    NotFound,
    Dismissed,
    /// The cluster couldn't be asked (the message is for the log, not the page).
    Unavailable(String),
}

/// A revealed password. `Debug` never prints it.
pub struct RevealedPassword(String);

impl RevealedPassword {
    pub fn new(value: String) -> Self {
        Self(value)
    }
}

impl fmt::Debug for RevealedPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RevealedPassword(..)")
    }
}

/// The server's setup password path (the cluster side lives in `melissa_cluster::credentials`).
pub trait SetupPasswords: Send + Sync {
    fn state<'a>(&'a self, app: &'a str) -> BoxFuture<'a, Result<SetupState, SetupError>>;
    fn reveal<'a>(&'a self, app: &'a str) -> BoxFuture<'a, Result<RevealedPassword, SetupError>>;
    fn dismiss<'a>(&'a self, app: &'a str) -> BoxFuture<'a, Result<(), SetupError>>;
}

pub(crate) fn routes(ui: Ui) -> Router {
    Router::new()
        .route("/ui/apps/{app}/setup-password", get(show))
        .route("/ui/apps/{app}/setup-password/reveal", post(reveal))
        .route("/ui/apps/{app}/setup-password/dismiss", post(dismiss))
        .with_state(ui)
}

fn url(app: &str) -> String {
    format!("/ui/apps/{app}/setup-password")
}

/// The card for `app` in `state`.
pub(crate) fn card(app: &str, login: Option<&str>, state: SetupPasswordState<'_>) -> Markup {
    let title = format!("{} setup password", display_app(app));
    setup_password_card(&SetupPasswordCard {
        title: &title,
        login,
        url: &url(app),
        state,
    })
}

/// A response nothing may keep (the browser, a proxy).
pub(crate) fn no_store(status: StatusCode, body: impl IntoResponse) -> Response {
    let mut res = (status, body).into_response();
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// Positive evidence that a browser sent this from Melissa's own page. Unlike the router's
/// check, a request with neither header is refused.
pub(crate) fn from_this_page(headers: &HeaderMap) -> bool {
    if let Some(site) = headers.get("sec-fetch-site") {
        return site == "same-origin";
    }
    match (headers.get(header::ORIGIN), headers.get(header::HOST)) {
        (Some(origin), Some(host)) => match (origin.to_str(), host.to_str()) {
            (Ok(origin), Ok(host)) => {
                origin == format!("http://{host}") || origin == format!("https://{host}")
            }
            _ => false,
        },
        _ => false,
    }
}

fn service(ui: &Ui) -> Option<&Arc<dyn SetupPasswords>> {
    ui.settings.setup_passwords()
}

#[derive(Deserialize)]
struct ShowQuery {
    #[serde(default)]
    confirm: Option<String>,
}

async fn show(
    State(ui): State<Ui>,
    Path(app): Path<String>,
    Query(q): Query<ShowQuery>,
) -> Response {
    if !valid(&app) {
        return no_store(StatusCode::NOT_FOUND, "no such app");
    }
    let Some(service) = service(&ui) else {
        return no_store(StatusCode::OK, card(&app, None, SetupPasswordState::Gone));
    };
    let confirm = q.confirm.as_deref() == Some("1");
    let markup = match service.state(&app).await {
        Ok(SetupState::Available { login }) => card(
            &app,
            login.as_deref(),
            SetupPasswordState::Hidden { confirm },
        ),
        Ok(SetupState::Dismissed) => card(&app, None, SetupPasswordState::Dismissed),
        Ok(SetupState::None) => card(&app, None, SetupPasswordState::Gone),
        Err(e) => {
            tracing::warn!(app, "setup password state: {e:?}");
            card(&app, None, SetupPasswordState::Failed)
        }
    };
    no_store(StatusCode::OK, markup)
}

fn wants_text(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/plain"))
}

async fn reveal(
    State(ui): State<Ui>,
    Path(app): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if !from_this_page(&headers) {
        return no_store(StatusCode::FORBIDDEN, "cross-site request rejected");
    }
    if !valid(&app) {
        return no_store(StatusCode::NOT_FOUND, "no such app");
    }
    let Some(service) = service(&ui) else {
        return no_store(StatusCode::NOT_FOUND, "no setup password");
    };
    // `purpose=copy` from the Copy button (said in the audit); nothing else is read.
    let copy = body.split(|b| *b == b'&').any(|p| p == b"purpose=copy");
    let text = wants_text(&headers);
    let password = match service.reveal(&app).await {
        Ok(p) => p,
        Err(e) => {
            let (status, state) = match e {
                SetupError::Dismissed => (StatusCode::GONE, SetupPasswordState::Dismissed),
                SetupError::NotFound => (StatusCode::NOT_FOUND, SetupPasswordState::Gone),
                SetupError::Unavailable(e) => {
                    tracing::warn!(app, "setup password reveal: {e}");
                    (StatusCode::BAD_GATEWAY, SetupPasswordState::Failed)
                }
            };
            return if text {
                no_store(status, "")
            } else {
                no_store(status, card(&app, None, state))
            };
        }
    };
    ui.settings.audit(
        "app.credentials_viewed",
        "ui",
        &format!(
            "{app}: setup password {}",
            if copy { "copied" } else { "shown" }
        ),
        None,
    );
    if text {
        let mut res = no_store(StatusCode::OK, password.0);
        res.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        return res;
    }
    let login = match service.state(&app).await {
        Ok(SetupState::Available { login }) => login,
        _ => None,
    };
    no_store(
        StatusCode::OK,
        card(
            &app,
            login.as_deref(),
            SetupPasswordState::Shown {
                password: &password.0,
            },
        ),
    )
}

async fn dismiss(State(ui): State<Ui>, Path(app): Path<String>, headers: HeaderMap) -> Response {
    if !from_this_page(&headers) {
        return no_store(StatusCode::FORBIDDEN, "cross-site request rejected");
    }
    if !valid(&app) {
        return no_store(StatusCode::NOT_FOUND, "no such app");
    }
    let Some(service) = service(&ui) else {
        return no_store(StatusCode::NOT_FOUND, "no setup password");
    };
    match service.dismiss(&app).await {
        Ok(()) => {
            ui.settings.audit(
                "app.credentials_dismissed",
                "ui",
                &format!("{app}: setup password dismissed"),
                None,
            );
            no_store(
                StatusCode::OK,
                card(&app, None, SetupPasswordState::Dismissed),
            )
        }
        Err(SetupError::Dismissed) => no_store(
            StatusCode::OK,
            card(&app, None, SetupPasswordState::Dismissed),
        ),
        Err(SetupError::NotFound) => no_store(
            StatusCode::NOT_FOUND,
            card(&app, None, SetupPasswordState::Gone),
        ),
        Err(SetupError::Unavailable(e)) => {
            tracing::warn!(app, "setup password dismiss: {e}");
            no_store(
                StatusCode::BAD_GATEWAY,
                card(&app, None, SetupPasswordState::Failed),
            )
        }
    }
}
