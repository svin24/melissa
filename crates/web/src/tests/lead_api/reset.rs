//! Settings → Reset (factory reset): the owner only, the password again, the typed word, no
//! reset while an update waits; the request is the marker the root unit acts on.

use std::sync::atomic::{AtomicU64, Ordering};

use melissa_auth::{Auth, Cost, NewSession};
use melissa_settings::factory_reset::{read_marker, Step, MARKER_NAME};

use super::*;
use crate::auth::{COOKIE_PLAIN, CSRF_HEADER};

const HOST: &str = "melissa.example.com";

struct Fx {
    app: Router,
    auth: Auth,
    clock: Arc<AtomicU64>,
    owner: NewSession,
    member: NewSession,
    settings: SettingsFixture,
}

fn fx(name: &str) -> Fx {
    let clock = Arc::new(AtomicU64::new(1_000_000));
    let c = clock.clone();
    let auth = Auth::in_memory()
        .with_cost(Cost::Test)
        .with_clock(Arc::new(move || c.load(Ordering::SeqCst)));
    let (code, _) = auth.setup_code().unwrap().unwrap();
    let owner = auth
        .claim(&code, "ana", "owner-password", "test", "Test")
        .unwrap();
    let inv = auth.create_invite(&owner.view.person.id, "test").unwrap();
    let member = auth
        .accept_invite(&inv.token, "bob", "member-password", "test", "Test")
        .unwrap();
    let settings = SettingsFixture::new(name);
    std::fs::create_dir_all(settings.dir.join("reset")).unwrap();
    let (board, mcp, lead) = lead_parts(None, false);
    let app = router_with_settings(
        board,
        mcp,
        lead,
        settings.settings(HostKind::Container, true),
        auth.clone(),
    );
    Fx {
        app,
        auth,
        clock,
        owner,
        member,
        settings,
    }
}

impl Fx {
    fn marker(&self) -> std::path::PathBuf {
        self.settings.dir.join("reset").join(MARKER_NAME)
    }
}

fn as_(s: &NewSession, method: &str, uri: &str) -> axum::http::request::Builder {
    axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, HOST)
        .header(header::COOKIE, format!("{COOKIE_PLAIN}={}", s.token))
        .header(CSRF_HEADER, &s.view.csrf)
}

/// The page's form, as htmx sends it.
fn ui_post(s: &NewSession, word: &str) -> axum::http::Request<Body> {
    as_(s, "POST", "/ui/settings/reset")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header("hx-request", "true")
        .header("hx-current-url", format!("http://{HOST}/settings/reset"))
        .body(Body::from(format!("confirm={word}")))
        .unwrap()
}

fn api_post(s: &NewSession, word: &str) -> axum::http::Request<Body> {
    as_(s, "POST", "/api/settings/reset")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(format!(r#"{{"confirm":"{word}"}}"#)))
        .unwrap()
}

#[tokio::test]
async fn the_owner_resets_after_typing_the_word() {
    let f = fx("reset-owner");
    let (status, _, page) = send(
        &f.app,
        as_(&f.owner, "GET", "/settings/reset")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for want in [
        "Reset Melissa",
        "Type <strong>melissa</strong> to confirm",
        "Erased",
        "Kept",
        "action=\"/ui/settings/reset\"",
    ] {
        assert!(page.contains(want), "{want}");
    }
    // No browser dialogs.
    assert!(!page.contains("hx-confirm") && !page.contains("confirm("));
    assert!(
        page.contains("href=\"/settings/reset\""),
        "listed in Settings"
    );

    // A wrong word is refused in the card; nothing is asked for.
    for wrong in ["", "Melissa", "reset", "melissa2"] {
        let (status, _, region) = send(&f.app, ui_post(&f.owner, wrong)).await;
        assert_eq!(status, StatusCode::OK, "{wrong}");
        assert!(
            region.contains("That&#39;s not the word.") || region.contains("That's not the word."),
            "{region}"
        );
        assert!(
            region.contains("<details") && region.contains(" open"),
            "the card stays open"
        );
        assert!(!f.marker().exists(), "{wrong}");
    }

    let (status, _, region) = send(&f.app, ui_post(&f.owner, "melissa")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(region.contains("Resetting Melissa"), "{region}");
    assert!(region.contains("hx-get=\"/reset-status\""));
    let m = read_marker(&f.marker()).unwrap().unwrap();
    assert!(
        m.step <= Step::OnlineDone && m.online && m.by == "ui",
        "{m:?}"
    );
    assert!(f.settings.audit().contains("factory_reset.requested"));

    // The card polls a public status: running (204) while the old Melissa still answers.
    let (status, _, _) = send(
        &f.app,
        axum::http::Request::get("/reset-status")
            .header(header::HOST, HOST)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // The page shows the reset until she is back.
    let (_, _, page) = send(
        &f.app,
        as_(&f.owner, "GET", "/settings/reset")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(page.contains("Resetting Melissa"));
    // Asking again changes nothing.
    let (status, _, _) = send(&f.app, api_post(&f.owner, "melissa")).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        f.settings
            .audit()
            .matches("factory_reset.requested")
            .count(),
        1
    );

    // The server's part ends by marking it done for the root unit.
    for _ in 0..200 {
        if read_marker(&f.marker()).unwrap().unwrap().step == Step::OnlineDone {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        read_marker(&f.marker()).unwrap().unwrap().step,
        Step::OnlineDone
    );
}

#[tokio::test]
async fn members_never_see_or_reach_it() {
    let f = fx("reset-member");
    let (status, _, _) = send(
        &f.app,
        as_(&f.member, "GET", "/settings/reset")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = send(&f.app, ui_post(&f.member, "melissa")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = send(&f.app, api_post(&f.member, "melissa")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(!f.marker().exists());
    let (_, _, page) = send(
        &f.app,
        as_(&f.member, "GET", "/settings/agents")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(!page.contains("href=\"/settings/reset\""));
}

#[tokio::test]
async fn an_old_password_check_sends_the_owner_to_enter_it_again() {
    let f = fx("reset-reauth");
    f.clock
        .fetch_add(melissa_auth::REAUTH_SECS, Ordering::SeqCst);
    let (status, headers, _) = send(&f.app, ui_post(&f.owner, "melissa")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(headers["hx-redirect"], "/reauth?next=/settings/reset");
    let (status, _, body) = send(&f.app, api_post(&f.owner, "melissa")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("reauth"));
    assert!(!f.marker().exists());
    // The page itself still opens (reading needs no fresh password).
    let (status, _, _) = send(
        &f.app,
        as_(&f.owner, "GET", "/settings/reset")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn the_api_checks_the_word_and_a_waiting_update() {
    let f = fx("reset-api");
    let (status, _, body) = send(&f.app, api_post(&f.owner, "nope")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("error"));
    assert!(!f.marker().exists());

    // An update waits to install: no reset now, and the page says why.
    std::fs::write(
        f.settings.dir.join("update/status.json"),
        include_str!("../../../../settings/tests/fixtures/container-status-staged.json"),
    )
    .unwrap();
    let (status, _, _) = send(&f.app, api_post(&f.owner, "melissa")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(!f.marker().exists());
    let (_, _, page) = send(
        &f.app,
        as_(&f.owner, "GET", "/settings/reset")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(page.contains("An update is waiting."));
    assert!(!page.contains("action=\"/ui/settings/reset\""));
    std::fs::remove_file(f.settings.dir.join("update/status.json")).unwrap();

    let (status, _, body) = send(&f.app, api_post(&f.owner, "melissa")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert!(f.marker().exists());
}

#[tokio::test]
async fn the_status_sends_the_browser_to_the_claim_once_melissa_is_back() {
    let f = fx("reset-status");
    let status_req = || {
        axum::http::Request::get("/reset-status")
            .header(header::HOST, HOST)
            .body(Body::empty())
            .unwrap()
    };
    // No reset happened: back to the start page.
    let (status, headers, _) = send(&f.app, status_req()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["hx-redirect"], "/");
    // Back unclaimed (a fresh login store, as after the wipe): the claim.
    let fresh = Auth::in_memory().with_cost(Cost::Test);
    let (board, mcp, lead) = lead_parts(None, false);
    let app = router_with_settings(
        board,
        mcp,
        lead,
        f.settings.settings(HostKind::Container, true),
        fresh,
    );
    let (status, headers, _) = send(&app, status_req()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["hx-redirect"], "/claim");
    let _ = &f.auth;
}

/// `melissa-server factory-reset --confirm <word>`: the host socket speaks for the owner.
#[tokio::test]
async fn the_host_cli_resets_over_its_socket() {
    let f = fx("reset-host");
    let dir = f.settings.dir.join("sock");
    let path = dir.join("api.sock");
    let listener = crate::auth::bind_host(&path).unwrap();
    let p = path.clone();
    let app = f.app.clone();
    let server = tokio::spawn(async move { crate::auth::serve_host(listener, &p, app).await });

    async fn post(path: &std::path::Path, body: &str) -> StatusCode {
        let stream = tokio::net::UnixStream::connect(path).await.unwrap();
        let (mut sender, conn) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
                .await
                .unwrap();
        tokio::spawn(conn);
        let req = axum::http::Request::post("/api/settings/reset")
            .header(header::HOST, "localhost")
            .header(header::CONTENT_TYPE, "application/json")
            .body(http_body_util::Full::new(axum::body::Bytes::from(
                body.to_string(),
            )))
            .unwrap();
        sender.send_request(req).await.unwrap().status()
    }

    assert_eq!(
        post(&path, r#"{"confirm":"yes"}"#).await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert!(!f.marker().exists());
    assert_eq!(
        post(&path, r#"{"confirm":"melissa"}"#).await,
        StatusCode::ACCEPTED
    );
    assert!(f.marker().exists());
    server.abort();
}
