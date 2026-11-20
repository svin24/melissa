//! Phone views (2026-11-20, owner design "Melissa — Mobile"): `/` is the agent list on
//! phones and `/chat` Melissa's chat; a chat has a back button; the agent menu is also an
//! action sheet; the details panel is a bottom sheet; the agent list has a tab bar.

use super::*;

const LIST_MARKER: &str = "data-phone-view=\"list\"";

#[tokio::test]
async fn home_is_the_agent_list_on_phones_and_chat_is_melissas_chat() {
    let (app, _lead) = app();
    let (status, _, home) = get_html(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(home.contains(LIST_MARKER), "home marks the list view");
    // The same page underneath (wide screens show Melissa's chat at `/`).
    assert!(home.contains("id=\"composer\""));

    for uri in ["/chat", "/?draft=Install%20", "/?all=1"] {
        let (status, _, page) = get_html(&app, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert!(!page.contains(LIST_MARKER), "{uri} is the chat");
        assert!(page.contains("id=\"composer\""), "{uri}");
    }
    let (_, _, draft) = get_html(&app, "/?draft=Install%20").await;
    assert!(draft.contains(">Install </textarea>"));

    // Melissa's row leads to her chat, not back to the list.
    assert!(home.contains("class=\"agent-link\" href=\"/chat\""));
}

#[tokio::test]
async fn a_chat_has_a_back_button_to_the_agent_list() {
    let (app, lead) = app();
    let ada = lead
        .agents()
        .registry()
        .create("Ada", "stuck", None, "the user")
        .unwrap()
        .id;
    for uri in ["/chat".to_string(), format!("/agents/{ada}")] {
        let (_, _, page) = get_html(&app, &uri).await;
        let head = &page[page
            .find("<header class=\"chat-head\">")
            .expect("a chat header")..];
        let head = &head[..head.find("</header>").unwrap()];
        assert!(
            head.starts_with(
                "<header class=\"chat-head\"><a class=\"icon-button back-link\" href=\"/\" title=\"Back to agents\">"
            ),
            "{uri}: {head}"
        );
        assert!(head.contains("<span class=\"visually-hidden\">Back to agents</span>"));
    }
}

#[tokio::test]
async fn the_agent_list_has_a_new_button_and_a_tab_bar() {
    let (app, _lead) = app();
    let (_, _, page) = get_html(&app, "/").await;
    let side = &page[page.find("<nav class=\"sidebar\"").unwrap()..];
    let side = &side[..side.find("</nav><div class=\"side-backdrop\"").unwrap()];
    // "+" next to the brand (phones) as well as in the search row (wide).
    assert!(side.contains(
        "<a class=\"icon-button round-button phone-new\" href=\"/settings/agents/new\" title=\"New helper\">"
    ));
    let tabs = &side[side
        .find("<nav class=\"tab-bar\" aria-label=\"Sections\">")
        .expect("tab bar")..];
    for (href, label) in [
        ("/settings", "Settings"),
        ("/cluster", "Apps"),
        ("/usage", "Usage"),
        ("/settings/updates", "Updates"),
    ] {
        assert!(tabs.contains(&format!("href=\"{href}\"")), "{href}");
        assert!(
            tabs.contains(&format!("<span class=\"tab-label\">{label}</span>")),
            "{label}"
        );
    }
    // No update known in the test app: no dot.
    assert!(!tabs.contains("update-dot"));
}

#[tokio::test]
async fn the_action_sheet_shows_the_agent_and_the_desktop_menu_items() {
    let (app, lead) = app();
    let ada = lead
        .agents()
        .registry()
        .create("Ada", "stuck", None, "the user")
        .unwrap()
        .id;
    let (_, _, page) = get_html(&app, &format!("/agents/{ada}")).await;
    let menu = &page[page
        .find("id=\"menu-head\" popover=\"auto\"")
        .expect("the header's menu")..];
    let menu = &menu[..menu.find("Cancel</button>").expect("Cancel") + 15];
    // The agent at the top (phones), then the same items as the wide menu.
    assert!(
        menu.contains("<p class=\"menu-head-name\">Ada</p>"),
        "{menu}"
    );
    assert!(menu.contains("<p class=\"menu-head-sub\">"), "{menu}");
    let (rename, icon, settings) = (
        menu.find("Rename agent").expect("Rename agent"),
        menu.find("Change icon").expect("Change icon"),
        menu.find("Agent settings").expect("Agent settings"),
    );
    assert!(rename < icon && icon < settings);
    assert!(menu.ends_with(
        "<button class=\"menu-cancel\" type=\"button\" popovertarget=\"menu-head\" popovertargetaction=\"hide\">Cancel</button>"
    ));
    // Each sidebar row's menu is an action sheet too.
    let row = &page[page
        .find(&format!("id=\"menu-{ada}\""))
        .expect("Ada's row menu")..];
    assert!(row.contains("<p class=\"menu-head-name\">Ada</p>"));
    assert!(row.contains(&format!(
        "popovertarget=\"menu-{ada}\" popovertargetaction=\"hide\""
    )));
}

#[tokio::test]
async fn the_details_panel_is_a_sheet_with_a_handle_and_says_whose() {
    let (app, lead) = app();
    let ada = lead
        .agents()
        .registry()
        .create("Ada", "stuck", None, "the user")
        .unwrap()
        .id;
    let (_, _, page) = get_html(&app, &format!("/agents/{ada}")).await;
    let panel = &page[page.find("<aside class=\"details\"").unwrap()..];
    let head = &panel[..panel.find("</button></div>").expect("the head")];
    assert!(head.contains("<span class=\"sheet-handle\" aria-hidden=\"true\" data-sheet-handle>"));
    assert!(head.contains("<span class=\"sheet-avatar\"><svg class=\"avatar "));
    assert!(head.contains("<h2 id=\"details-h\">Agent details</h2><p class=\"sheet-sub\">Ada · "));
    // Close: the panel glyph on wide screens, a cross on phones.
    assert!(head.contains("<span class=\"close-wide\">"));
    assert!(head.contains("<span class=\"close-phone\">"));
}

#[tokio::test]
async fn the_composer_says_changes_need_permission() {
    let (app, _lead) = app();
    let (_, _, page) = get_html(&app, "/chat").await;
    assert!(
        page.contains("<p class=\"composer-caption\">Device changes require your permission.</p>")
    );
}

#[tokio::test]
async fn a_plain_message_from_the_phone_chat_returns_there() {
    let (app, _lead) = app();
    let req = post("/ui/messages")
        .header(header::REFERER, "http://127.0.0.1:8080/chat")
        .body(Body::from("text=Hi"))
        .unwrap();
    let (status, headers, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/chat");
}
