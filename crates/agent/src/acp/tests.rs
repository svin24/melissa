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
    tools::{DeviceInfo, TaskContext, Tool, ToolRegistry},
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

/// Policy permissions over a registry with `device_info`, for an attended task 5.
fn policy(audit: Option<AuditLog>) -> (PolicyPermissions, ToolRegistry) {
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(DeviceInfo::new())];
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
    let opened = client::start(transport, open, None, StderrTail::default(), None)
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

const DEVICE_PROMPT: &str = "Use the device_info tool, then reply with the kernel version.";

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

    let stop = session.prompt(DEVICE_PROMPT).await.unwrap();
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
        title: "melissa__device_info".into(),
        kind: ToolKind::Other,
        status: ToolStatus::Pending,
    }));
    assert!(got.contains(&SessionEvent::Permission {
        tool_call_id: "t1".into(),
        title: "melissa__device_info".into(),
        decision: PermissionDecision::AllowOnce,
    }));
    assert!(got.contains(&SessionEvent::ToolFinished {
        id: "t1".into(),
        title: Some("melissa__device_info".into()),
        status: ToolStatus::Completed,
    }));
    assert!(got.contains(&SessionEvent::Usage {
        used: 1200,
        size: 256000
    }));
    assert!(got.contains(&SessionEvent::Cost {
        unit: "credits".into(),
        micros: 500_000
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
    let stop = one_turn(session.as_ref(), &mut events, DEVICE_PROMPT, &sink)
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
            err("[plan] Call device_info | Answer"),
            err("[tool] melissa__device_info → pending"),
            err("[permission] melissa__device_info → allow_once"),
            err("[tool] melissa__device_info → done"),
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
    opened.session.prompt(DEVICE_PROMPT).await.unwrap();
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
        opened.session.prompt(DEVICE_PROMPT).await.unwrap(),
        StopReason::EndTurn
    );
    opened.session.close().await.unwrap();
    agent.await.unwrap().unwrap();
}

#[tokio::test]
async fn real_grok_transcript_replays() {
    // Recorded from `grok agent stdio` (1.0.89) through the lead agent; see the fixture header.
    let audit_path = scratch("grok").join("audit.jsonl");
    let (perms, _) = policy(Some(AuditLog::new(&audit_path)));
    let steps = agent::parse(&fixture("grok_device_info.jsonl"));
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
        "Use the device_info tool, then reply with one line: the kernel version.",
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
            err("[tool] Search tools: \"device_info\" → done"),
            err("[tool] use_tool → pending"),
            err("[permission] melissa__device_info → allow_once"),
            err("[tool] melissa__device_info → done"),
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

#[tokio::test]
async fn real_opencode_transcript_replays() {
    // Recorded from `opencode acp` (2.3.2, flavor opencode) through the lead agent; see the header.
    // Melissa's MCP server is in OpenCode's config, so `session/new` carries none, and the
    // tool is named `melissa_device_info` (no Code Mode).
    let audit_path = scratch("opencode").join("audit.jsonl");
    let (perms, _) = policy(Some(AuditLog::new(&audit_path)));
    let steps = agent::parse(&fixture("opencode_device_info.jsonl"));
    let (opened, agent) = open_replay(steps, open_with(Arc::new(perms), false), None).await;
    let OpenedSession {
        session,
        mut events,
        ..
    } = opened;
    let caps = session.capabilities().clone();
    assert!(caps.load && caps.resume && caps.close && caps.mcp_http);
    assert_eq!(caps.agent.as_deref(), Some("OpenCode 2.3.2"));
    let (sink, mut rx) = EventSink::channel();
    let stop = one_turn(
        session.as_ref(),
        &mut events,
        "Use the device_info tool, then reply with one line: the kernel version.",
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
            err("[tool] melissa_device_info → pending"),
            err("[permission] melissa_device_info → allow_once"),
            err("[tool] melissa_device_info → done"),
            (Stream::Stdout, "Kernel: 7.3.4-300.fc45.x86_64".to_string()),
        ]
    );
    let log = std::fs::read_to_string(&audit_path).unwrap();
    assert!(log.contains("allow_once (other): melissa tool"), "{log}");
}

#[test]
fn opencode_flavor_owns_the_config() {
    use super::{opencode_env, AcpFlavor, AcpSpec};
    let servers = vec![McpServerSpec {
        name: "melissa".into(),
        url: "http://127.0.0.1:8080/mcp/0123456789abcdef".into(),
        headers: vec![("X-A".into(), "b".into())],
    }];
    let env = opencode_env(&servers, Some("opencode/big-pickle"));
    let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let config: Value = serde_json::from_str(&get("OPENCODE_CONFIG_CONTENT").unwrap()).unwrap();
    // Every tool asks; nothing is pre-approved.
    assert_eq!(config["permission"], serde_json::json!({"*": "ask"}));
    let mcp = &config["mcp"]["melissa"];
    assert_eq!(mcp["type"], "remote");
    assert_eq!(mcp["codemode"], false);
    assert_eq!(mcp["url"], "http://127.0.0.1:8080/mcp/0123456789abcdef");
    assert_eq!(mcp["headers"]["X-A"], "b");
    assert_eq!(config["model"], "opencode/big-pickle");
    assert_eq!(get("OPENCODE_DISABLE_PROJECT_CONFIG").as_deref(), Some("1"));
    let no_model: Value = serde_json::from_str(&opencode_env(&[], None)[0].1).unwrap();
    assert!(no_model.get("model").is_none());
    assert_eq!(no_model["mcp"], serde_json::json!({}));

    // The managed preset hides the user's OpenCode config; a generic opencode ACP spec does not.
    let spec = AcpSpec::new("opencode-acp", "opencode", &["acp"]).with_flavor(AcpFlavor::Opencode);
    assert!(spec.effective_sandbox().ro.is_empty());
    assert!(!AcpSpec::new("x", "opencode", &["acp"])
        .effective_sandbox()
        .ro
        .is_empty());
    assert_eq!(super::flavor_for("/usr/bin/opencode"), AcpFlavor::Opencode);
    // `model` is only for the opencode flavor, and must look like `provider/model`.
    let mut ok = spec.clone();
    ok.model = Some("opencode/big-pickle".into());
    ok.validate().unwrap();
    let mut bad = spec;
    bad.model = Some("x\"; rm".into());
    assert!(bad.validate().is_err());
    let mut generic = AcpSpec::new("g", "grok", &[]);
    generic.model = Some("a/b".into());
    assert!(generic.validate().is_err());
}

#[test]
fn prompts_carry_images_but_never_pdfs() {
    use crate::session::PromptBlock;
    use serde_json::json;
    let content = super::client::prompt_content(
        "Look".into(),
        vec![
            PromptBlock::Image {
                mime: "image/webp".into(),
                data: "UklG".into(),
            },
            PromptBlock::Pdf {
                data: "JVBE".into(),
            },
        ],
    );
    assert_eq!(
        serde_json::to_value(&content).unwrap(),
        json!([
            {"type": "image", "data": "UklG", "mimeType": "image/webp"},
            {"type": "text", "text": "Look"},
        ])
    );
}

/// `initialize` for an agent offering `auth` methods (no resume).
fn init_with_auth(auth: Value) -> Vec<agent::Step> {
    use serde_json::json;
    vec![
        agent::Step::send(
            json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": "<any>"}),
        ),
        agent::Step::recv(
            json!({"jsonrpc": "2.0", "id": 0, "result": {"protocolVersion": 1,
            "agentCapabilities": {"loadSession": true, "mcpCapabilities": {"http": true}},
            "authMethods": auth}}),
        ),
    ]
}

/// A mode option like Codex's (config category `mode`), currently `current`.
fn codex_modes(current: &str) -> Value {
    serde_json::json!([{"id": "mode", "name": "Mode", "category": "mode", "type": "select",
        "currentValue": current, "options": [
            {"value": "read-only", "name": "Read-only"},
            {"value": "agent", "name": "Auto review"},
            {"value": "agent-full-access", "name": "Full access"}]}])
}

#[tokio::test]
async fn codex_signs_in_with_its_key_and_is_switched_to_read_only() {
    use serde_json::json;
    let (perms, _) = policy(None);
    let mut steps = init_with_auth(json!([{"id": "api-key", "name": "API Key"},
        {"id": "chat-gpt", "name": "ChatGPT"}]));
    steps.push(agent::Step::send(
        json!({"jsonrpc": "2.0", "id": 1, "method": "authenticate",
        "params": {"methodId": "api-key"}}),
    ));
    steps.push(agent::Step::recv(
        json!({"jsonrpc": "2.0", "id": 1, "result": {}}),
    ));
    steps.push(agent::Step::send(
        json!({"jsonrpc": "2.0", "id": 2, "method": "session/new", "params": "<any>"}),
    ));
    steps.push(agent::Step::recv(json!({"jsonrpc": "2.0", "id": 2,
        "result": {"sessionId": "s-1", "configOptions": codex_modes("agent")}})));
    steps.push(agent::Step::send(json!({"jsonrpc": "2.0", "id": 3,
        "method": "session/set_config_option",
        "params": {"sessionId": "s-1", "configId": "mode", "value": "read-only"}})));
    steps.push(agent::Step::recv(json!({"jsonrpc": "2.0", "id": 3,
        "result": {"configOptions": codex_modes("read-only")}})));
    let mut open = open_with(Arc::new(perms), false);
    open.authenticate = Some("api-key".into());
    open.mode = Some("read-only".into());
    let (opened, agent) = open_replay(steps, open, None).await;
    assert_eq!(opened.session.id(), "s-1");
    opened.session.close().await.unwrap();
    agent.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_mode_the_agent_lacks_fails_the_session_and_unknown_sign_ins_are_skipped() {
    use serde_json::json;
    let (perms, _) = policy(None);
    // Asked to sign in with a method the agent doesn't offer: no `authenticate` is sent.
    let mut steps = init_with_auth(json!([{"id": "chat-gpt", "name": "ChatGPT"}]));
    steps.push(agent::Step::send(
        json!({"jsonrpc": "2.0", "id": 1, "method": "session/new", "params": "<any>"}),
    ));
    steps.push(agent::Step::recv(json!({"jsonrpc": "2.0", "id": 1,
        "result": {"sessionId": "s-1", "configOptions": [{"id": "mode", "name": "Mode",
            "category": "mode", "type": "select", "currentValue": "agent",
            "options": [{"value": "agent", "name": "Auto"}]}]}})));
    let (client_out, agent_in) = tokio::io::duplex(64 * 1024);
    let (agent_out, client_in) = tokio::io::duplex(64 * 1024);
    let agent = tokio::spawn(agent::run(steps, agent_in, agent_out));
    let transport = transport::lines(client_in, client_out, None);
    let mut open = open_with(Arc::new(perms), false);
    open.authenticate = Some("api-key".into());
    open.mode = Some("read-only".into());
    let err = client::start(transport, open, None, StderrTail::default(), None)
        .await
        .err()
        .expect("no `read-only` mode: the session must not open");
    assert!(err.to_string().contains("read-only"), "{err}");
    let _ = agent.await;
}

#[tokio::test]
async fn gemini_models_and_modes_come_from_the_older_session_fields() {
    use serde_json::json;
    let (perms, _) = policy(None);
    let reply = json!({"sessionId": "g-1",
        "modes": {"currentModeId": "autoEdit", "availableModes": [
            {"id": "default", "name": "Default"}, {"id": "autoEdit", "name": "Auto Edit"},
            {"id": "yolo", "name": "YOLO"}]},
        "models": {"currentModelId": "auto", "availableModels": [
            {"modelId": "auto", "name": "Auto"},
            {"modelId": "gemini-2.5-pro", "name": "gemini-2.5-pro"}]}});
    let mut steps = init_with_auth(json!([{"id": "gemini-api-key", "name": "Gemini API key"}]));
    steps.push(agent::Step::send(
        json!({"jsonrpc": "2.0", "id": 1, "method": "session/new", "params": "<any>"}),
    ));
    steps.push(agent::Step::recv(
        json!({"jsonrpc": "2.0", "id": 1, "result": reply}),
    ));
    steps.push(agent::Step::send(
        json!({"jsonrpc": "2.0", "id": 2, "method": "session/set_mode",
        "params": {"sessionId": "g-1", "modeId": "default"}}),
    ));
    steps.push(agent::Step::recv(
        json!({"jsonrpc": "2.0", "id": 2, "result": {}}),
    ));
    steps.push(agent::Step::send(
        json!({"jsonrpc": "2.0", "id": 3, "method": "session/set_model",
        "params": {"sessionId": "g-1", "modelId": "gemini-2.5-pro"}}),
    ));
    steps.push(agent::Step::recv(
        json!({"jsonrpc": "2.0", "id": 3, "result": {}}),
    ));
    let mut open = open_with(Arc::new(perms), false);
    open.mode = Some("default".into());
    open.model = Some("gemini-2.5-pro".into());
    let (opened, agent) = open_replay(steps, open, None).await;
    let models = opened
        .models
        .clone()
        .expect("models from the `models` field");
    assert_eq!(models.current.as_deref(), Some("gemini-2.5-pro"));
    assert_eq!(
        models
            .options
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        ["auto", "gemini-2.5-pro"]
    );
    opened.session.close().await.unwrap();
    agent.await.unwrap().unwrap();

    // Parsing: absent or empty lists are no list; labels fall back to ids.
    assert_eq!(
        super::client::legacy_models(&json!({"sessionId": "x"})),
        None
    );
    assert_eq!(
        super::client::legacy_models(&json!({"models": {"availableModels": []}})),
        None
    );
    let one = super::client::legacy_models(
        &json!({"models": {"availableModels": [{"modelId": "m"}, {"name": "no id"}]}}),
    )
    .unwrap();
    assert_eq!(one.options.len(), 1);
    assert_eq!(one.options[0].label, "m");
    assert_eq!(one.current, None);
}

#[test]
fn new_flavors_set_their_variables_and_codex_keeps_its_sign_in_file_out_with_a_key() {
    use super::{flavor_env, flavor_for, sign_in_method, AcpBackend, AcpFlavor, AcpSpec};
    assert_eq!(flavor_for("/x/gemini"), AcpFlavor::Gemini);
    assert_eq!(flavor_for("codex-acp"), AcpFlavor::Codex);
    assert_eq!(flavor_for("/usr/bin/muse"), AcpFlavor::Muse);
    assert_eq!(flavor_for("codex"), AcpFlavor::Generic);
    let names = |v: Vec<(String, String)>| v.into_iter().map(|(k, _)| k).collect::<Vec<_>>();

    let mut sandbox = crate::SandboxSpec::preset("gemini");
    // No API key: Gemini uses a Google sign-in, if there is one.
    assert_eq!(
        names(flavor_env(AcpFlavor::Gemini, &sandbox)),
        ["GEMINI_FORCE_FILE_STORAGE", "GOOGLE_GENAI_USE_GCA"]
    );
    sandbox.vars = vec![("GEMINI_API_KEY".into(), "k".into())];
    assert_eq!(
        names(flavor_env(AcpFlavor::Gemini, &sandbox)),
        ["GEMINI_FORCE_FILE_STORAGE"]
    );
    assert_eq!(sign_in_method(AcpFlavor::Gemini, &sandbox), None);
    assert_eq!(
        flavor_env(AcpFlavor::Muse, &sandbox),
        [("MUSE_EXPERIMENTAL_ACP".to_string(), "on".to_string())]
    );
    assert!(flavor_env(AcpFlavor::Generic, &sandbox).is_empty());

    // Codex: the ChatGPT sign-in file without a key; `authenticate api-key` with one.
    let spec = AcpSpec::new("codex-acp", "codex-acp", &[]).with_flavor(AcpFlavor::Codex);
    let backend = AcpBackend::new(spec.clone(), "/work".into());
    assert_eq!(sign_in_method(AcpFlavor::Codex, &backend.sandbox), None);
    assert!(backend.sandbox.rw.contains(&".codex/auth.json".to_string()));
    for key in ["OPENAI_API_KEY", "CODEX_API_KEY"] {
        let mut with_key = spec.effective_sandbox();
        with_key.vars = vec![(key.into(), "sk".into())];
        let backend = AcpBackend::new(spec.clone().with_sandbox(with_key), "/work".into());
        assert_eq!(
            sign_in_method(AcpFlavor::Codex, &backend.sandbox),
            Some("api-key")
        );
        assert!(!backend.sandbox.rw.contains(&".codex/auth.json".to_string()));
        assert_eq!(
            names(flavor_env(AcpFlavor::Codex, &backend.sandbox)),
            ["NO_BROWSER"]
        );
    }
}

#[tokio::test]
async fn grok_answers_its_billing_extension_request() {
    use crate::provider_usage::grok;
    use serde_json::json;
    let billing: serde_json::Value =
        serde_json::from_str(include_str!("../provider_usage/fixtures/grok_billing.json")).unwrap();
    let steps = vec![
        agent::Step::send(json!({"jsonrpc": "2.0", "id": 0, "method": "initialize"})),
        agent::Step::recv(
            json!({"jsonrpc": "2.0", "id": 0, "result": {"protocolVersion": 1,
            "agentCapabilities": {}, "authMethods": []}}),
        ),
        agent::Step::send(json!({"jsonrpc": "2.0", "id": 1, "method": "session/new"})),
        agent::Step::recv(json!({"jsonrpc": "2.0", "id": 1, "result": {"sessionId": "s-1"}})),
        agent::Step::send(
            json!({"jsonrpc": "2.0", "id": 2, "method": grok::BILLING_METHOD,
            "params": {}}),
        ),
        agent::Step::recv(json!({"jsonrpc": "2.0", "id": 2, "result": billing})),
    ];
    let (client_out, agent_in) = tokio::io::duplex(64 * 1024);
    let (agent_out, client_in) = tokio::io::duplex(64 * 1024);
    let agent = tokio::spawn(agent::run(steps, agent_in, agent_out));
    let (perms, _) = policy(None);
    let opened = client::start(
        transport::lines(client_in, client_out, None),
        open_with(Arc::new(perms), false),
        None,
        StderrTail::default(),
        Some(grok::BILLING_METHOD),
    )
    .await
    .unwrap();
    let answer = opened.session.plan_usage().await.unwrap().unwrap();
    let fetched = grok::billing(&answer, 1).unwrap();
    assert_eq!(fetched.plan.as_deref(), Some("example-tier"));
    opened.session.close().await.unwrap();
    agent.await.unwrap().unwrap();
}

#[tokio::test]
async fn agents_without_a_usage_request_answer_none() {
    let steps = agent::parse(&fixture("fake_tool_turn.jsonl"));
    let (perms, _) = policy(None);
    let (opened, _agent) = open_replay(steps, open_with(Arc::new(perms), true), None).await;
    assert_eq!(opened.session.plan_usage().await.unwrap(), None);
}
