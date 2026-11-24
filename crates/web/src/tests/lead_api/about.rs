//! Settings → About (2026-11-22, owner): for the owner and a member alike, last in the section
//! list, its links open a new tab without handing over the page, and the texts are built in.
//! Checked on the server with real sessions through the sign-in gate.

use melissa_auth::NewSession;

use super::*;

async fn get_as(app: &Router, s: &NewSession, uri: &str) -> (StatusCode, String) {
    let (status, _, body) = send(app, as_(s, "GET", uri).body(Body::empty()).unwrap()).await;
    (status, body)
}

#[tokio::test]
async fn the_owner_and_a_member_both_open_it() {
    let t = TestBox::new();
    let version =
        melissa_settings::updates::Release::read(&melissa_settings::updates::release_file())
            .version;
    for s in [&t.owner, &t.member] {
        let (status, page) = get_as(&t.app, s, "/settings/about").await;
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
async fn its_links_open_a_new_tab_and_say_so() {
    let t = TestBox::new();
    let (_, page) = get_as(&t.app, &t.member, "/settings/about").await;
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
