//! Switching the backend (and model) of the Queen or a bee at runtime.

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
        BeeLimits::default(),
        |q| q.backends = vec!["fake-acp".into(), "bee-acp".into()],
    );
    let current = |s: &Setup| -> Vec<String> {
        s.queen
            .backends()
            .into_iter()
            .filter(|b| b.current)
            .map(|b| b.name)
            .collect()
    };
    assert_eq!(current(&s), ["fake-acp"]);

    let (turn, since) = s.queen.send("Remember apple.").unwrap();
    // While the turn runs: recorded, applied afterwards.
    let sw = s.queen.use_backend("bee-acp", None).unwrap();
    assert!(sw.changed && sw.after_turn, "{sw:?}");
    let events = turn_events(&s.queen, since, turn).await;
    assert_eq!(finished(&events), "end_turn");
    assert!(events.contains(&QueenEvent::SessionOpened {
        backend: "fake-acp".into(),
        resumed: false
    }));
    assert!(events.contains(&QueenEvent::BackendSwitched {
        backend: "bee-acp".into(),
        model: None,
        model_label: None
    }));
    // The old session still serves until the next turn; the view shows the choice.
    assert_eq!(s.queen.status().backend.as_deref(), Some("fake-acp"));
    assert_eq!(current(&s), ["bee-acp"]);

    let (turn, since) = s.queen.send("What was it?").unwrap();
    let events = turn_events(&s.queen, since, turn).await;
    assert_eq!(finished(&events), "end_turn");
    assert!(events.contains(&QueenEvent::SessionOpened {
        backend: "bee-acp".into(),
        resumed: false
    }));
    assert_eq!(s.queen.status().backend.as_deref(), Some("bee-acp"));
    // Choosing it again changes nothing.
    let again = s.queen.use_backend("bee-acp", None).unwrap();
    assert!(!again.changed && !again.after_turn);
    s.close().await;

    let saved = std::fs::read_to_string(dir.join("queen/backend.json")).unwrap();
    assert!(saved.contains(r#""backend": "bee-acp""#), "{saved}");
    let switches = audit_actions(&s, "queen.backend");
    assert_eq!(switches.len(), 1, "{switches:?}");
    assert_eq!(switches[0]["actor"], "ui");
    assert!(switches[0]["detail"]
        .as_str()
        .unwrap()
        .contains("fake-acp → bee-acp; applies after the running turn"));

    // After a restart the choice is still there.
    let s = setup_full(dir, Vec::new(), Vec::new(), BeeLimits::default(), |q| {
        q.backends = vec!["fake-acp".into(), "bee-acp".into()]
    });
    assert_eq!(current(&s), ["bee-acp"]);
    assert_eq!(
        s.queen.backend_choice().map(|c| c.backend).as_deref(),
        Some("bee-acp")
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
        s.queen.use_backend("missing-acp", None),
        Err(QueenError::Unavailable(m)) if m.contains("not installed")
    ));
    assert!(matches!(
        s.queen.use_backend("bee-acp", None),
        Err(QueenError::Invalid(m)) if m.contains("not a backend the Queen can use")
    ));
    // No session yet: the models are unknown, so none can be chosen.
    assert!(matches!(
        s.queen.use_backend("fake-acp", Some("m2")),
        Err(QueenError::Invalid(m)) if m.contains("does not know the models")
    ));
    let view = s.queen.backends();
    let missing = view.iter().find(|b| b.name == "missing-acp").unwrap();
    assert!(!missing.available && missing.reason == Some("not_installed"));

    let (turn, since) = s.queen.send("Hello.").unwrap();
    turn_events(&s.queen, since, turn).await;
    let view = s.queen.backends();
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
        s.queen.use_backend("fake-acp", Some("m9")),
        Err(QueenError::Invalid(m)) if m.contains("does not offer a model `m9`")
    ));
    let sw = s.queen.use_backend("fake-acp", Some("m2")).unwrap();
    assert!(sw.changed && !sw.after_turn);
    let (turn, since) = s.queen.send("Again.").unwrap();
    let events = turn_events(&s.queen, since, turn).await;
    assert_eq!(finished(&events), "end_turn");
    assert!(events.contains(&QueenEvent::SessionOpened {
        backend: "fake-acp".into(),
        resumed: true
    }));
    assert_eq!(s.queen.status().model.as_deref(), Some("m2"));
    s.close().await;
    // The model list is kept for the next daemon run.
    let models = std::fs::read_to_string(s.dir.join("queen/models.json")).unwrap();
    assert!(models.contains("Model Two"), "{models}");
}

#[tokio::test]
async fn the_user_moves_a_bee_to_another_backend() {
    // Ada was made on `bee-acp`; after the switch her next message runs on `fake-acp`.
    let mut ada = handshake("f-1");
    ada.extend([
        prompt(2, "f-1", "<contains:You are bee Ada>"),
        text("f-1", "Hi from the other agent."),
        done(2, "end_turn"),
    ]);
    let s = setup_full(
        scratch("switch-bee"),
        vec![ada],
        Vec::new(),
        BeeLimits::default(),
        |_| {},
    );
    let q = s.queen_token();
    create_bees(&s, &q, &["Ada"]).await;
    let before = s.pool().backends_of("Ada").unwrap();
    assert!(before.iter().any(|b| b.name == "bee-acp" && b.current));
    assert!(matches!(
        s.pool().user_use_backend("Nobody", "fake-acp", None),
        Err(QueenError::NoSuchBee(_))
    ));
    assert!(matches!(
        s.pool().user_use_backend("Ada", "nope", None),
        Err(QueenError::Invalid(_))
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
    assert!(events.contains(&QueenEvent::SessionOpened {
        backend: "fake-acp".into(),
        resumed: false
    }));
    s.close().await;
    let switches = audit_actions(&s, "bee.backend");
    assert_eq!(switches.len(), 1);
    assert!(switches[0]["detail"]
        .as_str()
        .unwrap()
        .contains("bee-acp → fake-acp"));
    // Saved in the registry for the next daemon run.
    let reg = BeeRegistry::open(&s.dir.join("bees"), COMB).unwrap();
    assert_eq!(reg.get(ADA).map(|b| b.backend).as_deref(), Some("fake-acp"));
}
