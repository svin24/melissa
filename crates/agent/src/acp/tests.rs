//! ACP client tests against the replay agent (in-memory pipes, no processes, no network).

use std::{path::PathBuf, sync::Arc, time::Duration};

use serde_json::Value;

use super::{
    client::{self, StderrTail},
    one_turn,
    replay::{agent, Recorder},
    transport,
};
use crate::{
    mcp::McpServer,
    session::{
        McpServerSpec, OpenSession, OpenedSession, PermissionContext, PermissionDecision,
        PermissionHandler, PolicyPermissions, SessionEvent, StopReason, ToolKind, ToolStatus,
    },
    tools::{BeeInfo, TaskContext, Tool, ToolRegistry},
    AuditLog, EventSink, Stream,
};

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/acp")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("melissa-acp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Policy permissions over a registry with `bee_info`, for an attended task 5.
fn policy(audit: Option<AuditLog>) -> (PolicyPermissions, ToolRegistry) {
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(BeeInfo::new())];
    let registry = ToolRegistry::new(tools, audit).with_confirm_timeout(Duration::from_secs(30));
    let server = McpServer::new(registry.clone());
    let token = server
        .open_session(
            5,
            TaskContext {
                attended: true,
                ..TaskContext::default()
            },
            None,
        )
        .unwrap();
    let perms = PolicyPermissions::new(
        registry.clone(),
        PermissionContext {
            task: 5,
            backend: "fake-acp".into(),
            mcp_name: "melissa".into(),
            mcp: Some((server, token)),
        },
    );
    (perms, registry)
}

/// Open a session against the replay agent playing `steps`. Returns the session and the
/// agent's task (its result says whether the transcript was followed exactly).
async fn open_replay(
    steps: Vec<agent::Step>,
    open: OpenSession,
    recorder: Option<Arc<Recorder>>,
) -> (OpenedSession, tokio::task::JoinHandle<Result<(), String>>) {
    let (client_out, agent_in) = tokio::io::duplex(64 * 1024);
    let (agent_out, client_in) = tokio::io::duplex(64 * 1024);
    let agent = tokio::spawn(agent::run(steps, agent_in, agent_out));
    let transport = transport::lines(client_in, client_out, recorder);
    let opened = client::start(transport, open, None, StderrTail::default())
        .await
        .unwrap();
    (opened, agent)
}

fn open_with(perms: Arc<dyn PermissionHandler>, mcp: bool) -> OpenSession {
    OpenSession::new(
        "/work".into(),
        if mcp {
            vec![McpServerSpec {
                name: "melissa".into(),
                url: "http://127.0.0.1:8080/mcp/0123456789abcdef".into(),
                headers: Vec::new(),
            }]
        } else {
            Vec::new()
        },
        perms,
    )
}

const BEE_PROMPT: &str = "Use the bee_info tool, then reply with the kernel version.";

#[tokio::test]
async fn tool_turn_streams_events_and_allows_once() {
    let audit_path = scratch("turn").join("audit.jsonl");
    let (perms, _) = policy(Some(AuditLog::new(&audit_path)));
    let steps = agent::parse(&fixture("fake_tool_turn.jsonl"));
    let (opened, agent) = open_replay(steps, open_with(Arc::new(perms), true), None).await;
    let OpenedSession {
        session,
        mut events,
        ..
    } = opened;
    assert_eq!(session.id(), "s-1");
    let caps = session.capabilities();
    assert!(caps.load && caps.resume && caps.close && caps.mcp_http && !caps.fork);
    assert_eq!(caps.agent.as_deref(), Some("fake-agent 0.1.0"));

    let stop = session.prompt(BEE_PROMPT).await.unwrap();
    assert_eq!(stop, StopReason::EndTurn);
    session.close().await.unwrap();
    agent.await.unwrap().unwrap();

    let mut got = Vec::new();
    while let Some(e) = events.recv().await {
        got.push(e);
    }
    let text: String = got
        .iter()
        .filter_map(|e| match e {
            SessionEvent::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Kernel: 6.1.0-test\nDone.");
    assert!(got.contains(&SessionEvent::ToolStarted {
        id: "t1".into(),
        title: "melissa__bee_info".into(),
        kind: ToolKind::Other,
        status: ToolStatus::Pending,
    }));
    assert!(got.contains(&SessionEvent::Permission {
        tool_call_id: "t1".into(),
        title: "melissa__bee_info".into(),
        decision: PermissionDecision::AllowOnce,
    }));
    assert!(got.contains(&SessionEvent::ToolFinished {
        id: "t1".into(),
        title: Some("melissa__bee_info".into()),
        status: ToolStatus::Completed,
    }));
    assert!(got.contains(&SessionEvent::Usage {
        used: 1200,
        size: 256000
    }));
    assert!(got.contains(&SessionEvent::Other("vendor_specific_thing".into())));
    assert!(
        matches!(got.iter().find(|e| matches!(e, SessionEvent::Plan(_))), Some(SessionEvent::Plan(p)) if p.len() == 2)
    );
    assert_eq!(
        got.last(),
        Some(&SessionEvent::TurnFinished(StopReason::EndTurn))
    );

    // The permission answer is audited (actor agent).
    let log = std::fs::read_to_string(&audit_path).unwrap();
    let line: Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
    assert_eq!(line["action"], "agent.permission");
    assert_eq!(line["actor"], "agent");
    assert_eq!(line["task"], 5);
    assert_eq!(line["backend"], "fake-acp");
    assert!(line["detail"].as_str().unwrap().starts_with("allow_once"));
}

#[tokio::test]
async fn one_turn_renders_concise_output() {
    let (perms, _) = policy(None);
    let steps = agent::parse(&fixture("fake_tool_turn.jsonl"));
    let (opened, agent) = open_replay(steps, open_with(Arc::new(perms), true), None).await;
    let OpenedSession {
        session,
        mut events,
        ..
    } = opened;
    let (sink, mut rx) = EventSink::channel();
    let stop = one_turn(session.as_ref(), &mut events, BEE_PROMPT, &sink)
        .await
        .unwrap();
    assert_eq!(stop, StopReason::EndTurn);
    session.close().await.unwrap();
    agent.await.unwrap().unwrap();
    drop(sink);
    let mut lines = Vec::new();
    while let Some(l) = rx.recv().await {
        lines.push((l.stream, l.text));
    }
    let err = |s: &str| (Stream::Stderr, s.to_string());
    let out = |s: &str| (Stream::Stdout, s.to_string());
    assert_eq!(
        lines,
        [
            err("[plan] Call bee_info | Answer"),
            err("[tool] melissa__bee_info → pending"),
            err("[permission] melissa__bee_info → allow_once"),
            err("[tool] melissa__bee_info → done"),
            out("Kernel: 6.1.0-test"),
            out("Done."),
        ]
    );
}

#[tokio::test]
async fn cancel_answers_pending_permission_and_ends_turn() {
    let audit_path = scratch("cancel").join("audit.jsonl");
    let (perms, registry) = policy(Some(AuditLog::new(&audit_path)));
    let steps = agent::parse(&fixture("fake_cancel.jsonl"));
    let (opened, agent) = open_replay(steps, open_with(Arc::new(perms), false), None).await;
    let OpenedSession {
        session,
        mut events,
        ..
    } = opened;
    let session: Arc<dyn crate::session::AgentSession> = Arc::from(session);
    let turn = {
        let session = session.clone();
        tokio::spawn(async move { session.prompt("clean the build dir").await })
    };
    // The shell command needs a human: wait until it is in the inbox, then cancel.
    let store = registry.confirmations().clone();
    let pending = loop {
        if let Some(c) = store.pending().first().cloned() {
            break c;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert_eq!(pending.tool, "agent:execute");
    assert_eq!(pending.args["title"], "Execute `rm -rf build`");
    // A second prompt while one runs is refused.
    assert!(matches!(
        session.prompt("again").await,
        Err(crate::AgentError::Busy)
    ));
    session.cancel().await.unwrap();
    assert_eq!(turn.await.unwrap().unwrap(), StopReason::Cancelled);
    assert!(store.pending().is_empty(), "confirmation withdrawn");
    session.close().await.unwrap();
    agent.await.unwrap().unwrap();

    let mut got = Vec::new();
    while let Some(e) = events.recv().await {
        got.push(e);
    }
    assert!(got.contains(&SessionEvent::Permission {
        tool_call_id: "t9".into(),
        title: "Execute `rm -rf build`".into(),
        decision: PermissionDecision::Cancelled,
    }));
    let log = std::fs::read_to_string(&audit_path).unwrap();
    assert!(log.contains("tool.confirm_requested"));
    assert!(log.contains(r#""action":"agent.permission""#));
    assert!(log.contains("cancelled (execute)"));
    // Closed sessions refuse further work.
    assert!(session.prompt("more").await.is_err());
}

#[tokio::test]
async fn recorded_sessions_replay() {
    // Record a session against the hand-written transcript, then replay the recording.
    let dir = scratch("record");
    let recorder = Arc::new(
        Recorder::create(
            &dir,
            "fake",
            None,
            &PathBuf::from("/work"),
            vec!["0123456789abcdef".into()],
        )
        .unwrap(),
    );
    let (perms, _) = policy(None);
    let perms: Arc<dyn PermissionHandler> = Arc::new(perms);
    let steps = agent::parse(&fixture("fake_tool_turn.jsonl"));
    let (opened, agent) = open_replay(
        steps,
        open_with(perms.clone(), true),
        Some(recorder.clone()),
    )
    .await;
    opened.session.prompt(BEE_PROMPT).await.unwrap();
    opened.session.close().await.unwrap();
    agent.await.unwrap().unwrap();

    let recorded = std::fs::read_to_string(recorder.path()).unwrap();
    assert!(
        !recorded.contains("0123456789abcdef"),
        "MCP token must be scrubbed"
    );
    let steps = agent::parse(&recorded);
    assert_eq!(steps.len(), 18);
    // The scrubbed token no longer matches what the client sends; relax that one field.
    let steps = steps
        .into_iter()
        .map(|mut s| {
            if s.msg["method"] == "session/new" {
                s.msg["params"]["mcpServers"] = Value::String("<any>".into());
            }
            s
        })
        .collect();
    let (opened, agent) = open_replay(steps, open_with(perms, true), None).await;
    assert_eq!(
        opened.session.prompt(BEE_PROMPT).await.unwrap(),
        StopReason::EndTurn
    );
    opened.session.close().await.unwrap();
    agent.await.unwrap().unwrap();
}

#[tokio::test]
async fn real_grok_transcript_replays() {
    // Recorded from `grok agent stdio` (1.0.89) through the queen; see the fixture header.
    let audit_path = scratch("grok").join("audit.jsonl");
    let (perms, _) = policy(Some(AuditLog::new(&audit_path)));
    let steps = agent::parse(&fixture("grok_bee_info.jsonl"));
    let (opened, agent) = open_replay(steps, open_with(Arc::new(perms), true), None).await;
    let OpenedSession {
        session,
        mut events,
        ..
    } = opened;
    let caps = session.capabilities().clone();
    assert!(caps.load && caps.resume && caps.close && caps.mcp_http);
    let (sink, mut rx) = EventSink::channel();
    let stop = one_turn(
        session.as_ref(),
        &mut events,
        "Use the bee_info tool, then reply with one line: the kernel version.",
        &sink,
    )
    .await
    .unwrap();
    assert_eq!(stop, StopReason::EndTurn);
    session.close().await.unwrap();
    agent.await.unwrap().unwrap();
    drop(sink);
    let mut lines = Vec::new();
    while let Some(l) = rx.recv().await {
        lines.push((l.stream, l.text));
    }
    let err = |s: &str| (Stream::Stderr, s.to_string());
    assert_eq!(
        lines,
        [
            err("[tool] search_tool → pending"),
            err("[tool] Search tools: \"bee_info\" → done"),
            err("[tool] use_tool → pending"),
            err("[permission] melissa__bee_info → allow_once"),
            err("[tool] melissa__bee_info → done"),
            (Stream::Stdout, "7.3.4-300.fc45.x86_64".to_string()),
        ]
    );
    let log = std::fs::read_to_string(&audit_path).unwrap();
    assert!(log.contains(r#""action":"agent.permission""#) && log.contains("allow_once"));
}

fn init_steps(resume: bool) -> Vec<agent::Step> {
    use serde_json::json;
    let caps = if resume {
        json!({"loadSession": true, "mcpCapabilities": {"http": true}, "sessionCapabilities": {"resume": {}}})
    } else {
        json!({"loadSession": false, "mcpCapabilities": {"http": true}})
    };
    vec![
        agent::Step::send(
            json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": "<any>"}),
        ),
        agent::Step::recv(
            json!({"jsonrpc": "2.0", "id": 0, "result": {"protocolVersion": 1, "agentCapabilities": caps, "authMethods": []}}),
        ),
    ]
}

#[tokio::test]
async fn resume_reuses_the_session_or_falls_back_to_new() {
    use serde_json::json;
    let (perms, _) = policy(None);
    let perms: Arc<dyn PermissionHandler> = Arc::new(perms);

    // The agent knows the session: `session/resume` with the same cwd and fresh MCP servers.
    let mut steps = init_steps(true);
    steps.push(agent::Step::send(json!({"jsonrpc": "2.0", "id": 1, "method": "session/resume",
        "params": {"sessionId": "s-old", "cwd": "/work", "mcpServers": [{"type": "http", "name": "melissa", "url": "<any>", "headers": "<any>"}]}})));
    steps.push(agent::Step::recv(
        json!({"jsonrpc": "2.0", "id": 1, "result": {}}),
    ));
    let mut open = open_with(perms.clone(), true);
    open.resume = Some("s-old".into());
    let (opened, agent) = open_replay(steps, open.clone(), None).await;
    assert!(opened.resumed);
    assert_eq!(opened.session.id(), "s-old");
    opened.session.close().await.unwrap();
    agent.await.unwrap().unwrap();

    // It does not: a new session.
    let mut steps = init_steps(true);
    steps.push(agent::Step::send(
        json!({"jsonrpc": "2.0", "id": 1, "method": "session/resume", "params": "<any>"}),
    ));
    steps.push(agent::Step::recv(json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32002, "message": "session not found"}})));
    steps.push(agent::Step::send(
        json!({"jsonrpc": "2.0", "id": 2, "method": "session/new", "params": "<any>"}),
    ));
    steps.push(agent::Step::recv(
        json!({"jsonrpc": "2.0", "id": 2, "result": {"sessionId": "s-new"}}),
    ));
    let (opened, agent) = open_replay(steps, open.clone(), None).await;
    assert!(!opened.resumed);
    assert_eq!(opened.session.id(), "s-new");
    opened.session.close().await.unwrap();
    agent.await.unwrap().unwrap();

    // An agent without resume support is not asked.
    let mut steps = init_steps(false);
    steps.push(agent::Step::send(
        json!({"jsonrpc": "2.0", "id": 1, "method": "session/new", "params": "<any>"}),
    ));
    steps.push(agent::Step::recv(
        json!({"jsonrpc": "2.0", "id": 1, "result": {"sessionId": "s-3"}}),
    ));
    let (opened, agent) = open_replay(steps, open, None).await;
    assert!(!opened.resumed);
    opened.session.close().await.unwrap();
    agent.await.unwrap().unwrap();
}

#[tokio::test]
async fn agent_death_fails_the_turn_with_agent_exited() {
    use serde_json::json;
    let (perms, _) = policy(None);
    let mut steps = init_steps(false);
    steps.push(agent::Step::send(
        json!({"jsonrpc": "2.0", "id": 1, "method": "session/new", "params": "<any>"}),
    ));
    steps.push(agent::Step::recv(
        json!({"jsonrpc": "2.0", "id": 1, "result": {"sessionId": "s-1"}}),
    ));
    steps.push(agent::Step::send(
        json!({"jsonrpc": "2.0", "id": 2, "method": "session/prompt", "params": "<any>"}),
    ));
    steps.push(agent::Step::exit());
    let (opened, agent) = open_replay(steps, open_with(Arc::new(perms), false), None).await;
    let err = tokio::time::timeout(Duration::from_secs(5), opened.session.prompt("hi"))
        .await
        .expect("a dead agent must end the turn")
        .unwrap_err();
    assert!(matches!(err, crate::AgentError::AgentExited(_)), "{err:?}");
    agent.await.unwrap().unwrap();
}

#[tokio::test]
async fn agent_death_between_turns_ends_the_event_stream() {
    use serde_json::json;
    let (perms, _) = policy(None);
    let mut steps = init_steps(false);
    steps.push(agent::Step::send(
        json!({"jsonrpc": "2.0", "id": 1, "method": "session/new", "params": "<any>"}),
    ));
    steps.push(agent::Step::recv(
        json!({"jsonrpc": "2.0", "id": 1, "result": {"sessionId": "s-1"}}),
    ));
    steps.push(agent::Step::exit());
    let (opened, agent) = open_replay(steps, open_with(Arc::new(perms), false), None).await;
    let OpenedSession {
        session,
        mut events,
        ..
    } = opened;
    // No prompt is running; the stream still ends when the agent goes away.
    let end = tokio::time::timeout(Duration::from_secs(5), async {
        while events.recv().await.is_some() {}
    })
    .await;
    assert!(end.is_ok(), "event stream did not end");
    assert!(matches!(
        session.prompt("hi").await,
        Err(crate::AgentError::AgentExited(_))
    ));
    agent.await.unwrap().unwrap();
}
