//! The sign-in pages (`/login`, `/claim`, `/invite/{token}`, `/reauth`, `/logout`), Settings →
//! Account (everyone: password, sessions) and Settings → People (the owner: members, invite
//! and reset links), and the host socket's `/api/host/setup-code` and `/api/host/reset-owner`.
//!
//! Password checks hash on purpose (Argon2id): they run on a blocking thread.

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Form, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use maud::{html, Markup};
use melissa_auth::{AuthError, InviteKind, NewSession, Role};
use serde::Deserialize;
use serde_json::json;

use super::{
    browser_label, https, safe_next, session_cookie, set_cookie, signed_in, Auth, Gate, Viewer,
};
use crate::{
    components::{
        people::{
            account_region, auth_dead_end, auth_page, credential_field, people_region, AccountView,
            AuthForm, InviteRow, PeopleView, PersonRow, SessionRow,
        },
        Tone,
    },
    is_htmx,
    ui::Ui,
};

type Source = Option<Extension<ConnectInfo<SocketAddr>>>;

fn src(s: &Source) -> String {
    s.as_ref()
        .map(|c| c.0 .0.ip().to_string())
        .unwrap_or_else(|| "local".into())
}

/// The sign-in routes (state: the gate's login store).
pub(crate) fn routes(gate: Gate) -> Router {
    Router::new()
        .route("/login", get(login_page).post(login))
        .route("/claim", get(claim_page).post(claim))
        .route("/invite/{token}", get(invite_page).post(invite))
        .route("/reauth", get(reauth_page).post(reauth))
        .route("/logout", post(logout))
        .route("/api/host/setup-code", get(host_setup_code))
        .route("/api/host/reset-owner", post(host_reset_owner))
        .with_state(gate)
}

/// Settings → Account and People (state: the UI, which carries the login store).
pub(crate) fn settings_routes() -> Router<Ui> {
    Router::new()
        .route("/settings/account", get(account_page))
        .route("/ui/account/password", post(change_password))
        .route("/ui/account/sessions/all", post(sign_out_everywhere))
        .route("/ui/account/sessions/{id}/revoke", post(revoke_session))
        .route("/settings/people", get(people_page))
        .route("/ui/people/invites", post(create_invite))
        .route("/ui/people/invites/{id}/revoke", post(revoke_invite))
        .route("/ui/people/{id}/reset", post(reset_password))
        .route("/ui/people/{id}/remove", post(remove_member))
}

/// What a person reads for an error.
fn say(e: &AuthError) -> String {
    match e {
        AuthError::WrongCredentials => "Wrong username or password.".into(),
        AuthError::SlowDown { wait_secs } => {
            format!("Too many tries. Wait {wait_secs} seconds and try again.")
        }
        AuthError::Locked { wait_secs } => format!(
            "Too many failed tries. Try again in {} minutes.",
            wait_secs.div_ceil(60)
        ),
        AuthError::AlreadyClaimed => "This Melissa already has an owner.".into(),
        AuthError::WrongSetupCode => "Wrong setup code.".into(),
        AuthError::SetupCodeExpired => {
            "This setup code no longer works. Get a new one on the host.".into()
        }
        AuthError::InviteInvalid => {
            "This link is used, expired or withdrawn. Ask the owner for a new one.".into()
        }
        AuthError::UsernameTaken => "That username is taken.".into(),
        AuthError::UsernameInvalid(m) | AuthError::PasswordInvalid(m) => (*m).into(),
        AuthError::NotFound => "That person is gone.".into(),
        AuthError::OwnerProtected => "The owner can't be removed.".into(),
        AuthError::Io(e) => {
            tracing::error!("login store: {e}");
            "Couldn't save that. Try again.".into()
        }
    }
}

fn status_for(e: &AuthError) -> StatusCode {
    match e {
        AuthError::SlowDown { .. } | AuthError::Locked { .. } => StatusCode::TOO_MANY_REQUESTS,
        AuthError::WrongCredentials | AuthError::WrongSetupCode => StatusCode::UNAUTHORIZED,
        AuthError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::UNPROCESSABLE_ENTITY,
    }
}

/// Run a password check on a blocking thread.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, AuthError> + Send + 'static,
) -> Result<T, AuthError> {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| Err(AuthError::Io(e.to_string())))
}

/// Signed in: set the cookie and go on.
fn signed_in_to(headers: &HeaderMap, session: &NewSession, to: &str) -> Response {
    let mut res = Redirect::to(to).into_response();
    res.headers_mut().append(
        header::SET_COOKIE,
        set_cookie(headers, Some(&session.token)),
    );
    res
}

// --- Sign in --------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub(crate) struct NextQ {
    next: Option<String>,
}

fn login_form(next: &str, username: &str, error: Option<&str>) -> Markup {
    auth_page(&AuthForm {
        title: "Sign in",
        intro: None,
        error,
        action: "/login",
        next: Some(next),
        fields: html! {
            (credential_field("username", "Username", "text", "username", username, None))
            (credential_field("password", "Password", "password", "current-password", "", None))
        },
        submit: "Sign in",
    })
}

async fn login_page(State(g): State<Gate>, Query(q): Query<NextQ>) -> Response {
    if !g.auth.has_owner() {
        return Redirect::to("/claim").into_response();
    }
    let next = safe_next(q.next.as_deref());
    if signed_in().is_some() {
        return Redirect::to(&next).into_response();
    }
    login_form(&next, "", None).into_response()
}

#[derive(Deserialize)]
pub(crate) struct LoginIn {
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    next: Option<String>,
}

async fn login(
    State(g): State<Gate>,
    source: Source,
    headers: HeaderMap,
    Form(form): Form<LoginIn>,
) -> Response {
    let next = safe_next(form.next.as_deref());
    let (auth, src, label) = (g.auth.clone(), src(&source), browser_label(&headers));
    let (username, password) = (form.username.clone(), form.password);
    match blocking(move || auth.login(&username, &password, &src, &label)).await {
        Ok(s) => signed_in_to(&headers, &s, &next),
        Err(e) => (
            status_for(&e),
            login_form(&next, &form.username, Some(&say(&e))),
        )
            .into_response(),
    }
}

// --- Claim ----------------------------------------------------------------------------------

fn claim_form(username: &str, error: Option<&str>) -> Markup {
    auth_page(&AuthForm {
        title: "Claim this Melissa",
        intro: Some("You become the owner."),
        error,
        action: "/claim",
        next: None,
        fields: html! {
            (credential_field("code", "Setup code", "text", "one-time-code", "",
                Some("In the install output or Melissa's log. Or run melissa-server setup-code.")))
            (credential_field("username", "Username", "text", "username", username, None))
            (credential_field("password", "Password", "password", "new-password", "", Some("At least 8 characters.")))
        },
        submit: "Claim",
    })
}

async fn claim_page(State(g): State<Gate>) -> Response {
    if g.auth.has_owner() {
        return Redirect::to("/login").into_response();
    }
    claim_form("", None).into_response()
}

#[derive(Deserialize)]
pub(crate) struct ClaimIn {
    #[serde(default)]
    code: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
}

async fn claim(
    State(g): State<Gate>,
    source: Source,
    headers: HeaderMap,
    Form(form): Form<ClaimIn>,
) -> Response {
    if g.auth.has_owner() {
        return Redirect::to("/login").into_response();
    }
    // One password field with "Show password" (no "Password again").
    let (auth, src, label) = (g.auth.clone(), src(&source), browser_label(&headers));
    let (code, username, password) = (form.code, form.username.clone(), form.password);
    match blocking(move || auth.claim(&code, &username, &password, &src, &label)).await {
        // First-run setup comes next (`crate::settings::setup`).
        Ok(s) => signed_in_to(&headers, &s, "/setup"),
        Err(AuthError::AlreadyClaimed) => Redirect::to("/login").into_response(),
        Err(e) => (status_for(&e), claim_form(&form.username, Some(&say(&e)))).into_response(),
    }
}

// --- Invite and reset links -----------------------------------------------------------------

fn invite_form(
    token: &str,
    kind: &InviteKind,
    who: Option<&str>,
    username: &str,
    error: Option<&str>,
) -> Markup {
    let action = format!("/invite/{token}");
    let reset_title = format!("New password for {}", who.unwrap_or("you"));
    let (title, intro) = match kind {
        InviteKind::Join => ("Join Melissa", "Choose your username and password."),
        InviteKind::Reset { .. } => (
            reset_title.as_str(),
            "Choose a new password. Every browser signed in as you is signed out.",
        ),
    };
    auth_page(&AuthForm {
        title,
        intro: Some(intro),
        error,
        action: &action,
        next: None,
        fields: html! {
            @if matches!(kind, InviteKind::Join) {
                (credential_field("username", "Username", "text", "username", username, None))
            }
            (credential_field("password", "Password", "password", "new-password", "", Some("At least 8 characters.")))
            (credential_field("again", "Password again", "password", "new-password", "", None))
        },
        submit: match kind {
            InviteKind::Join => "Join",
            InviteKind::Reset { .. } => "Set password",
        },
    })
}

fn dead_link() -> Response {
    (
        StatusCode::NOT_FOUND,
        auth_dead_end(
            "Link not valid",
            "This link is used, expired or withdrawn. Ask the owner for a new one.",
        ),
    )
        .into_response()
}

async fn invite_page(State(g): State<Gate>, Path(token): Path<String>) -> Response {
    match g.auth.invite(&token) {
        Some(i) => invite_form(&token, &i.kind, i.username.as_deref(), "", None).into_response(),
        None => dead_link(),
    }
}

#[derive(Deserialize)]
pub(crate) struct InviteIn {
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    again: String,
}

async fn invite(
    State(g): State<Gate>,
    Path(token): Path<String>,
    source: Source,
    headers: HeaderMap,
    Form(form): Form<InviteIn>,
) -> Response {
    let Some(info) = g.auth.invite(&token) else {
        return dead_link();
    };
    if form.password != form.again {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            invite_form(
                &token,
                &info.kind,
                info.username.as_deref(),
                &form.username,
                Some("The passwords don't match."),
            ),
        )
            .into_response();
    }
    let (auth, src, label) = (g.auth.clone(), src(&source), browser_label(&headers));
    let (t, username, password) = (token.clone(), form.username.clone(), form.password);
    // A new member goes straight to the chat (first-run setup is the owner's).
    let to = match info.kind {
        InviteKind::Join => crate::pages::MELISSA_CHAT_URL,
        InviteKind::Reset { .. } => "/",
    };
    match blocking(move || auth.accept_invite(&t, &username, &password, &src, &label)).await {
        Ok(s) => signed_in_to(&headers, &s, to),
        Err(AuthError::InviteInvalid) => dead_link(),
        Err(e) => (
            status_for(&e),
            invite_form(
                &token,
                &info.kind,
                info.username.as_deref(),
                &form.username,
                Some(&say(&e)),
            ),
        )
            .into_response(),
    }
}

// --- Password again -------------------------------------------------------------------------

fn reauth_form(next: &str, error: Option<&str>) -> Markup {
    auth_page(&AuthForm {
        title: "Enter your password",
        intro: Some("This change needs your password again. It counts for 5 minutes."),
        error,
        action: "/reauth",
        next: Some(next),
        fields: credential_field(
            "password",
            "Password",
            "password",
            "current-password",
            "",
            None,
        ),
        submit: "Continue",
    })
}

async fn reauth_page(Query(q): Query<NextQ>) -> Markup {
    reauth_form(&safe_next(q.next.as_deref()), None)
}

#[derive(Deserialize)]
pub(crate) struct ReauthIn {
    #[serde(default)]
    password: String,
    next: Option<String>,
}

async fn reauth(
    State(g): State<Gate>,
    source: Source,
    headers: HeaderMap,
    Form(form): Form<ReauthIn>,
) -> Response {
    let next = safe_next(form.next.as_deref());
    let Some(token) = session_cookie(&headers) else {
        return Redirect::to("/login").into_response();
    };
    let (auth, src) = (g.auth.clone(), src(&source));
    let password = form.password;
    match blocking(move || auth.reauth(&token, &password, &src)).await {
        Ok(()) => Redirect::to(&next).into_response(),
        Err(e) => (status_for(&e), reauth_form(&next, Some(&say(&e)))).into_response(),
    }
}

async fn logout(State(g): State<Gate>, source: Source, headers: HeaderMap) -> Response {
    if let Some(token) = session_cookie(&headers) {
        g.auth.sign_out(&token, &src(&source));
    }
    let mut res = Redirect::to("/login").into_response();
    res.headers_mut()
        .append(header::SET_COOKIE, set_cookie(&headers, None));
    res
}

// --- The host socket ------------------------------------------------------------------------

/// Only over the host-local socket (no session there).
fn host_only(viewer: &Viewer) -> Option<Response> {
    viewer
        .session
        .is_some()
        .then(|| StatusCode::NOT_FOUND.into_response())
}

/// `melissa-server setup-code`: the current code (a new one when it expired).
async fn host_setup_code(State(g): State<Gate>, Extension(viewer): Extension<Viewer>) -> Response {
    if let Some(r) = host_only(&viewer) {
        return r;
    }
    match g.auth.setup_code() {
        Ok(Some((code, expires))) => {
            log_setup_code(&code);
            Json(json!({"code": code, "expires": expires})).into_response()
        }
        Ok(None) => Json(json!({"claimed": true})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// Print the setup code where the host's owner looks (the service's log / console).
pub fn log_setup_code(code: &str) {
    tracing::warn!(
        "Claim this Melissa: open the web UI and enter the setup code {code} (valid until used \
         or for 1 hour from when it was made; a new one: `melissa-server setup-code`)"
    );
}

/// `melissa-server reset-owner`: a one-hour reset link for the owner.
async fn host_reset_owner(State(g): State<Gate>, Extension(viewer): Extension<Viewer>) -> Response {
    if let Some(r) = host_only(&viewer) {
        return r;
    }
    match g.auth.owner_reset() {
        Ok(inv) => Json(json!({
            "path": format!("/invite/{}", inv.token),
            "expires": inv.info.expires,
        }))
        .into_response(),
        Err(AuthError::NotFound) => (
            StatusCode::CONFLICT,
            Json(json!({"error": "nobody has claimed this Melissa yet: use `melissa-server setup-code`"})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// --- Settings → Account ---------------------------------------------------------------------

/// "3 hours ago", "just now".
pub(crate) fn ago(now: u64, then: u64) -> String {
    let d = now.saturating_sub(then);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => plural(d / 60, "minute", "ago"),
        3600..=86_399 => plural(d / 3600, "hour", "ago"),
        _ => plural(d / 86_400, "day", "ago"),
    }
}

/// "in 23 hours".
fn within(now: u64, then: u64) -> String {
    let d = then.saturating_sub(now);
    match d {
        0..=3599 => format!("in {}", plural_n(d.div_ceil(60).max(1), "minute")),
        _ => format!("in {}", plural_n(d / 3600, "hour")),
    }
}

fn plural_n(n: u64, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

fn plural(n: u64, unit: &str, tail: &str) -> String {
    format!("{} {tail}", plural_n(n, unit))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn role_word(role: Role) -> &'static str {
    match role {
        Role::Owner => "Owner",
        Role::Member => "Member",
    }
}

fn account_markup(ui: &Ui, viewer: &Viewer, flash: Option<(&str, Tone)>) -> Markup {
    let auth = &ui.auth;
    let now = now();
    let sessions = auth.sessions(&viewer.person.id);
    let rows: Vec<SessionRow<'_>> = sessions
        .iter()
        .map(|s| {
            let current = viewer.session.as_deref() == Some(s.id.as_str());
            SessionRow {
                label: &s.label,
                when: if current {
                    format!("Signed in {}", ago(now, s.created))
                } else {
                    format!("Last used {}", ago(now, s.last_seen))
                },
                current,
                revoke: (!current).then(|| format!("/ui/account/sessions/{}/revoke", s.id)),
            }
        })
        .collect();
    account_region(&AccountView {
        username: viewer.name(),
        role: role_word(viewer.person.role),
        sessions: rows,
        flash,
    })
}

async fn account_page(State(ui): State<Ui>, Extension(viewer): Extension<Viewer>) -> Response {
    let region = account_markup(&ui, &viewer, None);
    ui.framed(|f| crate::settings::pages::frame_at(f, "Account", None, region))
        .await
        .into_response()
}

/// Answer an Account or People form: the region for htmx, else the page (or back to it).
async fn region_answer(
    ui: &Ui,
    headers: &HeaderMap,
    section: &str,
    url: &str,
    failed: bool,
    region: Markup,
) -> Response {
    if is_htmx(headers) {
        return region.into_response();
    }
    if failed {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            ui.framed(|f| crate::settings::pages::frame_at(f, section, None, region))
                .await,
        )
            .into_response();
    }
    Redirect::to(url).into_response()
}

#[derive(Deserialize)]
pub(crate) struct PasswordIn {
    #[serde(default)]
    current: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    again: String,
}

async fn change_password(
    State(ui): State<Ui>,
    Extension(viewer): Extension<Viewer>,
    source: Source,
    headers: HeaderMap,
    Form(form): Form<PasswordIn>,
) -> Response {
    let result = if form.password != form.again {
        Err("The new passwords don't match.".to_string())
    } else {
        match session_cookie(&headers) {
            Some(token) => {
                let (auth, src) = (ui.auth.clone(), src(&source));
                blocking(move || auth.change_password(&token, &form.current, &form.password, &src))
                    .await
                    .map_err(|e| match e {
                        AuthError::WrongCredentials => "Wrong current password.".to_string(),
                        e => say(&e),
                    })
            }
            None => Err("Sign in first.".to_string()),
        }
    };
    let (text, tone) = match &result {
        Ok(()) => (
            "Password changed. Other browsers are signed out.".to_string(),
            Tone::Ok,
        ),
        Err(e) => (e.clone(), Tone::Error),
    };
    let region = account_markup(&ui, &viewer, Some((&text, tone)));
    region_answer(
        &ui,
        &headers,
        "Account",
        "/settings/account",
        result.is_err(),
        region,
    )
    .await
}

async fn revoke_session(
    State(ui): State<Ui>,
    Extension(viewer): Extension<Viewer>,
    Path(id): Path<String>,
    source: Source,
    headers: HeaderMap,
) -> Response {
    let flash = if ui
        .auth
        .revoke_session(&viewer.person.id, &id, &src(&source))
    {
        ("Signed out.", Tone::Ok)
    } else {
        ("That session already ended.", Tone::Ok)
    };
    let region = account_markup(&ui, &viewer, Some(flash));
    region_answer(&ui, &headers, "Account", "/settings/account", false, region).await
}

async fn sign_out_everywhere(
    State(ui): State<Ui>,
    Extension(viewer): Extension<Viewer>,
    source: Source,
    headers: HeaderMap,
) -> Response {
    ui.auth.revoke_all(&viewer.person.id, &src(&source));
    let mut res = if is_htmx(&headers) {
        let mut r = StatusCode::NO_CONTENT.into_response();
        r.headers_mut().insert(
            "hx-redirect",
            axum::http::HeaderValue::from_static("/login"),
        );
        r
    } else {
        Redirect::to("/login").into_response()
    };
    res.headers_mut()
        .append(header::SET_COOKIE, set_cookie(&headers, None));
    res
}

// --- Settings → People ----------------------------------------------------------------------

fn link_base(headers: &HeaderMap) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    let scheme = if https(headers) { "https" } else { "http" };
    format!("{scheme}://{host}")
}

fn people_markup(
    auth: &Auth,
    viewer: &Viewer,
    new_link: Option<(String, String)>,
    flash: Option<(&str, Tone)>,
) -> Markup {
    let now = now();
    let people = auth.people();
    let rows: Vec<PersonRow<'_>> = people
        .iter()
        .map(|p| {
            let member = p.role == Role::Member;
            PersonRow {
                username: &p.username,
                role: if p.id == viewer.person.id {
                    "Owner (you)"
                } else {
                    role_word(p.role)
                },
                since: format!("joined {}", ago(now, p.created)),
                reset: member.then(|| format!("/ui/people/{}/reset", p.id)),
                remove: member.then(|| format!("/ui/people/{}/remove", p.id)),
            }
        })
        .collect();
    let invites = auth
        .invites()
        .into_iter()
        .map(|i| InviteRow {
            what: match &i.username {
                None => "Invite link".into(),
                Some(u) => format!("Password reset for {u}"),
            },
            expires: format!("Expires {}", within(now, i.expires)),
            revoke: format!("/ui/people/invites/{}/revoke", i.id),
        })
        .collect();
    people_region(&PeopleView {
        people: rows,
        invites,
        new_link,
        flash,
    })
}

async fn people_page(State(ui): State<Ui>, Extension(viewer): Extension<Viewer>) -> Response {
    let region = people_markup(&ui.auth, &viewer, None, None);
    ui.framed(|f| crate::settings::pages::frame_at(f, "People", None, region))
        .await
        .into_response()
}

async fn people_answer(
    ui: &Ui,
    viewer: &Viewer,
    headers: &HeaderMap,
    new_link: Option<(String, String)>,
    flash: Option<(&str, Tone)>,
) -> Response {
    let failed = flash.is_some_and(|(_, t)| t == Tone::Error);
    // The link shows once: without script the page is rendered here rather than redirected.
    let shown = new_link.is_some();
    let region = people_markup(&ui.auth, viewer, new_link, flash);
    region_answer(
        ui,
        headers,
        "People",
        "/settings/people",
        failed || shown,
        region,
    )
    .await
}

async fn create_invite(
    State(ui): State<Ui>,
    Extension(viewer): Extension<Viewer>,
    source: Source,
    headers: HeaderMap,
) -> Response {
    match ui.auth.create_invite(&viewer.person.id, &src(&source)) {
        Ok(inv) => {
            let link = format!("{}/invite/{}", link_base(&headers), inv.token);
            people_answer(
                &ui,
                &viewer,
                &headers,
                Some(("New invite link".into(), link)),
                None,
            )
            .await
        }
        Err(e) => people_answer(&ui, &viewer, &headers, None, Some((&say(&e), Tone::Error))).await,
    }
}

async fn revoke_invite(
    State(ui): State<Ui>,
    Extension(viewer): Extension<Viewer>,
    Path(id): Path<String>,
    source: Source,
    headers: HeaderMap,
) -> Response {
    let flash = if ui.auth.revoke_invite(&id, &viewer.person.id, &src(&source)) {
        "Withdrawn."
    } else {
        "That link was already used or expired."
    };
    people_answer(&ui, &viewer, &headers, None, Some((flash, Tone::Ok))).await
}

async fn reset_password(
    State(ui): State<Ui>,
    Extension(viewer): Extension<Viewer>,
    Path(id): Path<String>,
    source: Source,
    headers: HeaderMap,
) -> Response {
    let person = ui.auth.person(&id);
    if person.as_ref().is_none_or(|p| p.role == Role::Owner) {
        let msg = "Only a member's password can be reset here.";
        return people_answer(&ui, &viewer, &headers, None, Some((msg, Tone::Error))).await;
    }
    match ui.auth.reset_link(&viewer.person.id, &id, &src(&source)) {
        Ok(inv) => {
            let link = format!("{}/invite/{}", link_base(&headers), inv.token);
            let what = format!(
                "Password reset link for {}",
                inv.info.username.as_deref().unwrap_or("them")
            );
            people_answer(&ui, &viewer, &headers, Some((what, link)), None).await
        }
        Err(e) => people_answer(&ui, &viewer, &headers, None, Some((&say(&e), Tone::Error))).await,
    }
}

async fn remove_member(
    State(ui): State<Ui>,
    Extension(viewer): Extension<Viewer>,
    Path(id): Path<String>,
    source: Source,
    headers: HeaderMap,
) -> Response {
    match ui.auth.remove_member(&id, &viewer.person.id, &src(&source)) {
        Ok(p) => {
            let msg = format!("Removed {}.", p.username);
            people_answer(&ui, &viewer, &headers, None, Some((&msg, Tone::Ok))).await
        }
        Err(e) => people_answer(&ui, &viewer, &headers, None, Some((&say(&e), Tone::Error))).await,
    }
}
