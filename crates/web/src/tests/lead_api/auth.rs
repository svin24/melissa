//! Sign-in (PLAN 6c step 1): the gate in front of every page, API call and event stream; the
//! owner's claim; invites; sessions; CSRF tokens; the password again for sensitive actions;
//! owner-only sections; the host-local socket for the CLI.

use std::sync::atomic::{AtomicU64, Ordering};

use melissa_auth::{Auth, Cost, NewSession};

use super::*;
use crate::auth::{COOKIE_PLAIN, CSRF_HEADER};

const HOST: &str = "melissa.example.com";

/// A login store on a clock the test moves.
pub(super) fn store() -> (Auth, Arc<AtomicU64>) {
    let clock = Arc::new(AtomicU64::new(1_000_000));
    let c = clock.clone();
    let auth = Auth::in_memory()
        .with_cost(Cost::Test)
        .with_clock(Arc::new(move || c.load(Ordering::SeqCst)));
    (auth, clock)
}

pub(super) fn claimed(auth: &Auth) -> NewSession {
    let (code, _) = auth.setup_code().unwrap().unwrap();
    auth.claim(&code, "ana", "owner-password", "test", "Test")
        .unwrap()
}

pub(super) fn member(auth: &Auth, owner: &NewSession) -> NewSession {
    let inv = auth.create_invite(&owner.view.person.id, "test").unwrap();
    auth.accept_invite(&inv.token, "bob", "member-password", "test", "Test")
        .unwrap()
}

fn cookie(s: &NewSession) -> String {
    format!("{COOKIE_PLAIN}={}", s.token)
}

fn req(method: &str, uri: &str) -> axum::http::request::Builder {
    axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, HOST)
}

pub(super) fn as_(s: &NewSession, method: &str, uri: &str) -> axum::http::request::Builder {
    req(method, uri).header(header::COOKIE, cookie(s))
}

fn form_post(uri: &str, _body: &str) -> axum::http::request::Builder {
    req("POST", uri).header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
}

fn set_cookie_of(headers: &HeaderMap) -> String {
    headers
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default()
}

fn token_of(set_cookie: &str) -> String {
    set_cookie
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1
        .to_string()
}

#[tokio::test]
async fn a_box_without_an_owner_sends_everyone_to_claim() {
    let (auth, _) = store();
    let (app, _) = app_signed_out(auth.clone());
    let (status, headers, _) = send(&app, req("GET", "/").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/claim");
    let (status, _, body) = send(
        &app,
        req("GET", "/api/lead/status").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("claim"), "{body}");
    // The sign-in page itself goes to the claim too.
    let (status, headers, _) = send(&app, req("GET", "/login").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/claim");
    // Assets and the health check stay open.
    let (status, _, _) = send(
        &app,
        req("GET", "/assets/melissa.css")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(&app, req("GET", "/healthz").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn claim_needs_the_code_and_works_once() {
    let (auth, _) = store();
    let (app, _) = app_signed_out(auth.clone());
    let (status, _, page) = send(&app, req("GET", "/claim").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("Setup code") && page.contains("autocomplete=\"new-password\""));
    // One password field with "Show password" (2026-11-21: no "Password again").
    assert!(!page.contains("Password again") && !page.contains("name=\"again\""));
    assert!(page.contains("data-show-password") && page.contains("Show password"));
    assert!(page.contains("melissa-server setup-code"));

    let (code, _) = auth.setup_code().unwrap().unwrap();
    let body = "code=WRONG-CODE2&username=ana&password=owner-password";
    let (status, headers, page) = send(
        &app,
        form_post("/claim", body).body(Body::from(body)).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(page.contains("Wrong setup code."));
    assert!(headers.get(header::SET_COOKIE).is_none());
    assert!(!auth.has_owner());

    let body = format!("code={code}&username=ana&password=owner-password");
    let (status, headers, _) = send(
        &app,
        form_post("/claim", &body)
            .body(Body::from(body.clone()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    // First-run setup comes next.
    assert_eq!(headers[header::LOCATION], "/setup");
    let set = set_cookie_of(&headers);
    assert!(set.starts_with("melissa_session="), "{set}");
    for part in ["HttpOnly", "SameSite=Strict", "Path=/"] {
        assert!(set.contains(part), "{set}");
    }
    assert!(!set.contains("Secure"), "plain HTTP: no Secure flag");
    assert!(auth.has_owner());
    // The cookie opens the chat.
    let token = token_of(&set);
    let (status, _, page) = send(
        &app,
        req("GET", "/")
            .header(header::COOKIE, format!("{COOKIE_PLAIN}={token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("name=\"csrf-token\""));
    assert!(page.contains("Sign out"));

    // A second claim goes nowhere.
    let (status, headers, _) = send(
        &app,
        form_post("/claim", &body)
            .body(Body::from(body.clone()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/login");
    assert_eq!(auth.people().len(), 1);
}

#[tokio::test]
async fn over_https_the_cookie_is_host_prefixed_and_secure() {
    let (auth, _) = store();
    let owner = claimed(&auth);
    let _ = owner;
    let (app, _) = app_signed_out(auth.clone());
    let body = "username=ana&password=owner-password&next=%2Fsettings";
    let (status, headers, _) = send(
        &app,
        form_post("/login", body)
            .header("x-forwarded-proto", "https")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/settings");
    let set = set_cookie_of(&headers);
    assert!(
        set.starts_with("__Host-melissa_session=") && set.contains("; Secure"),
        "{set}"
    );
}

#[tokio::test]
async fn unauthenticated_ui_api_and_streams_are_refused() {
    let (auth, _) = store();
    claimed(&auth);
    let (app, _) = app_signed_out(auth);
    // Pages: to the sign-in page, coming back afterwards.
    let (status, headers, _) = send(
        &app,
        req("GET", "/settings/agents?x=1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        headers[header::LOCATION],
        "/login?next=/settings/agents%3Fx%3D1"
    );
    // htmx: a whole-page redirect.
    let (status, headers, _) = send(
        &app,
        req("GET", "/ui/cluster/apps")
            .header("hx-request", "true")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(headers["hx-redirect"]
        .to_str()
        .unwrap()
        .starts_with("/login"));
    // The API.
    for (method, uri) in [
        ("GET", "/api/lead/status"),
        ("GET", "/api/confirmations"),
        ("POST", "/api/lead/messages"),
    ] {
        let (status, _, body) = send(
            &app,
            req(method, uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{\"text\":\"hi\"}"))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}: {body}");
    }
    // Server-sent events.
    let (status, _, _) = send(
        &app,
        req("GET", "/ui/stream?since=0")
            .header(header::ACCEPT, "text/event-stream")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // A forged cookie is no better, and is cleared.
    let (status, headers, _) = send(
        &app,
        req("GET", "/api/lead/status")
            .header(header::COOKIE, format!("{COOKIE_PLAIN}=forged"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(set_cookie_of(&headers).contains("Max-Age=0"));
    // The sandboxed agents' MCP endpoint is unchanged: unknown token, 404 (not 401).
    let (status, _, _) = send(
        &app,
        req("POST", "/mcp/nope")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn login_logout_and_lockout_over_http() {
    let (auth, clock) = store();
    claimed(&auth);
    let (app, _) = app_signed_out(auth.clone());
    let (status, _, page) = send(
        &app,
        req("GET", "/login?next=//evil.example")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        page.contains("name=\"next\" value=\"/\""),
        "never another site"
    );
    assert!(page.contains("autocomplete=\"current-password\""));

    let bad = "username=ana&password=nope-nope";
    let (status, _, page) = send(
        &app,
        form_post("/login", bad).body(Body::from(bad)).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(page.contains("Wrong username or password."));
    for _ in 0..9 {
        clock.fetch_add(61, Ordering::SeqCst);
        send(
            &app,
            form_post("/login", bad).body(Body::from(bad)).unwrap(),
        )
        .await;
    }
    clock.fetch_add(61, Ordering::SeqCst);
    let good = "username=ana&password=owner-password";
    let (status, _, page) = send(
        &app,
        form_post("/login", good).body(Body::from(good)).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        page.contains("Try again in 14 minutes"),
        "one minute of the 15 has passed: {page}"
    );
    clock.fetch_add(melissa_auth::LOCKOUT_SECS, Ordering::SeqCst);
    let (status, headers, _) = send(
        &app,
        form_post("/login", good).body(Body::from(good)).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let token = token_of(&set_cookie_of(&headers));
    let s = auth.session(&token).unwrap();

    // Sign out: needs the CSRF token, ends the session, clears the cookie.
    let c = format!("{COOKIE_PLAIN}={token}");
    let (status, _, _) = send(
        &app,
        req("POST", "/logout")
            .header(header::COOKIE, &c)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, headers, _) = send(
        &app,
        req("POST", "/logout")
            .header(header::COOKIE, &c)
            .header(CSRF_HEADER, &s.csrf)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/login");
    assert!(set_cookie_of(&headers).contains("Max-Age=0"));
    assert!(auth.session(&token).is_none());
}

#[tokio::test]
async fn sessions_expire_and_revoked_ones_stop_working() {
    let (auth, clock) = store();
    let owner = claimed(&auth);
    let other = auth
        .login("ana", "owner-password", "test", "Phone")
        .unwrap();
    let (app, _) = app_signed_out(auth.clone());
    let ok = |s: &NewSession| {
        as_(s, "GET", "/api/lead/status")
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(send(&app, ok(&other)).await.0, StatusCode::OK);

    // The account page lists both; "Sign out" on the other one.
    let (_, _, page) = send(
        &app,
        as_(&owner, "GET", "/settings/account")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(
        page.contains("This browser") && page.contains("Phone"),
        "{page}"
    );
    let uri = format!("/ui/account/sessions/{}/revoke", other.view.id);
    let (status, _, _) = send(
        &app,
        as_(&owner, "POST", &uri)
            .header(CSRF_HEADER, &owner.view.csrf)
            .header("hx-request", "true")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(send(&app, ok(&other)).await.0, StatusCode::UNAUTHORIZED);

    // Idle timeout.
    clock.fetch_add(melissa_auth::SESSION_IDLE_SECS, Ordering::SeqCst);
    assert_eq!(send(&app, ok(&owner)).await.0, StatusCode::UNAUTHORIZED);

    // Sign out everywhere ends this browser's session too.
    let a = auth.login("ana", "owner-password", "test", "").unwrap();
    let b = auth.login("ana", "owner-password", "test", "").unwrap();
    let (status, headers, _) = send(
        &app,
        as_(&a, "POST", "/ui/account/sessions/all")
            .header(CSRF_HEADER, &a.view.csrf)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/login");
    assert_eq!(send(&app, ok(&a)).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(send(&app, ok(&b)).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mutations_without_the_csrf_token_are_refused() {
    let (auth, _) = store();
    let owner = claimed(&auth);
    let (app, lead) = app_signed_out(auth);
    let body = "pinned=on";
    // No token at all.
    let (status, _, page) = send(
        &app,
        as_(&owner, "POST", "/ui/settings/agents/lead/pin")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(page.contains("out of date"));
    // Another session's token.
    let other: String = owner.view.csrf.chars().rev().collect();
    let (status, _, _) = send(
        &app,
        as_(&owner, "POST", "/api/lead/cancel")
            .header(CSRF_HEADER, other)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // JSON API: the header.
    let (status, _, _) = send(
        &app,
        as_(&owner, "POST", "/api/lead/cancel")
            .header(CSRF_HEADER, &owner.view.csrf)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_ne!(status, StatusCode::FORBIDDEN);
    // A form without script: the hidden field works and the handler never sees it.
    let body = format!("_csrf={}&name=Hazel", owner.view.csrf);
    let (status, _, _) = send(
        &app,
        as_(&owner, "POST", "/ui/settings/agents/lead/rename")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(lead.agents().lead_name(), "Hazel");
    // Pages carry the token in their forms.
    let (_, _, page) = send(
        &app,
        as_(&owner, "GET", "/settings/agents/lead")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(
        page.contains(&format!("name=\"_csrf\" value=\"{}\"", owner.view.csrf)),
        "{page}"
    );
    // Cross-site posts are still stopped by the same-origin check first.
    let (status, _, _) = send(
        &app,
        as_(&owner, "POST", "/api/lead/cancel")
            .header(CSRF_HEADER, &owner.view.csrf)
            .header("sec-fetch-site", "cross-site")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn multipart_forms_carry_the_token_as_their_first_part() {
    let (auth, _) = store();
    let owner = claimed(&auth);
    let (app, _) = app_signed_out(auth);
    let make = |token: &str| {
        let body = format!(
            "--XyZ\r\nContent-Disposition: form-data; name=\"_csrf\"\r\n\r\n{token}\r\n--XyZ\r\nContent-Disposition: form-data; name=\"text\"\r\n\r\nhello\r\n--XyZ--\r\n"
        );
        as_(&owner, "POST", "/ui/messages")
            .header(header::CONTENT_TYPE, "multipart/form-data; boundary=XyZ")
            .body(Body::from(body))
            .unwrap()
    };
    let (status, _, _) = send(&app, make("wrong")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = send(&app, make(&owner.view.csrf)).await;
    assert_ne!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn provider_usage_is_the_owners_only() {
    let (auth, _) = store();
    let owner = claimed(&auth);
    let bob = member(&auth, &owner);
    let (app, _) = app_signed_out(auth);
    let uri = "/api/usage/providers";
    let (status, _, _) = send(&app, req("GET", uri).body(Body::empty()).unwrap()).await;
    assert!(
        status == StatusCode::UNAUTHORIZED || status.is_redirection(),
        "signed out: {status}"
    );
    let (status, _, _) = send(&app, as_(&bob, "GET", uri).body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, body) = send(&app, as_(&owner, "GET", uri).body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json["providers"].is_array(), "{body}");
    assert!(json["generated_at"].is_u64(), "{body}");
}

#[tokio::test]
async fn owner_only_sections_refuse_members() {
    let (auth, _) = store();
    let owner = claimed(&auth);
    let bob = member(&auth, &owner);
    let (app, _) = app_signed_out(auth);
    for uri in [
        "/settings/people",
        "/settings/providers",
        "/settings/updates",
        "/settings/apps",
        "/api/settings/providers",
        "/api/settings/updates",
    ] {
        let (status, _, _) = send(&app, as_(&bob, "GET", uri).body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
        let (status, _, _) = send(&app, as_(&owner, "GET", uri).body(Body::empty()).unwrap()).await;
        assert_ne!(status, StatusCode::FORBIDDEN, "{uri}");
    }
    for uri in [
        "/ui/people/invites",
        "/ui/apps/nextcloud/force-delete",
        "/api/apps/nextcloud/force-delete",
        "/ui/settings/updates/check",
    ] {
        let (status, _, _) = send(
            &app,
            as_(&bob, "POST", uri)
                .header(CSRF_HEADER, &bob.view.csrf)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
    }
    // Members keep everything else, and Settings opens on a section they have.
    let (status, _, _) = send(
        &app,
        as_(&bob, "GET", "/api/lead/status")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // `/settings` is the list over the Agents section, with no owner section or value in it.
    let (status, _, list) = send(
        &app,
        as_(&bob, "GET", "/settings").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(list.contains("data-section=\"/settings/agents\" data-list"));
    assert!(
        !list.contains("href=\"/settings/providers\"")
            && !list.contains("settings-label\">Updates<")
    );
    let (_, _, page) = send(
        &app,
        as_(&bob, "GET", "/settings/agents")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(
        !page.contains("href=\"/settings/providers\"")
            && !page.contains("href=\"/settings/people\"")
    );
    assert!(page.contains("href=\"/settings/account\""));
}

#[tokio::test]
async fn sensitive_actions_need_the_password_again() {
    let (auth, clock) = store();
    let owner = claimed(&auth);
    let (app, _) = app_signed_out(auth.clone());
    // Fresh from signing in: People works.
    let invite = |s: &NewSession| {
        as_(s, "POST", "/ui/people/invites")
            .header(CSRF_HEADER, &s.view.csrf)
            .header("hx-request", "true")
            .header("hx-current-url", format!("http://{HOST}/settings/people"))
            .body(Body::empty())
            .unwrap()
    };
    let (status, _, region) = send(&app, invite(&owner)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        region.contains(&format!("http://{HOST}/invite/")),
        "{region}"
    );

    clock.fetch_add(melissa_auth::REAUTH_SECS, Ordering::SeqCst);
    let (status, headers, _) = send(&app, invite(&owner)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(headers["hx-redirect"], "/reauth?next=/settings/people");
    assert_eq!(auth.invites().len(), 1, "nothing made");
    // Every kind of sensitive action is held the same way.
    for uri in [
        "/ui/apps/nextcloud/setup-password/reveal",
        "/ui/apps/nextcloud/setup-password/dismiss",
        "/ui/apps/nextcloud/force-delete",
        "/ui/settings/apps",
        "/ui/settings/providers/claude/enabled",
        "/ui/people/p-1/remove",
    ] {
        let (status, _, _) = send(
            &app,
            as_(&owner, "POST", uri)
                .header(CSRF_HEADER, &owner.view.csrf)
                .header("hx-request", "true")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
    }
    // The API says so too.
    let (status, _, body) = send(
        &app,
        as_(&owner, "POST", "/api/settings/providers/claude")
            .header(CSRF_HEADER, &owner.view.csrf)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("reauth"));

    // The password page: wrong, then right; back to where it was.
    let (status, _, page) = send(
        &app,
        as_(&owner, "GET", "/reauth?next=/settings/people")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("name=\"next\" value=\"/settings/people\""));
    let post = |pw: &str| {
        let body = format!(
            "_csrf={}&password={pw}&next=%2Fsettings%2Fpeople",
            owner.view.csrf
        );
        as_(&owner, "POST", "/reauth")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap()
    };
    let (status, _, page) = send(&app, post("wrong-pass")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(page.contains("Wrong username or password."));
    let (status, headers, _) = send(&app, post("owner-password")).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/settings/people");
    let (status, _, _) = send(&app, invite(&owner)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(auth.invites().len(), 2);
}

#[tokio::test]
async fn people_invites_resets_and_removal() {
    let (auth, _) = store();
    let owner = claimed(&auth);
    let (app, _) = app_signed_out(auth.clone());
    let post = |uri: &str| {
        as_(&owner, "POST", uri)
            .header(CSRF_HEADER, &owner.view.csrf)
            .header("hx-request", "true")
            .body(Body::empty())
            .unwrap()
    };
    let (_, _, region) = send(&app, post("/ui/people/invites")).await;
    let link = region.split(&format!("http://{HOST}")).nth(1).unwrap();
    let path = &link[..link.find('<').unwrap()];
    assert!(path.starts_with("/invite/"));

    // The invitee joins over HTTP (no session needed).
    let (status, _, page) = send(&app, req("GET", path).body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("Join Melissa"));
    let body = "username=bob&password=member-password&again=member-password";
    let (status, headers, _) =
        send(&app, form_post(path, body).body(Body::from(body)).unwrap()).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(set_cookie_of(&headers).starts_with("melissa_session="));
    // Single use.
    let (status, _, page) = send(&app, req("GET", path).body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(page.contains("used, expired or withdrawn"));
    let (status, _, _) = send(&app, form_post(path, body).body(Body::from(body)).unwrap()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let bob = auth
        .people()
        .into_iter()
        .find(|p| p.username == "bob")
        .unwrap();
    let (_, _, page) = send(
        &app,
        as_(&owner, "GET", "/settings/people")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(page.contains("bob") && page.contains("Owner (you)"));
    assert!(page.contains(&format!("/ui/people/{}/remove", bob.id)));
    assert!(
        !page.contains(&format!("/ui/people/{}/remove", owner.view.person.id)),
        "the owner stays"
    );

    // Reset: a link for bob, which sets his password.
    let (_, _, region) = send(&app, post(&format!("/ui/people/{}/reset", bob.id))).await;
    assert!(region.contains("Password reset link for bob"));
    let link = region.split(&format!("http://{HOST}")).nth(1).unwrap();
    let path = link[..link.find('<').unwrap()].to_string();
    let body = "password=fresh-password&again=fresh-password";
    let (status, _, _) = send(&app, form_post(&path, body).body(Body::from(body)).unwrap()).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(auth.login("bob", "fresh-password", "t", "").is_ok());

    // The owner can't be removed; a member can.
    let (_, _, region) = send(
        &app,
        post(&format!("/ui/people/{}/remove", owner.view.person.id)),
    )
    .await;
    assert!(
        region.contains("The owner can&#39;t be removed.")
            || region.contains("The owner can't be removed.")
    );
    let (_, _, region) = send(&app, post(&format!("/ui/people/{}/remove", bob.id))).await;
    assert!(region.contains("Removed bob."));
    assert_eq!(auth.people().len(), 1);
}

#[tokio::test]
async fn approvals_name_the_person_in_the_audit() {
    let (auth, _) = store();
    let owner = claimed(&auth);
    let (app, lead) = app_signed_out(auth);
    let dir = std::env::temp_dir().join(format!("melissa-web-auth-audit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let log = melissa_agent::AuditLog::new(dir.join("audit.jsonl"));
    // A record made while a request is handled carries the person.
    let record = melissa_agent::AUDIT_PERSON
        .scope("ana".into(), async {
            melissa_agent::AuditRecord::now("x", 0, "", "")
        })
        .await;
    log.append(&record).unwrap();
    let text = std::fs::read_to_string(log.path()).unwrap();
    assert!(text.contains("\"person\":\"ana\""));
    // An approval of a confirmation that no longer exists is still only for signed-in people.
    let (status, _, _) = send(
        &app,
        req("POST", "/api/confirmations/1/approve")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = send(
        &app,
        as_(&owner, "POST", "/api/confirmations/1/approve")
            .header(CSRF_HEADER, &owner.view.csrf)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let _ = lead;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_host_socket_speaks_for_the_owner() {
    let (auth, _) = store();
    let (app, _) = app_signed_out(auth.clone());
    let dir = std::env::temp_dir().join(format!("melissa-web-host-sock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("api.sock");
    let listener = crate::auth::bind_host(&path).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    let p = path.clone();
    let server = tokio::spawn(async move { crate::auth::serve_host(listener, &p, app).await });

    async fn over_socket(path: &std::path::Path, method: &str, uri: &str) -> (StatusCode, String) {
        let stream = tokio::net::UnixStream::connect(path).await.unwrap();
        let (mut sender, conn) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
                .await
                .unwrap();
        tokio::spawn(conn);
        let req = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, "localhost")
            .body(http_body_util::Full::new(axum::body::Bytes::new()))
            .unwrap();
        let res = sender.send_request(req).await.unwrap();
        let status = res.status();
        let body = res.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    // Before the claim: the setup code (a new one each restart), and the API works.
    let (status, body) = over_socket(&path, "GET", "/api/host/setup-code").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let code: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(code["code"].as_str().unwrap().len(), 11);
    let (status, _) = over_socket(&path, "GET", "/api/lead/status").await;
    assert_eq!(status, StatusCode::OK);
    // No owner yet: nothing to reset.
    let (status, _) = over_socket(&path, "POST", "/api/host/reset-owner").await;
    assert_eq!(status, StatusCode::CONFLICT);

    let owner = claimed(&auth);
    // The CLI needs no cookie and no CSRF token; owner-only paths work.
    let (status, _) = over_socket(&path, "POST", "/api/lead/cancel").await;
    assert_ne!(status, StatusCode::FORBIDDEN);
    assert_ne!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = over_socket(&path, "GET", "/api/settings/updates").await;
    assert_ne!(status, StatusCode::FORBIDDEN);
    let (status, body) = over_socket(&path, "GET", "/api/host/setup-code").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("claimed"));
    let (status, body) = over_socket(&path, "POST", "/api/host/reset-owner").await;
    assert_eq!(status, StatusCode::OK);
    let link: Value = serde_json::from_str(&body).unwrap();
    let token = link["path"]
        .as_str()
        .unwrap()
        .strip_prefix("/invite/")
        .unwrap();
    assert!(auth.invite(token).is_some());

    // Over TCP the host routes are not there, even for the owner.
    let (status, _, _) = send(
        &app_signed_out(auth.clone()).0,
        as_(&owner, "GET", "/api/host/setup-code")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    server.abort();
    let _ = std::fs::remove_dir_all(&dir);
}
