//! Silent compaction (2026-11-20): a conversation past its context threshold is condensed
//! once it has been idle long enough (by Tokio's paused clock here), and the next session
//! starts from the summary and the recent messages. The chat itself shows nothing.

use super::*;
use crate::lead::events::EventRecord;

/// A chat of `n` earlier turns in the lead agent's transcript (as after a restart).
fn earlier_chat(dir: &std::path::Path, n: u64) {
    std::fs::create_dir_all(dir.join("lead")).unwrap();
    let mut lines = String::new();
    for i in 0..n {
        for (j, e) in [
            LeadEvent::UserMessage {
                owner_msg: None,
                turn: i + 1,
                text: format!("question {i}"),
                attachments: vec![],
                scheduled: None,
            },
            LeadEvent::AgentMessage {
                turn: i + 1,
                text: format!("answer {i}"),
            },
        ]
        .into_iter()
        .enumerate()
        {
            let r = EventRecord {
                seq: i * 2 + j as u64 + 1,
                ts: 1_795_000_000 + i * 60,
                event: e,
            };
            lines.push_str(&serde_json::to_string(&r).unwrap());
            lines.push('\n');
        }
    }
    std::fs::write(dir.join("lead/transcript.jsonl"), lines).unwrap();
}

fn usage(sid: &str, used: u64, size: u64) -> Step {
    update(
        sid,
        json!({"sessionUpdate": "usage_update", "used": used, "size": size}),
    )
}

fn compacted(s: &Setup) -> Option<String> {
    s.lead
        .events()
        .since(0, 10_000)
        .events
        .into_iter()
        .find_map(|r| match r.event {
            LeadEvent::ContextCompacted { summary } => Some(summary),
            _ => None,
        })
}

#[tokio::test]
async fn an_idle_conversation_past_its_threshold_is_condensed_and_the_next_session_starts_from_the_summary(
) {
    let dir = scratch("compaction");
    earlier_chat(&dir, 6);
    let mut first = handshake("s-1");
    first.extend([
        prompt(2, "s-1", "<any>"),
        // 75% of the window: past the default 70%.
        usage("s-1", 150_000, 200_000),
        text("s-1", "Done."),
        done(2, "end_turn"),
    ]);
    let mut second = handshake("s-2");
    second.extend([
        prompt(2, "s-2", "<contains:Summary of the earlier conversation>"),
        text("s-2", "Still here."),
        done(2, "end_turn"),
    ]);
    let s = setup_in(dir.clone(), vec![first, second], |_| {});
    let (turn, since) = s.lead.send("One more thing.").unwrap();
    assert_eq!(
        finished(&turn_events(&s.lead, since, turn).await),
        "end_turn"
    );
    assert_eq!(s.lead.usage().context.map(|c| c.used), Some(150_000));

    // Idle, by the clock: nothing before the policy's wait, the summary after it.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(119)).await;
    assert!(compacted(&s).is_none(), "not before two idle minutes");
    tokio::time::advance(Duration::from_secs(2)).await;
    for _ in 0..50 {
        if compacted(&s).is_some() {
            break;
        }
        tokio::task::yield_now().await;
    }
    tokio::time::resume();
    let summary = compacted(&s).expect("condensed");
    // The older messages, one line each; the last six messages stay whole for the session.
    assert!(
        summary.starts_with("user: question 0\nyou: answer 0"),
        "{summary}"
    );
    assert!(!summary.contains("One more thing."), "{summary}");
    assert!(
        s.lead.usage().context.is_none(),
        "the old figure no longer applies"
    );
    let saved = std::fs::read_to_string(dir.join("lead/session.json")).unwrap();
    assert!(saved.contains(r#""session_id":"""#), "no resume: {saved}");
    let audit = s.audit();
    assert!(audit.contains("lead.compact"), "{audit}");
    assert!(audit.contains("context 150000 of 200000 tokens"), "{audit}");
    // The chat shows nothing of it.
    let transcript = std::fs::read_to_string(dir.join("lead/transcript.jsonl")).unwrap();
    assert!(transcript.contains("context_compacted"));

    // The next message opens a new session primed with the summary and the recent messages.
    let (turn, since) = s.lead.send("Are you there?").unwrap();
    let events = turn_events(&s.lead, since, turn).await;
    assert_eq!(finished(&events), "end_turn");
    assert!(events.contains(&LeadEvent::SessionOpened {
        backend: "fake-acp".into(),
        resumed: false
    }));
    s.close().await;
}

#[tokio::test]
async fn below_the_threshold_or_busy_again_nothing_is_condensed() {
    let dir = scratch("no-compaction");
    earlier_chat(&dir, 6);
    let mut first = handshake("s-1");
    first.extend([
        prompt(2, "s-1", "<any>"),
        usage("s-1", 20_000, 200_000),
        text("s-1", "Done."),
        done(2, "end_turn"),
    ]);
    let s = setup_in(dir.clone(), vec![first], |_| {});
    let (turn, since) = s.lead.send("Hi.").unwrap();
    turn_events(&s.lead, since, turn).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(600)).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    tokio::time::resume();
    assert!(compacted(&s).is_none(), "10% of the window");
    // Asked for, it condenses anyway (the API's `POST /api/lead/compact`).
    assert!(s.lead.compact());
    assert!(compacted(&s).is_some());
    s.close().await;
}
