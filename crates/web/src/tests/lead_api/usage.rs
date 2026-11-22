//! Usage and resources with the providers' limits (2026-11-22, owner design B1–B4): the owner's
//! views and their switcher, the providers' part loading after the page, Check again with and
//! without script, and a member who sees only the agents and devices. Checked on the server
//! with real sessions through the sign-in gate.

use melissa_auth::{Auth, Cost, NewSession};

use super::*;
use crate::auth::{COOKIE_PLAIN, CSRF_HEADER};

struct Box2 {
    app: Router,
    owner: NewSession,
    member: NewSession,
}

fn box2() -> Box2 {
    let auth = Auth::in_memory().with_cost(Cost::Test);
    let (code, _) = auth.setup_code().unwrap().unwrap();
    let owner = auth
        .claim(&code, "ana", "owner-password", "test", "Test")
        .unwrap();
    let inv = auth.create_invite(&owner.view.person.id, "test").unwrap();
    let member = auth
        .accept_invite(&inv.token, "bob", "member-password", "test", "Test")
        .unwrap();
    let (board, mcp, lead) = lead_parts_with(None, false, Vec::new());
    let app = router_with_settings(board, mcp, lead, Settings::detached(), auth);
    Box2 { app, owner, member }
}

/// A request as `s`; `htmx`: sent by htmx (else a plain browser request).
fn as_(s: &NewSession, method: &str, uri: &str, htmx: bool) -> axum::http::Request<Body> {
    let mut b = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "melissa.example.com")
        .header(header::COOKIE, format!("{COOKIE_PLAIN}={}", s.token))
        .header(CSRF_HEADER, s.view.csrf.clone())
        .header("sec-fetch-site", "same-origin")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    if htmx {
        b = b.header("hx-request", "true");
    }
    b.body(Body::empty()).unwrap()
}

async fn get(app: &Router, s: &NewSession, uri: &str) -> (StatusCode, String) {
    let (status, _, body) = send(app, as_(s, "GET", uri, false)).await;
    (status, body)
}

#[tokio::test]
async fn the_owner_switches_between_limits_cost_and_tokens() {
    let b = box2();
    let (status, body) = get(&b.app, &b.owner, "/usage").await;
    assert_eq!(status, StatusCode::OK);
    // The switcher: plain links with htmx on top, Limits current by default.
    for want in [
        "<nav class=\"show-filter usage-tabs\" aria-label=\"Usage views\">",
        "href=\"/usage?view=limits\" hx-get=\"/ui/usage/view?view=limits\"",
        "href=\"/usage?view=cost\" hx-get=\"/ui/usage/view?view=cost\"",
        "href=\"/usage?view=tokens\"",
        "hx-target=\"#usage-view\"",
        "aria-current=\"page\">Limits</a>",
        "Check again",
        // Limits keeps the agents and the devices below the providers.
        "Approvals asked",
        "<h2>Devices</h2>",
    ] {
        assert!(body.contains(want), "missing {want}");
    }
    assert!(!body.contains("role=\"tablist\""));

    let (_, cost) = get(&b.app, &b.owner, "/usage?view=cost").await;
    assert!(cost.contains("aria-current=\"page\">Cost</a>"), "{cost}");
    assert!(cost.contains("By agent"));
    assert!(cost.contains("Melissa never estimates cost."));
    assert!(
        !cost.contains("Approvals asked"),
        "the table is under Limits only"
    );

    // The switcher's region: the view alone, not a page.
    let (status, tokens) = get(&b.app, &b.owner, "/ui/usage/view?view=tokens").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        tokens.starts_with("<div class=\"usage-view\" id=\"usage-view\">"),
        "{tokens}"
    );
    assert!(tokens.contains("aria-current=\"page\">Tokens</a>"));
    assert!(tokens.contains("Context, by agent"));
    assert!(!tokens.contains("<html"));

    // Anything else is Limits.
    let (_, odd) = get(&b.app, &b.owner, "/usage?view=nonsense").await;
    assert!(odd.contains("aria-current=\"page\">Limits</a>"));
}

#[tokio::test]
async fn the_providers_load_after_the_page_and_then_come_with_it() {
    let b = box2();
    let (_, first) = get(&b.app, &b.owner, "/usage").await;
    // Nothing asked yet: the page doesn't wait, the part loads itself.
    assert!(
        first.contains("hx-get=\"/ui/usage/providers?view=limits\" hx-trigger=\"load\""),
        "{first}"
    );
    let (status, part) = get(&b.app, &b.owner, "/ui/usage/providers?view=limits").await;
    assert_eq!(status, StatusCode::OK);
    assert!(part.contains("id=\"usage-providers\""), "{part}");
    assert!(!part.contains("hx-trigger=\"load\""));
    // "Checked …" comes along out of band.
    assert!(
        part.contains("id=\"usage-checked\" role=\"status\" hx-swap-oob=\"true\">Checked just now")
    );
    // Now fresh: the next page has the numbers in it.
    let (_, again) = get(&b.app, &b.owner, "/usage").await;
    assert!(!again.contains("hx-trigger=\"load\""), "{again}");
    assert!(again.contains("Checked just now"));
}

#[tokio::test]
async fn check_again_refreshes_in_place_and_without_script_goes_back_to_the_page() {
    let b = box2();
    let (status, _, part) = send(
        &b.app,
        as_(&b.owner, "POST", "/ui/usage/check?view=cost", true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        part.starts_with("<div class=\"usage-providers\" id=\"usage-providers\">"),
        "{part}"
    );
    assert!(part.contains("hx-swap-oob=\"true\">Checked just now"));
    // The button posts a form (works without script) and htmx swaps only the providers' part,
    // dropping presses while one check runs.
    let (_, page) = get(&b.app, &b.owner, "/usage?view=cost").await;
    assert!(page.contains(
        "<form method=\"post\" action=\"/ui/usage/check?view=cost\" hx-post=\"/ui/usage/check?view=cost\" hx-target=\"#usage-providers\" hx-swap=\"outerHTML\" hx-sync=\"this:drop\">"
    ), "{page}");

    let (status, headers, _) = send(
        &b.app,
        as_(&b.owner, "POST", "/ui/usage/check?view=tokens", false),
    )
    .await;
    assert!(status.is_redirection(), "{status}");
    assert_eq!(headers[header::LOCATION], "/usage?view=tokens");
}

#[tokio::test]
async fn a_member_sees_the_agents_and_devices_but_no_provider_limits() {
    let b = box2();
    let (status, body) = get(&b.app, &b.member, "/usage?view=limits").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Approvals asked"));
    assert!(body.contains("<h2>Devices</h2>"));
    for absent in [
        "usage-tabs",
        "usage-providers",
        "Check again",
        "Plan limits",
    ] {
        assert!(!body.contains(absent), "unexpected {absent}");
    }
    for uri in [
        "/ui/usage/view?view=limits",
        "/ui/usage/providers?view=limits",
    ] {
        let (status, _) = get(&b.app, &b.member, uri).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
    }
    let (status, _, _) = send(
        &b.app,
        as_(&b.member, "POST", "/ui/usage/check?view=limits", true),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
