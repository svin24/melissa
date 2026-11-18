//! Queen tests against the replay ACP agent (in-memory pipes, no processes, no network).
//! The fake agent calls Melissa's tools over its MCP session like a real agent does.

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::{json, Value};

use super::{
    events::{EventLog, QueenEvent, TurnId},
    transcript::Transcript,
    workers::{WorkerLimits, WorkerPool},
    Queen, QueenError, QueenSettings,
};
use crate::{
    acp::{
        client::{self, StderrTail},
        replay::agent::{self, Step},
        transport,
    },
    mcp::McpServer,
    session::{OpenSession, OpenedSession, SessionBackend},
    tools::{
        BeeInfo, JobScope, Labels, Owner, ResourceKind, ResourceOwners, Scope, TaskContext, Tool,
        ToolError, ToolRegistry, ToolSpec, Verdict,
    },
    AgentError, AuditLog, Backend, BackendInfo, BoxFuture, EchoBackend, EventSink, McpLink,
    RunRequest, TaskBoard, TaskStatus,
};

/// A session backend that plays one scripted agent per `open`.
struct Replay {
    name: &'static str,
    fail: bool,
    scripts: Mutex<VecDeque<Vec<Step>>>,
    server: McpServer,
    agents: Mutex<Vec<tokio::task::JoinHandle<Result<(), String>>>>,
}

impl Replay {
    fn new(name: &'static str, server: McpServer, scripts: Vec<Vec<Step>>) -> Arc<Self> {
        Arc::new(Self {
            name,
            fail: false,
            scripts: Mutex::new(scripts.into()),
            server,
            agents: Mutex::new(Vec::new()),
        })
    }

    fn broken(name: &'static str, server: McpServer) -> Arc<Self> {
        Arc::new(Self {
            fail: true,
            ..Arc::into_inner(Self::new(name, server, Vec::new())).unwrap()
        })
    }

    /// Every scripted agent followed its script.
    async fn check(&self) {
        let agents: Vec<_> = std::mem::take(&mut *self.agents.lock().unwrap());
        for a in agents {
            a.await.unwrap().unwrap();
        }
        assert!(self.scripts.lock().unwrap().is_empty(), "unused scripts");
    }
}

impl SessionBackend for Replay {
    fn info(&self) -> BackendInfo {
        BackendInfo {
            name: self.name.into(),
            kind: "test",
            detail: String::new(),
            available: true,
            tools: true,
        }
    }

    fn open(&self, req: OpenSession) -> BoxFuture<'_, Result<OpenedSession, AgentError>> {
        Box::pin(async move {
            if self.fail {
                return Err(AgentError::Spawn {
                    command: self.name.into(),
                    source: std::io::Error::other("no such agent"),
                });
            }
            let steps = self
                .scripts
                .lock()
                .unwrap()
                .pop_front()
                .expect("a script for this session");
            let (client_out, agent_in) = tokio::io::duplex(64 * 1024);
            let (agent_out, client_in) = tokio::io::duplex(64 * 1024);
            let agent = tokio::spawn(agent::run_with(
                steps,
                agent_in,
                agent_out,
                Some(self.server.clone()),
            ));
            self.agents.lock().unwrap().push(agent);
            client::start(
                transport::lines(client_in, client_out, None),
                req,
                None,
                StderrTail::default(),
            )
            .await
        })
    }
}

/// A board backend with tools that never finishes on its own.
struct Hang;
impl Backend for Hang {
    fn info(&self) -> BackendInfo {
        BackendInfo {
            name: "hang".into(),
            kind: "test",
            detail: String::new(),
            available: true,
            tools: true,
        }
    }
    fn run<'a>(&'a self, _: &'a RunRequest, _: EventSink) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(std::future::pending())
    }
}

/// A tool that reads untrusted content (taints the task), like the real `app_logs`; the
/// Queen in some tests is not allowed to use it.
struct AppLogs;
impl Tool for AppLogs {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "app_logs",
            description: "test",
            labels: Labels {
                reads_untrusted: true,
                ..Labels::READ_ONLY
            },
            params: json!({"type": "object"}),
        }
    }
    fn call<'a>(&'a self, _: Value) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(async { Ok(json!("logs")) })
    }
}

struct Setup {
    queen: Queen,
    server: McpServer,
    replay: Arc<Replay>,
    dir: PathBuf,
}

impl Setup {
    fn audit(&self) -> String {
        std::fs::read_to_string(self.dir.join("audit.jsonl")).unwrap_or_default()
    }
}

/// `nextcloud` is the owner's, `blog` belongs to an earlier job, anything else doesn't exist.
struct FakeOwners;
impl ResourceOwners for FakeOwners {
    fn owner<'a>(
        &'a self,
        _: ResourceKind,
        name: &'a str,
    ) -> BoxFuture<'a, Result<Option<Owner>, String>> {
        Box::pin(async move {
            Ok(match name {
                "nextcloud" => Some(Owner::Human),
                "blog" => Some(Owner::Job("0ld".into())),
                _ => None,
            })
        })
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("melissa-queen-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A Queen on a board with `echo` and `hang`, tools `bee_info`, `app_logs` and the worker
/// tools, and a replay backend `fake-acp` playing `scripts` (one per session).
fn setup_in(dir: PathBuf, scripts: Vec<Vec<Step>>, tune: impl FnOnce(&mut QueenSettings)) -> Setup {
    let events = EventLog::new(Some(Transcript::new(
        dir.join("queen/transcript.jsonl"),
        1 << 20,
    )));
    let pool = WorkerPool::new(WorkerLimits::default(), events.clone());
    let mut tools: Vec<Arc<dyn Tool>> = vec![Arc::new(BeeInfo::new()), Arc::new(AppLogs)];
    tools.extend(pool.tools());
    let audit = AuditLog::new(dir.join("audit.jsonl"));
    let registry = ToolRegistry::new(tools, Some(audit.clone()))
        .with_confirm_timeout(Duration::from_millis(200))
        .with_owners(Arc::new(FakeOwners));
    let server = McpServer::new(registry);
    let board = TaskBoard::with_tools(
        vec![Arc::new(EchoBackend::new()), Arc::new(Hang)],
        Duration::from_secs(60),
        Some(audit),
        McpLink {
            server: server.clone(),
            base_url: "http://127.0.0.1:9/mcp/".into(),
            workroot: dir.join("work"),
        },
    );
    let replay = Replay::new("fake-acp", server.clone(), scripts);
    let broken = Replay::broken("broken-acp", server.clone());
    let mut settings = QueenSettings {
        backends: vec!["broken-acp".into(), "fake-acp".into()],
        system_prompt: "You are the Queen (test).".into(),
        worker_prompt: "You are a worker (test).".into(),
        worker_backend: Some("echo".into()),
        state_dir: dir.join("queen"),
        workdir: dir.join("work/queen"),
        tools: None,
    };
    tune(&mut settings);
    let backends: Vec<Arc<dyn SessionBackend>> = vec![broken, replay.clone()];
    let queen = Queen::new(settings, backends, board, events, &pool).unwrap();
    Setup {
        queen,
        server,
        replay,
        dir,
    }
}

fn setup(name: &str, scripts: Vec<Vec<Step>>) -> Setup {
    setup_in(scratch(name), scripts, |_| {})
}

// --- script helpers -------------------------------------------------------------------

fn handshake(sid: &str) -> Vec<Step> {
    vec![
        Step::send(json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": "<any>"})),
        Step::recv(
            json!({"jsonrpc": "2.0", "id": 0, "result": {"protocolVersion": 1,
            "agentCapabilities": {"loadSession": true, "mcpCapabilities": {"http": true},
                                  "sessionCapabilities": {"resume": {}, "close": {}}},
            "authMethods": []}}),
        ),
        Step::send(
            json!({"jsonrpc": "2.0", "id": 1, "method": "session/new", "params": {"cwd": "<any>",
            "mcpServers": [{"type": "http", "name": "melissa", "url": "<contains:http://127.0.0.1:9/mcp/>", "headers": "<any>"}]}}),
        ),
        Step::recv(json!({"jsonrpc": "2.0", "id": 1, "result": {"sessionId": sid}})),
    ]
}

fn prompt(id: u64, sid: &str, text: &str) -> Step {
    Step::send(
        json!({"jsonrpc": "2.0", "id": id, "method": "session/prompt",
        "params": {"sessionId": sid, "prompt": [{"type": "text", "text": text}]}}),
    )
}

fn update(sid: &str, update: Value) -> Step {
    Step::recv(json!({"jsonrpc": "2.0", "method": "session/update",
        "params": {"sessionId": sid, "update": update}}))
}

fn text(sid: &str, t: &str) -> Step {
    update(
        sid,
        json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": t}}),
    )
}

fn tool(sid: &str, id: &str, title: &str) -> Step {
    update(
        sid,
        json!({"sessionUpdate": "tool_call", "toolCallId": id, "title": title, "kind": "other", "status": "pending"}),
    )
}

fn tool_done(sid: &str, id: &str) -> Step {
    update(
        sid,
        json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": "completed"}),
    )
}

fn done(id: u64, stop: &str) -> Step {
    Step::recv(json!({"jsonrpc": "2.0", "id": id, "result": {"stopReason": stop}}))
}

/// Events from `since` until `turn` finished.
async fn turn_events(queen: &Queen, since: u64, turn: TurnId) -> Vec<QueenEvent> {
    let mut out = Vec::new();
    let mut since = since;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "turn did not finish: {out:#?}"
        );
        let page = queen
            .events()
            .wait_since(since, 100, Duration::from_millis(200))
            .await;
        since = page.next;
        for r in page.events {
            let end = matches!(&r.event, QueenEvent::TurnFinished { turn: t, .. } if *t == turn);
            out.push(r.event);
            if end {
                return out;
            }
        }
    }
}

fn finished(events: &[QueenEvent]) -> &str {
    match events.last() {
        Some(QueenEvent::TurnFinished { stop_reason, .. }) => stop_reason,
        other => panic!("not finished: {other:?}"),
    }
}

// --- tests ----------------------------------------------------------------------------

#[tokio::test]
async fn turn_with_a_tool_call_then_a_second_turn_in_the_same_session() {
    let mut script = handshake("s-1");
    script.extend([
        // The first message carries the system prompt.
        prompt(2, "s-1", "<contains:You are the Queen (test).>"),
        tool("s-1", "t1", "melissa__bee_info"),
        Step::recv(
            json!({"jsonrpc": "2.0", "id": "perm-1", "method": "session/request_permission",
            "params": {"sessionId": "s-1", "toolCall": {"toolCallId": "t1"}, "options": [
                {"optionId": "allow-always", "name": "Always", "kind": "allow_always"},
                {"optionId": "allow-once", "name": "Allow", "kind": "allow_once"}]}}),
        ),
        Step::send(
            json!({"jsonrpc": "2.0", "id": "perm-1", "result": {"outcome": {"outcome": "selected", "optionId": "allow-once"}}}),
        ),
        Step::mcp(
            "bee_info",
            json!({}),
            json!({"result": {"isError": false}}),
        ),
        tool_done("s-1", "t1"),
        text("s-1", "Kernel: "),
        text("s-1", "6.1.0-test"),
        done(2, "end_turn"),
        // The second message goes as is: same session, no preamble.
        prompt(3, "s-1", "And the OS?"),
        text("s-1", "Linux."),
        done(3, "end_turn"),
    ]);
    let s = setup("turn", vec![script]);
    let (turn, since) = s.queen.send("What kernel does this comb run?").unwrap();
    assert_eq!(s.queen.send("too soon"), Err(QueenError::Busy));
    assert_eq!(s.queen.send("  "), Err(QueenError::Empty));
    let events = turn_events(&s.queen, since, turn).await;
    assert_eq!(finished(&events), "end_turn");
    let expect = [
        QueenEvent::UserMessage {
            turn,
            text: "What kernel does this comb run?".into(),
        },
        QueenEvent::BackendFailed {
            backend: "broken-acp".into(),
            error: "failed to start `broken-acp`: no such agent".into(),
        },
        QueenEvent::SessionOpened {
            backend: "fake-acp".into(),
            resumed: false,
        },
        QueenEvent::ToolCall {
            turn: Some(turn),
            id: "t1".into(),
            title: "melissa__bee_info".into(),
            status: "pending".into(),
        },
        QueenEvent::Permission {
            turn: Some(turn),
            title: "melissa__bee_info".into(),
            decision: "allow_once".into(),
        },
        QueenEvent::ToolCall {
            turn: Some(turn),
            id: "t1".into(),
            title: "melissa__bee_info".into(),
            status: "done".into(),
        },
        QueenEvent::Text {
            turn,
            text: "Kernel: ".into(),
        },
        QueenEvent::Text {
            turn,
            text: "6.1.0-test".into(),
        },
        QueenEvent::QueenMessage {
            turn,
            text: "Kernel: 6.1.0-test".into(),
        },
        QueenEvent::TurnFinished {
            turn,
            stop_reason: "end_turn".into(),
        },
    ];
    assert_eq!(events, expect);
    assert_eq!(s.queen.status().backend.as_deref(), Some("fake-acp"));

    let (turn2, since) = s.queen.send("And the OS?").unwrap();
    let events = turn_events(&s.queen, since, turn2).await;
    assert!(events.contains(&QueenEvent::QueenMessage {
        turn: turn2,
        text: "Linux.".into()
    }));
    assert!(!events
        .iter()
        .any(|e| matches!(e, QueenEvent::SessionOpened { .. })));
    s.queen.close().await;
    s.replay.check().await;

    // Audit: the user's messages, the session, the agent's permission and the MCP call,
    // all under the Queen's task id.
    let audit = s.audit();
    let lines: Vec<Value> = audit
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let actions: Vec<&str> = lines
        .iter()
        .map(|l| l["action"].as_str().unwrap())
        .collect();
    assert_eq!(
        actions,
        [
            "queen.message",
            "queen.session",
            "agent.permission",
            "tool.call",
            "queen.message"
        ]
    );
    let qt = s.queen.task_id();
    assert!(lines.iter().all(|l| l["task"] == qt));
    assert_eq!(lines[3]["tool"], "bee_info");
    // Transcript: no text pieces, but the whole reply.
    let transcript = std::fs::read_to_string(s.dir.join("queen/transcript.jsonl")).unwrap();
    assert!(!transcript.contains(r#""type":"text""#));
    assert!(transcript.contains(r#""type":"queen_message""#));
    assert!(transcript.contains("Kernel: 6.1.0-test"));
}

#[tokio::test]
async fn queen_starts_a_worker_and_reads_its_result() {
    let mut script = handshake("s-1");
    script.extend([
        prompt(2, "s-1", "<any>"),
        tool("s-1", "t1", "melissa__start_worker"),
        // The Queen's task is 1, so the first worker is task 2.
        Step::mcp(
            "start_worker",
            json!({"goal": "Report the kernel version.", "tools": ["bee_info"]}),
            json!({"result": {"isError": false, "structuredContent": {
                "worker": 2, "backend": "echo", "tools": ["bee_info"], "status": "running",
                "timeout_secs": 300}}}),
        ),
        tool_done("s-1", "t1"),
        tool("s-1", "t2", "melissa__worker_status"),
        Step::mcp(
            "worker_status",
            json!({"worker": 2, "wait_secs": 5}),
            json!({"result": {"isError": false, "structuredContent": {
                "worker": 2, "status": "succeeded", "goal": "Report the kernel version.",
                "result": "<contains:echo: Report the kernel version.>"}}}),
        ),
        tool_done("s-1", "t2"),
        text("s-1", "The worker is done."),
        done(2, "end_turn"),
    ]);
    let s = setup("worker", vec![script]);
    let (turn, since) = s
        .queen
        .send("Start a worker that reports the kernel version.")
        .unwrap();
    let events = turn_events(&s.queen, since, turn).await;
    assert_eq!(finished(&events), "end_turn");
    s.queen.close().await;
    s.replay.check().await;
    assert!(events.contains(&QueenEvent::WorkerStarted {
        worker: 2,
        backend: "echo".into(),
        goal: "Report the kernel version.".into(),
    }));
    // The watcher publishes the end of the worker (it may come after the turn's end).
    let all = s.queen.events().since(0, 1000).events;
    let finished_at = all.iter().position(|r| {
        r.event
            == QueenEvent::WorkerFinished {
                worker: 2,
                status: "succeeded".into(),
                error: None,
            }
    });
    assert!(finished_at.is_some(), "{all:#?}");
    let workers = s.queen.workers();
    assert_eq!(workers.len(), 1);
    assert_eq!(workers[0].parent, Some(s.queen.task_id()));
    assert_eq!(workers[0].status, TaskStatus::Succeeded);

    let audit = s.audit();
    let dispatch: Value = audit
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|l| l["action"] == "task.dispatch")
        .unwrap();
    assert_eq!(dispatch["actor"], "queen");
    assert_eq!(dispatch["parent"], s.queen.task_id());
    assert_eq!(dispatch["task"], 2);
    assert!(
        audit.contains(r#""actor":"queen","action":"task.finish""#),
        "{audit}"
    );
    for needle in [
        r#""tool":"start_worker""#,
        r#""tool":"worker_status""#,
        r#""action":"task.finish""#,
    ] {
        assert!(audit.contains(needle), "{needle} missing:\n{audit}");
    }
}

/// Call a tool over MCP as session `token`; returns the `result` object.
async fn call(server: &McpServer, token: &str, name: &str, args: Value) -> Value {
    let reply = server
        .handle(
            token,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                   "params": {"name": name, "arguments": args}}),
        )
        .await
        .unwrap()
        .unwrap();
    reply["result"].clone()
}

async fn tool_names(server: &McpServer, token: &str) -> Vec<String> {
    let reply = server
        .handle(
            token,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        )
        .await
        .unwrap()
        .unwrap();
    reply["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

fn err_text(result: &Value) -> String {
    assert_eq!(result["isError"], true, "{result}");
    result["content"][0]["text"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn worker_guards_allowlist_depth_and_concurrency() {
    // The Queen may use bee_info and the worker tools, not app_logs.
    let s = setup_in(scratch("guards"), Vec::new(), |q| {
        q.tools = Some(
            [
                "bee_info",
                "start_worker",
                "worker_status",
                "list_workers",
                "cancel_worker",
            ]
            .map(String::from)
            .to_vec(),
        );
    });
    let (ctx, allow) = s.queen.session_policy();
    let queen = s
        .server
        .open_session(s.queen.task_id(), ctx, allow)
        .unwrap();
    let mut names = tool_names(&s.server, &queen).await;
    names.sort();
    assert_eq!(
        names,
        [
            "bee_info",
            "cancel_worker",
            "list_workers",
            "start_worker",
            "worker_status"
        ]
    );

    // Allowlist: only a subset of the Queen's own tools, never the worker tools.
    for tools in [
        json!(["app_logs"]),
        json!(["start_worker"]),
        json!(["nope"]),
    ] {
        let r = call(
            &s.server,
            &queen,
            "start_worker",
            json!({"goal": "x", "backend": "hang", "tools": tools}),
        )
        .await;
        assert!(err_text(&r).contains("cannot be given to a worker"), "{r}");
    }
    let r = call(
        &s.server,
        &queen,
        "start_worker",
        json!({"goal": "x", "timeout_secs": 100_000}),
    )
    .await;
    assert!(err_text(&r).contains("timeout_secs"));
    let r = call(
        &s.server,
        &queen,
        "start_worker",
        json!({"goal": "x", "bogus": 1}),
    )
    .await;
    assert!(err_text(&r).contains("unknown argument"));

    // A worker gets the Queen's tools minus the worker tools, unattended, no delegation.
    let r = call(
        &s.server,
        &queen,
        "start_worker",
        json!({"goal": "wait", "backend": "hang"}),
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["tools"], json!(["bee_info"]));
    let w1 = r["structuredContent"]["worker"].as_u64().unwrap();
    let config: Value = serde_json::from_str(
        &std::fs::read_to_string(s.dir.join(format!("work/task-{w1}/mcp.json"))).unwrap(),
    )
    .unwrap();
    let url = config["mcpServers"]["melissa"]["url"].as_str().unwrap();
    let worker = url.rsplit('/').next().unwrap().to_string();
    assert_eq!(tool_names(&s.server, &worker).await, ["bee_info"]);
    let r = call(
        &s.server,
        &worker,
        "start_worker",
        json!({"goal": "recurse"}),
    )
    .await;
    assert!(err_text(&r).contains("unknown tool `start_worker`"));
    let r = call(&s.server, &worker, "app_logs", json!({})).await;
    assert!(err_text(&r).contains("unknown tool"));
    let wctx = s.server.session_context(&worker).unwrap();
    assert!(!wctx.attended && !wctx.delegate);

    // At most two at once.
    let r = call(
        &s.server,
        &queen,
        "start_worker",
        json!({"goal": "wait", "backend": "hang"}),
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    let r = call(
        &s.server,
        &queen,
        "start_worker",
        json!({"goal": "wait", "backend": "hang"}),
    )
    .await;
    assert!(err_text(&r).contains("at most 2"), "{r}");
    let r = call(&s.server, &queen, "list_workers", json!({})).await;
    assert_eq!(
        r["structuredContent"]["workers"].as_array().unwrap().len(),
        2
    );
    let r = call(&s.server, &queen, "cancel_worker", json!({"worker": w1})).await;
    assert_eq!(r["structuredContent"]["status"], "cancelled");
    assert!(
        !s.server.has_session(&worker),
        "cancel closes the worker's tools"
    );
    let r = call(
        &s.server,
        &queen,
        "start_worker",
        json!({"goal": "wait", "backend": "hang"}),
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");

    // Only the Queen's own workers.
    let r = call(
        &s.server,
        &queen,
        "worker_status",
        json!({"worker": s.queen.task_id()}),
    )
    .await;
    assert!(err_text(&r).contains("no worker"));
    let r = call(&s.server, &queen, "worker_status", json!({"worker": w1})).await;
    assert_eq!(r["structuredContent"]["status"], "cancelled");

    // A UI task (no delegation) does not see the worker tools either.
    let ui = s
        .server
        .open_session(
            99,
            TaskContext {
                attended: true,
                ..TaskContext::default()
            },
            None,
        )
        .unwrap();
    let mut names = tool_names(&s.server, &ui).await;
    names.sort();
    assert_eq!(names, ["app_logs", "bee_info"]);
}

#[tokio::test]
async fn a_tainted_queen_needs_approval_to_start_workers() {
    let s = setup("taint", Vec::new());
    let registry = s.server.registry().clone();
    let mut ctx = s.queen.session_policy().0;
    ctx.tainted = true;
    let since = s.queen.events().last_seq();
    let call = {
        let registry = registry.clone();
        let task = s.queen.task_id();
        tokio::spawn(async move {
            registry
                .call(task, &mut ctx, "start_worker", json!({"goal": "x"}))
                .await
        })
    };
    // The confirmation shows up in the Queen's event stream; deny it.
    let mut cursor = since;
    let id = loop {
        let page = s
            .queen
            .events()
            .wait_since(cursor, 10, Duration::from_secs(5))
            .await;
        cursor = page.next;
        if let Some(id) = page.events.iter().find_map(|r| match &r.event {
            QueenEvent::ConfirmationNeeded { id, tool, .. } if tool == "start_worker" => Some(*id),
            _ => None,
        }) {
            break id;
        }
    };
    s.queen.confirmations().resolve(id, Verdict::Denied);
    assert_eq!(call.await.unwrap(), Err(ToolError::Denied));
    assert!(s.queen.workers().is_empty());
    // The resolution is published too.
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(s
        .queen
        .events()
        .since(since, 100)
        .events
        .iter()
        .any(|r| r.event
            == QueenEvent::ConfirmationResolved {
                id,
                outcome: "denied".into()
            }));
}

#[tokio::test]
async fn cancel_stops_the_turn() {
    let mut script = handshake("s-1");
    script.extend([
        prompt(2, "s-1", "<any>"),
        text("s-1", "Working on it"),
        // The agent waits until Melissa cancels.
        Step::send(
            json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": "s-1"}}),
        ),
        done(2, "cancelled"),
    ]);
    let s = setup("cancel", vec![script]);
    assert!(!s.queen.cancel().await, "nothing to cancel yet");
    let (turn, since) = s.queen.send("Do something long.").unwrap();
    // Wait for the first text, then cancel.
    let mut cursor = since;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{:#?}",
            s.queen.events().since(0, 100)
        );
        let page = s
            .queen
            .events()
            .wait_since(cursor, 100, Duration::from_secs(5))
            .await;
        cursor = page.next;
        if page
            .events
            .iter()
            .any(|r| matches!(r.event, QueenEvent::Text { .. }))
        {
            break;
        }
    }
    assert!(s.queen.cancel().await);
    let events = turn_events(&s.queen, since, turn).await;
    assert_eq!(finished(&events), "cancelled");
    assert!(events.contains(&QueenEvent::QueenMessage {
        turn,
        text: "Working on it".into()
    }));
    assert_eq!(s.queen.status().turn, None);
    s.queen.close().await;
    s.replay.check().await;
    assert!(s.audit().contains(r#""action":"queen.cancel""#));
}

#[tokio::test]
async fn agent_death_ends_the_turn_and_the_next_message_starts_a_new_session() {
    let mut first = handshake("s-1");
    first.extend([
        prompt(2, "s-1", "<any>"),
        text("s-1", "partial"),
        Step::exit(),
    ]);
    // The new session tries to resume s-1 (the agent no longer has it), then starts fresh,
    // primed with the earlier conversation from the transcript.
    let mut second = handshake("s-2");
    second.splice(
        2..2,
        [
            Step::send(json!({"jsonrpc": "2.0", "id": 5, "method": "session/resume", "params": {"sessionId": "s-1", "cwd": "<any>", "mcpServers": "<any>"}})),
            Step::recv(json!({"jsonrpc": "2.0", "id": 5, "error": {"code": -32002, "message": "no such session"}})),
        ],
    );
    second.extend([
        prompt(2, "s-2", "<contains:user: Remember the word apple.>"),
        text("s-2", "The word was apple."),
        done(2, "end_turn"),
    ]);
    let s = setup("death", vec![first, second]);
    let (turn, since) = s.queen.send("Remember the word apple.").unwrap();
    let events = turn_events(&s.queen, since, turn).await;
    assert_eq!(finished(&events), "error");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, QueenEvent::SessionLost { .. })),
        "{events:#?}"
    );
    assert!(events.iter().any(|e| matches!(e, QueenEvent::Error { message, .. } if message.contains("context may be lost"))));
    assert!(events.contains(&QueenEvent::QueenMessage {
        turn,
        text: "partial".into()
    }));
    assert_eq!(s.queen.status().backend, None);

    let (turn, since) = s.queen.send("What was the word?").unwrap();
    let events = turn_events(&s.queen, since, turn).await;
    assert_eq!(finished(&events), "end_turn");
    assert!(events.contains(&QueenEvent::SessionOpened {
        backend: "fake-acp".into(),
        resumed: false
    }));
    s.queen.close().await;
    s.replay.check().await;
}

#[tokio::test]
async fn a_restarted_daemon_resumes_the_agent_session() {
    let dir = scratch("resume");
    let mut first = handshake("s-1");
    first.extend([
        prompt(2, "s-1", "<any>"),
        text("s-1", "Hi."),
        done(2, "end_turn"),
    ]);
    let s = setup_in(dir.clone(), vec![first], |_| {});
    let (turn, since) = s.queen.send("Hello.").unwrap();
    turn_events(&s.queen, since, turn).await;
    s.queen.close().await;
    s.replay.check().await;
    let saved = std::fs::read_to_string(dir.join("queen/session.json")).unwrap();
    assert!(saved.contains(r#""session_id":"s-1""#), "{saved}");

    // A new daemon (new Queen, same state dir) resumes s-1 and does not send the system
    // prompt again: the agent still has it.
    let mut second = handshake("unused");
    second.truncate(2);
    second.extend([
        Step::send(json!({"jsonrpc": "2.0", "id": 1, "method": "session/resume", "params": {"sessionId": "s-1", "cwd": "<any>", "mcpServers": "<any>"}})),
        Step::recv(json!({"jsonrpc": "2.0", "id": 1, "result": {}})),
        prompt(2, "s-1", "Still there?"),
        text("s-1", "Yes."),
        done(2, "end_turn"),
    ]);
    let s = setup_in(dir, vec![second], |_| {});
    let (turn, since) = s.queen.send("Still there?").unwrap();
    let events = turn_events(&s.queen, since, turn).await;
    assert!(events.contains(&QueenEvent::SessionOpened {
        backend: "fake-acp".into(),
        resumed: true
    }));
    s.queen.close().await;
    s.replay.check().await;
}

#[tokio::test]
async fn taint_outlives_the_session_and_the_daemon() {
    let dir = scratch("taint-restart");
    let mut first = handshake("s-1");
    first.extend([
        prompt(2, "s-1", "<any>"),
        Step::mcp("app_logs", json!({}), json!({"result": {"isError": false}})),
        text("s-1", "The logs look fine."),
        done(2, "end_turn"),
    ]);
    let s = setup_in(dir.clone(), vec![first], |_| {});
    let (turn, since) = s.queen.send("Check the logs.").unwrap();
    turn_events(&s.queen, since, turn).await;
    let saved = std::fs::read_to_string(dir.join("queen/session.json")).unwrap();
    assert!(saved.contains(r#""tainted":true"#), "{saved}");
    // Simulate a crash: no clean close, the agent just goes away.
    drop(s);

    // A new daemon resumes s-1, which still holds the logs: its MCP session starts tainted,
    // so starting a worker needs a confirmation (none comes: the call times out).
    let mut second = handshake("unused");
    second.truncate(2);
    second.extend([
        Step::send(json!({"jsonrpc": "2.0", "id": 1, "method": "session/resume", "params": {"sessionId": "s-1", "cwd": "<any>", "mcpServers": "<any>"}})),
        Step::recv(json!({"jsonrpc": "2.0", "id": 1, "result": {}})),
        prompt(2, "s-1", "Start a worker."),
        Step::mcp(
            "start_worker",
            json!({"goal": "x"}),
            json!({"result": {"isError": true}}),
        ),
        text("s-1", "That needs approval."),
        done(2, "end_turn"),
    ]);
    let s = setup_in(dir.clone(), vec![second], |_| {});
    let (turn, since) = s.queen.send("Start a worker.").unwrap();
    let events = turn_events(&s.queen, since, turn).await;
    assert!(events.contains(&QueenEvent::SessionOpened {
        backend: "fake-acp".into(),
        resumed: true
    }));
    assert!(
        s.audit().contains("tool.confirm_requested"),
        "{}",
        s.audit()
    );
    assert!(s.queen.workers().is_empty());
    s.queen.close().await;
    s.replay.check().await;
    let saved = std::fs::read_to_string(dir.join("queen/session.json")).unwrap();
    assert!(
        saved.contains(r#""tainted":true"#),
        "still tainted: {saved}"
    );
}

#[tokio::test]
async fn agent_death_between_turns_is_noticed_before_the_next_message() {
    let mut first = handshake("s-1");
    first.extend([
        prompt(2, "s-1", "<any>"),
        text("s-1", "Hi."),
        done(2, "end_turn"),
        Step::exit(),
    ]);
    let mut second = handshake("s-2");
    second.splice(
        2..2,
        [
            Step::send(
                json!({"jsonrpc": "2.0", "id": 5, "method": "session/resume", "params": "<any>"}),
            ),
            Step::recv(json!({"jsonrpc": "2.0", "id": 5, "result": {}})),
        ],
    );
    // Resumed: the id stays s-1 and the message goes as is.
    second.truncate(4);
    second.extend([
        prompt(2, "s-1", "Hello again."),
        text("s-1", "Hi again."),
        done(2, "end_turn"),
    ]);
    let s = setup("between", vec![first, second]);
    let (turn, since) = s.queen.send("Hello.").unwrap();
    turn_events(&s.queen, since, turn).await;
    // The agent exits after the turn; the Queen notices without a new message.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while s.queen.status().backend.is_some() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "session loss not noticed"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(s
        .queen
        .events()
        .since(0, 100)
        .events
        .iter()
        .any(|r| matches!(r.event, QueenEvent::SessionLost { .. })));
    // The next message works: a resumed session, no error.
    let (turn, since) = s.queen.send("Hello again.").unwrap();
    let events = turn_events(&s.queen, since, turn).await;
    assert_eq!(finished(&events), "end_turn");
    assert!(events.contains(&QueenEvent::SessionOpened {
        backend: "fake-acp".into(),
        resumed: true
    }));
    s.queen.close().await;
    s.replay.check().await;
}

#[tokio::test]
async fn no_working_backend_fails_the_turn() {
    let s = setup_in(scratch("nobackend"), Vec::new(), |q| {
        q.backends = vec!["broken-acp".into(), "missing".into()];
    });
    let (turn, since) = s.queen.send("Hello?").unwrap();
    let events = turn_events(&s.queen, since, turn).await;
    assert_eq!(finished(&events), "error");
    assert!(events.iter().any(|e| matches!(e, QueenEvent::Error { message, .. } if message.contains("tried: broken-acp, missing"))));
    // The Queen is free for the next message.
    assert_eq!(s.queen.status().turn, None);
}

/// The MCP token of worker `id` (from its mcp.json).
fn worker_token(s: &Setup, id: u64) -> String {
    let config: Value = serde_json::from_str(
        &std::fs::read_to_string(s.dir.join(format!("work/task-{id}/mcp.json"))).unwrap(),
    )
    .unwrap();
    let url = config["mcpServers"]["melissa"]["url"].as_str().unwrap();
    url.rsplit('/').next().unwrap().to_string()
}

#[tokio::test]
async fn worker_scopes_are_a_subset_of_the_queens_and_owner_grants_need_the_owner() {
    let s = setup("scopes", Vec::new());
    let (ctx, allow) = s.queen.session_policy();
    assert!(matches!(&ctx.scope, Scope::ForOwner { agent } if agent == "the Queen"));
    let queen = s
        .server
        .open_session(s.queen.task_id(), ctx, allow)
        .unwrap();

    // Default: only what the worker creates; its job key is fresh and random.
    let r = call(
        &s.server,
        &queen,
        "start_worker",
        json!({"goal": "wait", "backend": "hang"}),
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    let scope = &r["structuredContent"]["scope"];
    assert_eq!(scope["apps"], json!([]));
    assert_eq!(scope["create_apps"], true);
    assert_eq!(scope["for_owner"], false);
    let key = scope["job"].as_str().unwrap().to_string();
    assert!(
        crate::tools::scope::valid_job_key(&key) && key.len() == 12,
        "{key}"
    );
    let w1 = r["structuredContent"]["worker"].as_u64().unwrap();
    let wctx = s.server.session_context(&worker_token(&s, w1)).unwrap();
    assert!(matches!(&wctx.scope, Scope::Job(j) if j.key == key && j.apps.is_empty() && j.create));
    let r = call(&s.server, &queen, "cancel_worker", json!({"worker": w1})).await;
    assert_eq!(r["isError"], false, "{r}");

    // An app of an earlier job: the Queen may change it, so it may grant it, no question.
    let r = call(
        &s.server,
        &queen,
        "start_worker",
        json!({"goal": "fix the blog", "backend": "hang", "apps": ["blog"], "create_apps": false}),
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["scope"]["apps"], json!(["blog"]));
    assert!(s.queen.confirmations().pending().is_empty());
    let w2 = r["structuredContent"]["worker"].as_u64().unwrap();
    call(&s.server, &queen, "cancel_worker", json!({"worker": w2})).await;

    // The owner's Nextcloud: the owner must approve the grant; the question names them.
    let registry = s.server.registry().clone();
    let task = s.queen.task_id();
    let grant = {
        let registry = registry.clone();
        let mut ctx = s.queen.session_policy().0;
        tokio::spawn(async move {
            registry
                .call(
                    task,
                    &mut ctx,
                    "start_worker",
                    json!({"goal": "update nextcloud", "backend": "hang", "apps": ["nextcloud"]}),
                )
                .await
        })
    };
    let pending = loop {
        if let Some(c) = s.queen.confirmations().pending().first().cloned() {
            break c;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert_eq!(pending.tool, "start_worker");
    assert_eq!(
        pending.reason,
        "`nextcloud` belongs to you; the Queen wants to let a worker change it"
    );
    s.queen.confirmations().resolve(pending.id, Verdict::Denied);
    assert_eq!(grant.await.unwrap(), Err(ToolError::Denied));
    assert_eq!(
        s.queen
            .workers()
            .iter()
            .filter(|w| w.status == TaskStatus::Running)
            .count(),
        0,
        "no worker without the owner's yes"
    );

    // Creating for the owner needs the owner's yes too (timeout = no).
    let r = call(
        &s.server,
        &queen,
        "start_worker",
        json!({"goal": "x", "backend": "hang", "for_owner": true}),
    )
    .await;
    assert!(err_text(&r).contains("timed out"), "{r}");

    // A session without the owner's authority can't hand out apps, even with delegation.
    let mut job = TaskContext {
        delegate: true,
        attended: true,
        scope: Scope::Job(JobScope {
            key: "k1".into(),
            apps: Default::default(),
            create: true,
            for_owner: false,
        }),
        ..TaskContext::default()
    };
    let r = registry
        .call(
            99,
            &mut job,
            "start_worker",
            json!({"goal": "x", "backend": "hang", "apps": ["blog"]}),
        )
        .await;
    assert!(
        matches!(&r, Err(ToolError::Refused(m)) if m.contains("not yours")),
        "{r:?}"
    );
    let r = call(
        &s.server,
        &queen,
        "start_worker",
        json!({"goal": "x", "apps": ["Bad Name"]}),
    )
    .await;
    assert!(err_text(&r).contains("app names"), "{r}");
}

#[tokio::test]
async fn agents_reach_only_the_workers_they_started() {
    let s = setup("own-workers", Vec::new());
    let (ctx, allow) = s.queen.session_policy();
    let queen = s
        .server
        .open_session(s.queen.task_id(), ctx.clone(), allow.clone())
        .unwrap();
    let r = call(
        &s.server,
        &queen,
        "start_worker",
        json!({"goal": "wait", "backend": "hang"}),
    )
    .await;
    let w = r["structuredContent"]["worker"].as_u64().unwrap();

    // Another delegating session (not the Queen's task) sees none of them.
    let other = s.server.open_session(500, ctx, allow).unwrap();
    for (tool, args) in [
        ("worker_status", json!({"worker": w})),
        ("cancel_worker", json!({"worker": w})),
    ] {
        let r = call(&s.server, &other, tool, args).await;
        assert!(err_text(&r).contains("no worker"), "{tool}: {r}");
    }
    let r = call(&s.server, &other, "list_workers", json!({})).await;
    assert_eq!(r["structuredContent"]["workers"], json!([]));

    // A worker sees no worker tools at all, so it can't read its siblings' output.
    let worker = worker_token(&s, w);
    let names = tool_names(&s.server, &worker).await;
    assert!(!names.iter().any(|n| n.contains("worker")), "{names:?}");
    let r = call(&s.server, &worker, "worker_status", json!({"worker": w})).await;
    assert!(err_text(&r).contains("unknown tool"), "{r}");

    // The Queen still can.
    let r = call(&s.server, &queen, "worker_status", json!({"worker": w})).await;
    assert_eq!(r["structuredContent"]["status"], "running");
    call(&s.server, &queen, "cancel_worker", json!({"worker": w})).await;
}
