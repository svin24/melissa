//! Switching the backend (and model) of the lead agent or an agent at runtime.

use serde_json::json;

use super::*;

/// `session/new` answer that reports models `m1` (current) and `m2`.
fn handshake_with_models(sid: &str, current: &str) -> Vec<Step> {
    let mut s = handshake(sid);
    s.pop();
    s.push(Step::recv(json!({"jsonrpc": "2.0", "id": 1, "result": {
        "sessionId": sid, "configOptions": [model_option(current)]}})));
    s
}

fn model_option(current: &str) -> Value {
    json!({"id": "model", "name": "Model", "category": "model", "type": "select",
        "currentValue": current, "options": [
            {"value": "m1", "name": "Model One"}, {"value": "m2", "name": "Model Two"}]})
}

fn audit_actions(s: &Setup, action: &str) -> Vec<Value> {
    s.audit_lines()
        .into_iter()
        .filter(|l| l["action"] == action)
        .collect()
}

#[tokio::test]
async fn a_switch_waits_for_the_running_turn_and_carries_the_conversation_over() {
    let dir = scratch("switch");
    let mut first = handshake("s-1");
    first.extend([
        prompt(2, "s-1", "<any>"),
        text("s-1", "Noted."),
        done(2, "end_turn"),
    ]);
    // The other backend starts a new session, primed with the transcript.
    let mut second = handshake("b-1");
    second.extend([
        prompt(2, "b-1", "<contains:user: Remember apple.>"),
        text("b-1", "It was apple."),
        done(2, "end_turn"),
    ]);
    let s = setup_full(
        dir.clone(),
        vec![first],
        vec![second],
        AgentLimits::default(),
        |q| q.backends = vec!["fake-acp".into(), "agent-acp".into()],
    );
    let current = |s: &Setup| -> Vec<String> {
        s.lead
            .backends()
            .into_iter()
            .filter(|b| b.current)
            .map(|b| b.name)
            .collect()
    };
    assert_eq!(current(&s), ["fake-acp"]);

    let (turn, since) = s.lead.send("Remember apple.").unwrap();
    // While the turn runs: recorded, applied afterwards.
    let sw = s.lead.use_backend("agent-acp", None).unwrap();
    assert!(sw.changed && sw.after_turn, "{sw:?}");
    let events = turn_events(&s.lead, since, turn).await;
    assert_eq!(finished(&events), "end_turn");
    assert!(events.contains(&LeadEvent::SessionOpened {
        backend: "fake-acp".into(),
        resumed: false
    }));
    assert!(events.contains(&LeadEvent::BackendSwitched {
        backend: "agent-acp".into(),
        model: None,
        model_label: None
    }));
    // The old session still serves until the next turn; the view shows the choice.
    assert_eq!(s.lead.status().backend.as_deref(), Some("fake-acp"));
    assert_eq!(current(&s), ["agent-acp"]);

    let (turn, since) = s.lead.send("What was it?").unwrap();
    let events = turn_events(&s.lead, since, turn).await;
    assert_eq!(finished(&events), "end_turn");
    assert!(events.contains(&LeadEvent::SessionOpened {
        backend: "agent-acp".into(),
        resumed: false
    }));
    assert_eq!(s.lead.status().backend.as_deref(), Some("agent-acp"));
    // Choosing it again changes nothing.
    let again = s.lead.use_backend("agent-acp", None).unwrap();
    assert!(!again.changed && !again.after_turn);
    s.close().await;

    let saved = std::fs::read_to_string(dir.join("lead/backend.json")).unwrap();
    assert!(saved.contains(r#""backend": "agent-acp""#), "{saved}");
    let switches = audit_actions(&s, "lead.backend");
    assert_eq!(switches.len(), 1, "{switches:?}");
    assert_eq!(switches[0]["actor"], "ui");
    assert!(switches[0]["detail"]
        .as_str()
        .unwrap()
        .contains("fake-acp → agent-acp; applies after the running turn"));

    // After a restart the choice is still there.
    let s = setup_full(dir, Vec::new(), Vec::new(), AgentLimits::default(), |q| {
        q.backends = vec!["fake-acp".into(), "agent-acp".into()]
    });
    assert_eq!(current(&s), ["agent-acp"]);
    assert_eq!(
        s.lead.backend_choice().map(|c| c.backend).as_deref(),
        Some("agent-acp")
    );
    s.close().await;
}

#[tokio::test]
async fn unusable_backends_and_unknown_models_are_refused_and_models_come_from_the_agent() {
    let mut first = handshake_with_models("s-1", "m1");
    first.extend([
        prompt(2, "s-1", "<any>"),
        text("s-1", "Hi."),
        done(2, "end_turn"),
    ]);
    // Same backend, other model: the session is resumed and switched to `m2`.
    let mut second = handshake("unused");
    second.truncate(2);
    second.extend([
        Step::send(
            json!({"jsonrpc": "2.0", "id": 1, "method": "session/resume",
            "params": {"sessionId": "s-1", "cwd": "<any>", "mcpServers": "<any>"}}),
        ),
        Step::recv(json!({"jsonrpc": "2.0", "id": 1, "result": {
            "configOptions": [model_option("m1")]}})),
        Step::send(
            json!({"jsonrpc": "2.0", "id": 2, "method": "session/set_config_option",
            "params": {"sessionId": "s-1", "configId": "model", "value": "m2"}}),
        ),
        Step::recv(json!({"jsonrpc": "2.0", "id": 2, "result": {
            "configOptions": [model_option("m2")]}})),
        prompt(3, "s-1", "Again."),
        text("s-1", "Hello again."),
        done(3, "end_turn"),
    ]);
    let s = setup_in(scratch("switch-refuse"), vec![first, second], |q| {
        q.backends = vec!["fake-acp".into(), "missing-acp".into()];
    });
    // `missing-acp` is not in the test's backends at all: not installed.
    assert!(matches!(
        s.lead.use_backend("missing-acp", None),
        Err(LeadError::Unavailable(m)) if m.contains("not installed")
    ));
    assert!(matches!(
        s.lead.use_backend("agent-acp", None),
        Err(LeadError::Invalid(m)) if m.contains("not a backend the lead agent can use")
    ));
    // No session yet: the models are unknown, so none can be chosen.
    assert!(matches!(
        s.lead.use_backend("fake-acp", Some("m2")),
        Err(LeadError::Invalid(m)) if m.contains("does not know the models")
    ));
    let view = s.lead.backends();
    let missing = view.iter().find(|b| b.name == "missing-acp").unwrap();
    assert!(!missing.available && missing.reason == Some("not_installed"));

    let (turn, since) = s.lead.send("Hello.").unwrap();
    turn_events(&s.lead, since, turn).await;
    let view = s.lead.backends();
    let fake = view.iter().find(|b| b.name == "fake-acp").unwrap();
    assert!(fake.current);
    assert_eq!(fake.model.as_deref(), Some("m1"));
    let ids: Vec<&str> = fake
        .models
        .iter()
        .flatten()
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(ids, ["m1", "m2"]);
    assert!(matches!(
        s.lead.use_backend("fake-acp", Some("m9")),
        Err(LeadError::Invalid(m)) if m.contains("does not offer a model `m9`")
    ));
    let sw = s.lead.use_backend("fake-acp", Some("m2")).unwrap();
    assert!(sw.changed && !sw.after_turn);
    let (turn, since) = s.lead.send("Again.").unwrap();
    let events = turn_events(&s.lead, since, turn).await;
    assert_eq!(finished(&events), "end_turn");
    assert!(events.contains(&LeadEvent::SessionOpened {
        backend: "fake-acp".into(),
        resumed: true
    }));
    assert_eq!(s.lead.status().model.as_deref(), Some("m2"));
    s.close().await;
    // The model list is kept for the next daemon run.
    let models = std::fs::read_to_string(s.dir.join("lead/models.json")).unwrap();
    assert!(models.contains("Model Two"), "{models}");
}

#[tokio::test]
async fn the_user_moves_a_agent_to_another_backend() {
    // Ada was made on `agent-acp`; after the switch her next message runs on `fake-acp`.
    let mut ada = handshake("f-1");
    ada.extend([
        prompt(2, "f-1", "<contains:You are agent Ada>"),
        text("f-1", "Hi from the other agent."),
        done(2, "end_turn"),
    ]);
    let s = setup_full(
        scratch("switch-agent"),
        vec![ada],
        Vec::new(),
        AgentLimits::default(),
        |_| {},
    );
    let q = s.lead_token();
    create_agents(&s, &q, &["Ada"]).await;
    let before = s.pool().backends_of("Ada").unwrap();
    assert!(before.iter().any(|b| b.name == "agent-acp" && b.current));
    assert!(matches!(
        s.pool().user_use_backend("Nobody", "fake-acp", None),
        Err(LeadError::NoSuchAgent(_))
    ));
    assert!(matches!(
        s.pool().user_use_backend("Ada", "nope", None),
        Err(LeadError::Invalid(_))
    ));
    let sw = s.pool().user_use_backend("ada", "fake-acp", None).unwrap();
    assert!(sw.changed);
    assert_eq!(
        s.pool().registry().get(ADA).map(|b| b.backend).as_deref(),
        Some("fake-acp")
    );
    assert_eq!(
        s.pool().get("Ada").map(|b| b.backend).as_deref(),
        Some("fake-acp")
    );

    let (turn, since) = s.pool().user_send("Ada", "Hello Ada").unwrap();
    let log = s.pool().events_of(ADA).unwrap();
    let events = turn_events_in(&log, since, turn).await;
    assert_eq!(finished(&events), "end_turn");
    assert!(events.contains(&LeadEvent::SessionOpened {
        backend: "fake-acp".into(),
        resumed: false
    }));
    s.close().await;
    let switches = audit_actions(&s, "agent.backend");
    assert_eq!(switches.len(), 1);
    assert!(switches[0]["detail"]
        .as_str()
        .unwrap()
        .contains("agent-acp → fake-acp"));
    // Saved in the registry for the next daemon run.
    let reg = AgentRegistry::open(&s.dir.join("agents"), DEVICE).unwrap();
    assert_eq!(reg.get(ADA).map(|b| b.backend).as_deref(), Some("fake-acp"));
}

#[tokio::test]
async fn new_agents_follow_the_leads_switch_and_existing_agents_keep_theirs() {
    let s = setup_in(scratch("switch-follow"), Vec::new(), |q| {
        q.backends = vec!["fake-acp".into(), "agent-acp".into()];
        q.agent_backend = Some("agent-acp".into());
    });
    let q = s.lead_token();
    let backend_of = |name: &str| s.pool().get(name).map(|b| b.backend);
    // Before any switch: the configured agent backend.
    create_agents(&s, &q, &["Ada"]).await;
    assert_eq!(backend_of("Ada").as_deref(), Some("agent-acp"));

    // The user switches the lead agent: new agents follow, Ada keeps hers.
    s.lead.use_backend("fake-acp", None).unwrap();
    create_agents(&s, &q, &["Bo"]).await;
    assert_eq!(backend_of("Bo").as_deref(), Some("fake-acp"));
    assert_eq!(backend_of("Ada").as_deref(), Some("agent-acp"));
    // A backend the lead agent names explicitly still wins.
    let r = call(
        &s.server,
        &q,
        "create_agent",
        json!({"name": "Cy", "backend": "agent-acp"}),
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(backend_of("Cy").as_deref(), Some("agent-acp"));
    // An agent the user moved keeps the user's choice when the lead agent switches again.
    s.pool().user_use_backend("Bo", "agent-acp", None).unwrap();
    s.lead.use_backend("agent-acp", None).unwrap();
    s.lead.use_backend("fake-acp", None).unwrap();
    assert_eq!(backend_of("Bo").as_deref(), Some("agent-acp"));
    s.close().await;

    // After a restart the saved lead agent choice still leads new agents.
    let s = setup_in(s.dir.clone(), Vec::new(), |q| {
        q.backends = vec!["fake-acp".into(), "agent-acp".into()];
        q.agent_backend = Some("agent-acp".into());
    });
    let q = s.lead_token();
    create_agents(&s, &q, &["Di"]).await;
    assert_eq!(
        s.pool().get("Di").map(|b| b.backend).as_deref(),
        Some("fake-acp")
    );
}
