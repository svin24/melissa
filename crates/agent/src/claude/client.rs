//! The client side of one Claude Code stream-json session.
//!
//! Protocol (Claude Code 2.1.327, verified on the dev laptop 2026-11-18): newline-delimited
//! JSON objects with a `type`.
//! - Melissa → Claude: `{"type":"user","message":{"role":"user","content":"…"}, …}` starts
//!   a turn; `{"type":"control_request","request_id":…,"request":{"subtype":"initialize"}}`
//!   (handshake) and `{"subtype":"interrupt"}` (cancel the turn); `control_response`s to
//!   Claude's requests.
//! - Claude → Melissa: `stream_event` (Anthropic streaming events: text and thinking
//!   deltas, `tool_use` blocks), `assistant`/`user` (whole messages; `user` carries tool
//!   results), `system` (`init` with the session id, status), `control_request`
//!   `can_use_tool` (a permission prompt, answered `{"behavior":"allow","updatedInput":…}`
//!   or `{"behavior":"deny","message":…}`), `control_cancel_request` (a pending prompt was
//!   withdrawn, e.g. after an interrupt), and one `result` per turn.
//!
//! One Tokio task (the actor) owns the agent's stdout and the session state; a writer task
//! owns stdin. The [`ClaudeSession`] handle talks to the actor over a command channel, so
//! `prompt` and `cancel` can be called concurrently. Dropping the handle aborts the actor,
//! which kills the agent and withdraws pending permission decisions.

use std::{collections::HashMap, sync::Arc, time::Duration};

use serde_json::{json, Value};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    sync::{mpsc, oneshot, watch},
};

use crate::{
    acp::{
        client::{Shutdown, StderrTail},
        replay::{Direction, Recorder},
        transport::read_line,
        OPEN_TIMEOUT,
    },
    session::{
        AgentSession, EffortOption, ModelList, ModelOption, OpenedSession, PermissionDecision,
        PermissionHandler, PermissionRequest, PromptBlock, SessionCapabilities, SessionEvent,
        StopReason, ToolKind, ToolStatus,
    },
    AgentError, BoxFuture,
};

/// How long [`AgentSession::close`] waits for the agent to exit.
const CLOSE_WAIT: Duration = Duration::from_secs(6);

/// Request id of the handshake.
pub(crate) const INIT_ID: &str = "melissa-init";

/// Everything [`start`] needs besides the pipes.
pub(crate) struct Start {
    /// The session id passed with `--session-id` or `--resume`.
    pub session_id: String,
    /// The session was reopened with `--resume`.
    pub resumed: bool,
    /// The session is stored on disk (it can be resumed later).
    pub persistent: bool,
    pub permissions: Arc<dyn PermissionHandler>,
    pub recorder: Option<Arc<Recorder>>,
    pub stderr: StderrTail,
    /// Runs after the agent's stdout closed (stop the process, close the egress sockets).
    pub shutdown: Option<Shutdown>,
    /// The `--model` the process was started with, if any.
    pub model: Option<String>,
}

/// What the handshake answer says: Claude Code's version and the models it offers.
type InitInfo = (Option<String>, Vec<ModelOption>);

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

/// Why a pending permission decision stopped early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Withdrawn {
    No,
    /// Melissa cancelled the turn: answer `deny` + interrupt.
    ByUs,
    /// Claude withdrew the request (`control_cancel_request`): no answer expected.
    ByAgent,
}

/// Start the session over the agent's stdout (`reader`) and stdin (`writer`): handshake,
/// then hand out the session handle and its events.
pub(crate) async fn start<R, W>(reader: R, writer: W, s: Start) -> Result<OpenedSession, AgentError>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let (out_tx, out_rx) = mpsc::unbounded_channel::<String>();
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (init_tx, init_rx) = oneshot::channel();
    let (done_tx, done_rx) = watch::channel(false);
    let persistent = s.persistent;

    let writer = tokio::spawn(write_lines(writer, out_rx, s.recorder.clone()));
    let mut actor = Actor {
        out: out_tx,
        events: events_tx,
        permissions: s.permissions,
        turn: None,
        interrupted: false,
        pending: HashMap::new(),
        tools: HashMap::new(),
        init: Some(init_tx),
        next_id: 0,
        session_id: s.session_id.clone(),
    };
    let stderr = s.stderr.clone();
    let recorder = s.recorder;
    let shutdown = s.shutdown;
    let task = tokio::spawn(async move {
        actor.send(json!({"type": "control_request", "request_id": INIT_ID,
                          "request": {"subtype": "initialize"}}));
        actor.run(reader, cmd_rx, recorder.as_deref()).await;
        actor.finish(&stderr);
        // Closing stdin (the writer ends when its channel closes) lets the agent exit.
        drop(actor);
        let _ = writer.await;
        if let Some(shutdown) = shutdown {
            shutdown().await;
        }
        let _ = done_tx.send(true);
    });
    let abort = AbortOnDrop(task);

    let (version, offered) = match tokio::time::timeout(OPEN_TIMEOUT, init_rx).await {
        Ok(Ok(Ok(v))) => v,
        Ok(Ok(Err(e))) => return Err(AgentError::Protocol(format!("{e}{}", s.stderr.summary()))),
        Ok(Err(_)) => {
            return Err(AgentError::Protocol(format!(
                "agent ended before the handshake{}",
                s.stderr.summary()
            )))
        }
        Err(_) => {
            return Err(AgentError::Protocol(format!(
                "agent did not answer the handshake within {OPEN_TIMEOUT:?}{}",
                s.stderr.summary()
            )))
        }
    };
    let caps = SessionCapabilities {
        protocol_version: 0,
        agent: Some(match version {
            Some(v) => format!("claude-code {v}"),
            None => "claude-code".into(),
        }),
        load: false,
        resume: persistent,
        fork: false,
        close: true,
        mcp_http: true,
        images: true,
        pdfs: true,
    };
    Ok(OpenedSession {
        session: Box::new(ClaudeSession {
            id: s.session_id,
            caps,
            cmd_tx,
            done: done_rx,
            stderr: s.stderr,
            closing: std::sync::atomic::AtomicBool::new(false),
            _task: abort,
        }),
        events: events_rx,
        resumed: s.resumed,
        models: (!offered.is_empty()).then(|| {
            // No `--model`: Claude uses its default, which it lists as `default`.
            let current = s.model.or_else(|| {
                offered
                    .iter()
                    .any(|o| o.id == "default")
                    .then(|| "default".into())
            });
            ModelList {
                current,
                options: offered,
            }
        }),
    })
}

async fn write_lines<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut lines: mpsc::UnboundedReceiver<String>,
    recorder: Option<Arc<Recorder>>,
) {
    while let Some(line) = lines.recv().await {
        if let Some(r) = &recorder {
            r.record(Direction::Send, &line);
        }
        let ok = writer.write_all(line.as_bytes()).await.is_ok()
            && writer.write_all(b"\n").await.is_ok()
            && writer.flush().await.is_ok();
        if !ok {
            break;
        }
    }
    let _ = writer.shutdown().await;
}

/// A pending permission decision.
struct Pending {
    withdraw: watch::Sender<Withdrawn>,
    _task: AbortOnDrop,
}

struct Actor {
    out: mpsc::UnboundedSender<String>,
    events: mpsc::UnboundedSender<SessionEvent>,
    permissions: Arc<dyn PermissionHandler>,
    turn: Option<oneshot::Sender<Result<StopReason, AgentError>>>,
    /// An interrupt was sent for the running turn.
    interrupted: bool,
    pending: HashMap<String, Pending>,
    /// Tool name per `tool_use` id seen in this session.
    tools: HashMap<String, String>,
    init: Option<oneshot::Sender<Result<InitInfo, String>>>,
    next_id: u64,
    session_id: String,
}

impl Actor {
    fn send(&self, msg: Value) {
        let _ = self.out.send(msg.to_string());
    }

    fn emit(&self, event: SessionEvent) {
        let _ = self.events.send(event);
    }

    async fn run<R: AsyncRead + Unpin>(
        &mut self,
        reader: R,
        mut commands: mpsc::UnboundedReceiver<Command>,
        recorder: Option<&Recorder>,
    ) {
        let mut reader = BufReader::new(reader);
        let (perm_done_tx, mut perm_done) = mpsc::unbounded_channel::<String>();
        loop {
            tokio::select! {
                line = read_line(&mut reader) => match line {
                    Ok(Some(line)) => {
                        if let Some(r) = recorder {
                            r.record(Direction::Recv, &line);
                        }
                        match serde_json::from_str::<Value>(&line) {
                            Ok(msg) => self.handle(msg, &perm_done_tx),
                            Err(_) => tracing::debug!("non-JSON line from claude: {line}"),
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!("reading from claude: {e}");
                        break;
                    }
                },
                cmd = commands.recv() => match cmd {
                    Some(Command::Prompt { text, blocks, reply }) => self.prompt(text, blocks, reply),
                    Some(Command::Cancel { reply }) => {
                        self.cancel();
                        let _ = reply.send(());
                    }
                    Some(Command::Close) | None => break,
                },
                Some(id) = perm_done.recv() => {
                    self.pending.remove(&id);
                }
            }
        }
    }

    /// The connection ended: fail what still waits.
    fn finish(&mut self, stderr: &StderrTail) {
        if let Some(init) = self.init.take() {
            let _ = init.send(Err(format!("agent ended the session{}", stderr.summary())));
        }
        if let Some(turn) = self.turn.take() {
            let _ = turn.send(Err(AgentError::AgentExited(stderr.summary())));
        }
        self.pending.clear();
    }

    fn prompt(
        &mut self,
        text: String,
        blocks: Vec<PromptBlock>,
        reply: oneshot::Sender<Result<StopReason, AgentError>>,
    ) {
        if self.turn.is_some() || self.init.is_some() {
            let _ = reply.send(Err(AgentError::Busy));
            return;
        }
        self.interrupted = false;
        self.send(json!({
            "type": "user",
            "message": {"role": "user", "content": user_content(text, &blocks)},
            "parent_tool_use_id": null,
            "session_id": "",
        }));
        self.turn = Some(reply);
    }

    fn cancel(&mut self) {
        if self.turn.is_none() || self.interrupted {
            return;
        }
        self.interrupted = true;
        self.next_id += 1;
        self.send(json!({
            "type": "control_request",
            "request_id": format!("melissa-interrupt-{}", self.next_id),
            "request": {"subtype": "interrupt"},
        }));
        for p in self.pending.values() {
            let _ = p.withdraw.send(Withdrawn::ByUs);
        }
    }

    fn handle(&mut self, msg: Value, perm_done: &mpsc::UnboundedSender<String>) {
        match msg["type"].as_str().unwrap_or_default() {
            "control_response" => self.on_control_response(&msg["response"]),
            "control_request" => self.on_control_request(&msg, perm_done),
            "control_cancel_request" => {
                if let Some(p) = msg["request_id"]
                    .as_str()
                    .and_then(|id| self.pending.get(id))
                {
                    let _ = p.withdraw.send(Withdrawn::ByAgent);
                }
            }
            "stream_event" if msg["parent_tool_use_id"].is_null() => {
                self.on_stream_event(&msg["event"]);
            }
            "assistant" if msg["parent_tool_use_id"].is_null() => {
                // Tool calls normally arrive as stream events first; catch any that did not.
                for block in msg["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] == "tool_use" {
                        self.tool_started(block);
                    }
                }
            }
            "user" if msg["parent_tool_use_id"].is_null() => {
                for block in msg["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] != "tool_result" {
                        continue;
                    }
                    let id = block["tool_use_id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    let status = if block["is_error"].as_bool() == Some(true) {
                        ToolStatus::Failed
                    } else {
                        ToolStatus::Completed
                    };
                    let title = self.tools.get(&id).cloned();
                    self.emit(SessionEvent::ToolFinished { id, title, status });
                }
            }
            "system" => {
                if msg["subtype"] == "init" {
                    if let Some(id) = msg["session_id"].as_str() {
                        if id != self.session_id {
                            tracing::warn!(
                                "claude reports session {id}, expected {}",
                                self.session_id
                            );
                        }
                    }
                }
            }
            "result" => self.on_result(&msg),
            // Known chatter Melissa does not use.
            "stream_event" | "assistant" | "user" | "rate_limit_event" | "keep_alive"
            | "command_lifecycle" | "prompt_suggestion" => {}
            other => self.emit(SessionEvent::Other(other.to_string())),
        }
    }

    fn on_control_response(&mut self, response: &Value) {
        if response["request_id"] != INIT_ID {
            return;
        }
        let Some(init) = self.init.take() else { return };
        let result = if response["subtype"] == "success" {
            let answer = &response["response"];
            Ok((
                answer["claude_code_version"].as_str().map(str::to_string),
                offered_models(&answer["models"]),
            ))
        } else {
            Err(format!(
                "handshake refused: {}",
                response["error"].as_str().unwrap_or("unknown error")
            ))
        };
        let _ = init.send(result);
    }

    fn on_control_request(&mut self, msg: &Value, perm_done: &mpsc::UnboundedSender<String>) {
        let Some(request_id) = msg["request_id"].as_str() else {
            return;
        };
        let request = &msg["request"];
        if request["subtype"] != "can_use_tool" {
            // Hooks, elicitation, MCP over the control channel: not offered by Melissa.
            self.send(json!({"type": "control_response", "response": {
                "subtype": "error", "request_id": request_id,
                "error": "not supported by this client"}}));
            return;
        }
        let tool = request["tool_name"].as_str().unwrap_or("tool").to_string();
        let input = request["input"].clone();
        let req = PermissionRequest {
            tool_call_id: request["tool_use_id"].as_str().unwrap_or_default().into(),
            title: tool.clone(),
            kind: tool_kind(&tool),
            tool_name: Some(tool),
            raw_input: input.clone(),
        };
        let (withdraw, mut withdrawn) = watch::channel(Withdrawn::No);
        if self.interrupted {
            let _ = withdraw.send(Withdrawn::ByUs);
        }
        let handler = self.permissions.clone();
        let out = self.out.clone();
        let events = self.events.clone();
        let done = perm_done.clone();
        let id = request_id.to_string();
        let task = tokio::spawn(async move {
            let (decision, by) = tokio::select! {
                d = handler.decide(&req) => (d, Withdrawn::No),
                by = async {
                    let _ = withdrawn.wait_for(|w| *w != Withdrawn::No).await;
                    *withdrawn.borrow()
                } => (PermissionDecision::Cancelled, by),
            };
            if by != Withdrawn::ByAgent {
                let _ = out.send(permission_response(&id, decision, input).to_string());
            }
            let _ = events.send(SessionEvent::Permission {
                tool_call_id: req.tool_call_id,
                title: req.title,
                decision,
            });
            let _ = done.send(id);
        });
        self.pending.insert(
            request_id.to_string(),
            Pending {
                withdraw,
                _task: AbortOnDrop(task),
            },
        );
    }

    fn tool_started(&mut self, block: &Value) {
        let Some(id) = block["id"].as_str() else {
            return;
        };
        if self.tools.contains_key(id) {
            return;
        }
        let name = block["name"].as_str().unwrap_or("tool").to_string();
        self.tools.insert(id.to_string(), name.clone());
        self.emit(SessionEvent::ToolStarted {
            id: id.to_string(),
            kind: tool_kind(&name),
            title: name,
            status: ToolStatus::Pending,
        });
    }

    fn on_stream_event(&mut self, event: &Value) {
        match event["type"].as_str().unwrap_or_default() {
            "content_block_start" if event["content_block"]["type"] == "tool_use" => {
                let block = event["content_block"].clone();
                self.tool_started(&block);
            }
            "content_block_delta" => {
                let delta = &event["delta"];
                match delta["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        if let Some(t) = delta["text"].as_str().filter(|t| !t.is_empty()) {
                            self.emit(SessionEvent::Text(t.to_string()));
                        }
                    }
                    "thinking_delta" => {
                        if let Some(t) = delta["thinking"].as_str().filter(|t| !t.is_empty()) {
                            self.emit(SessionEvent::Thought(t.to_string()));
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn on_result(&mut self, msg: &Value) {
        let Some(turn) = self.turn.take() else {
            return;
        };
        if let Some((used, size)) = usage(msg) {
            self.emit(SessionEvent::Usage { used, size });
        }
        let result = stop_reason(msg, self.interrupted);
        if let Ok(stop) = &result {
            self.emit(SessionEvent::TurnFinished(*stop));
        }
        self.interrupted = false;
        let _ = turn.send(result);
    }
}

/// Map a `result` message to the turn's end. After an interrupt every ending counts as
/// cancelled; errors (API errors, crashes inside Claude) fail the turn.
/// The models in Claude's handshake answer (`[{value, displayName, supportedEffortLevels,
/// supportsFastMode, ...}]`). The id, the display name, the reasoning levels and whether it
/// has a fast mode are kept; anything that is not a usable model id is skipped.
pub(crate) fn offered_models(models: &Value) -> Vec<ModelOption> {
    models
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|m| {
                    let id = m["value"].as_str()?;
                    if !crate::session::valid_model_id(id) {
                        return None;
                    }
                    let label = m["displayName"].as_str().unwrap_or(id);
                    Some(ModelOption {
                        id: id.into(),
                        label: label.chars().take(80).collect(),
                        efforts: offered_efforts(&m["supportedEffortLevels"]),
                        fast: m["supportsFastMode"].as_bool() == Some(true),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A model's reasoning levels in Claude's handshake answer (`supportedEffortLevels`: ids
/// such as `low` or `xhigh`, which `--effort` takes). Claude gives no names: they are
/// worded here; an id Melissa has no words for keeps its id.
pub(crate) fn offered_efforts(levels: &Value) -> Vec<EffortOption> {
    levels
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .filter(|id| crate::session::valid_model_id(id))
                .take(12)
                .map(|id| EffortOption {
                    id: id.into(),
                    label: effort_words(id).map_or_else(|| id.to_string(), str::to_string),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn effort_words(id: &str) -> Option<&'static str> {
    Some(match id {
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" => "Extra high",
        "max" => "Max",
        _ => return None,
    })
}

pub(crate) fn stop_reason(msg: &Value, interrupted: bool) -> Result<StopReason, AgentError> {
    if interrupted {
        return Ok(StopReason::Cancelled);
    }
    let subtype = msg["subtype"].as_str().unwrap_or_default();
    let is_error = msg["is_error"].as_bool().unwrap_or(false);
    match subtype {
        "success" if !is_error => Ok(match msg["stop_reason"].as_str() {
            Some("max_tokens") => StopReason::MaxTokens,
            Some("refusal") => StopReason::Refusal,
            _ => StopReason::EndTurn,
        }),
        "error_max_turns" => Ok(StopReason::MaxTurnRequests),
        _ => {
            let detail = msg["result"]
                .as_str()
                .map(str::to_string)
                .or_else(|| {
                    msg["errors"].as_array().map(|e| {
                        e.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("; ")
                    })
                })
                .unwrap_or_default();
            let detail: String = detail.chars().take(300).collect();
            Err(AgentError::Protocol(format!(
                "Claude ended the turn with `{subtype}`: {detail}"
            )))
        }
    }
}

/// Context use after a turn: all input (incl. cache) and output tokens of the last call,
/// and the model's context window.
fn usage(msg: &Value) -> Option<(u64, u64)> {
    let u = &msg["usage"];
    let used = [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
        "output_tokens",
    ]
    .iter()
    .filter_map(|k| u[*k].as_u64())
    .sum::<u64>();
    let size = msg["modelUsage"]
        .as_object()?
        .values()
        .find_map(|m| m["contextWindow"].as_u64())?;
    (used > 0).then_some((used, size))
}

/// The answer to a `can_use_tool` request. Allow passes the input through unchanged; there
/// are no "always" answers (no `updatedPermissions`).
pub(crate) fn permission_response(id: &str, decision: PermissionDecision, input: Value) -> Value {
    let response = match decision {
        PermissionDecision::AllowOnce => json!({"behavior": "allow", "updatedInput": input}),
        PermissionDecision::RejectOnce => json!({"behavior": "deny",
            "message": "Melissa's policy refused this tool call."}),
        PermissionDecision::Cancelled => json!({"behavior": "deny",
            "message": "Cancelled by Melissa.", "interrupt": true}),
    };
    json!({"type": "control_response", "response": {
        "subtype": "success", "request_id": id, "response": response}})
}

/// Policy kind of a Claude tool by name. With `--tools ""` only MCP tools (`mcp__…`,
/// [`ToolKind::Other`]) exist; built-in names are mapped in case a config enables them.
pub(crate) fn tool_kind(name: &str) -> ToolKind {
    match name {
        "Read" | "NotebookRead" => ToolKind::Read,
        "Glob" | "Grep" | "LS" | "ToolSearch" => ToolKind::Search,
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => ToolKind::Edit,
        "Bash" | "BashOutput" | "KillShell" | "KillBash" => ToolKind::Execute,
        "WebFetch" | "WebSearch" => ToolKind::Fetch,
        "ExitPlanMode" | "EnterPlanMode" => ToolKind::SwitchMode,
        _ => ToolKind::Other,
    }
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Handle to an open Claude session.
struct ClaudeSession {
    id: String,
    caps: SessionCapabilities,
    cmd_tx: mpsc::UnboundedSender<Command>,
    done: watch::Receiver<bool>,
    stderr: StderrTail,
    closing: std::sync::atomic::AtomicBool,
    _task: AbortOnDrop,
}

impl ClaudeSession {
    fn closed(&self) -> AgentError {
        if self.closing.load(std::sync::atomic::Ordering::SeqCst) {
            AgentError::SessionClosed
        } else {
            AgentError::AgentExited(self.stderr.summary())
        }
    }
}

/// A user message's `content`: the plain text, or (with files) the files as image and
/// document blocks first, then the text (the order Anthropic recommends).
pub(crate) fn user_content(text: String, blocks: &[PromptBlock]) -> Value {
    if blocks.is_empty() {
        return Value::String(text);
    }
    let mut parts: Vec<Value> = blocks
        .iter()
        .map(|b| match b {
            PromptBlock::Image { mime, data } => json!({
                "type": "image",
                "source": {"type": "base64", "media_type": mime, "data": data},
            }),
            PromptBlock::Pdf { data } => json!({
                "type": "document",
                "source": {"type": "base64", "media_type": "application/pdf", "data": data},
            }),
        })
        .collect();
    parts.push(json!({"type": "text", "text": text}));
    Value::Array(parts)
}

impl AgentSession for ClaudeSession {
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
            self.closing
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = self.cmd_tx.send(Command::Close);
            let mut done = self.done.clone();
            let _ = tokio::time::timeout(CLOSE_WAIT, done.wait_for(|d| *d)).await;
            Ok(())
        })
    }
}
