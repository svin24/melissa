//! The message queue and steering (owner, 2026-11-21): a message while a turn runs waits in
//! the conversation's queue and starts when the turn ends (also a cancelled one); the user
//! can take one back, or steer it into the running turn (sessions that take messages
//! mid-turn) or in place of it (the others: the turn stops, the message runs next). Helpers'
//! reports give the lead agent turns of their own, at most `MAX_AUTO_TURNS` in a row.

use super::*;
use crate::lead::{
    conversation::{Report, MAX_AUTO_TURNS},
    SendMode, Sent,
};

/// A lead agent turn whose tool call waits for the user's yes (it holds the turn open).
fn waits_for_yes(id: u64) -> Vec<Step> {
    vec![
        prompt(id, "s-1", "<any>"),
        Step::mcp(
            "uninstall_app",
            json!({"app": "nextcloud"}),
            json!({"result": {"isError": false}}),
        ),
        text("s-1", "Removed."),
        done(id, "end_turn"),
    ]
}

async fn submit(s: &Setup, text: &str, mode: SendMode) -> Sent {
    s.lead.submit(text, Vec::new(), mode).await.unwrap()
}

fn user(turn: TurnId, text: &str) -> LeadEvent {
    LeadEvent::UserMessage {
        turn,
        text: text.into(),
        attachments: vec![],
        scheduled: None,
    }
}

#[tokio::test]
async fn messages_wait_in_the_queue_and_run_in_order_after_the_turn() {
    let mut script = handshake("s-1");
    script.extend(waits_for_yes(2));
    script.extend([
        prompt(3, "s-1", "Second"),
        text("s-1", "Second done."),
        done(3, "end_turn"),
    ]);
    let s = setup("queue-order", vec![script]);
    s.workspaces.set("nextcloud", Owner::Unassigned);
    let Sent::Started { turn, since } = submit(&s, "Remove nextcloud.", SendMode::Queue).await
    else {
        panic!("not started");
    };
    let asked = pending(&s).await;
    // While it works: queued, not refused.
    assert!(matches!(
        submit(&s, "Second", SendMode::Queue).await,
        Sent::Queued { id: 1, .. }
    ));
    assert!(matches!(
        submit(&s, "Third", SendMode::Queue).await,
        Sent::Queued { id: 2, .. }
    ));
    assert_eq!(
        s.lead.submit(" ", Vec::new(), SendMode::Queue).await,
        Err(LeadError::Empty)
    );
    let texts: Vec<String> = s.lead.queued().into_iter().map(|m| m.text).collect();
    assert_eq!(texts, ["Second", "Third"]);
    // Taken back before it ran.
    assert_eq!(s.lead.unqueue(2, false).unwrap().text, "Third");
    assert!(s.lead.unqueue(2, false).is_none());

    s.lead.confirmations().resolve(asked.id, Verdict::Approved);
    let events = turn_events(&s.lead, since, turn + 1).await;
    let ends: Vec<&LeadEvent> = events
        .iter()
        .filter(|e| matches!(e, LeadEvent::TurnFinished { .. }))
        .collect();
    assert_eq!(ends.len(), 2, "{events:#?}");
    assert!(events.contains(&user(turn + 1, "Second")));
    assert!(events.contains(&LeadEvent::MessageUnqueued {
        id: 1,
        reason: "started".into()
    }));
    assert!(events.contains(&LeadEvent::MessageUnqueued {
        id: 2,
        reason: "removed".into()
    }));
    assert!(!events.contains(&user(turn + 1, "Third")));
    assert!(s.lead.queued().is_empty());
    s.close().await;
    let audit = s.audit();
    assert!(audit.contains(r#""action":"lead.queued""#));
    assert!(audit.contains(r#""action":"lead.unqueued""#));
}

#[tokio::test]
async fn a_cancelled_turn_keeps_the_queue() {
    let mut script = handshake("s-1");
    script.extend([
        prompt(2, "s-1", "<any>"),
        text("s-1", "Working on it"),
        Step::send(
            json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": "s-1"}}),
        ),
        done(2, "cancelled"),
        prompt(3, "s-1", "Next"),
        text("s-1", "Next done."),
        done(3, "end_turn"),
    ]);
    let s = setup("queue-cancel", vec![script]);
    let Sent::Started { turn, since } = submit(&s, "Think long.", SendMode::Queue).await else {
        panic!("not started");
    };
    lead_event(
        &s,
        |e| matches!(e, LeadEvent::Text { text, .. } if text == "Working on it"),
    )
    .await;
    assert!(matches!(
        submit(&s, "Next", SendMode::Queue).await,
        Sent::Queued { id: 1, .. }
    ));
    assert!(s.lead.cancel().await);
    let events = turn_events(&s.lead, since, turn + 1).await;
    assert!(events.contains(&LeadEvent::TurnFinished {
        turn,
        stop_reason: "cancelled".into()
    }));
    assert!(events.contains(&user(turn + 1, "Next")));
    assert_eq!(finished(&events), "end_turn");
    // A plain stop, not a steer: the chat says "Stopped.".
    assert!(!events
        .iter()
        .any(|e| matches!(e, LeadEvent::TurnSteered { .. })));
    s.close().await;
}

#[tokio::test]
async fn steering_goes_into_the_running_turn_where_the_session_takes_it() {
    let mut script = handshake("s-1");
    script.extend(waits_for_yes(2));
    let s = setup("steer-inject", vec![script]);
    s.replay.set_steerable();
    s.workspaces.set("nextcloud", Owner::Unassigned);
    let Sent::Started { turn, since } = submit(&s, "Remove nextcloud.", SendMode::Queue).await
    else {
        panic!("not started");
    };
    let asked = pending(&s).await;
    // Double Enter: straight into the turn.
    assert!(matches!(
        submit(&s, "Also the wiki.", SendMode::Steer).await,
        Sent::Steered { turn: t, .. } if t == turn
    ));
    // "Send now" on a queued one.
    let Sent::Queued { id, .. } = submit(&s, "And the blog.", SendMode::Queue).await else {
        panic!("not queued");
    };
    assert!(matches!(
        s.lead.steer(id).await,
        Some(Sent::Steered { turn: t, .. }) if t == turn
    ));
    assert!(s.lead.steer(id).await.is_none(), "not queued any more");
    assert_eq!(s.replay.steered(), ["Also the wiki.", "And the blog."]);
    s.lead.confirmations().resolve(asked.id, Verdict::Approved);
    // The turn goes on and ends once; nothing was cancelled.
    let events = turn_events(&s.lead, since, turn).await;
    assert_eq!(finished(&events), "end_turn");
    assert!(events.contains(&user(turn, "Also the wiki.")));
    assert!(events.contains(&user(turn, "And the blog.")));
    assert!(events.contains(&LeadEvent::MessageUnqueued {
        id,
        reason: "steered".into()
    }));
    assert!(s.lead.queued().is_empty());
    // Nothing was stopped, so no steered note either.
    assert!(!events
        .iter()
        .any(|e| matches!(e, LeadEvent::TurnSteered { .. })));
    s.close().await;
    assert!(!s.audit().contains(r#""action":"lead.cancel""#));
    assert!(s.audit().contains(r#""action":"lead.steer""#));
}

#[tokio::test]
async fn steering_stops_the_turn_where_the_session_cannot_take_it() {
    let mut script = handshake("s-1");
    script.extend([
        prompt(2, "s-1", "<any>"),
        text("s-1", "Working on it"),
        Step::send(
            json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": "s-1"}}),
        ),
        done(2, "cancelled"),
        prompt(3, "s-1", "Do this instead."),
        text("s-1", "Done instead."),
        done(3, "end_turn"),
    ]);
    let s = setup("steer-fallback", vec![script]);
    let Sent::Started { turn, since } = submit(&s, "Think long.", SendMode::Queue).await else {
        panic!("not started");
    };
    lead_event(
        &s,
        |e| matches!(e, LeadEvent::Text { text, .. } if text == "Working on it"),
    )
    .await;
    let sent = submit(&s, "Do this instead.", SendMode::Steer).await;
    assert!(
        matches!(sent, Sent::Next { .. } | Sent::Started { .. }),
        "{sent:?}"
    );
    let events = turn_events(&s.lead, since, turn + 1).await;
    assert!(events.contains(&LeadEvent::TurnFinished {
        turn,
        stop_reason: "cancelled".into()
    }));
    assert!(events.contains(&user(turn + 1, "Do this instead.")));
    assert_eq!(finished(&events), "end_turn");
    assert!(s.replay.steered().is_empty());
    // The stopped turn is marked as steered before it ends (the chat says "Conversation
    // steered." instead of "Stopped.").
    let at = |want: &LeadEvent| events.iter().position(|e| e == want).unwrap();
    let steered = at(&LeadEvent::TurnSteered { turn });
    assert!(
        steered
            < at(&LeadEvent::TurnFinished {
                turn,
                stop_reason: "cancelled".into()
            })
    );
    assert!(steered < at(&user(turn + 1, "Do this instead.")));
    s.close().await;
}

#[tokio::test]
async fn helper_reports_get_turns_of_their_own_but_never_loop() {
    let mut script = handshake("s-1");
    for i in 0..MAX_AUTO_TURNS {
        let id = u64::from(i) + 2;
        script.extend([
            prompt(id, "s-1", &format!("<contains:report {i}>")),
            text("s-1", "Noted."),
            done(id, "end_turn"),
        ]);
    }
    // One report too many waits for the user's next message, as a note.
    let next = u64::from(MAX_AUTO_TURNS) + 2;
    script.extend([
        prompt(
            next,
            "s-1",
            &format!("<contains:<melissa-notes>\nreport {MAX_AUTO_TURNS}\n</melissa-notes>>"),
        ),
        text("s-1", "Hi."),
        done(next, "end_turn"),
    ]);
    let s = setup("reports", vec![script]);
    let conv = &s.lead.inner.conv;
    for i in 0..=MAX_AUTO_TURNS {
        conv.report(
            &format!("report {i}"),
            Report {
                agent: ADA.into(),
                name: "Ada".into(),
                status: "succeeded".into(),
            },
        );
    }
    let events = turn_events(&s.lead, 0, TurnId::from(MAX_AUTO_TURNS)).await;
    let reports = events
        .iter()
        .filter(|e| matches!(e, LeadEvent::HelperReport { .. }))
        .count();
    assert_eq!(reports, MAX_AUTO_TURNS as usize);
    assert!(!events
        .iter()
        .any(|e| matches!(e, LeadEvent::UserMessage { .. })));
    lead_event(
        &s,
        |e| matches!(e, LeadEvent::MessageUnqueued { reason, .. } if reason == "noted"),
    )
    .await;
    assert_eq!(s.lead.status().turn, None);
    // The user's message: a turn with the held report as a note.
    let (turn, since) = s.lead.send("Hello").unwrap();
    let events = turn_events(&s.lead, since, turn).await;
    assert_eq!(finished(&events), "end_turn");
    s.close().await;
}
