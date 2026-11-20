//! Scheduled jobs in real conversations: the lead agent's chat and an agent's.

use jiff::tz::TimeZone;

use super::*;
use crate::lead::schedules::{Creator, LastRun, Outcome, Principal, ScheduleInput, Schedules};

fn input(name: &str, agent: Principal, instruction: &str) -> ScheduleInput {
    ScheduleInput {
        name: name.into(),
        agent,
        instruction: instruction.into(),
        cron: "0 3 * * *".into(),
        enabled: true,
        max_minutes: None,
    }
}

/// Wait until schedule `id`'s last run has `want` as its outcome.
async fn outcome(s: &Schedules, id: u64, want: Outcome) -> LastRun {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(run) = s.get(id).and_then(|s| s.last_run) {
            if run.outcome == want {
                return run;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "still {:?}",
            s.get(id).and_then(|s| s.last_run)
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn a_scheduled_turn_in_the_lead_chat_is_labelled_and_runs_unattended() {
    let mut script = handshake("s-1");
    script.extend([
        // The agent is told it is a scheduled job nobody watches.
        prompt(2, "s-1", "<contains:Nightly updates>"),
        text("s-1", "Working on it"),
        Step::send(
            json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": "s-1"}}),
        ),
        done(2, "cancelled"),
    ]);
    let s = setup("sched-lead", vec![script]);
    let schedules = Schedules::in_memory(TimeZone::UTC)
        .with_audit(Some(AuditLog::new(s.dir.join("audit.jsonl"))));
    let a = schedules
        .create(
            &input("Nightly updates", Principal::Lead, "Update all apps."),
            &Creator::Owner,
        )
        .unwrap();
    let scheduler = s.lead.start_schedules(schedules.clone());
    let run = scheduler.run_now(a.id, &Creator::Owner).unwrap();
    assert_eq!(run.outcome, Outcome::Running);
    let turn = run.turn.unwrap();
    let shown = lead_event(&s, |e| matches!(e, LeadEvent::UserMessage { .. })).await;
    assert_eq!(
        shown,
        LeadEvent::UserMessage {
            turn,
            text: "Update all apps.".into(),
            attachments: vec![],
            scheduled: Some("Nightly updates".into()),
        }
    );
    // While it runs, the lead agent's MCP session is unattended (so actions that need a
    // watching human ask, or match an "Always allow" rule), and the chat is busy.
    lead_event(&s, |e| matches!(e, LeadEvent::Text { .. })).await;
    let token = s.lead.inner.conv.token().unwrap();
    assert!(!s.server.session_context(&token).unwrap().attended);
    assert_eq!(s.lead.send("hi"), Err(LeadError::Busy));
    assert_eq!(
        scheduler.run_now(a.id, &Creator::Owner).unwrap().outcome,
        Outcome::Skipped
    );
    assert!(s.lead.cancel().await);
    outcome(&schedules, a.id, Outcome::Cancelled).await;
    // The user's next message is attended again.
    assert!(s.server.session_context(&token).unwrap().attended);
    s.close().await;
    let lines = s.audit_lines();
    let actions: Vec<&str> = lines.iter().filter_map(|l| l["action"].as_str()).collect();
    for want in [
        "schedule.created",
        "schedule.run_now",
        "schedule.run_started",
        "schedule.run_skipped",
        "schedule.run_finished",
    ] {
        assert!(actions.contains(&want), "{want}: {actions:?}");
    }
    assert!(lines.iter().any(|l| l["action"] == "lead.message"
        && l["actor"] == "schedule"
        && l["detail"]
            .as_str()
            .unwrap()
            .starts_with("scheduled `Nightly updates`")));
}

#[tokio::test]
async fn a_scheduled_job_wakes_its_agent_in_the_agents_own_chat() {
    let agent = agent_reply("a-1", "Nightly check", "All up to date.");
    let s = setup_full(
        scratch("sched-agent"),
        Vec::new(),
        vec![agent],
        AgentLimits::default(),
        |_| {},
    );
    let q = s.lead_token();
    create_agents(&s, &q, &["Ada"]).await;
    let schedules = Schedules::in_memory(TimeZone::UTC);
    let a = schedules
        .create(
            &input(
                "Nightly check",
                Principal::Agent(ADA.into()),
                "Check for updates.",
            ),
            &Creator::Owner,
        )
        .unwrap();
    let scheduler = s.lead.start_schedules(schedules.clone());
    // Started once: a second start returns the same scheduler.
    assert!(s
        .lead
        .start_schedules(Schedules::in_memory(TimeZone::UTC))
        .schedules()
        .get(a.id)
        .is_some());
    let run = scheduler.run_now(a.id, &Creator::Owner).unwrap();
    assert_eq!(run.outcome, Outcome::Running);
    let done = outcome(&schedules, a.id, Outcome::Done).await;
    assert!(done.ended.is_some());
    let events: Vec<LeadEvent> = s
        .pool()
        .events_of(ADA)
        .unwrap()
        .since(0, 1000)
        .events
        .into_iter()
        .map(|r| r.event)
        .collect();
    assert!(events.contains(&LeadEvent::UserMessage {
        turn: run.turn.unwrap(),
        text: "Check for updates.".into(),
        attachments: vec![],
        scheduled: Some("Nightly check".into()),
    }));
    assert!(events
        .iter()
        .any(|e| matches!(e, LeadEvent::AgentMessage { text, .. } if text == "All up to date.")));
    // Nothing of it in the lead agent's chat.
    assert!(!s
        .lead
        .events()
        .since(0, 1000)
        .events
        .iter()
        .any(|r| matches!(&r.event, LeadEvent::UserMessage { .. })));

    // An agent that no longer exists: the run fails, with why.
    let gone = schedules
        .create(
            &input(
                "Orphan",
                Principal::Agent("d-0a1b2c3d.a9".into()),
                "Anything.",
            ),
            &Creator::Owner,
        )
        .unwrap();
    let run = scheduler.run_now(gone.id, &Creator::Owner).unwrap();
    assert_eq!(run.outcome, Outcome::Failed);
    assert!(run.detail.unwrap().contains("no agent"));
    s.close().await;
}
