//! One long-lived agent conversation: the Queen's, or a bee's. The Queen and every bee run
//! the same machinery, each with its own agent session, MCP session (scope), sandbox,
//! session store, event log and transcript.
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
//! - **Taint survives.** Once the conversation's MCP session has read untrusted content (app
//!   logs, a bee that did, a message from a tainted Queen), every later session of this
//!   conversation starts tainted too: a resumed session still has that content, and a new
//!   one is primed from the transcript. The flag is saved in `session.json`, so it also
//!   survives a daemon restart.
//! - **System prompt.** The first message of every new session carries the system prompt
//!   (a reviewed text file shipped with Melissa) as a preamble: ACP has no system prompt
//!   field that Grok supports.

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

use super::{
    events::{EventLog, QueenEvent, TurnId},
    transcript::shorten,
    QueenError,
};
use crate::{
    acp::MCP_SERVER_NAME,
    mcp::McpServer,
    session::{
        AgentSession, McpServerSpec, OpenSession, PermissionContext, PolicyPermissions,
        SessionBackend, SessionEvent, ToolStatus,
    },
    tools::TaskContext,
    AgentError, AuditLog, AuditRecord, TaskId, MAX_PROMPT_BYTES,
};

/// How long a cancelled turn may take to stop before Melissa kills the session.
const CANCEL_GRACE: Duration = Duration::from_secs(10);
/// Characters of earlier conversation replayed into a fresh session.
const SUMMARY_CHARS: usize = 3000;

/// Everything that makes a conversation the Queen's or a particular bee's.
#[derive(Clone)]
pub(crate) struct ConversationSpec {
    /// Who the agent is, for messages ("the Queen", "bee Ada").
    pub who: String,
    /// Prefix of audit actions (`queen.message`, `bee.session`, ...).
    pub audit_kind: &'static str,
    /// Session backends to try, in order.
    pub backends: Vec<String>,
    pub system_prompt: String,
    /// `session.json`, the agent's session store, the transcript live here.
    pub state_dir: PathBuf,
    /// The agent's working directory.
    pub workdir: PathBuf,
    /// The conversation's id in audit records, confirmations and as its MCP session's task.
    pub task: TaskId,
    /// The MCP session's policy context (scope, attended, delegate).
    pub ctx: TaskContext,
    /// Tools the agent may see (None = all its context allows).
    pub allow: Option<Vec<&'static str>>,
}

/// What a client sees of a conversation right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueenStatus {
    /// The running turn, if any.
    pub turn: Option<TurnId>,
    /// Backend of the open session, if any.
    pub backend: Option<String>,
    /// Newest event sequence number.
    pub last_seq: u64,
}

/// How the last finished turn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnOutcome {
    pub turn: TurnId,
    pub stop_reason: String,
    /// The agent's whole reply (may be empty).
    pub reply: String,
    pub error: Option<String>,
}

/// The agent session the conversation talks to.
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
    spec: ConversationSpec,
    backends: Vec<Arc<dyn SessionBackend>>,
    server: McpServer,
    base_url: String,
    events: EventLog,
    audit: Option<AuditLog>,
    turn: Mutex<TurnState>,
    /// The running turn's reply so far.
    text: Mutex<String>,
    live: Mutex<Option<Live>>,
    gen: AtomicU64,
    /// The conversation has read untrusted content (sticky, saved in `session.json`).
    tainted: AtomicBool,
    last: Mutex<Option<TurnOutcome>>,
}

/// Handle to one conversation. Cheap to clone.
#[derive(Clone)]
pub(crate) struct Conversation {
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

impl Conversation {
    pub(crate) fn new(
        spec: ConversationSpec,
        backends: Vec<Arc<dyn SessionBackend>>,
        server: McpServer,
        base_url: String,
        events: EventLog,
        audit: Option<AuditLog>,
    ) -> Self {
        let tainted = load_session(&spec.state_dir.join("session.json")).is_some_and(|s| s.tainted);
        Self {
            inner: Arc::new(Inner {
                spec,
                backends,
                server,
                base_url,
                events,
                audit,
                turn: Mutex::new(TurnState::default()),
                text: Mutex::new(String::new()),
                live: Mutex::new(None),
                gen: AtomicU64::new(0),
                tainted: AtomicBool::new(tainted),
                last: Mutex::new(None),
            }),
        }
    }

    pub(crate) fn events(&self) -> &EventLog {
        &self.inner.events
    }

    pub(crate) fn task_id(&self) -> TaskId {
        self.inner.spec.task
    }

    #[cfg(test)]
    pub(crate) fn spec(&self) -> &ConversationSpec {
        &self.inner.spec
    }

    pub(crate) fn status(&self) -> QueenStatus {
        QueenStatus {
            turn: self.busy(),
            backend: lock(&self.inner.live).as_ref().map(|l| l.backend.clone()),
            last_seq: self.inner.events.last_seq(),
        }
    }

    /// The running turn, if any.
    pub(crate) fn busy(&self) -> Option<TurnId> {
        lock(&self.inner.turn).busy
    }

    pub(crate) fn last_outcome(&self) -> Option<TurnOutcome> {
        lock(&self.inner.last).clone()
    }

    /// The open session's MCP token (tests use it to act as the agent).
    #[cfg(test)]
    pub(crate) fn token(&self) -> Option<String> {
        lock(&self.inner.live).as_ref().map(|l| l.token.clone())
    }

    /// The conversation has read untrusted content (now or in an earlier session).
    pub(crate) fn is_tainted(&self) -> bool {
        if self.inner.tainted.load(Ordering::SeqCst) {
            return true;
        }
        let token = lock(&self.inner.live).as_ref().map(|l| l.token.clone());
        token.is_some_and(|t| {
            self.inner
                .server
                .session_context(&t)
                .is_some_and(|c| c.tainted)
        })
    }

    /// The conversation was handed untrusted content outside a tool call (e.g. by a tainted
    /// Queen): it is tainted from now on, in this session and every later one.
    pub(crate) fn taint(&self) {
        let token = lock(&self.inner.live).as_ref().map(|l| l.token.clone());
        if let Some(t) = token {
            self.inner.server.taint_session(&t);
        }
        self.remember_taint();
    }

    /// Send a message: starts a turn and returns its id and the event sequence number to
    /// follow from (the `user_message` event comes right after it). `actor` is recorded in
    /// the audit log (`ui`, `queen`). Must be called inside a Tokio runtime.
    pub(crate) fn send(&self, text: &str, actor: &str) -> Result<(TurnId, u64), QueenError> {
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
        self.audit("message", actor, &shorten(text, 500));
        let handle = tokio::spawn(self.clone().run_turn(turn, text.to_string()));
        t.abort = Some(handle.abort_handle());
        Ok((turn, since))
    }

    /// Wait up to `wait` for `turn` to end; true if it has.
    pub(crate) async fn wait_turn(&self, turn: TurnId, wait: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let since = self.inner.events.last_seq();
            if self.busy() != Some(turn) {
                return true;
            }
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            self.inner
                .events
                .wait_since(since, 1, left.min(Duration::from_secs(1)))
                .await;
        }
    }

    /// Cancel the running turn (only `turn`, if given). Returns false if no such turn runs.
    pub(crate) async fn cancel(&self, only: Option<TurnId>, actor: &str) -> bool {
        let (turn, abort) = {
            let t = lock(&self.inner.turn);
            match t.busy {
                Some(turn) if only.is_none_or(|o| o == turn) => (turn, t.abort.clone()),
                _ => return false,
            }
        };
        self.audit("cancel", actor, &format!("turn {turn}"));
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
            tracing::warn!(who = %self.inner.spec.who, "cancelling a turn: {e}");
        }
        let conv = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(CANCEL_GRACE).await;
            if lock(&conv.inner.turn).busy != Some(turn) {
                return;
            }
            if let Some(a) = abort {
                a.abort();
            }
            conv.finish_turn(
                turn,
                "cancelled",
                Some("the agent did not stop; Melissa stopped its session".into()),
            );
            conv.lose_session(live.gen, "stopped after a cancel the agent ignored");
        });
        true
    }

    /// Close the agent session (daemon shutdown, a stopped bee). The next message opens a
    /// new one (resuming the agent's session where it can).
    pub(crate) async fn close(&self) {
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
    /// the message.
    fn preamble(&self, text: &str) -> String {
        let mut p = format!(
            "<melissa-system-prompt>\n{}\n</melissa-system-prompt>\n\n",
            self.inner.spec.system_prompt.trim()
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
        p.push_str("The message:\n\n");
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
        let spec = &inner.spec;
        let saved_path = spec.state_dir.join("session.json");
        let saved = load_session(&saved_path);
        let mut ctx = spec.ctx.clone();
        ctx.tainted = inner.tainted.load(Ordering::SeqCst);
        let mut tried = Vec::new();
        for name in &spec.backends {
            tried.push(name.as_str());
            let backend = inner.backends.iter().find(|b| b.info().name == *name);
            let Some(backend) = backend.filter(|b| b.info().available) else {
                self.backend_failed(name, "not available on this comb");
                continue;
            };
            let token = match inner
                .server
                .open_session(spec.task, ctx.clone(), spec.allow.clone())
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
                    task: spec.task,
                    backend: name.clone(),
                    mcp_name: MCP_SERVER_NAME.into(),
                    mcp: Some((inner.server.clone(), token.token().to_string())),
                },
            );
            if let Err(e) = std::fs::create_dir_all(&spec.workdir) {
                self.backend_failed(name, &format!("cannot create its working directory: {e}"));
                continue;
            }
            let mut open = OpenSession::new(
                spec.workdir.clone(),
                vec![McpServerSpec {
                    name: MCP_SERVER_NAME.into(),
                    url: format!("{}{}", inner.base_url, token.token()),
                    headers: Vec::new(),
                }],
                Arc::new(permissions),
            );
            open.state_dir = Some(spec.state_dir.join("agent-sessions").join(name));
            open.task = Some(spec.task);
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
            self.audit(
                "session",
                spec.audit_kind,
                &format!("opened {name} ({how})"),
            );
            return Ok(live);
        }
        Err(format!(
            "no backend could start for {} (tried: {})",
            spec.who,
            tried.join(", ")
        ))
    }

    fn backend_failed(&self, backend: &str, error: &str) {
        tracing::warn!(who = %self.inner.spec.who, backend, "session backend failed: {error}");
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
        tracing::warn!(who = %self.inner.spec.who, backend = %live.backend, "session lost: {reason}");
        self.inner.events.push(QueenEvent::SessionLost {
            reason: reason.to_string(),
        });
        self.audit(
            "session",
            self.inner.spec.audit_kind,
            &format!("lost {}: {reason}", live.backend),
        );
        let busy = self.busy();
        if let Some(turn) = busy {
            self.finish_turn(
                turn,
                "error",
                Some(format!(
                    "the agent of {} stopped during this turn ({reason}); its context may be \
                     lost. Send your message again to start a new session.",
                    self.inner.spec.who
                )),
            );
        }
        tokio::spawn(async move {
            let _ = live.session.close().await;
        });
    }

    /// Remember (and save) that the MCP session `token` read untrusted content, so the
    /// next session of this conversation starts tainted too.
    fn keep_taint(&self, token: &str) {
        let tainted = self
            .inner
            .server
            .session_context(token)
            .is_some_and(|c| c.tainted);
        if tainted {
            self.remember_taint();
        }
    }

    fn remember_taint(&self) {
        if self.inner.tainted.swap(true, Ordering::SeqCst) {
            return;
        }
        let path = self.inner.spec.state_dir.join("session.json");
        if let Some(mut saved) = load_session(&path) {
            saved.tainted = true;
            save_session(&path, &saved);
        }
    }

    /// End `turn` (once): publish the full reply, an error if any, then `turn_finished`.
    pub(crate) fn finish_turn(&self, turn: TurnId, stop_reason: &str, error: Option<String>) {
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
        *lock(&self.inner.last) = Some(TurnOutcome {
            turn,
            stop_reason: stop_reason.into(),
            reply: text.clone(),
            error: error.clone(),
        });
        let events = &self.inner.events;
        if !text.is_empty() {
            events.push(QueenEvent::AgentMessage { turn, text });
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
        let turn = self.busy();
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

    /// Audit `<kind>.<action>` (e.g. `queen.message`, `bee.session`) on this conversation's
    /// task.
    pub(crate) fn audit(&self, action: &str, actor: &str, detail: &str) {
        if let Some(log) = &self.inner.audit {
            let action = format!("{}.{action}", self.inner.spec.audit_kind);
            let record = AuditRecord::now(&action, self.inner.spec.task, "", detail).by(actor);
            if let Err(e) = log.append(&record) {
                tracing::error!(path = %log.path().display(), "audit log write failed: {e}");
            }
        }
    }
}

/// Feeds one session's events to the conversation in order; notices when the session ends.
async fn pump(conv: Weak<Inner>, gen: u64, mut events: mpsc::UnboundedReceiver<SessionEvent>) {
    while let Some(event) = events.recv().await {
        let Some(inner) = conv.upgrade() else { return };
        Conversation { inner }.on_session_event(event);
    }
    if let Some(inner) = conv.upgrade() {
        Conversation { inner }.lose_session(gen, "the agent's session ended");
    }
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
        tracing::warn!(path = %path.display(), "cannot save the session id: {e}");
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

pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // Poisoning only means another thread panicked mid-update; the data is still usable.
    m.lock().unwrap_or_else(|p| p.into_inner())
}
