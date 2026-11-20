//! The ACP client side of one session: handshake, prompts, cancel, permission requests and
//! session updates, on top of the `agent-client-protocol` crate (protocol v1).
//!
//! One Tokio task owns the connection (and, through [`Shutdown`], the agent process). The
//! [`AcpSession`] handle talks to it over a command channel, so `prompt` and `cancel` can be
//! called concurrently. Dropping the handle aborts the task, which kills the agent.

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use agent_client_protocol::{
    schema::ProtocolVersion,
    v1::{
        schema::{
            CancelNotification, ClientCapabilities, ContentBlock, HttpHeader, ImageContent,
            Implementation, InitializeRequest, McpServer, McpServerHttp, NewSessionRequest,
            PermissionOption, PermissionOptionKind, PromptRequest, RequestPermissionOutcome,
            RequestPermissionRequest, RequestPermissionResponse, ResumeSessionRequest,
            SelectedPermissionOutcome, SessionConfigKind, SessionConfigOption,
            SessionConfigOptionCategory, SessionConfigOptionValue, SessionConfigSelectOptions,
            SessionNotification, SessionUpdate, SetSessionConfigOptionRequest, TextContent,
        },
        ConnectionTo, Responder,
    },
    Agent, Client, ConnectTo, UntypedMessage,
};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};

use crate::{
    session::{
        AgentSession, EffortOption, ModelList, ModelOption, OpenSession, OpenedSession,
        PermissionDecision, PermissionHandler, PermissionRequest, PlanStep, PromptBlock,
        SessionCapabilities, SessionEvent, StopReason, ToolKind, ToolStatus,
    },
    AgentError, BoxFuture,
};

/// How long the handshake (initialize + session/new) may take.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(90);
/// How long [`AgentSession::close`] waits for the agent to exit before killing it.
const CLOSE_GRACE: Duration = Duration::from_secs(3);

/// Cleanup that runs after the connection ends (e.g. stop the agent process).
pub(crate) type Shutdown = Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>;

/// Recent stderr lines of the agent, for error messages.
#[derive(Clone, Default)]
pub(crate) struct StderrTail(Arc<Mutex<VecDeque<String>>>);

impl StderrTail {
    const KEEP: usize = 8;

    pub(crate) fn push(&self, line: String) {
        let mut q = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if q.len() == Self::KEEP {
            q.pop_front();
        }
        q.push_back(line);
    }

    pub(crate) fn summary(&self) -> String {
        let q = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if q.is_empty() {
            String::new()
        } else {
            format!(
                " (agent stderr: {})",
                q.iter().cloned().collect::<Vec<_>>().join(" | ")
            )
        }
    }
}

enum Command {
    Prompt {
        text: String,
        blocks: Vec<PromptBlock>,
        reply: oneshot::Sender<Result<StopReason, AgentError>>,
    },
    Cancel {
        reply: oneshot::Sender<()>,
    },
    Close,
}

/// Title and kind of each tool call seen in this session (updates may omit them).
type ToolTable = Arc<Mutex<HashMap<String, (String, ToolKind)>>>;

/// Start a session over an established transport. `shutdown` runs after the connection
/// ends, whether it ended normally or not; it does not run if the task is aborted (drop),
/// which is why the agent process must also be killed on drop.
pub(crate) async fn start<T>(
    transport: T,
    open: OpenSession,
    shutdown: Option<Shutdown>,
    stderr: StderrTail,
) -> Result<OpenedSession, AgentError>
where
    T: ConnectTo<Client> + Send + 'static,
{
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (ready_tx, ready_rx) = oneshot::channel();
    let (done_tx, done_rx) = watch::channel(false);

    let task = tokio::spawn({
        let stderr = stderr.clone();
        async move {
            let result = run(transport, open, events_tx, cmd_rx, ready_tx).await;
            if let Err(e) = result {
                tracing::debug!("ACP connection ended: {e}{}", stderr.summary());
            }
            if let Some(shutdown) = shutdown {
                shutdown().await;
            }
            let _ = done_tx.send(true);
        }
    });
    let abort = AbortOnDrop(task);

    let ready = tokio::time::timeout(OPEN_TIMEOUT, ready_rx).await;
    let Ready {
        id,
        caps,
        resumed,
        models,
    } = match ready {
        Ok(Ok(Ok(ready))) => ready,
        Ok(Ok(Err(e))) => return Err(AgentError::Protocol(format!("{e}{}", stderr.summary()))),
        Ok(Err(_)) => {
            return Err(AgentError::Protocol(format!(
                "agent ended the connection during setup{}",
                stderr.summary()
            )))
        }
        Err(_) => {
            return Err(AgentError::Protocol(format!(
                "agent did not finish setup within {OPEN_TIMEOUT:?}{}",
                stderr.summary()
            )))
        }
    };
    Ok(OpenedSession {
        session: Box::new(AcpSession {
            id,
            caps,
            cmd_tx,
            done: done_rx,
            stderr,
            closing: AtomicBool::new(false),
            _task: abort,
        }),
        events: events_rx,
        resumed,
        models,
    })
}

/// The connection: handlers for agent → client messages, then the command loop.
async fn run<T>(
    transport: T,
    open: OpenSession,
    events: mpsc::UnboundedSender<SessionEvent>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    ready: oneshot::Sender<Result<Ready, String>>,
) -> Result<(), agent_client_protocol::Error>
where
    T: ConnectTo<Client> + Send + 'static,
{
    let tools: ToolTable = Arc::default();
    // Bumped on every cancel; pending permission requests watch it.
    let (cancel_tx, _) = watch::channel(0u64);
    let cancel_tx = Arc::new(cancel_tx);

    let on_notification = {
        let events = events.clone();
        let tools = tools.clone();
        async move |msg: UntypedMessage, _cx: ConnectionTo<Agent>| {
            handle_notification(&msg, &events, &tools);
            Ok(())
        }
    };
    let on_permission = {
        let events = events.clone();
        let tools = tools.clone();
        let handler = open.permissions.clone();
        let cancel_tx = cancel_tx.clone();
        async move |req: RequestPermissionRequest,
                    responder: Responder<RequestPermissionResponse>,
                    cx: ConnectionTo<Agent>| {
            let preq = permission_request(&req, &tools);
            let task = answer_permission(
                preq,
                req.options,
                handler.clone(),
                cancel_tx.subscribe(),
                events.clone(),
                responder,
            );
            // Off the dispatch loop: a decision may wait minutes for a human.
            cx.spawn(task)
        }
    };

    Client
        .v1()
        .on_receive_request(on_permission, agent_client_protocol::on_receive_request!())
        .on_receive_notification(
            on_notification,
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_with(transport, async move |cx: ConnectionTo<Agent>| {
            let ok = match handshake(&cx, &open).await {
                Ok(ok) => ok,
                Err(e) => {
                    let _ = ready.send(Err(e.to_string()));
                    return Err(e);
                }
            };
            let session_id = ok.id.clone();
            let _ = ready.send(Ok(ok));
            let busy = Arc::new(AtomicBool::new(false));
            loop {
                let cmd = tokio::select! {
                    cmd = commands.recv() => cmd,
                    // The agent closed its stdout (it exited): end the connection now, so the
                    // event stream ends and the owner learns it even between turns.
                    () = cx.incoming_closed() => None,
                };
                let Some(cmd) = cmd else { break };
                match cmd {
                    Command::Prompt {
                        text,
                        blocks,
                        reply,
                    } => {
                        if busy.swap(true, Ordering::SeqCst) {
                            let _ = reply.send(Err(AgentError::Busy));
                            continue;
                        }
                        let request =
                            PromptRequest::new(session_id.clone(), prompt_content(text, blocks));
                        let cx2 = cx.clone();
                        let busy = busy.clone();
                        let events = events.clone();
                        cx.spawn(async move {
                            let result = cx2.send_request(request).block_task().await;
                            busy.store(false, Ordering::SeqCst);
                            let result = result.map(|r| stop_reason(r.stop_reason)).map_err(|e| {
                                if agent_client_protocol::is_incoming_transport_closed(&e)
                                    || agent_client_protocol::is_connection_terminated(&e)
                                {
                                    AgentError::AgentExited(String::new())
                                } else {
                                    AgentError::Protocol(e.to_string())
                                }
                            });
                            if let Ok(stop) = &result {
                                let _ = events.send(SessionEvent::TurnFinished(*stop));
                            }
                            let _ = reply.send(result);
                            Ok(())
                        })?;
                    }
                    Command::Cancel { reply } => {
                        if busy.load(Ordering::SeqCst) {
                            // Spec order: notify the agent, then answer pending permission
                            // requests with `cancelled`.
                            cx.send_notification(CancelNotification::new(session_id.clone()))?;
                            cancel_tx.send_modify(|g| *g += 1);
                        }
                        let _ = reply.send(());
                    }
                    Command::Close => break,
                }
            }
            Ok(())
        })
        .await
}

/// What the handshake found out.
struct Ready {
    id: String,
    caps: SessionCapabilities,
    resumed: bool,
    models: Option<ModelList>,
}

/// `initialize` (v1, no client fs/terminal capabilities), then `session/resume` if asked
/// for and supported, else (or if resuming fails) `session/new`; then the model, if one was
/// asked for and the agent offers it (`session/set_config_option`).
async fn handshake(
    cx: &ConnectionTo<Agent>,
    open: &OpenSession,
) -> Result<Ready, agent_client_protocol::Error> {
    let init = InitializeRequest::new(ProtocolVersion::V1)
        .client_capabilities(ClientCapabilities::new())
        .client_info(Implementation::new("melissa", env!("CARGO_PKG_VERSION")));
    let init = cx.send_request(init).block_task().await?;
    if init.protocol_version != ProtocolVersion::V1 {
        return Err(
            agent_client_protocol::Error::invalid_request().data(format!(
                "agent speaks ACP protocol {:?}; Melissa supports 1",
                init.protocol_version
            )),
        );
    }
    let agent_caps = &init.agent_capabilities;
    let caps = SessionCapabilities {
        protocol_version: 1,
        agent: init
            .agent_info
            .as_ref()
            .map(|i| format!("{} {}", i.name, i.version)),
        load: agent_caps.load_session,
        resume: agent_caps.session_capabilities.resume.is_some(),
        fork: false,
        close: agent_caps.session_capabilities.close.is_some(),
        mcp_http: agent_caps.mcp_capabilities.http,
        images: agent_caps.prompt_capabilities.image,
        // PDFs would be embedded resources, which ACP agents handle unevenly: they go to the
        // agent's working directory instead.
        pdfs: false,
    };
    if !open.mcp_servers.is_empty() && !caps.mcp_http {
        return Err(agent_client_protocol::Error::invalid_request()
            .data("agent does not accept MCP servers over HTTP; Melissa's tools need it"));
    }
    let servers: Vec<McpServer> = open
        .mcp_servers
        .iter()
        .map(|s| {
            McpServer::Http(
                McpServerHttp::new(s.name.clone(), s.url.clone()).headers(
                    s.headers
                        .iter()
                        .map(|(k, v)| HttpHeader::new(k.clone(), v.clone()))
                        .collect(),
                ),
            )
        })
        .collect();
    if let (Some(id), true) = (&open.resume, caps.resume) {
        let resume =
            ResumeSessionRequest::new(id.clone(), open.cwd.clone()).mcp_servers(servers.clone());
        match cx.send_request(resume).block_task().await {
            Ok(r) => {
                let models = choose(cx, id, r.config_options, open).await;
                return Ok(Ready {
                    id: id.clone(),
                    caps,
                    resumed: true,
                    models,
                });
            }
            Err(e) => tracing::info!("cannot resume agent session {id}: {e}; starting a new one"),
        }
    }
    let new = NewSessionRequest::new(open.cwd.clone()).mcp_servers(servers);
    let new = cx.send_request(new).block_task().await?;
    let id = new.session_id.to_string();
    let models = choose(cx, &id, new.config_options, open).await;
    Ok(Ready {
        id,
        caps,
        resumed: false,
        models,
    })
}

/// The agent's model option (a select in category `model`), its id and choices.
fn model_option(options: &[SessionConfigOption]) -> Option<(String, ModelList)> {
    options.iter().find_map(|o| {
        if o.category != Some(SessionConfigOptionCategory::Model) {
            return None;
        }
        let SessionConfigKind::Select(select) = &o.kind else {
            return None;
        };
        let flat: Vec<ModelOption> = match &select.options {
            SessionConfigSelectOptions::Ungrouped(list) => list
                .iter()
                .map(|c| ModelOption::new(c.value.to_string(), c.name.clone()))
                .collect(),
            SessionConfigSelectOptions::Grouped(groups) => groups
                .iter()
                .flat_map(|g| g.options.iter())
                .map(|c| ModelOption::new(c.value.to_string(), c.name.clone()))
                .collect(),
            _ => Vec::new(),
        };
        Some((
            o.id.to_string(),
            ModelList {
                current: Some(select.current_value.to_string()),
                options: flat,
            },
        ))
    })
}

/// The agent's reasoning level option (a select in category `thought_level`): its id, the
/// levels and the current one.
fn effort_option(options: &[SessionConfigOption]) -> Option<(String, Vec<EffortOption>, String)> {
    options.iter().find_map(|o| {
        if o.category != Some(SessionConfigOptionCategory::ThoughtLevel) {
            return None;
        }
        let SessionConfigKind::Select(select) = &o.kind else {
            return None;
        };
        let levels: Vec<EffortOption> = match &select.options {
            SessionConfigSelectOptions::Ungrouped(list) => list
                .iter()
                .map(|c| EffortOption {
                    id: c.value.to_string(),
                    label: c.name.clone(),
                })
                .collect(),
            SessionConfigSelectOptions::Grouped(groups) => groups
                .iter()
                .flat_map(|g| g.options.iter())
                .map(|c| EffortOption {
                    id: c.value.to_string(),
                    label: c.name.clone(),
                })
                .collect(),
            _ => Vec::new(),
        };
        Some((o.id.to_string(), levels, select.current_value.to_string()))
    })
}

/// An agent's speed option (ACP category `speed`, still unstable in the protocol, so it
/// arrives as a custom category): a boolean toggle (on: fast), or a select with exactly two
/// values, one of them "fast". Anything else is not a speed Melissa can offer and is ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SpeedOption {
    Toggle {
        id: String,
        on: bool,
    },
    Select {
        id: String,
        standard: String,
        fast: String,
        on: bool,
    },
}

impl SpeedOption {
    fn on(&self) -> bool {
        match self {
            Self::Toggle { on, .. } | Self::Select { on, .. } => *on,
        }
    }

    fn request(&self, session: &str, fast: bool) -> SetSessionConfigOptionRequest {
        let (id, value) = match self {
            Self::Toggle { id, .. } => (id, SessionConfigOptionValue::boolean(fast)),
            Self::Select {
                id,
                standard,
                fast: f,
                ..
            } => (
                id,
                SessionConfigOptionValue::value_id(if fast { f.clone() } else { standard.clone() }),
            ),
        };
        SetSessionConfigOptionRequest::new(session.to_string(), id.clone(), value)
    }
}

/// The agent's speed option, if it reports one Melissa understands ([`SpeedOption`]).
pub(crate) fn speed_option(options: &[SessionConfigOption]) -> Option<SpeedOption> {
    options.iter().find_map(|o| {
        if !matches!(&o.category, Some(SessionConfigOptionCategory::Other(c)) if c == "speed") {
            return None;
        }
        match &o.kind {
            SessionConfigKind::Boolean(b) => Some(SpeedOption::Toggle {
                id: o.id.to_string(),
                on: b.current_value,
            }),
            SessionConfigKind::Select(select) => {
                let values: Vec<(String, String)> = match &select.options {
                    SessionConfigSelectOptions::Ungrouped(list) => list
                        .iter()
                        .map(|c| (c.value.to_string(), c.name.clone()))
                        .collect(),
                    SessionConfigSelectOptions::Grouped(groups) => groups
                        .iter()
                        .flat_map(|g| g.options.iter())
                        .map(|c| (c.value.to_string(), c.name.clone()))
                        .collect(),
                    _ => Vec::new(),
                };
                let [a, b] = values.as_slice() else {
                    return None;
                };
                let is_fast = |(v, n): &(String, String)| {
                    v.eq_ignore_ascii_case("fast") || n.eq_ignore_ascii_case("fast")
                };
                let (fast, standard) = match (is_fast(a), is_fast(b)) {
                    (true, false) => (a, b),
                    (false, true) => (b, a),
                    _ => return None,
                };
                Some(SpeedOption::Select {
                    id: o.id.to_string(),
                    on: select.current_value.to_string() == fast.0,
                    standard: standard.0.clone(),
                    fast: fast.0.clone(),
                })
            }
            _ => None,
        }
    })
}

/// The model (see [`choose_model`]), then the reasoning level and the speed: switch to the
/// ones asked for if the agent offers them. Levels and speed are reported with the model
/// the session uses (an agent offers them for its current model only).
async fn choose(
    cx: &ConnectionTo<Agent>,
    session: &str,
    options: Option<Vec<SessionConfigOption>>,
    open: &OpenSession,
) -> Option<ModelList> {
    let effort = options.as_deref().and_then(effort_option);
    let speed = options.as_deref().and_then(speed_option);
    let mut models = choose_model(cx, session, options, open.model.as_deref()).await?;
    let mut levels = Vec::new();
    if let Some((config_id, offered, current)) = effort {
        levels = offered;
        if let Some(wanted) = open.effort.as_deref() {
            if wanted != current && levels.iter().any(|l| l.id == wanted) {
                let req = SetSessionConfigOptionRequest::new(
                    session.to_string(),
                    config_id,
                    SessionConfigOptionValue::value_id(wanted.to_string()),
                );
                match cx.send_request(req).block_task().await {
                    Ok(r) => {
                        if let Some((_, after, _)) = effort_option(&r.config_options) {
                            levels = after;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("cannot set the agent's reasoning level `{wanted}`: {e}")
                    }
                }
            } else if wanted != current {
                tracing::warn!(
                    "the agent does not offer reasoning level `{wanted}`; keeping its own"
                );
            }
        }
    }
    let fast = speed.is_some();
    if let Some(speed) = speed {
        if speed.on() != open.fast {
            if let Err(e) = cx
                .send_request(speed.request(session, open.fast))
                .block_task()
                .await
            {
                tracing::warn!("cannot set the agent's speed (fast: {}): {e}", open.fast);
            }
        }
    }
    if let Some(current) = models.current.clone() {
        if let Some(m) = models.options.iter_mut().find(|m| m.id == current) {
            if !levels.is_empty() {
                m.efforts = levels;
            }
            m.fast = fast;
        }
    }
    Some(models)
}

/// Report the agent's models; switch to `wanted` if it is one of them and not the current
/// one. A model the agent does not offer (or a failed switch) keeps the agent's choice: the
/// session still works, and the returned list says which model it really uses.
async fn choose_model(
    cx: &ConnectionTo<Agent>,
    session: &str,
    options: Option<Vec<SessionConfigOption>>,
    wanted: Option<&str>,
) -> Option<ModelList> {
    let (config_id, mut models) = model_option(options.as_deref().unwrap_or_default())?;
    let Some(wanted) = wanted else {
        return Some(models);
    };
    if models.current.as_deref() == Some(wanted) {
        return Some(models);
    }
    if !models.contains(wanted) {
        tracing::warn!("the agent does not offer model `{wanted}`; keeping its default");
        return Some(models);
    }
    let req = SetSessionConfigOptionRequest::new(
        session.to_string(),
        config_id,
        SessionConfigOptionValue::value_id(wanted.to_string()),
    );
    match cx.send_request(req).block_task().await {
        Ok(r) => match model_option(&r.config_options) {
            Some((_, after)) => models = after,
            None => models.current = Some(wanted.to_string()),
        },
        Err(e) => tracing::warn!("cannot switch the agent to model `{wanted}`: {e}"),
    }
    Some(models)
}

/// Decide one permission request and answer it; a cancel answers `cancelled` instead.
async fn answer_permission(
    req: PermissionRequest,
    options: Vec<PermissionOption>,
    handler: Arc<dyn PermissionHandler>,
    mut cancelled: watch::Receiver<u64>,
    events: mpsc::UnboundedSender<SessionEvent>,
    responder: Responder<RequestPermissionResponse>,
) -> Result<(), agent_client_protocol::Error> {
    let decision = tokio::select! {
        d = handler.decide(&req) => d,
        _ = cancelled.changed() => PermissionDecision::Cancelled,
    };
    let (outcome, decision) = pick_option(&options, decision);
    let _ = events.send(SessionEvent::Permission {
        tool_call_id: req.tool_call_id,
        title: req.title,
        decision,
    });
    responder.respond(RequestPermissionResponse::new(outcome))
}

/// The option matching `decision`. Only `*_once` options are ever chosen; if the agent did
/// not offer the matching one, the answer is `cancelled`.
fn pick_option(
    options: &[PermissionOption],
    decision: PermissionDecision,
) -> (RequestPermissionOutcome, PermissionDecision) {
    let kind = match decision {
        PermissionDecision::AllowOnce => PermissionOptionKind::AllowOnce,
        PermissionDecision::RejectOnce => PermissionOptionKind::RejectOnce,
        PermissionDecision::Cancelled => {
            return (
                RequestPermissionOutcome::Cancelled,
                PermissionDecision::Cancelled,
            )
        }
    };
    match options.iter().find(|o| o.kind == kind) {
        Some(o) => (
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(o.option_id.clone())),
            decision,
        ),
        None => (
            RequestPermissionOutcome::Cancelled,
            PermissionDecision::Cancelled,
        ),
    }
}

fn permission_request(req: &RequestPermissionRequest, tools: &ToolTable) -> PermissionRequest {
    let id = req.tool_call.tool_call_id.to_string();
    let f = &req.tool_call.fields;
    let known = lock(tools).get(&id).cloned();
    let title = f
        .title
        .clone()
        .or_else(|| known.as_ref().map(|k| k.0.clone()))
        .unwrap_or_else(|| "tool call".into());
    // Some agents omit the kind on the request; use the one from the earlier tool_call.
    let kind = f
        .kind
        .map(tool_kind)
        .or_else(|| known.map(|k| k.1))
        .unwrap_or(ToolKind::Other);
    PermissionRequest {
        tool_call_id: id,
        title,
        kind,
        tool_name: f.name.clone(),
        raw_input: f.raw_input.clone().unwrap_or(Value::Null),
    }
}

/// Map `session/update` notifications to events; other notifications (vendor extensions
/// such as Grok's `_x.ai/*`) are ignored.
fn handle_notification(
    msg: &UntypedMessage,
    events: &mpsc::UnboundedSender<SessionEvent>,
    tools: &ToolTable,
) {
    if msg.method != "session/update" {
        tracing::trace!(method = %msg.method, "ignoring ACP notification");
        return;
    }
    let note: SessionNotification = match serde_json::from_value(msg.params.clone()) {
        Ok(n) => n,
        Err(_) => {
            let kind = msg.params["update"]["sessionUpdate"]
                .as_str()
                .unwrap_or("unknown")
                .to_string();
            let _ = events.send(SessionEvent::Other(kind));
            return;
        }
    };
    for event in session_events(note.update, tools) {
        let _ = events.send(event);
    }
}

fn session_events(update: SessionUpdate, tools: &ToolTable) -> Vec<SessionEvent> {
    match update {
        SessionUpdate::AgentMessageChunk(chunk) => text(chunk.content)
            .map(SessionEvent::Text)
            .into_iter()
            .collect(),
        SessionUpdate::AgentThoughtChunk(chunk) => text(chunk.content)
            .map(SessionEvent::Thought)
            .into_iter()
            .collect(),
        SessionUpdate::ToolCall(call) => {
            let id = call.tool_call_id.to_string();
            let kind = tool_kind(call.kind);
            let status = tool_status(call.status);
            lock(tools).insert(id.clone(), (call.title.clone(), kind));
            let mut out = vec![SessionEvent::ToolStarted {
                id: id.clone(),
                title: call.title.clone(),
                kind,
                status,
            }];
            if status.is_finished() {
                out.push(SessionEvent::ToolFinished {
                    id,
                    title: Some(call.title),
                    status,
                });
            }
            out
        }
        SessionUpdate::ToolCallUpdate(update) => {
            let id = update.tool_call_id.to_string();
            let f = update.fields;
            let title = {
                let mut table = lock(tools);
                let entry = table
                    .entry(id.clone())
                    .or_insert_with(|| (String::new(), ToolKind::Other));
                if let Some(t) = &f.title {
                    entry.0 = t.clone();
                }
                if let Some(k) = f.kind {
                    entry.1 = tool_kind(k);
                }
                (!entry.0.is_empty()).then(|| entry.0.clone())
            };
            let status = f.status.map(tool_status);
            match status {
                Some(s) if s.is_finished() => vec![SessionEvent::ToolFinished {
                    id,
                    title,
                    status: s,
                }],
                _ => vec![SessionEvent::ToolUpdated {
                    id,
                    title: f.title,
                    status,
                }],
            }
        }
        SessionUpdate::Plan(plan) => vec![SessionEvent::Plan(
            plan.entries
                .into_iter()
                .map(|e| PlanStep {
                    content: e.content,
                    status: plan_status(&e.status),
                })
                .collect(),
        )],
        SessionUpdate::UsageUpdate(u) => vec![SessionEvent::Usage {
            used: u.used,
            size: u.size,
        }],
        other => {
            let kind = serde_json::to_value(&other)
                .ok()
                .and_then(|v| v["sessionUpdate"].as_str().map(str::to_string))
                .unwrap_or_else(|| "unknown".into());
            vec![SessionEvent::Other(kind)]
        }
    }
}

/// A prompt's content: images first (only sent when the agent advertised `image`), then the
/// text. PDFs never reach here (see the capabilities).
pub(crate) fn prompt_content(text: String, blocks: Vec<PromptBlock>) -> Vec<ContentBlock> {
    let mut content: Vec<ContentBlock> = blocks
        .into_iter()
        .filter_map(|b| match b {
            PromptBlock::Image { mime, data } => {
                Some(ContentBlock::Image(ImageContent::new(data, mime)))
            }
            PromptBlock::Pdf { .. } => None,
        })
        .collect();
    content.push(ContentBlock::Text(TextContent::new(text)));
    content
}

fn text(block: ContentBlock) -> Option<String> {
    match block {
        ContentBlock::Text(t) => Some(t.text),
        _ => None,
    }
}

fn tool_kind(kind: agent_client_protocol::v1::schema::ToolKind) -> ToolKind {
    use agent_client_protocol::v1::schema::ToolKind as K;
    match kind {
        K::Read => ToolKind::Read,
        K::Edit => ToolKind::Edit,
        K::Delete => ToolKind::Delete,
        K::Move => ToolKind::Move,
        K::Search => ToolKind::Search,
        K::Execute => ToolKind::Execute,
        K::Think => ToolKind::Think,
        K::Fetch => ToolKind::Fetch,
        K::SwitchMode => ToolKind::SwitchMode,
        _ => ToolKind::Other,
    }
}

fn tool_status(status: agent_client_protocol::v1::schema::ToolCallStatus) -> ToolStatus {
    use agent_client_protocol::v1::schema::ToolCallStatus as S;
    match status {
        S::Pending => ToolStatus::Pending,
        S::InProgress => ToolStatus::InProgress,
        S::Completed => ToolStatus::Completed,
        _ => ToolStatus::Failed,
    }
}

fn plan_status(status: &agent_client_protocol::v1::schema::PlanEntryStatus) -> &'static str {
    use agent_client_protocol::v1::schema::PlanEntryStatus as S;
    match status {
        S::Pending => "pending",
        S::InProgress => "in_progress",
        _ => "completed",
    }
}

fn stop_reason(reason: agent_client_protocol::v1::schema::StopReason) -> StopReason {
    use agent_client_protocol::v1::schema::StopReason as S;
    match reason {
        S::EndTurn => StopReason::EndTurn,
        S::MaxTokens => StopReason::MaxTokens,
        S::MaxTurnRequests => StopReason::MaxTurnRequests,
        S::Refusal => StopReason::Refusal,
        _ => StopReason::Cancelled,
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Handle to an open ACP session.
struct AcpSession {
    id: String,
    caps: SessionCapabilities,
    cmd_tx: mpsc::UnboundedSender<Command>,
    done: watch::Receiver<bool>,
    stderr: StderrTail,
    /// `close` was called: later failures are "closed", not "the agent died".
    closing: AtomicBool,
    _task: AbortOnDrop,
}

impl AcpSession {
    fn closed(&self) -> AgentError {
        if self.closing.load(Ordering::SeqCst) {
            AgentError::SessionClosed
        } else {
            AgentError::AgentExited(self.stderr.summary())
        }
    }
}

impl AgentSession for AcpSession {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &SessionCapabilities {
        &self.caps
    }

    fn prompt<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<StopReason, AgentError>> {
        self.prompt_with(text, &[])
    }

    fn prompt_with<'a>(
        &'a self,
        text: &'a str,
        blocks: &'a [PromptBlock],
    ) -> BoxFuture<'a, Result<StopReason, AgentError>> {
        Box::pin(async move {
            let (reply, rx) = oneshot::channel();
            self.cmd_tx
                .send(Command::Prompt {
                    text: text.to_string(),
                    blocks: blocks.to_vec(),
                    reply,
                })
                .map_err(|_| self.closed())?;
            match rx.await {
                // The agent died mid-turn: say so, with its last words, unless we closed it.
                Ok(Err(AgentError::AgentExited(_))) | Err(_) => Err(self.closed()),
                Ok(result) => result,
            }
        })
    }

    fn cancel(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async move {
            let (reply, rx) = oneshot::channel();
            self.cmd_tx
                .send(Command::Cancel { reply })
                .map_err(|_| self.closed())?;
            rx.await.map_err(|_| self.closed())
        })
    }

    fn close(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async move {
            // Ends the command loop; the connection closes the agent's stdin and the
            // shutdown hook stops the process.
            self.closing.store(true, Ordering::SeqCst);
            let _ = self.cmd_tx.send(Command::Close);
            let mut done = self.done.clone();
            let _ = tokio::time::timeout(CLOSE_GRACE * 2, done.wait_for(|d| *d)).await;
            Ok(())
        })
    }
}

/// Stop the agent process: wait briefly for it to exit on its own (stdin closed), then kill.
pub(crate) fn process_shutdown(mut child: tokio::process::Child) -> Shutdown {
    Box::new(move || {
        Box::pin(async move {
            if tokio::time::timeout(CLOSE_GRACE, child.wait())
                .await
                .is_err()
            {
                let _ = child.kill().await;
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(id: &str, kind: PermissionOptionKind) -> PermissionOption {
        PermissionOption::new(id.to_string(), id.to_string(), kind)
    }

    #[test]
    fn never_picks_always_options() {
        let all = vec![
            option("always", PermissionOptionKind::AllowAlways),
            option("once", PermissionOptionKind::AllowOnce),
            option("no", PermissionOptionKind::RejectOnce),
            option("never", PermissionOptionKind::RejectAlways),
        ];
        let id = |o: RequestPermissionOutcome| match o {
            RequestPermissionOutcome::Selected(s) => s.option_id.to_string(),
            _ => "cancelled".into(),
        };
        assert_eq!(
            id(pick_option(&all, PermissionDecision::AllowOnce).0),
            "once"
        );
        assert_eq!(
            id(pick_option(&all, PermissionDecision::RejectOnce).0),
            "no"
        );
        assert_eq!(
            id(pick_option(&all, PermissionDecision::Cancelled).0),
            "cancelled"
        );
        let only_always = vec![
            option("always", PermissionOptionKind::AllowAlways),
            option("never", PermissionOptionKind::RejectAlways),
        ];
        let (outcome, decision) = pick_option(&only_always, PermissionDecision::AllowOnce);
        assert_eq!(id(outcome), "cancelled");
        assert_eq!(decision, PermissionDecision::Cancelled);
    }
}
