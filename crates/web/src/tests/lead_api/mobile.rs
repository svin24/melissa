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

    for uri in ["/chat", "/?draft=Install%20", "/?before=40"] {
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
