//! First-run setup (2026-11-21): the owner goes through Device → Provider → Model → Extras
//! after the claim; members never see it; "Later" lets the chat open and lists the step.

use melissa_auth::{Auth, Cost, NewSession};

use super::*;
use crate::auth::{COOKIE_PLAIN, CSRF_HEADER};

struct Box_ {
    app: Router,
    owner: NewSession,
    member: NewSession,
    fx: SettingsFixture,
}

/// The full router with setup on, an owner and a member. `signed_out`: the lead agent's
/// provider isn't signed in (so no provider works).
fn setup_box(name: &str, onboarding: bool, signed_out: bool) -> Box_ {
    let mut fx = SettingsFixture::new(name);
    fx.onboarding = onboarding;
    let settings = fx.settings(HostKind::Container, false);
    let auth = Auth::in_memory().with_cost(Cost::Test);
    let (code, _) = auth.setup_code().unwrap().unwrap();
    let owner = auth
        .claim(&code, "ana", "owner-password", "test", "Test")
        .unwrap();
    let inv = auth.create_invite(&owner.view.person.id, "test").unwrap();
    let member = auth
        .accept_invite(&inv.token, "bob", "member-password", "test", "Test")
        .unwrap();
    let (board, mcp, lead) = lead_parts(None, signed_out);
    let app = router_with_settings(board, mcp, lead, settings, auth);
    Box_ {
        app,
        owner,
        member,
        fx,
    }
}

fn as_get(s: &NewSession, uri: &str) -> axum::http::Request<Body> {
    axum::http::Request::get(uri)
        .header(header::HOST, "127.0.0.1:8080")
        .header(header::COOKIE, format!("{COOKIE_PLAIN}={}", s.token))
        .body(Body::empty())
        .unwrap()
}

fn as_post(s: &NewSession, uri: &str) -> axum::http::request::Builder {
    post(uri)
        .header(header::COOKIE, format!("{COOKIE_PLAIN}={}", s.token))
        .header(CSRF_HEADER, s.view.csrf.clone())
}

async fn go(b: &Box_, s: &NewSession, uri: &str, body: &str) -> (StatusCode, HeaderMap, String) {
    send(
        &b.app,
        as_post(s, uri).body(Body::from(body.to_string())).unwrap(),
    )
    .await
}

fn location(h: &HeaderMap) -> &str {
    h.get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

#[tokio::test]
async fn the_owner_goes_through_setup_and_members_never_see_it() {
    let b = setup_box("setup-flow", true, false);

    // Nothing done: the owner's home and chat go to the first step.
    for uri in ["/", "/chat", "/chat?draft=hi"] {
        let (status, h, _) = send(&b.app, as_get(&b.owner, uri)).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{uri}");
        assert_eq!(location(&h), "/setup/device", "{uri}");
    }
    let (_, h, _) = send(&b.app, as_get(&b.owner, "/setup")).await;
    assert_eq!(location(&h), "/setup/device");

    // A member goes straight to the chat and can't use setup.
    let (status, _, page) = send(&b.app, as_get(&b.member, "/chat")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!page.contains("setup-checklist"), "{page}");
    let (status, h, _) = send(&b.app, as_get(&b.member, "/setup/device")).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location(&h), "/chat");
    let (status, _, _) = go(&b, &b.member, "/ui/setup/device", "name=Mine").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = go(&b, &b.member, "/ui/setup/device/later", "").await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The Device step: one field, one button, "Later".
    let (status, _, page) = send(&b.app, as_get(&b.owner, "/setup/device")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("Name this device") && page.contains("name=\"name\""));
    assert!(page.contains("Save name") && page.contains("/ui/setup/device/later"));
    assert!(page.contains("aria-current=\"step\""));
    // An empty name is refused in the page.
    let (status, _, page) = go(&b, &b.owner, "/ui/setup/device", "name=+++").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(page.contains("Give the device a name."));
    // Saved: Provider is next, even though one works already (the lead agent's): the owner
    // picks which one there.
    let (status, h, _) = go(&b, &b.owner, "/ui/setup/device", "name=+Living+room+").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location(&h), "/setup/provider");
    let ui_toml = std::fs::read_to_string(b.fx.dir.join("config/ui.toml")).unwrap();
    assert!(
        ui_toml.contains("device_name = \"Living room\""),
        "{ui_toml}"
    );
    let (_, h, _) = send(&b.app, as_get(&b.owner, "/chat")).await;
    assert_eq!(location(&h), "/setup/model");

    // Model: put off. The chat opens and lists the step.
    let (status, _, page) = send(&b.app, as_get(&b.owner, "/setup/model")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("Use this model") && page.contains("name=\"choice\""));
    let (status, h, _) = go(&b, &b.owner, "/ui/setup/model/later", "").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location(&h), "/setup/extras");
    let (status, _, page) = send(&b.app, as_get(&b.owner, "/chat")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("Finish setup") && page.contains("Choose Melissa's model"));
    assert!(page.contains("href=\"/setup/model\""));
    // The device's new name shows where the UI names the device.
    assert!(page.contains("Living room"), "{page}");
    // The fixed suggestions fill the message box through the chat link.
    assert!(page.contains("/chat?draft=What%27s%20running%20on%20this%20device%3F"));
    // A member doesn't get the owner's checklist.
    let (_, _, page) = send(&b.app, as_get(&b.member, "/chat")).await;
    assert!(!page.contains("setup-checklist"));

    // The Model step with a provider that doesn't work lists every working one.
    let (_, _, page) = send(&b.app, as_get(&b.owner, "/setup/model?p=gemini")).await;
    assert!(page.contains("value=\"stuck\""), "{page}");
    // An unknown provider doesn't narrow the list or reach the poll's URL.
    let (_, _, page) = send(&b.app, as_get(&b.owner, "/setup/model?p=x%26y")).await;
    assert!(page.contains("value=\"stuck\"") && !page.contains("x&amp;y"));

    // The model chosen: nothing left, no checklist, and the chat is still empty (its
    // greeting and suggestions, no "Switched to" line).
    let (status, h, _) = go(&b, &b.owner, "/ui/setup/model", "choice=stuck").await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{h:?}");
    assert_eq!(location(&h), "/setup/extras");
    let (status, _, page) = send(&b.app, as_get(&b.owner, "/chat")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!page.contains("setup-checklist"));
    assert!(
        page.contains("Hi, I&#39;m") || page.contains("Hi, I'm"),
        "{page}"
    );
    assert!(page.contains("class=\"suggestions\""));
    assert!(!page.contains("Switched to"), "{page}");

    // Extras: neither voice input nor Tailscale is possible in this box: one line and
    // "Finish setup" into the chat, no switches and no "Later".
    let (status, _, page) = send(&b.app, as_get(&b.owner, "/setup/extras")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("Voice input and Tailscale aren"));
    assert!(!page.contains("voice-settings") && !page.contains("tailscale-settings"));
    assert!(!page.contains("/ui/setup/extras/later"));
    assert!(page.contains("Finish setup") && page.contains("href=\"/chat\""));
}

#[tokio::test]
async fn later_is_kept_and_the_checklist_can_be_hidden() {
    let b = setup_box("setup-later", true, false);
    for step in ["device", "model", "extras"] {
        let (status, _, _) = go(&b, &b.owner, &format!("/ui/setup/{step}/later"), "").await;
        assert_eq!(status, StatusCode::SEE_OTHER);
    }
    let ui_toml = std::fs::read_to_string(b.fx.dir.join("config/ui.toml")).unwrap();
    assert!(
        ui_toml.contains("skipped = [\"device\", \"model\", \"extras\"]"),
        "{ui_toml}"
    );
    let (status, _, page) = send(&b.app, as_get(&b.owner, "/chat")).await;
    assert_eq!(status, StatusCode::OK);
    for line in ["Name this device", "Tailscale and voice", "Later"] {
        assert!(page.contains(line), "{line}");
    }
    // Hide: htmx removes the card; it stays hidden.
    let (status, _, body) = send(
        &b.app,
        as_post(&b.owner, "/ui/setup/dismiss")
            .header("hx-request", "true")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty());
    let (_, _, page) = send(&b.app, as_get(&b.owner, "/chat")).await;
    assert!(!page.contains("setup-checklist"));
}

#[tokio::test]
async fn the_provider_step_reuses_the_provider_forms() {
    let b = setup_box("setup-provider", true, false);
    let (status, _, page) = send(&b.app, as_get(&b.owner, "/setup/provider")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("class=\"ptile\"") && page.contains("/setup/provider?p=grok"));
    // A provider works: one way on to the model.
    assert!(page.contains("Choose a model"));
    // The Settings → Providers handlers answer with this step when asked from it.
    let (status, _, region) = send(
        &b.app,
        as_post(&b.owner, "/ui/settings/providers/refresh")
            .header("hx-request", "true")
            .header(
                "hx-current-url",
                "http://127.0.0.1:8080/setup/provider?p=grok",
            )
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(region.contains("Choose a model") && region.contains("ptile"));
    assert!(!region.contains("Add provider"));
    // From Settings they still get the Settings region.
    let (_, _, region) = send(
        &b.app,
        as_post(&b.owner, "/ui/settings/providers/refresh")
            .header("hx-request", "true")
            .header("hx-current-url", "http://127.0.0.1:8080/settings/providers")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(region.contains("Add provider") && !region.contains("Choose a model"));
}

#[tokio::test]
async fn the_chat_says_when_no_provider_works() {
    let b = setup_box("setup-none", false, true);
    let (status, _, page) = send(&b.app, as_get(&b.owner, "/chat")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("No provider is ready yet."));
    assert!(page.contains("href=\"/setup/provider\""));
    // Nothing to suggest while nothing can answer.
    assert!(!page.contains("class=\"suggestions\""));
    // A member reads the same, without the owner's link.
    let (_, _, page) = send(&b.app, as_get(&b.member, "/chat")).await;
    assert!(page.contains("No provider is ready yet."));
    assert!(!page.contains("href=\"/setup/provider\""));
    // Setup off (this router): the owner isn't sent anywhere.
    let (status, _, _) = send(&b.app, as_get(&b.owner, "/")).await;
    assert_eq!(status, StatusCode::OK);
}
