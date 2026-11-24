//! Agent threads in the web UI (owner design 2026-11-24): event lines that open the thread,
//! the thread panel (`/ui/threads/{helper}`, `?thread=` without script), the details'
//! Conversations, all owner-only; the chat's "New" divider.

use melissa_agent::lead::threads::{ThreadEntry, LEAD};

use super::{
    auth::{as_, claimed, member, store},
    *,
};

/// Ada, with one exchange with Melissa and a message Melissa passed on from Ivy.
fn talked(lead: &Lead) -> (String, String) {
    let reg = lead.agents().registry().clone();
    let ada = reg.create("Ada", "stuck", None, "the user").unwrap().id;
    let ivy = reg.create("Ivy", "stuck", None, "the user").unwrap().id;
    let threads = lead.agents().threads();
    let at = |ts, e: ThreadEntry| ThreadEntry { ts, ..e };
    threads.record(&at(
        100,
        ThreadEntry::new(
            LEAD,
            &ada,
            "message",
            "data",
            "Is last night's backup done?",
        ),
    ));
    threads.record(&at(
        200,
        ThreadEntry::new(&ada, LEAD, "reply", "data", "Yes, at 02:10."),
    ));
    threads.record(&at(
        300,
        ThreadEntry::new(
            &ivy,
            &ada,
            "message",
            "relayed",
            "Which port does <Immich> use?",
        ),
    ));
    lead.events().push(LeadEvent::AgentMessaged {
        agent: ada.clone(),
        name: "Ada".into(),
    });
    (ada, ivy)
}

#[tokio::test]
async fn the_owner_opens_a_thread_from_the_chat_and_from_conversations() {
    let (app, lead) = app();
    let (ada, _) = talked(&lead);
    // The event line in Melissa's chat links to the thread (page and panel fragment).
    let (status, _, page) = get_html(&app, "/chat").await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains(">Messaged Ada<"), "{page}");
    assert!(
        page.contains(&format!("href=\"/chat?thread={ada}\"")),
        "{page}"
    );
    assert!(page.contains(&format!("hx-get=\"/ui/threads/{ada}?in=lead\"")));
    // Conversations: one row for Ada, the last message under the name.
    assert!(page.contains("id=\"details-conversations\""), "{page}");
    assert!(page.contains("Which port does &lt;Immich&gt; use?"));
    assert!(page.contains(&format!("href=\"/chat?thread={ada}&amp;back=1\"")));
    // The panel: every message, Ivy's relayed one marked, escaped; no composer.
    let (status, _, panel) = get_html(&app, &format!("/ui/threads/{ada}?in=lead&back=1")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        panel.contains("Is last night&#39;s backup done?")
            || panel.contains("Is last night's backup done?"),
        "{panel}"
    );
    assert!(panel.contains("Yes, at 02:10."));
    assert!(panel.contains("from Ivy"), "{panel}");
    assert!(panel.contains("data-thread-back"));
    assert!(!panel.contains("<script"));
    assert!(!panel.contains("<form"));
    // Without script: the chat page with the thread in the panel's place.
    let (_, _, page) = get_html(&app, &format!("/chat?thread={ada}")).await;
    assert!(page.contains("data-view=\"thread\""), "{page}");
    assert!(page.contains("aria-labelledby=\"thread-h\""));
    assert!(page.contains("Yes, at 02:10."));
    // Ada's chat: Melissa is the one conversation; another helper's thread is not shown there.
    let (_, _, page) = get_html(&app, &format!("/agents/{ada}")).await;
    assert!(page.contains(&format!("href=\"/agents/{ada}?thread={ada}&amp;back=1\"")));
    let (status, _, _) = get_html(&app, &format!("/ui/threads/{ada}?in=d-0a1b2c3d.a9")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Only agent ids.
    let (status, _, _) = get_html(&app, "/ui/threads/..%2Fsecrets?in=lead").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn threads_are_the_owners_only() {
    let (auth, _) = store();
    let owner = claimed(&auth);
    let bob = member(&auth, &owner);
    let (app, lead) = app_signed_out(auth);
    let (ada, _) = talked(&lead);
    let get = |s, uri: String| send(&app, as_(s, "GET", &uri).body(Body::empty()).unwrap());
    let (status, _, _) = get(&bob, format!("/ui/threads/{ada}?in=lead")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = get(&owner, format!("/ui/threads/{ada}?in=lead")).await;
    assert_eq!(status, StatusCode::OK);
    // A member's chat: the event line without a link, no Conversations, no thread.
    let (status, _, page) = get(&bob, format!("/chat?thread={ada}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("Messaged Ada"), "{page}");
    assert!(!page.contains("/ui/threads/"), "{page}");
    assert!(!page.contains("data-view=\"thread\""));
    assert!(!page.contains("Yes, at 02:10."));
    assert!(!page.contains("Which port"));
    let (_, _, page) = get(&owner, "/chat".into()).await;
    assert!(
        page.contains(&format!("/ui/threads/{ada}?in=lead")),
        "{page}"
    );
}

#[tokio::test]
async fn the_new_divider_comes_before_what_this_browser_has_not_seen() {
    let (app, lead) = app();
    let log = lead.events();
    log.push(LeadEvent::AgentMessage {
        turn: 1,
        text: "Ada looks after backups.".into(),
    });
    let seen = log.last_seq();
    log.push(LeadEvent::HelperReport {
        turn: 2,
        agent: "d-0a1b2c3d.a1".into(),
        name: "Ada".into(),
        status: "replied".into(),
        message: Some("reply".into()),
    });
    let cookie = format!("melissa_seen=d-0a1b2c3d.a7~1-1|lead~{}-{seen}", log.epoch());
    let (_, _, page) = send(&app, get_with_cookie("/chat", &cookie)).await;
    let divider = page.find("unread-divider").expect("a New divider");
    let message = page.find(">Message from Ada<").expect("the event line");
    assert!(divider < message);
    assert!(page[..message].contains("Ada looks after backups."));
    // Seen everything, from another log, or nothing remembered: no divider.
    for cookie in [
        format!("melissa_seen=lead~{}-{}", log.epoch(), log.last_seq()),
        format!("melissa_seen=lead~{}-{seen}", log.epoch() + 1),
        "melissa_banner=dismissed".to_string(),
    ] {
        let (_, _, page) = send(&app, get_with_cookie("/chat", &cookie)).await;
        assert!(!page.contains("unread-divider"), "{cookie}");
    }
}
