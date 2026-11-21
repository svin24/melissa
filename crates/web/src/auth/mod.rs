//! Sign-in for the web UI and the API (PLAN 6c, step 1; DECISIONS 2026-11-21 "Logins").
//!
//! [`layer`] runs around every request:
//! - **Public** paths (sign-in, claim, invite links, assets, `/healthz`, the sandboxed agents'
//!   `/mcp/<token>`) pass as before.
//! - A box without an owner sends every other page to `/claim`; the API answers 401.
//! - Everything else needs a session cookie (or the host-local socket, see [`serve_host`]).
//!   Pages redirect to `/login`, htmx gets `HX-Redirect`, the API and SSE get 401.
//! - Owner-only paths ([`need`]) refuse members with 403.
//! - Every mutating request from a session carries the session's CSRF token: the
//!   `X-CSRF-Token` header (htmx and fetch, set by melissa.js from the page's
//!   `<meta name="csrf-token">`) or a `_csrf` form field (forms without script; taken out of
//!   the body before the handler sees it). The same-origin check in `security` stays.
//! - Sensitive actions ([`sensitive`]) need the password entered within the last 5 minutes:
//!   otherwise the browser goes to `/reauth` and comes back.
//!
//! The person and the CSRF token travel as a task-local for the request, so components
//! can add the token to forms and show who is signed in without every handler passing it
//! on; the person also goes into every audit record made while handling the request
//! (`melissa_agent::AUDIT_PERSON`).

pub(crate) mod pages;

use std::{path::Path, time::Duration};

use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
    Json,
};
use futures::StreamExt;
pub use melissa_auth::{Auth, Person, Role};
use serde_json::json;

/// Session cookie over HTTPS: `__Host-` pins it to this host and path `/` and needs `Secure`.
pub(crate) const COOKIE_SECURE: &str = "__Host-melissa_session";
/// Session cookie over plain HTTP (the LAN today): browsers refuse `Secure` cookies from an
/// `http://` page, and a `__Host-` cookie must be `Secure`, so this one is `HttpOnly;
/// SameSite=Strict; Path=/` without the prefix. HTTPS comes with Q22 (Tailscale certificates
/// or an internal CA).
pub(crate) const COOKIE_PLAIN: &str = "melissa_session";
/// The form field that carries the CSRF token in forms posted without script.
pub(crate) const CSRF_FIELD: &str = "_csrf";
/// The header htmx and melissa.js send it in.
pub(crate) const CSRF_HEADER: &str = "x-csrf-token";
/// Forms are small; a bigger urlencoded body is refused rather than buffered.
const MAX_FORM_BYTES: usize = 2 << 20;
/// How much of a multipart body is read to find the `_csrf` field (always its first part).
const MULTIPART_PREFIX: usize = 4096;

/// Who is asking, in the request's extensions (handlers: `Extension<Viewer>`).
#[derive(Clone, Debug)]
pub struct Viewer {
    pub person: Person,
    /// The session's public id (`None` on the host socket).
    pub session: Option<String>,
    /// The password was entered again recently (always true on the host socket).
    pub reauth_fresh: bool,
}

impl Viewer {
    /// The name the audit and the UI use.
    pub fn name(&self) -> &str {
        &self.person.username
    }
}

/// Requests that came in over the host-local socket (set by [`serve_host`] only).
#[derive(Clone, Copy)]
struct HostLocal;

#[derive(Clone, Default)]
struct Current {
    csrf: Option<String>,
    person: Option<Person>,
}

tokio::task_local! {
    static CURRENT: Current;
}

/// The CSRF token of the request being rendered, if it comes from a session.
pub(crate) fn csrf_token() -> Option<String> {
    CURRENT.try_with(|c| c.csrf.clone()).ok().flatten()
}

/// The signed-in person of the request being rendered.
pub(crate) fn signed_in() -> Option<Person> {
    CURRENT.try_with(|c| c.person.clone()).ok().flatten()
}

/// The middleware's state: the login store.
#[derive(Clone)]
pub struct Gate {
    pub(crate) auth: Auth,
}

impl Gate {
    pub fn new(auth: Auth) -> Self {
        Self { auth }
    }
}

/// What a path needs.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum Need {
    Public,
    Member,
    Owner,
}

/// Paths anyone may open (signed in or not).
fn public(path: &str) -> bool {
    matches!(path, "/login" | "/claim" | "/healthz" | "/reset-status")
        || ["/assets/", "/invite/", "/mcp/"]
            .iter()
            .any(|p| path.starts_with(p))
}

/// Owner-only for now (PLAN 6c step 1): People, providers, app access, Tailscale, updates, force
/// delete, factory reset.
const OWNER_PREFIXES: &[&str] = &[
    "/settings/people",
    "/ui/people",
    "/api/people",
    "/settings/providers",
    "/ui/settings/providers",
    "/api/settings/providers",
    "/settings/apps",
    "/ui/settings/apps",
    "/api/settings/app-access",
    "/settings/tailscale",
    "/ui/settings/tailscale",
    "/settings/updates",
    "/ui/settings/updates",
    "/api/settings/updates",
    "/ui/settings/restart",
    "/settings/reset",
    "/ui/settings/reset",
    "/api/settings/reset",
];

pub(crate) fn need(path: &str) -> Need {
    if public(path) {
        return Need::Public;
    }
    if OWNER_PREFIXES.iter().any(|p| under(path, p)) || path.ends_with("/force-delete") {
        return Need::Owner;
    }
    Need::Member
}

/// `path` is `prefix` or below it.
fn under(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Mutations that need the password entered again within 5 minutes: the setup password's
/// Show/Copy/Dismiss, app access changes, Tailscale changes and sign-in, provider settings,
/// People, force delete, factory reset. (An
/// agent's request to widen app access is checked where it is approved: see
/// [`require_owner_reauth`].)
pub(crate) fn sensitive(method: &Method, path: &str) -> bool {
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return false;
    }
    (path.starts_with("/ui/apps/")
        && (path.ends_with("/setup-password/reveal") || path.ends_with("/setup-password/dismiss")))
        || path.ends_with("/force-delete")
        || [
            "/ui/settings/apps",
            "/api/settings/app-access",
            "/ui/settings/tailscale",
            "/ui/settings/providers",
            "/api/settings/providers",
            "/ui/people",
            "/api/people",
            "/ui/settings/reset",
            "/api/settings/reset",
        ]
        .iter()
        .any(|p| under(path, p))
}

fn is_api(path: &str) -> bool {
    path.starts_with("/api/")
}

/// The request came over HTTPS (directly or through a TLS proxy such as `tailscale serve`).
pub(crate) fn https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"https"))
        || headers
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|o| o.starts_with("https://"))
}

/// The session cookie's value, under either name.
pub(crate) fn session_cookie(headers: &HeaderMap) -> Option<String> {
    for value in headers.get_all(header::COOKIE) {
        let Ok(text) = value.to_str() else { continue };
        for pair in text.split(';') {
            let Some((name, v)) = pair.trim().split_once('=') else {
                continue;
            };
            if (name == COOKIE_SECURE || name == COOKIE_PLAIN) && !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// `Set-Cookie` for a new session (`None`: clear it).
pub(crate) fn set_cookie(headers: &HeaderMap, token: Option<&str>) -> HeaderValue {
    let secure = https(headers);
    let name = if secure { COOKIE_SECURE } else { COOKIE_PLAIN };
    let (value, age) = match token {
        Some(t) => (t, melissa_auth::SESSION_MAX_SECS),
        None => ("", 0),
    };
    let text = format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={age}{}",
        if secure { "; Secure" } else { "" }
    );
    // Tokens are URL-safe base64: always a valid header value.
    HeaderValue::from_str(&text).unwrap_or_else(|_| HeaderValue::from_static("melissa_session="))
}

/// A short name for the browser, from its User-Agent ("Firefox on Linux").
pub(crate) fn browser_label(headers: &HeaderMap) -> String {
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let browser = if ua.contains("Firefox/") {
        "Firefox"
    } else if ua.contains("Edg/") {
        "Edge"
    } else if ua.contains("Chrome/") || ua.contains("Chromium/") {
        "Chrome"
    } else if ua.contains("Safari/") {
        "Safari"
    } else {
        "Browser"
    };
    let os = if ua.contains("Android") {
        "Android"
    } else if ua.contains("iPhone") || ua.contains("iPad") {
        "iOS"
    } else if ua.contains("Windows") {
        "Windows"
    } else if ua.contains("Mac OS") {
        "macOS"
    } else if ua.contains("Linux") {
        "Linux"
    } else {
        ""
    };
    if os.is_empty() {
        browser.into()
    } else {
        format!("{browser} on {os}")
    }
}

/// A local path to come back to after signing in (never another site).
pub(crate) fn safe_next(next: Option<&str>) -> String {
    match next {
        Some(n)
            if n.starts_with('/')
                && !n.starts_with("//")
                && !n.contains('\\')
                && !n.starts_with("/login")
                && !n.starts_with("/claim")
                && !n.starts_with("/reauth") =>
        {
            n.to_string()
        }
        _ => "/".into(),
    }
}

/// The page to come back to: the path of this GET, else the page the request came from.
fn back_to(req: &Request) -> String {
    if req.method() == Method::GET && !crate::is_htmx(req.headers()) {
        return req
            .uri()
            .path_and_query()
            .map(|p| p.as_str().to_string())
            .unwrap_or_else(|| "/".into());
    }
    let from = req
        .headers()
        .get("hx-current-url")
        .or_else(|| req.headers().get(header::REFERER))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("/");
    // Only the path of a same-origin URL.
    let path = match from.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("/", |i| &rest[i..]),
        None => from,
    };
    safe_next(Some(path))
}

/// Percent-encode `s` for a URL's query (keeps `/` and the unreserved characters).
pub(crate) fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Send the browser to `to`: `HX-Redirect` for htmx (a whole-page load), else 303. The API
/// gets `status` with a JSON error and the place to go.
fn go(req: &Request, to: &str, status: StatusCode, message: &str) -> Response {
    if is_api(req.uri().path()) {
        return (status, Json(json!({"error": message, "location": to}))).into_response();
    }
    if crate::is_htmx(req.headers()) {
        let mut res = status.into_response();
        if let Ok(v) = HeaderValue::from_str(to) {
            res.headers_mut().insert("hx-redirect", v);
        }
        return res;
    }
    // The server-sent event streams can't follow a redirect: plain 401.
    if req
        .headers()
        .get(header::ACCEPT)
        .is_some_and(|v| v.as_bytes().starts_with(b"text/event-stream"))
    {
        return status.into_response();
    }
    Redirect::to(to).into_response()
}

fn refuse(req: &Request, status: StatusCode, message: &str) -> Response {
    if is_api(req.uri().path()) || !accepts_html(req.headers()) {
        return (status, Json(json!({"error": message}))).into_response();
    }
    (status, crate::pages::error_page("Not allowed", message)).into_response()
}

fn accepts_html(headers: &HeaderMap) -> bool {
    crate::is_htmx(headers)
        || headers
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|a| a.contains("text/html"))
}

/// The middleware (see the module docs).
pub(crate) async fn layer(State(gate): State<Gate>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let need = need(&path);

    if req.extensions().get::<HostLocal>().is_some() {
        // The host's own CLI: the owner's authority (root or Melissa's own user on the box).
        let person = gate.auth.owner().unwrap_or(Person {
            id: "host".into(),
            username: "host".into(),
            role: Role::Owner,
            created: 0,
        });
        return run_as(
            Viewer {
                person,
                session: None,
                reauth_fresh: true,
            },
            None,
            "host",
            req,
            next,
        )
        .await;
    }

    if need == Need::Public {
        // Show who is signed in on public pages too (the sign-in page sends them on).
        let current = match session_cookie(req.headers()).and_then(|t| gate.auth.session(&t)) {
            Some(v) => Current {
                csrf: Some(v.csrf),
                person: Some(v.person),
            },
            None => Current::default(),
        };
        return CURRENT.scope(current, next.run(req)).await;
    }

    if !gate.auth.has_owner() {
        return go(
            &req,
            "/claim",
            StatusCode::UNAUTHORIZED,
            "Nobody has claimed this Melissa yet. Open the web UI to claim it.",
        );
    }

    let Some(view) = session_cookie(req.headers()).and_then(|t| gate.auth.session(&t)) else {
        let to = format!("/login?next={}", encode(&back_to(&req)));
        let mut res = go(&req, &to, StatusCode::UNAUTHORIZED, "Sign in first.");
        // A stale cookie goes away.
        if session_cookie(req.headers()).is_some() {
            res.headers_mut()
                .append(header::SET_COOKIE, set_cookie(req.headers(), None));
        }
        return res;
    };

    if need == Need::Owner && !view.person.is_owner() {
        return refuse(&req, StatusCode::FORBIDDEN, "Only the owner can do this.");
    }

    let mutating = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    let req = if mutating {
        match check_csrf(req, &view.csrf).await {
            Ok(req) => req,
            Err(res) => return *res,
        }
    } else {
        req
    };

    if sensitive(req.method(), &path) && !view.reauth_fresh {
        return reauth_needed(&req);
    }

    let name = view.person.username.clone();
    let csrf = view.csrf.clone();
    run_as(
        Viewer {
            person: view.person,
            session: Some(view.id),
            reauth_fresh: view.reauth_fresh,
        },
        Some(csrf),
        &name,
        req,
        next,
    )
    .await
}

async fn run_as(
    viewer: Viewer,
    csrf: Option<String>,
    audit_name: &str,
    mut req: Request,
    next: Next,
) -> Response {
    let current = Current {
        csrf,
        person: Some(viewer.person.clone()),
    };
    req.extensions_mut().insert(viewer);
    let fut = melissa_agent::AUDIT_PERSON.scope(audit_name.to_string(), next.run(req));
    CURRENT.scope(current, fut).await
}

/// The answer when a sensitive action needs the password again.
pub(crate) fn reauth_needed(req: &Request) -> Response {
    let to = format!("/reauth?next={}", encode(&back_to(req)));
    if is_api(req.uri().path()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Enter your password again first.", "reauth": to})),
        )
            .into_response();
    }
    // fetch() from melissa.js (the setup password's Copy) follows this header.
    if !crate::is_htmx(req.headers()) && !accepts_html(req.headers()) {
        let mut res =
            (StatusCode::UNAUTHORIZED, "Enter your password again first.").into_response();
        if let Ok(v) = HeaderValue::from_str(&to) {
            res.headers_mut().insert("x-melissa-reauth", v);
        }
        return res;
    }
    go(
        req,
        &to,
        StatusCode::UNAUTHORIZED,
        "Enter your password again first.",
    )
}

/// For handlers: an action only the owner may take, with the password entered recently
/// (an agent's request to widen app access). `Err` is the answer to send.
pub(crate) fn require_owner_reauth(
    viewer: Option<&Viewer>,
    headers: &HeaderMap,
) -> Result<(), Box<Response>> {
    let Some(v) = viewer else {
        return Err(Box::new(
            (StatusCode::UNAUTHORIZED, "Sign in first.").into_response(),
        ));
    };
    if !v.person.is_owner() {
        return Err(Box::new(
            (StatusCode::FORBIDDEN, "Only the owner can approve this.").into_response(),
        ));
    }
    if !v.reauth_fresh {
        let from = headers
            .get("hx-current-url")
            .or_else(|| headers.get(header::REFERER))
            .and_then(|v| v.to_str().ok())
            .unwrap_or("/");
        let path = match from.split_once("://") {
            Some((_, rest)) => rest.find('/').map_or("/", |i| &rest[i..]),
            None => from,
        };
        let to = format!("/reauth?next={}", encode(&safe_next(Some(path))));
        let mut res = (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Enter your password again first.", "reauth": to})),
        )
            .into_response();
        if crate::is_htmx(headers) {
            if let Ok(v) = HeaderValue::from_str(&to) {
                res.headers_mut().insert("hx-redirect", v);
            }
        }
        return Err(Box::new(res));
    }
    Ok(())
}

/// Tools whose approval widens who can reach the box: only the owner, with the password
/// entered recently, may approve them (denying stays open to everyone signed in).
const OWNER_REAUTH_TOOLS: &[&str] = &["set_app_access", "enable_tailscale"];

/// For the approve/always handlers: refuse when confirmation `id` is one only the owner
/// may approve and this person can't right now.
pub(crate) fn check_approval(
    lead: &melissa_agent::lead::Lead,
    id: u64,
    viewer: Option<&Viewer>,
    headers: &HeaderMap,
) -> Result<(), Box<Response>> {
    let owner_only = lead
        .confirmations()
        .get(id)
        .is_some_and(|c| OWNER_REAUTH_TOOLS.contains(&c.tool.as_str()));
    if owner_only {
        require_owner_reauth(viewer, headers)?;
    }
    Ok(())
}

/// Check the CSRF token: the header, else the `_csrf` field of a form body (taken out).
async fn check_csrf(req: Request, expected: &str) -> Result<Request, Box<Response>> {
    let ok = |t: &[u8]| melissa_auth::constant_time_eq(t, expected.as_bytes());
    let header_ok = req
        .headers()
        .get(CSRF_HEADER)
        .is_some_and(|v| ok(v.as_bytes()));
    let content_type = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let api = is_api(req.uri().path()) || content_type.starts_with("application/json");
    let stale = || {
        let msg = "This page is out of date. Reload it and try again.";
        if api {
            (StatusCode::FORBIDDEN, Json(json!({"error": msg}))).into_response()
        } else {
            (
                StatusCode::FORBIDDEN,
                crate::pages::error_page("Page out of date", msg),
            )
                .into_response()
        }
    };
    if content_type.starts_with("application/x-www-form-urlencoded") {
        // Buffer the (small) form, check and strip `_csrf`, hand the rest on.
        let (parts, body) = req.into_parts();
        let Ok(bytes) = axum::body::to_bytes(body, MAX_FORM_BYTES).await else {
            return Err(Box::new(
                (StatusCode::PAYLOAD_TOO_LARGE, "form too large").into_response(),
            ));
        };
        let mut field_ok = false;
        let kept: Vec<&[u8]> = bytes
            .split(|b| *b == b'&')
            .filter(|pair| {
                let is_token = pair.starts_with(CSRF_FIELD.as_bytes())
                    && pair.get(CSRF_FIELD.len()) == Some(&b'=');
                if is_token && ok(&pair[CSRF_FIELD.len() + 1..]) {
                    field_ok = true;
                }
                !is_token && !pair.is_empty()
            })
            .collect();
        if !(header_ok || field_ok) {
            return Err(Box::new(stale()));
        }
        let body = kept.join(&b'&');
        return Ok(Request::from_parts(parts, Body::from(body)));
    }
    if header_ok {
        return Ok(req);
    }
    if let Some(boundary) = content_type
        .strip_prefix("multipart/form-data")
        .and_then(|rest| {
            rest.split(';')
                .find_map(|p| p.trim().strip_prefix("boundary="))
        })
        .map(|b| b.trim_matches('"').to_string())
    {
        // Without script: the token is the form's first field. Read just enough to see it.
        let (parts, body) = req.into_parts();
        let mut stream = body.into_data_stream();
        let mut prefix = Vec::new();
        while prefix.len() < MULTIPART_PREFIX {
            match stream.next().await {
                Some(Ok(chunk)) => prefix.extend_from_slice(&chunk),
                Some(Err(_)) => return Err(Box::new(StatusCode::BAD_REQUEST.into_response())),
                None => break,
            }
        }
        if !multipart_token_ok(&prefix, &boundary, expected) {
            return Err(Box::new(stale()));
        }
        let rest =
            futures::stream::once(async move { Ok::<Bytes, axum::Error>(Bytes::from(prefix)) })
                .chain(stream);
        return Ok(Request::from_parts(parts, Body::from_stream(rest)));
    }
    Err(Box::new(stale()))
}

/// The multipart body starts with the `_csrf` part and it holds `expected`.
fn multipart_token_ok(prefix: &[u8], boundary: &str, expected: &str) -> bool {
    let start = format!("--{boundary}\r\n");
    let Some(rest) = prefix.strip_prefix(start.as_bytes()) else {
        return false;
    };
    let Some(end) = find(rest, b"\r\n\r\n") else {
        return false;
    };
    let head = String::from_utf8_lossy(&rest[..end]).to_ascii_lowercase();
    if !head.contains(&format!("name=\"{CSRF_FIELD}\"")) {
        return false;
    }
    let value = &rest[end + 4..];
    let Some(stop) = find(value, b"\r\n") else {
        return false;
    };
    melissa_auth::constant_time_eq(&value[..stop], expected.as_bytes())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

// --- The host-local socket ------------------------------------------------------------------

/// A Unix socket listener that accepts only root and the server's own user (peer
/// credentials), for the host's `melissa-server` CLI.
struct PeerChecked {
    inner: tokio::net::UnixListener,
    own_uid: u32,
}

impl axum::serve::Listener for PeerChecked {
    type Io = tokio::net::UnixStream;
    type Addr = ();

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.inner.accept().await {
                Ok((stream, _)) => match stream.peer_cred() {
                    Ok(cred) if cred.uid() == 0 || cred.uid() == self.own_uid => {
                        return (stream, ());
                    }
                    Ok(cred) => {
                        tracing::warn!(uid = cred.uid(), "host socket: refused a connection");
                    }
                    Err(e) => tracing::warn!("host socket: no peer credentials: {e}"),
                },
                Err(e) => {
                    tracing::warn!("host socket: accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(())
    }
}

/// Bind the host-local socket at `path` (0600, in a 0700 directory) for the CLI.
pub fn bind_host(path: &Path) -> std::io::Result<tokio::net::UnixListener> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    // A socket left by an earlier run.
    match std::fs::symlink_metadata(path) {
        Ok(m) if std::os::unix::fs::FileTypeExt::is_socket(&m.file_type()) => {
            std::fs::remove_file(path)?;
        }
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a socket", path.display()),
            ))
        }
        Err(_) => {}
    }
    let listener = tokio::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Serve `app` on the host-local socket: requests carry the owner's authority (the CLI).
pub async fn serve_host(
    listener: tokio::net::UnixListener,
    path: &Path,
    app: axum::Router,
) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let own_uid = std::fs::metadata(path)?.uid();
    let app = app.layer(axum::Extension(HostLocal));
    axum::serve(
        PeerChecked {
            inner: listener,
            own_uid,
        },
        app,
    )
    .await
}

#[cfg(test)]
mod tests;

/// A signed-in test owner and a layer that sends their cookie and CSRF token with every
/// request that has none of its own.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use melissa_auth::{Cost, NewSession};

    pub(crate) const PASSWORD: &str = "owner-password";

    /// A login store with an owner `owner` (password [`PASSWORD`]) and their session.
    pub(crate) fn owner() -> (Auth, NewSession) {
        let auth = Auth::in_memory().with_cost(Cost::Test);
        let (code, _) = auth.setup_code().unwrap().unwrap();
        let session = auth
            .claim(&code, "owner", PASSWORD, "test", "Test browser")
            .unwrap();
        (auth, session)
    }

    pub(crate) fn signed_in(router: axum::Router, s: &NewSession) -> axum::Router {
        let cookie = format!("{COOKIE_PLAIN}={}", s.token);
        let csrf = s.view.csrf.clone();
        router.layer(axum::middleware::from_fn(
            move |mut req: Request, next: Next| {
                let (cookie, csrf) = (cookie.clone(), csrf.clone());
                async move {
                    let h = req.headers_mut();
                    if !h.contains_key(header::COOKIE) {
                        if let Ok(v) = HeaderValue::from_str(&cookie) {
                            h.insert(header::COOKIE, v);
                        }
                    }
                    if !h.contains_key(CSRF_HEADER) && !h.contains_key("x-test-no-csrf") {
                        if let Ok(v) = HeaderValue::from_str(&csrf) {
                            h.insert(CSRF_HEADER, v);
                        }
                    }
                    next.run(req).await
                }
            },
        ))
    }
}
