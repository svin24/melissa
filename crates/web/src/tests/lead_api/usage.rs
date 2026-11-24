//! Usage and resources with the providers' limits (2026-11-22, owner design B1–B4): the owner's
//! views and their switcher, the providers' part loading after the page, Check again with and
//! without script, and a member who sees only the agents and devices. Checked on the server
//! with real sessions through the sign-in gate.

use melissa_auth::NewSession;

use super::*;

/// A request as `s`; `htmx`: sent by htmx (else a plain browser request).
fn req(s: &NewSession, method: &str, uri: &str, htmx: bool) -> axum::http::Request<Body> {
    let mut b = as_(s, method, uri);
    if htmx {
        b = b.header("hx-request", "true");
    }
    b.body(Body::empty()).unwrap()
}

async fn get(app: &Router, s: &NewSession, uri: &str) -> (StatusCode, String) {
    let (status, _, body) = send(app, req(s, "GET", uri, false)).await;
    (status, body)
}

#[tokio::test]
async fn the_providers_load_after_the_page_and_then_come_with_it() {
    let b = TestBox::new();
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
    let b = TestBox::new();
    let (status, _, part) = send(
        &b.app,
        req(&b.owner, "POST", "/ui/usage/check?view=cost", true),
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
        req(&b.owner, "POST", "/ui/usage/check?view=tokens", false),
    )
    .await;
    assert!(status.is_redirection(), "{status}");
    assert_eq!(headers[header::LOCATION], "/usage?view=tokens");
}

#[tokio::test]
async fn a_member_sees_the_agents_and_devices_but_no_provider_limits() {
    let b = TestBox::new();
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
        req(&b.member, "POST", "/ui/usage/check?view=limits", true),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
