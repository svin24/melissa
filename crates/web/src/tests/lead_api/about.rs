//! Settings → About (2026-11-22, owner): for the owner and a member alike, last in the section
//! list, its links open a new tab without handing over the page, and the texts are built in.
//! Checked on the server with real sessions through the sign-in gate.

use melissa_auth::{Auth, Cost, NewSession};

use super::*;
use crate::auth::COOKIE_PLAIN;

struct Two {
    app: Router,
    owner: NewSession,
    member: NewSession,
}

fn two() -> Two {
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
    Two { app, owner, member }
}

/// A GET as `s`; `target`: the htmx target (a section swapped into `#settings-main`).
async fn get_as(
    app: &Router,
    s: &NewSession,
    uri: &str,
    target: Option<&str>,
) -> (StatusCode, String) {
    let mut b = axum::http::Request::get(uri)
        .header(header::HOST, "melissa.example.com")
        .header(header::COOKIE, format!("{COOKIE_PLAIN}={}", s.token));
    if let Some(t) = target {
        b = b.header("hx-request", "true").header("hx-target", t);
    }
    let (status, _, body) = send(app, b.body(Body::empty()).unwrap()).await;
    (status, body)
}

#[tokio::test]
async fn the_owner_and_a_member_both_open_it() {
    let t = two();
    let version =
        melissa_settings::updates::Release::read(&melissa_settings::updates::release_file())
            .version;
    for s in [&t.owner, &t.member] {
        let (status, page) = get_as(&t.app, s, "/settings/about", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("<h1 class=\"settings-here\" tabindex=\"-1\">About</h1>"));
        assert!(page.contains("<p class=\"setting-title\">Melissa</p>"));
        assert!(page.contains(&format!("Version {version}")));
        assert!(page.contains("href=\"/settings/about\" aria-current=\"page\""));
        // No tagline (owner, 2026-11-22).
        assert!(!page.contains("AI agents run for you"));
    }
}

#[tokio::test]
async fn it_is_last_in_the_list_with_the_version_on_a_phone() {
    let t = two();
    let version =
        melissa_settings::updates::Release::read(&melissa_settings::updates::release_file())
            .version;
    for s in [&t.owner, &t.member] {
        let (_, list) = get_as(&t.app, s, "/settings", None).await;
        let nav = &list[list.find("id=\"settings-sections\"").unwrap()..];
        let nav = &nav[..nav.find("</nav>").unwrap()];
        let last = nav.rfind("<li ").unwrap();
        assert!(
            nav[last..].contains("href=\"/settings/about\""),
            "About is the last section"
        );
        assert!(
            nav.rfind(">Melissa</p>").unwrap() < last,
            "in its own group"
        );
        assert!(nav[last..].contains(&format!(
            "<span class=\"settings-label\">About</span><span class=\"settings-value\">{version}</span>"
        )));
    }
}

#[tokio::test]
async fn its_links_open_a_new_tab_and_say_so() {
    let t = two();
    let (_, page) = get_as(&t.app, &t.member, "/settings/about", None).await;
    for (href, label) in [
        ("https://github.com/sponsors/svin24", "Sponsor"),
        ("https://github.com/svin24/melissa", "Source code"),
        (
            "https://github.com/svin24/melissa/issues",
            "Report a problem",
        ),
    ] {
        let at = page
            .find(&format!(
                "href=\"{href}\" target=\"_blank\" rel=\"noopener noreferrer\">"
            ))
            .unwrap_or_else(|| panic!("{href}"));
        let link = &page[at..at + page[at..].find("</a>").unwrap()];
        assert!(link.contains(label), "{href}");
        assert!(link.contains("(opens in a new tab)"), "{href}");
    }
    // The heart: the Sponsor button's glyph, hidden from screen readers, label visible.
    let sponsor = &page[page.find("https://github.com/sponsors/svin24").unwrap()..];
    let sponsor = &sponsor[..sponsor.find("</a>").unwrap()];
    assert!(sponsor.contains("<svg class=\"glyph\" viewBox=\"0 0 16 16\" aria-hidden=\"true\""));
    assert!(sponsor.contains("class=\"heart\""));
    assert!(sponsor.contains("Sponsor<span class=\"visually-hidden\">"));
    // Nothing remote is loaded: no images, frames or scripts from elsewhere.
    for bad in ["<img", "<iframe", "src=\"http"] {
        assert!(!page.contains(bad), "{bad}");
    }
}

#[tokio::test]
async fn the_license_and_the_credits_are_built_in() {
    let t = two();
    let (_, page) = get_as(&t.app, &t.owner, "/settings/about", None).await;
    assert!(page.contains("MIT License"));
    assert!(page.contains("Permission is hereby granted, free of charge"));
    // The credits: all of ACKNOWLEDGMENTS.md, T3 Code included, links in a new tab.
    let credits = &page[page.find(">Credits<").unwrap()..];
    assert!(credits.contains("T3 Code"));
    assert!(credits.contains(
        "<a href=\"https://github.com/pingdotgg/t3code\" target=\"_blank\" rel=\"noopener noreferrer nofollow\">"
    ));
    for name in ["Universal Blue", "Hermes Agent", "tasteskill"] {
        assert!(credits.contains(name), "{name}");
    }
    // The bundled fonts' and scripts' texts.
    assert!(page.contains("SIL OPEN FONT LICENSE Version 1.1"));
    assert!(page.contains("BSD Zero Clause License"));
}

#[tokio::test]
async fn search_finds_it() {
    let t = two();
    let (_, page) = get_as(&t.app, &t.member, "/settings/about", None).await;
    let at = page
        .find("<li data-keywords=\"About ")
        .expect("About's search words");
    let words = &page[at..at + page[at..].find("\">").unwrap()].to_lowercase();
    for w in [
        "about", "version", "license", "licence", "credits", "sponsor", "donate", "source",
        "github",
    ] {
        assert!(words.contains(w), "{w}");
    }
}

#[tokio::test]
async fn it_swaps_in_place() {
    let t = two();
    let (status, part) = get_as(&t.app, &t.member, "/settings/about", Some("settings-main")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!part.contains("<html"));
    assert!(part.contains("data-section=\"/settings/about\""));
    assert!(part.contains("<p class=\"setting-title\">Melissa</p>"));
}
