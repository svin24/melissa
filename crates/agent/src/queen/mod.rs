//! The Queen: Melissa's head agent. One long-lived agent session per daemon that the user
//! talks to; it plans, uses Melissa's tools and starts bee agents ([`bees`]).
//!
//! - **Lazy session, ordered backends.** The session opens on the first message, trying
//!   the configured session backends in order (a backend that fails to start is logged
//!   and the next one tried). It stays open across messages.
//! - **One turn at a time.** A message while a turn runs is refused with
//!   [`QueenError::Busy`]: clients see the turn end in the event stream and send the next
//!   message then; nothing runs behind the user's back on a stale queued instruction.
//! - **Cancel** asks the agent to stop the turn; if it has not stopped after a grace
//!   period, Melissa kills the session.
//! - **Agent death.** If the agent process dies, the turn fails, a `session_lost` event
//!   tells the user, and the next message opens a new session. That session first tries to
//!   resume the old one from the agent's own session store (kept in Melissa's state
//!   directory, see [`OpenSession::state_dir`]); if that fails, the new session is primed
//!   with a short summary from Melissa's transcript.
//! - **Taint survives.** Once the Queen's MCP session has read untrusted content (app logs,
//!   a bee that did), every later session of this conversation starts tainted too: a
//!   resumed session still has that content, and a new one is primed from the transcript.
//!   The flag is saved in `session.json`, so it also survives a daemon restart.
//! - **System prompt.** The first message of every new session carries the Queen's system
//!   prompt (a reviewed text file shipped with Melissa) as a preamble: ACP has no system
//!   prompt field that Grok supports.
//! - Everything is recorded in the [`events::EventLog`] (clients follow it with a cursor)
//!   and the JSONL [`transcript::Transcript`].

pub mod bees;
pub mod events;
pub mod transcript;

#[cfg(test)]
mod tests;

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, Weak,
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tokio::{sync::mpsc, task::AbortHandle};

use crate::{
    acp::MCP_SERVER_NAME,
    mcp::McpServer,
    session::{
        AgentSession, McpServerSpec, OpenSession, PermissionContext, PolicyPermissions,
        SessionBackend, SessionEvent, ToolStatus,
    },
    tools::{
        Confirmation, ConfirmationEvent, ConfirmationId, ConfirmationStore, Scope, TaskContext,
        Verdict,
    },
    AgentError, AuditLog, AuditRecord, TaskBoard, TaskId, TaskSnapshot, MAX_PROMPT_BYTES,
};

use bees::BeePool;
use events::{EventLog, QueenEvent, TurnId};
use transcript::shorten;

/// How long a cancelled turn may take to stop before Melissa kills the session.
const CANCEL_GRACE: Duration = Duration::from_secs(10);
/// Characters of earlier conversation replayed into a fresh session.
const SUMMARY_CHARS: usize = 3000;

/// How the Queen is set up (`[queen]` in the queen config, plus the shipped prompts).
#[derive(Debug, Clone)]
pub struct QueenSettings {
    /// Session backends to try, in order (e.g. `["grok-acp"]`).
    pub backends: Vec<String>,
    /// The Queen's system prompt.
    pub system_prompt: String,
    /// Instructions put in front of every bee's goal.
    pub bee_prompt: String,
    /// Backend for bees when the Queen names none (default: the first Queen backend).
    pub bee_backend: Option<String>,
    /// Melissa's state for the Queen: `session.json`, the agent's session store.
    pub state_dir: PathBuf,
    /// The Queen agent's working directory.
    pub workdir: PathBuf,
    /// Tools the Queen may use (None = all). Bees get at most these.
    pub tools: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QueenError {
    #[error("the Queen is still working on the previous message; wait for it or cancel it")]
    Busy,
    #[error("message is empty")]
    Empty,
    #[error("message is longer than {MAX_PROMPT_BYTES} bytes")]
    TooLong,
    #[error("the Queen needs a task board with Melissa's tool endpoint")]
    NoTools,
    #[error("unknown tool `{0}` in the Queen's tool list")]
    UnknownTool(String),
    #[error("no Queen backends are configured")]
    NoBackends,
}

/// What a client sees of the Queen right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueenStatus {
    /// The running turn, if any.
    pub turn: Option<TurnId>,
    /// Backend of the open session, if any.
    pub backend: Option<String>,
    /// Newest event sequence number.
    pub last_seq: u64,
}

/// The agent session the Queen talks to.
#[derive(Clone)]
struct Live {
    /// Changes with every new session, so stale notices about an old one are ignored.
    gen: u64,
    session: Arc<dyn AgentSession>,
    backend: String,
    /// The session's MCP token (closed with the session).
    token: String,
    /// The system prompt has been sent in this session.
    primed: Arc<AtomicBool>,
}

#[derive(Default)]
struct TurnState {
    busy: Option<TurnId>,
    last: TurnId,
    abort: Option<AbortHandle>,
}

struct Inner {
    settings: QueenSettings,
    backends: Vec<Arc<dyn SessionBackend>>,
    board: TaskBoard,
    server: McpServer,
    base_url: String,
    events: EventLog,
    audit: Option<AuditLog>,
    /// The Queen's id in audit records, confirmations and as the bees' parent.
    task: TaskId,
    allow: Option<Vec<&'static str>>,
    turn: Mutex<TurnState>,
    /// The running turn's reply so far.
    text: Mutex<String>,
    live: Mutex<Option<Live>>,
    gen: AtomicU64,
    pool: BeePool,
    /// The conversation has read untrusted content (sticky, saved in `session.json`).
    tainted: AtomicBool,
}

/// Handle to the Queen. Cheap to clone.
#[derive(Clone)]
pub struct Queen {
    inner: Arc<Inner>,
}

/// Saved between daemon runs so the next session can try to resume the agent's session.
#[derive(Debug, Serialize, Deserialize)]
struct SavedSession {
    backend: String,
    session_id: String,
    /// The conversation read untrusted content; the next session starts tainted.
    #[serde(default)]
    tainted: bool,
}

fn load_session(path: &std::path::Path) -> Option<SavedSession> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

impl Queen {
    /// Set up the Queen. `board` must have Melissa's MCP endpoint ([`TaskBoard::with_tools`])
    /// whose registry includes `pool`'s tools. Must be called inside a Tokio runtime.
    pub fn new(
        settings: QueenSettings,
        backends: Vec<Arc<dyn SessionBackend>>,
        board: TaskBoard,
        events: EventLog,
        pool: &BeePool,
    ) -> Result<Self, QueenError> {
        let first = settings
            .backends
            .first()
            .cloned()
            .ok_or(QueenError::NoBackends)?;
        let link = board.mcp_link().cloned().ok_or(QueenError::NoTools)?;
        let registry = link.server.registry().clone();
        let allow = match &settings.tools {
            None => None,
            Some(names) => Some(
                names
                    .iter()
                    .map(|n| {
                        registry
                            .name(n)
                            .ok_or_else(|| QueenError::UnknownTool(n.clone()))
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        };
        let task = board.reserve_id();
        let tainted =
            load_session(&settings.state_dir.join("session.json")).is_some_and(|s| s.tainted);
        pool.attach(
            board.clone(),
            settings.bee_backend.clone().unwrap_or(first),
            settings.bee_prompt.clone(),
            allow.clone(),
        );
        forward_confirmations(registry.confirmations(), events.clone());
        Ok(Self {
            inner: Arc::new(Inner {
                audit: board.audit_log().cloned(),
                settings,
                backends,
                board,
                server: link.server,
                base_url: link.base_url,
                events,
                task,
                allow,
                turn: Mutex::new(TurnState::default()),
                text: Mutex::new(String::new()),
                live: Mutex::new(None),
                gen: AtomicU64::new(0),
                pool: pool.clone(),
                tainted: AtomicBool::new(tainted),
            }),
        })
    }

    pub fn events(&self) -> &EventLog {
        &self.inner.events
    }

    pub fn confirmations(&self) -> &ConfirmationStore {
        self.inner.server.registry().confirmations()
    }

    /// The Queen's task id (audit records, confirmations, bees' parent).
    pub fn task_id(&self) -> TaskId {
        self.inner.task
    }

    /// The Queen's bees, newest first.
    pub fn bees(&self) -> Vec<TaskSnapshot> {
        self.inner
            .board
            .list()
            .into_iter()
            .filter(|t| t.parent == Some(self.inner.task))
            .collect()
    }

    /// The goal the Queen gave bee `id`.
    pub fn bee_goal(&self, id: TaskId) -> Option<String> {
        self.inner.pool.goal(id)
    }

    /// A human approves or denies a pending confirmation (audited, actor `ui`). `None` if
    /// it no longer exists.
    pub fn resolve_confirmation(
        &self,
        id: ConfirmationId,
        verdict: Verdict,
    ) -> Option<Confirmation> {
        let c = self.confirmations().resolve(id, verdict)?;
        if let Some(log) = &self.inner.audit {
            let record = AuditRecord::now(
                &format!("confirmation.{}", verdict.as_str()),
                c.task,
                "",
                format!("confirmation {id}: {}", c.reason),
            )
            .by("ui");
            if let Err(e) =
                log.append(&record.with_tool(&c.tool, &crate::tools::args_hash(&c.args)))
            {
                tracing::error!(path = %log.path().display(), "audit log write failed: {e}");
            }
        }
        Some(c)
    }

    pub fn status(&self) -> QueenStatus {
        QueenStatus {
            turn: lock(&self.inner.turn).busy,
            backend: lock(&self.inner.live).as_ref().map(|l| l.backend.clone()),
            last_seq: self.inner.events.last_seq(),
        }
    }

    /// The policy context and tool allowlist of the Queen's MCP session.
    pub(crate) fn session_policy(&self) -> (TaskContext, Option<Vec<&'static str>>) {
        let ctx = TaskContext {
            // The Queen acts only on a message from the user, who is in the chat.
            attended: true,
            delegate: true,
            // Every app; the owner's apps only after a confirmation naming them.
            scope: Scope::ForOwner {
                agent: "the Queen".into(),
            },
            ..TaskContext::default()
        };
        (ctx, self.inner.allow.clone())
    }

    /// Send a user message: starts a turn and returns its id and the event sequence number
    /// to follow from (the `user_message` event comes right after it). Must be called
    /// inside a Tokio runtime.
    pub fn send(&self, text: &str) -> Result<(TurnId, u64), QueenError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(QueenError::Empty);
        }
        if text.len() > MAX_PROMPT_BYTES {
            return Err(QueenError::TooLong);
        }
        let mut t = lock(&self.inner.turn);
        if t.busy.is_some() {
            return Err(QueenError::Busy);
        }
        t.last += 1;
        let turn = t.last;
        t.busy = Some(turn);
        let since = self.inner.events.last_seq();
        self.inner.events.push(QueenEvent::UserMessage {
            turn,
            text: text.to_string(),
        });
        self.audit("queen.message", "ui", &shorten(text, 500));
        let handle = tokio::spawn(self.clone().run_turn(turn, text.to_string()));
        t.abort = Some(handle.abort_handle());
        Ok((turn, since))
    }

    /// Cancel the running turn. Returns false if no turn runs.
    pub async fn cancel(&self) -> bool {
        let (turn, abort) = {
            let t = lock(&self.inner.turn);
            match t.busy {
                Some(turn) => (turn, t.abort.clone()),
                None => return false,
            }
        };
        self.audit("queen.cancel", "ui", &format!("turn {turn}"));
        let live = lock(&self.inner.live).clone();
        let Some(live) = live else {
            // Still opening the session: stop that.
            if let Some(a) = abort {
                a.abort();
            }
            self.finish_turn(turn, "cancelled", None);
            return true;
        };
        if let Err(e) = live.session.cancel().await {
            tracing::warn!("cancelling the Queen's turn: {e}");
        }
        let queen = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(CANCEL_GRACE).await;
            if lock(&queen.inner.turn).busy != Some(turn) {
                return;
            }
            if let Some(a) = abort {
                a.abort();
            }
            queen.finish_turn(
                turn,
                "cancelled",
                Some("the agent did not stop; Melissa stopped its session".into()),
            );
            queen.lose_session(live.gen, "stopped after a cancel the agent ignored");
        });
        true
    }

    /// Close the agent session (daemon shutdown).
    pub async fn close(&self) {
        let live = lock(&self.inner.live).take();
        if let Some(live) = live {
            self.keep_taint(&live.token);
            self.inner.server.close_session(&live.token);
            let _ = live.session.close().await;
        }
    }

    async fn run_turn(self, turn: TurnId, text: String) {
        let live = match self.live_session().await {
            Ok(live) => live,
            Err(e) => {
                self.finish_turn(turn, "error", Some(e));
                return;
            }
        };
        let prompt = if live.primed.swap(true, Ordering::SeqCst) {
            text
        } else {
            self.preamble(&text)
        };
        match live.session.prompt(&prompt).await {
            // The event pump ends the turn when it sees the agent's `TurnFinished`, so the
            // turn's last events are published before it.
            Ok(_) => {}
            Err(e @ (AgentError::AgentExited(_) | AgentError::SessionClosed)) => {
                self.lose_session(live.gen, &e.to_string());
            }
            Err(e) => {
                // The agent may not have seen the system prompt; send it again next time.
                live.primed.store(false, Ordering::SeqCst);
                self.finish_turn(turn, "error", Some(e.to_string()));
            }
        }
    }

    /// The system prompt, the earlier conversation if this session starts without it, then
    /// the user's message.
    fn preamble(&self, text: &str) -> String {
        let mut p = format!(
            "<melissa-system-prompt>\n{}\n</melissa-system-prompt>\n\n",
            self.inner.settings.system_prompt.trim()
        );
        if let Some(t) = self.inner.events.transcript() {
            let mut recent = t.recent_messages(SUMMARY_CHARS);
            // The message being sent is already in the transcript.
            if recent
                .last()
                .is_some_and(|(w, m)| *w == "user" && m == text)
            {
                recent.pop();
            }
            if !recent.is_empty() {
                p.push_str(
                    "Earlier conversation, restored by Melissa from its transcript (your previous \
                     session was lost, so details may be missing):\n<earlier-conversation>\n",
                );
                for (who, msg) in recent {
                    p.push_str(&format!("{who}: {msg}\n"));
                }
                p.push_str("</earlier-conversation>\n\n");
            }
        }
        p.push_str("The user's message:\n\n");
        p.push_str(text);
        p
    }

    async fn live_session(&self) -> Result<Live, String> {
        if let Some(live) = lock(&self.inner.live).clone() {
            return Ok(live);
        }
        self.open_session().await
    }

    /// Open a session with the first backend that starts.
    async fn open_session(&self) -> Result<Live, String> {
        let inner = &self.inner;
        let saved_path = inner.settings.state_dir.join("session.json");
        let saved = load_session(&saved_path);
        let (mut ctx, allow) = self.session_policy();
        ctx.tainted = inner.tainted.load(Ordering::SeqCst);
        let mut tried = Vec::new();
        for name in &inner.settings.backends {
            tried.push(name.as_str());
            let backend = inner.backends.iter().find(|b| b.info().name == *name);
            let Some(backend) = backend.filter(|b| b.info().available) else {
                self.backend_failed(name, "not available on this comb");
                continue;
            };
            let token = match inner
                .server
                .open_session(inner.task, ctx.clone(), allow.clone())
            {
                Ok(t) => TokenGuard::new(inner.server.clone(), t),
                Err(e) => {
                    self.backend_failed(name, &format!("cannot open a tool session: {e}"));
                    continue;
                }
            };
            let permissions = PolicyPermissions::new(
                inner.server.registry().clone(),
                PermissionContext {
                    task: inner.task,
                    backend: name.clone(),
                    mcp_name: MCP_SERVER_NAME.into(),
                    mcp: Some((inner.server.clone(), token.token().to_string())),
                },
            );
            let mut open = OpenSession::new(
                inner.settings.workdir.clone(),
                vec![McpServerSpec {
                    name: MCP_SERVER_NAME.into(),
                    url: format!("{}{}", inner.base_url, token.token()),
                    headers: Vec::new(),
                }],
                Arc::new(permissions),
            );
            open.state_dir = Some(inner.settings.state_dir.join("agent-sessions").join(name));
            open.task = Some(inner.task);
            open.resume = saved
                .as_ref()
                .filter(|s| s.backend == *name)
                .map(|s| s.session_id.clone());
            let opened = match backend.open(open).await {
                Ok(o) => o,
                Err(e) => {
                    self.backend_failed(name, &e.to_string());
                    continue;
                }
            };
            let live = Live {
                gen: inner.gen.fetch_add(1, Ordering::SeqCst) + 1,
                session: Arc::from(opened.session),
                backend: name.clone(),
                token: token.keep(),
                primed: Arc::new(AtomicBool::new(opened.resumed)),
            };
            save_session(
                &saved_path,
                &SavedSession {
                    backend: name.clone(),
                    session_id: live.session.id().to_string(),
                    tainted: ctx.tainted,
                },
            );
            tokio::spawn(pump(Arc::downgrade(&self.inner), live.gen, opened.events));
            *lock(&inner.live) = Some(live.clone());
            inner.events.push(QueenEvent::SessionOpened {
                backend: name.clone(),
                resumed: opened.resumed,
            });
            let how = if opened.resumed { "resumed" } else { "new" };
            self.audit("queen.session", "queen", &format!("opened {name} ({how})"));
            return Ok(live);
        }
        Err(format!(
            "no Queen backend could start (tried: {})",
            tried.join(", ")
        ))
    }

    fn backend_failed(&self, backend: &str, error: &str) {
        tracing::warn!(backend, "Queen backend failed: {error}");
        self.inner.events.push(QueenEvent::BackendFailed {
            backend: backend.into(),
            error: error.into(),
        });
    }

    /// The session `gen` is gone (agent died or was killed): forget it, tell the user, end
    /// the running turn. A no-op for a session that was already replaced or closed.
    fn lose_session(&self, gen: u64, reason: &str) {
        let live = {
            let mut l = lock(&self.inner.live);
            if l.as_ref().map(|x| x.gen) != Some(gen) {
                return;
            }
            l.take()
        };
        let Some(live) = live else { return };
        self.keep_taint(&live.token);
        self.inner.server.close_session(&live.token);
        tracing::warn!(backend = %live.backend, "Queen session lost: {reason}");
        self.inner.events.push(QueenEvent::SessionLost {
            reason: reason.to_string(),
        });
        self.audit(
            "queen.session",
            "queen",
            &format!("lost {}: {reason}", live.backend),
        );
        let busy = lock(&self.inner.turn).busy;
        if let Some(turn) = busy {
            self.finish_turn(
                turn,
                "error",
                Some(format!(
                    "the Queen's agent stopped during this turn ({reason}); its context may be \
                     lost. Send your message again to start a new session."
                )),
            );
        }
        tokio::spawn(async move {
            let _ = live.session.close().await;
        });
    }

    /// End `turn` (once): publish the full reply, an error if any, then `turn_finished`.
    /// Remember (and save) that the MCP session `token` read untrusted content, so the
    /// next session of this conversation starts tainted too.
    fn keep_taint(&self, token: &str) {
        let tainted = self
            .inner
            .server
            .session_context(token)
            .is_some_and(|c| c.tainted);
        if !tainted || self.inner.tainted.swap(true, Ordering::SeqCst) {
            return;
        }
        let path = self.inner.settings.state_dir.join("session.json");
        if let Some(mut saved) = load_session(&path) {
            saved.tainted = true;
            save_session(&path, &saved);
        }
    }

    fn finish_turn(&self, turn: TurnId, stop_reason: &str, error: Option<String>) {
        // Taint comes from tool calls, which happen during turns.
        let token = lock(&self.inner.live).as_ref().map(|l| l.token.clone());
        if let Some(token) = token {
            self.keep_taint(&token);
        }
        let mut t = lock(&self.inner.turn);
        if t.busy != Some(turn) {
            return;
        }
        t.busy = None;
        t.abort = None;
        let text = std::mem::take(&mut *lock(&self.inner.text));
        let events = &self.inner.events;
        if !text.is_empty() {
            events.push(QueenEvent::QueenMessage { turn, text });
        }
        if let Some(message) = error {
            events.push(QueenEvent::Error {
                turn: Some(turn),
                message,
            });
        }
        events.push(QueenEvent::TurnFinished {
            turn,
            stop_reason: stop_reason.into(),
        });
        // Released last, so the next turn's events come after this turn's end.
        drop(t);
    }

    fn on_session_event(&self, event: SessionEvent) {
        let turn = lock(&self.inner.turn).busy;
        let events = &self.inner.events;
        match event {
            SessionEvent::Text(text) => {
                // Text outside a turn (late chunks after a cancel) is dropped.
                if let Some(turn) = turn {
                    lock(&self.inner.text).push_str(&text);
                    events.push(QueenEvent::Text { turn, text });
                }
            }
            SessionEvent::ToolStarted {
                id, title, status, ..
            } => {
                events.push(QueenEvent::ToolCall {
                    turn,
                    id,
                    title,
                    status: status.as_str().into(),
                });
            }
            SessionEvent::ToolFinished { id, title, status }
            | SessionEvent::ToolUpdated {
                id,
                title,
                status: Some(status @ ToolStatus::Failed),
            } => {
                events.push(QueenEvent::ToolCall {
                    turn,
                    id,
                    title: title.unwrap_or_else(|| "tool call".into()),
                    status: status.as_str().into(),
                });
            }
            SessionEvent::Permission {
                title, decision, ..
            } => {
                events.push(QueenEvent::Permission {
                    turn,
                    title,
                    decision: decision.as_str().into(),
                });
            }
            SessionEvent::TurnFinished(stop) => {
                if let Some(turn) = turn {
                    self.finish_turn(turn, stop.as_str(), None);
                }
            }
            SessionEvent::ToolUpdated { .. }
            | SessionEvent::Thought(_)
            | SessionEvent::Plan(_)
            | SessionEvent::Usage { .. }
            | SessionEvent::Other(_) => {}
        }
    }

    fn audit(&self, action: &str, actor: &str, detail: &str) {
        if let Some(log) = &self.inner.audit {
            let record = AuditRecord::now(action, self.inner.task, "", detail).by(actor);
            if let Err(e) = log.append(&record) {
                tracing::error!(path = %log.path().display(), "audit log write failed: {e}");
            }
        }
    }
}

/// Feeds one session's events to the Queen in order; notices when the session ends.
async fn pump(queen: Weak<Inner>, gen: u64, mut events: mpsc::UnboundedReceiver<SessionEvent>) {
    while let Some(event) = events.recv().await {
        let Some(inner) = queen.upgrade() else { return };
        Queen { inner }.on_session_event(event);
    }
    if let Some(inner) = queen.upgrade() {
        Queen { inner }.lose_session(gen, "the agent's session ended");
    }
}

/// Publishes confirmation inbox changes as Queen events.
fn forward_confirmations(store: &ConfirmationStore, events: EventLog) {
    let mut rx = store.subscribe();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ConfirmationEvent::Requested(c)) => {
                    events.push(QueenEvent::ConfirmationNeeded {
                        id: c.id,
                        task: c.task,
                        tool: c.tool,
                        reason: c.reason,
                    });
                }
                Ok(ConfirmationEvent::Resolved { id, verdict }) => {
                    events.push(QueenEvent::ConfirmationResolved {
                        id,
                        outcome: verdict.map_or("withdrawn", |v| v.as_str()).into(),
                    });
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("Queen missed {n} confirmation events");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

fn save_session(path: &std::path::Path, saved: &SavedSession) {
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(
            path,
            serde_json::to_vec(saved).map_err(std::io::Error::other)?,
        )
    };
    if let Err(e) = write() {
        tracing::warn!(path = %path.display(), "cannot save the Queen's session id: {e}");
    }
}

/// Closes an MCP session token unless it is kept (open failed or was aborted).
struct TokenGuard {
    server: McpServer,
    token: Option<String>,
}

impl TokenGuard {
    fn new(server: McpServer, token: String) -> Self {
        Self {
            server,
            token: Some(token),
        }
    }

    fn token(&self) -> &str {
        self.token.as_deref().unwrap_or_default()
    }

    fn keep(mut self) -> String {
        self.token.take().unwrap_or_default()
    }
}

impl Drop for TokenGuard {
    fn drop(&mut self) {
        if let Some(token) = self.token.take() {
            self.server.close_session(&token);
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // Poisoning only means another thread panicked mid-update; the data is still usable.
    m.lock().unwrap_or_else(|p| p.into_inner())
}
