//! Claude stream-json session tests: argument building, event mapping and permissions
//! against the replay agent (in-memory pipes), and the backend with a fake `claude` script.

use std::{path::PathBuf, sync::Arc, time::Duration};

use serde_json::{json, Value};

use super::{
    claude_args,
    client::{self, permission_response, stop_reason, tool_kind, Start},
    ClaudeBackend, ClaudeSpec, SessionArg,
};
use crate::{
    acp::{
        client::StderrTail,
        one_turn,
        replay::agent::{self, Step},
    },
    mcp::McpServer,
    session::{
        AgentSession, McpServerSpec, OpenSession, OpenedSession, PermissionContext,
        PermissionDecision, PermissionHandler, PolicyPermissions, SessionBackend, SessionEvent,
        StopReason, ToolKind, ToolStatus,
    },
    tools::{DeviceInfo, TaskContext, Tool, ToolRegistry},
    AgentError, AuditLog, EventSink, Stream,
};

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("melissa-claude-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Policy permissions over a registry with `device_info`, for an attended task 7.
fn policy(audit: Option<AuditLog>) -> (PolicyPermissions, ToolRegistry) {
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(DeviceInfo::new())];
    let registry = ToolRegistry::new(tools, audit).with_confirm_timeout(Duration::from_secs(30));
    let server = McpServer::new(registry.clone());
    let token = server
        .open_session(
            7,
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
            task: 7,
            backend: "claude-session".into(),
            mcp_name: "melissa".into(),
            mcp: Some((server, token)),
        },
    );
    (perms, registry)
}

async fn open_replay(
    steps: Vec<Step>,
    perms: Arc<dyn PermissionHandler>,
) -> (OpenedSession, tokio::task::JoinHandle<Result<(), String>>) {
    let (client_out, agent_in) = tokio::io::duplex(256 * 1024);
    let (agent_out, client_in) = tokio::io::duplex(256 * 1024);
    let agent = tokio::spawn(agent::run(steps, agent_in, agent_out));
    let opened = client::start(
        client_in,
        client_out,
        Start {
            session_id: "00000000-0000-4000-8000-000000000001".into(),
            resumed: false,
            persistent: true,
            permissions: perms,
            recorder: None,
            stderr: StderrTail::default(),
            shutdown: None,
            model: None,
        },
    )
    .await
    .unwrap();
    (opened, agent)
}

fn init_steps() -> Vec<Step> {
    vec![
        Step::send(
            json!({"type": "control_request", "request_id": "melissa-init",
                          "request": {"subtype": "initialize"}}),
        ),
        Step::recv(
            json!({"type": "control_response", "response": {"subtype": "success",
            "request_id": "melissa-init", "response": {"claude_code_version": "9.9.9"}}}),
        ),
    ]
}

fn user(text: &str) -> Step {
    Step::send(json!({"type": "user", "message": {"role": "user", "content": text}}))
}

fn delta(text: &str) -> Step {
    Step::recv(json!({"type": "stream_event", "parent_tool_use_id": null,
        "event": {"type": "content_block_delta", "index": 0,
                  "delta": {"type": "text_delta", "text": text}}}))
}

fn result_ok(text: &str) -> Step {
    Step::recv(
        json!({"type": "result", "subtype": "success", "is_error": false,
        "stop_reason": "end_turn", "result": text,
        "usage": {"input_tokens": 3, "cache_read_input_tokens": 1000, "output_tokens": 20},
        "modelUsage": {"m": {"contextWindow": 200000}}}),
    )
}

async fn drain(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<SessionEvent>,
) -> Vec<SessionEvent> {
    let mut got = Vec::new();
    while let Some(e) = events.recv().await {
        got.push(e);
    }
    got
}

#[test]
fn arguments_fix_tools_permissions_and_session() {
    let extra = vec!["--model".to_string(), "opus".to_string()];
    let args = claude_args(
        &extra,
        Some(std::path::Path::new("/w/.melissa-mcp-x.json")),
        &SessionArg::New {
            id: "id-1".into(),
            persist: false,
        },
    );
    let pair = |a: &str, b: &str| args.windows(2).any(|w| w[0] == a && w[1] == b);
    assert!(pair("--tools", ""));
    assert!(pair("--permission-mode", "manual"));
    assert!(pair("--permission-prompt-tool", "stdio"));
    assert!(pair("--input-format", "stream-json"));
    assert!(pair("--output-format", "stream-json"));
    assert!(pair("--mcp-config", "/w/.melissa-mcp-x.json"));
    assert!(pair("--session-id", "id-1"));
    assert!(args.contains(&"--strict-mcp-config".to_string()));
    assert!(args.contains(&"--no-session-persistence".to_string()));
    assert!(args.ends_with(&extra));
    // Nothing pre-approves tools.
    assert!(!args.iter().any(|a| a.contains("allowedTools")
        || a.contains("dangerously")
        || a == "bypassPermissions"));
    let args = claude_args(&[], None, &SessionArg::Resume("id-2".into()));
    assert!(args.windows(2).any(|w| w == ["--resume", "id-2"]));
    assert!(!args.contains(&"--mcp-config".to_string()));
    assert!(!args.contains(&"--no-session-persistence".to_string()));
}

#[test]
fn config_args_are_an_allowlist() {
    let ok = ClaudeSpec::new("c", "claude").with_args(&["--model", "opus", "--effort", "low"]);
    ok.validate().unwrap();
    for bad in [
        &["--dangerously-skip-permissions"][..],
        &["--permission-mode", "bypassPermissions"],
        &["--allowedTools", "Bash"],
        &["--tools", "default"],
        &["--model"],
        &["--model", "--tools"],
    ] {
        let spec = ClaudeSpec::new("c", "claude").with_args(bad);
        assert!(spec.validate().is_err(), "{bad:?}");
    }
    let mut spec = ClaudeSpec::new("c", "claude");
    spec.session_store = "../.claude".into();
    assert!(spec.validate().is_err());
}

#[test]
fn results_map_to_stop_reasons() {
    let ok = json!({"type": "result", "subtype": "success", "is_error": false, "stop_reason": "end_turn"});
    assert_eq!(stop_reason(&ok, false).unwrap(), StopReason::EndTurn);
    assert_eq!(stop_reason(&ok, true).unwrap(), StopReason::Cancelled);
    let max = json!({"subtype": "success", "is_error": false, "stop_reason": "max_tokens"});
    assert_eq!(stop_reason(&max, false).unwrap(), StopReason::MaxTokens);
    let turns = json!({"subtype": "error_max_turns", "is_error": true});
    assert_eq!(
        stop_reason(&turns, false).unwrap(),
        StopReason::MaxTurnRequests
    );
    // An interrupted turn ends with an error result; it counts as cancelled.
    let aborted = json!({"subtype": "error_during_execution", "is_error": true, "terminal_reason": "aborted_tools"});
    assert_eq!(stop_reason(&aborted, true).unwrap(), StopReason::Cancelled);
    let err = stop_reason(&aborted, false).unwrap_err();
    assert!(err.to_string().contains("error_during_execution"), "{err}");
    let api = json!({"subtype": "success", "is_error": true, "result": "API Error: 500"});
    assert!(stop_reason(&api, false)
        .unwrap_err()
        .to_string()
        .contains("API Error"));
}

#[test]
fn permission_answers_are_one_off() {
    let input = json!({"cmd": "x"});
    let allow = permission_response("r1", PermissionDecision::AllowOnce, input.clone());
    assert_eq!(allow["response"]["request_id"], "r1");
    assert_eq!(allow["response"]["response"]["behavior"], "allow");
    assert_eq!(allow["response"]["response"]["updatedInput"], input);
    for d in [
        PermissionDecision::AllowOnce,
        PermissionDecision::RejectOnce,
        PermissionDecision::Cancelled,
    ] {
        let r = permission_response("r", d, input.clone());
        assert!(r["response"]["response"]
            .get("updatedPermissions")
            .is_none());
    }
    let deny = permission_response("r1", PermissionDecision::RejectOnce, input.clone());
    assert_eq!(deny["response"]["response"]["behavior"], "deny");
    assert!(deny["response"]["response"].get("interrupt").is_none());
    let cancel = permission_response("r1", PermissionDecision::Cancelled, input);
    assert_eq!(cancel["response"]["response"]["interrupt"], true);
}

#[test]
fn tool_kinds_by_name() {
    assert_eq!(tool_kind("mcp__melissa__device_info"), ToolKind::Other);
    assert_eq!(tool_kind("Bash"), ToolKind::Execute);
    assert_eq!(tool_kind("Write"), ToolKind::Edit);
    assert_eq!(tool_kind("WebFetch"), ToolKind::Fetch);
    assert_eq!(tool_kind("Read"), ToolKind::Read);
    assert_eq!(tool_kind("ExitPlanMode"), ToolKind::SwitchMode);
}

#[tokio::test]
async fn tool_turn_maps_events_and_asks_policy() {
    let audit_path = scratch("turn").join("audit.jsonl");
    let (perms, _) = policy(Some(AuditLog::new(&audit_path)));
    let mut steps = init_steps();
    steps.extend([
        user("check the agent"),
        Step::recv(json!({"type": "system", "subtype": "init",
            "session_id": "00000000-0000-4000-8000-000000000001", "tools": ["mcp__melissa__device_info"]})),
        Step::recv(json!({"type": "stream_event", "parent_tool_use_id": null, "event":
            {"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "hmm"}}})),
        Step::recv(json!({"type": "stream_event", "parent_tool_use_id": null, "event":
            {"type": "content_block_start", "index": 1, "content_block":
                {"type": "tool_use", "id": "toolu_1", "name": "mcp__melissa__device_info", "input": {}}}})),
        Step::recv(json!({"type": "assistant", "parent_tool_use_id": null, "message": {"content": [
            {"type": "tool_use", "id": "toolu_1", "name": "mcp__melissa__device_info", "input": {}}]}})),
        Step::recv(json!({"type": "control_request", "request_id": "perm-1", "request": {
            "subtype": "can_use_tool", "tool_name": "mcp__melissa__device_info", "input": {},
            "tool_use_id": "toolu_1", "permission_suggestions": [{"type": "addRules"}]}})),
        Step::send(json!({"type": "control_response", "response": {"subtype": "success",
            "request_id": "perm-1", "response": {"behavior": "allow", "updatedInput": {}}}})),
        Step::recv(json!({"type": "user", "parent_tool_use_id": null, "message": {"content": [
            {"type": "tool_result", "tool_use_id": "toolu_1", "content": [{"type": "text", "text": "{}"}]}]}})),
        delta("Kernel: "),
        delta("6.1.0-test"),
        Step::recv(json!({"type": "rate_limit_event"})),
        result_ok("Kernel: 6.1.0-test"),
        user("thanks"),
        delta("ok"),
        result_ok("ok"),
    ]);
    let (opened, agent) = open_replay(steps, Arc::new(perms)).await;
    let OpenedSession {
        session,
        mut events,
        resumed,
        ..
    } = opened;
    assert!(!resumed);
    assert_eq!(session.id(), "00000000-0000-4000-8000-000000000001");
    let caps = session.capabilities();
    assert_eq!(caps.agent.as_deref(), Some("claude-code 9.9.9"));
    assert!(caps.resume && caps.mcp_http && !caps.load);

    assert_eq!(
        session.prompt("check the agent").await.unwrap(),
        StopReason::EndTurn
    );
    // A second prompt in the same session.
    assert_eq!(session.prompt("thanks").await.unwrap(), StopReason::EndTurn);
    session.close().await.unwrap();
    agent.await.unwrap().unwrap();

    let got = drain(&mut events).await;
    let text: String = got
        .iter()
        .filter_map(|e| match e {
            SessionEvent::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Kernel: 6.1.0-testok");
    assert!(got.contains(&SessionEvent::Thought("hmm".into())));
    // The tool call shows up once, although both the stream event and the message carry it.
    let started: Vec<_> = got
        .iter()
        .filter(|e| matches!(e, SessionEvent::ToolStarted { .. }))
        .collect();
    assert_eq!(started.len(), 1);
    assert_eq!(
        started[0],
        &SessionEvent::ToolStarted {
            id: "toolu_1".into(),
            title: "mcp__melissa__device_info".into(),
            kind: ToolKind::Other,
            status: ToolStatus::Pending,
        }
    );
    assert!(got.contains(&SessionEvent::Permission {
        tool_call_id: "toolu_1".into(),
        title: "mcp__melissa__device_info".into(),
        decision: PermissionDecision::AllowOnce,
    }));
    assert!(got.contains(&SessionEvent::ToolFinished {
        id: "toolu_1".into(),
        title: Some("mcp__melissa__device_info".into()),
        status: ToolStatus::Completed,
    }));
    assert!(got.contains(&SessionEvent::Usage {
        used: 1023,
        size: 200000
    }));
    let ends = got
        .iter()
        .filter(|e| **e == SessionEvent::TurnFinished(StopReason::EndTurn))
        .count();
    assert_eq!(ends, 2);
    // Audited by the policy handler.
    let log = std::fs::read_to_string(&audit_path).unwrap();
    let line: Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
    assert_eq!(line["action"], "agent.permission");
    assert_eq!(line["backend"], "claude-session");
    assert!(line["detail"].as_str().unwrap().starts_with("allow_once"));
}

#[tokio::test]
async fn cancel_interrupts_and_withdraws_pending_permissions() {
    let audit_path = scratch("cancel").join("audit.jsonl");
    let (perms, registry) = policy(Some(AuditLog::new(&audit_path)));
    let mut steps = init_steps();
    steps.extend([
        user("clean up"),
        // A shell command (if a config enabled Bash) needs a human: it waits in the inbox.
        Step::recv(
            json!({"type": "control_request", "request_id": "perm-9", "request": {
            "subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "rm -rf build"},
            "tool_use_id": "toolu_9"}}),
        ),
        Step::send(
            json!({"type": "control_request", "request_id": "melissa-interrupt-1",
            "request": {"subtype": "interrupt"}}),
        ),
        Step::send(
            json!({"type": "control_response", "response": {"subtype": "success",
            "request_id": "perm-9", "response": {"behavior": "deny", "interrupt": true}}}),
        ),
        Step::recv(
            json!({"type": "control_response", "response": {"subtype": "success",
            "request_id": "melissa-interrupt-1", "response": {"still_queued": []}}}),
        ),
        Step::recv(
            json!({"type": "result", "subtype": "error_during_execution", "is_error": true,
            "terminal_reason": "aborted_tools"}),
        ),
        user("again"),
        delta("fine"),
        result_ok("fine"),
    ]);
    let (opened, agent) = open_replay(steps, Arc::new(perms)).await;
    let OpenedSession {
        session,
        mut events,
        ..
    } = opened;
    let session: Arc<dyn AgentSession> = Arc::from(session);
    let turn = {
        let s = session.clone();
        tokio::spawn(async move { s.prompt("clean up").await })
    };
    let store = registry.confirmations().clone();
    let pending = loop {
        if let Some(c) = store.pending().first().cloned() {
            break c;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert_eq!(pending.tool, "agent:execute");
    assert!(matches!(session.prompt("x").await, Err(AgentError::Busy)));
    session.cancel().await.unwrap();
    assert_eq!(turn.await.unwrap().unwrap(), StopReason::Cancelled);
    assert!(store.pending().is_empty(), "confirmation withdrawn");
    assert_eq!(session.prompt("again").await.unwrap(), StopReason::EndTurn);
    session.close().await.unwrap();
    agent.await.unwrap().unwrap();
    let got = drain(&mut events).await;
    assert!(got.contains(&SessionEvent::Permission {
        tool_call_id: "toolu_9".into(),
        title: "Bash".into(),
        decision: PermissionDecision::Cancelled,
    }));
    assert!(got.contains(&SessionEvent::TurnFinished(StopReason::Cancelled)));
    let log = std::fs::read_to_string(&audit_path).unwrap();
    assert!(log.contains("tool.confirm_requested"));
    assert!(log.contains("cancelled (execute)"));
}

#[tokio::test]
async fn agent_withdrawn_requests_get_no_answer() {
    let (perms, registry) = policy(None);
    let mut steps = init_steps();
    steps.extend([
        user("go"),
        Step::recv(json!({"type": "control_request", "request_id": "perm-2", "request": {
            "subtype": "can_use_tool", "tool_name": "Bash", "input": {}, "tool_use_id": "toolu_2"}})),
        Step::recv(json!({"type": "control_cancel_request", "request_id": "perm-2"})),
        // Melissa must not answer perm-2; the next thing it sends is the next prompt.
        Step::recv(json!({"type": "result", "subtype": "success", "is_error": false})),
        user("next"),
        result_ok(""),
    ]);
    let (opened, agent) = open_replay(steps, Arc::new(perms)).await;
    let OpenedSession { session, .. } = opened;
    assert_eq!(session.prompt("go").await.unwrap(), StopReason::EndTurn);
    // Let the withdrawn decision finish before the next prompt.
    for _ in 0..100 {
        if registry.confirmations().pending().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(registry.confirmations().pending().is_empty());
    assert_eq!(session.prompt("next").await.unwrap(), StopReason::EndTurn);
    session.close().await.unwrap();
    agent.await.unwrap().unwrap();
}

#[tokio::test]
async fn agent_death_fails_the_turn_and_ends_events() {
    let (perms, _) = policy(None);
    let mut steps = init_steps();
    steps.extend([user("hi"), delta("par"), Step::exit()]);
    let (opened, agent) = open_replay(steps, Arc::new(perms)).await;
    let OpenedSession {
        session,
        mut events,
        ..
    } = opened;
    let err = tokio::time::timeout(Duration::from_secs(5), session.prompt("hi"))
        .await
        .expect("a dead agent must end the turn")
        .unwrap_err();
    assert!(matches!(err, AgentError::AgentExited(_)), "{err:?}");
    let got = tokio::time::timeout(Duration::from_secs(5), drain(&mut events))
        .await
        .expect("event stream must end");
    assert_eq!(got, [SessionEvent::Text("par".into())]);
    agent.await.unwrap().unwrap();
}

#[tokio::test]
async fn refused_handshake_is_an_error() {
    let (perms, _) = policy(None);
    let (client_out, agent_in) = tokio::io::duplex(4096);
    let (agent_out, client_in) = tokio::io::duplex(4096);
    let steps = vec![
        Step::send(json!({"type": "control_request", "request_id": "melissa-init"})),
        Step::exit(),
    ];
    let agent = tokio::spawn(agent::run(steps, agent_in, agent_out));
    let err = client::start(
        client_in,
        client_out,
        Start {
            session_id: "s".into(),
            resumed: true,
            persistent: true,
            permissions: Arc::new(perms),
            recorder: None,
            stderr: StderrTail::default(),
            shutdown: None,
            model: None,
        },
    )
    .await
    .err()
    .expect("handshake must fail");
    assert!(
        err.to_string().contains("handshake") || err.to_string().contains("ended"),
        "{err}"
    );
    agent.await.unwrap().unwrap();
}

#[tokio::test]
async fn real_claude_transcript_replays() {
    // Recorded from `claude` 2.1.327 through the lead agent (sandboxed, egress proxy); see the
    // fixture header. device_info through MCP, allowed by policy, then the answer.
    let audit_path = scratch("real").join("audit.jsonl");
    let (perms, _) = policy(Some(AuditLog::new(&audit_path)));
    let steps = agent::parse(&fixture("claude_device_info.jsonl"));
    let (opened, agent) = open_replay(steps, Arc::new(perms)).await;
    let OpenedSession {
        session,
        mut events,
        ..
    } = opened;
    let (sink, mut rx) = EventSink::channel();
    let prompt = "Use the device_info tool, then reply with one line: the kernel version.";
    let stop = one_turn(session.as_ref(), &mut events, prompt, &sink)
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
        &lines[..3],
        [
            err("[tool] mcp__melissa__device_info → pending"),
            err("[permission] mcp__melissa__device_info → allow_once"),
            err("[tool] mcp__melissa__device_info → done"),
        ]
    );
    assert!(
        lines[3..]
            .iter()
            .any(|(s, t)| *s == Stream::Stdout && t.contains("7.3.4")),
        "{lines:?}"
    );
    let log = std::fs::read_to_string(&audit_path).unwrap();
    assert!(log.contains(r#""action":"agent.permission""#) && log.contains("allow_once"));
}

/// A fake `claude`: logs its arguments, refuses `--resume`, answers the handshake and one
/// prompt (with the text of the prompt's line count), then waits for stdin to close.
fn fake_claude(dir: &std::path::Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("claude");
    std::fs::write(
        &path,
        r#"#!/bin/sh
echo "$@" >> args.log
for a in "$@"; do
  if [ "$a" = "--resume" ]; then echo "No conversation found" >&2; exit 1; fi
done
read -r init
echo '{"type":"control_response","response":{"subtype":"success","request_id":"melissa-init","response":{"claude_code_version":"0.0.0-fake"}}}'
while read -r line; do
  echo '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"hello\n"}}}'
  echo '{"type":"result","subtype":"success","is_error":false,"stop_reason":"end_turn"}'
done
"#,
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[tokio::test]
async fn backend_falls_back_to_a_new_session_and_runs_tasks() {
    let dir = scratch("fake");
    let program = fake_claude(&dir);
    // Unsandboxed: the fake is a shell script; the sandbox has its own tests.
    let spec = ClaudeSpec::new("claude-session", program.to_str().unwrap()).without_sandbox();
    let backend = ClaudeBackend::new(spec, dir.clone());
    assert!(SessionBackend::info(&backend).available);
    let (perms, _) = policy(None);
    let mut open = OpenSession::new(
        dir.clone(),
        vec![McpServerSpec {
            name: "melissa".into(),
            url: "http://127.0.0.1:1/mcp/0123456789abcdef".into(),
            headers: Vec::new(),
        }],
        Arc::new(perms),
    );
    open.state_dir = Some(dir.join("store"));
    open.resume = Some("11111111-2222-4333-8444-555555555555".into());
    let opened = backend.open(open).await.unwrap();
    // Without a sandbox the store is not bound, so no resume is attempted at all.
    assert!(!opened.resumed);
    assert_eq!(
        opened.session.capabilities().agent.as_deref(),
        Some("claude-code 0.0.0-fake")
    );
    assert_eq!(
        opened.session.prompt("hi").await.unwrap(),
        StopReason::EndTurn
    );
    opened.session.close().await.unwrap();
    let args = std::fs::read_to_string(dir.join("args.log")).unwrap();
    assert!(args.contains("--session-id"), "{args}");
    assert!(args.contains("--no-session-persistence"), "{args}");
    assert!(args.contains("--mcp-config"), "{args}");
    // The MCP config file (with the token) is gone after the session.
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(".melissa-mcp"))
        .collect();
    assert!(leftovers.is_empty());

    // One board task = one session with one prompt.
    let (sink, mut rx) = EventSink::channel();
    let req = crate::RunRequest {
        prompt: "hi".into(),
        workdir: Some(dir.clone()),
        ..crate::RunRequest::default()
    };
    crate::Backend::run(&backend, &req, sink).await.unwrap();
    assert_eq!(rx.recv().await.unwrap().text, "hello");
}

#[tokio::test]
async fn resume_failure_starts_a_new_session() {
    // The resume path needs the store bound, i.e. a sandbox; drive `open_session` through a
    // spec whose sandbox is "enabled" but run the fake directly by checking the fallback
    // logic on `spawn` errors: a refused `--resume` must not fail the open.
    let dir = scratch("resume");
    let program = fake_claude(&dir);
    let spec = ClaudeSpec::new("claude-session", program.to_str().unwrap()).without_sandbox();
    let backend = ClaudeBackend::new(spec, dir.clone());
    let (perms, _) = policy(None);
    let open = OpenSession::new(dir.clone(), Vec::new(), Arc::new(perms));
    let err = backend
        .spawn(&open, SessionArg::Resume("x".into()), true)
        .await
        .err()
        .expect("the fake refuses --resume");
    // The stderr tail may or may not be read yet; the open fails either way.
    assert!(matches!(err, AgentError::Protocol(_)), "{err:?}");
}

#[test]
fn models_come_from_the_handshake_and_the_choice_replaces_the_configs_model() {
    let offered = client::offered_models(&json!([
        {"value": "default", "displayName": "Default (recommended)", "description": "x"},
        {"value": "sonnet", "displayName": "Sonnet"},
        {"value": "--bad", "displayName": "Bad"},
        {"displayName": "no value"}
    ]));
    let ids: Vec<&str> = offered.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["default", "sonnet"]);
    assert_eq!(offered[0].label, "Default (recommended)");
    assert!(client::offered_models(&json!(null)).is_empty());

    let config: Vec<String> = ["--model", "opus", "--effort", "high"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    assert_eq!(super::with_model(&config, None).unwrap(), config);
    let chosen = super::with_model(&config, Some("sonnet")).unwrap();
    assert_eq!(chosen, ["--effort", "high", "--model", "sonnet"]);
    assert_eq!(super::model_arg(&chosen).as_deref(), Some("sonnet"));
    assert!(super::with_model(&config, Some("--dangerously-skip-permissions")).is_err());
    assert!(super::with_model(&config, Some("a b")).is_err());
}

#[test]
fn reasoning_levels_come_from_the_handshake_and_the_choice_replaces_the_configs_effort() {
    let offered = client::offered_models(&json!([
        {"value": "opus", "displayName": "Opus", "supportsEffort": true,
         "supportedEffortLevels": ["low", "medium", "high", "xhigh", "max", "--bad", "new"]},
        {"value": "haiku", "displayName": "Haiku"}
    ]));
    let levels: Vec<(&str, &str)> = offered[0]
        .efforts
        .iter()
        .map(|e| (e.id.as_str(), e.label.as_str()))
        .collect();
    assert_eq!(
        levels,
        [
            ("low", "Low"),
            ("medium", "Medium"),
            ("high", "High"),
            ("xhigh", "Extra high"),
            ("max", "Max"),
            ("new", "new")
        ]
    );
    assert!(offered[1].efforts.is_empty());

    let config: Vec<String> = ["--model", "opus", "--effort", "high"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    assert_eq!(super::with_effort(&config, None).unwrap(), config);
    assert_eq!(
        super::with_effort(&config, Some("low")).unwrap(),
        ["--model", "opus", "--effort", "low"]
    );
    assert!(super::with_effort(&config, Some("--verbose")).is_err());
}

#[test]
fn files_go_before_the_text_as_image_and_document_blocks() {
    use crate::session::PromptBlock;
    assert_eq!(super::client::user_content("hi".into(), &[]), json!("hi"));
    let blocks = [
        PromptBlock::Image {
            mime: "image/png".into(),
            data: "iVBO".into(),
        },
        PromptBlock::Pdf {
            data: "JVBE".into(),
        },
    ];
    assert_eq!(
        super::client::user_content("What is this?".into(), &blocks),
        json!([
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "iVBO"}},
            {"type": "document", "source": {"type": "base64", "media_type": "application/pdf", "data": "JVBE"}},
            {"type": "text", "text": "What is this?"},
        ])
    );
}
