//! The Queen: Melissa's head agent on this comb. One long-lived agent conversation per
//! daemon that the user talks to; it plans, uses Melissa's tools, and creates, names and
//! directs this comb's **bees** ([`bees`]): persistent agents that each look after their own
//! cells.
//!
//! The conversation machinery (lazy session, ordered backends, one turn at a time, cancel,
//! agent death, resume, sticky taint, system prompt) is shared with every bee
//! ([`conversation`]). Everything is recorded in the [`events::EventLog`] (clients follow it
//! with a cursor) and the JSONL [`transcript::Transcript`].

pub mod bees;
pub(crate) mod conversation;
pub mod events;
pub mod registry;
pub mod transcript;

#[cfg(test)]
mod tests;

use std::{path::PathBuf, sync::Arc};

use crate::{
    mcp::McpServer,
    session::SessionBackend,
    tools::{
        Confirmation, ConfirmationEvent, ConfirmationId, ConfirmationStore, Scope, TaskContext,
        Verdict,
    },
    AuditRecord, TaskBoard, TaskId, MAX_PROMPT_BYTES,
};

use bees::BeePool;
use conversation::{Conversation, ConversationSpec};
pub use conversation::{QueenStatus, TurnOutcome};
use events::{EventLog, QueenEvent, TurnId};

/// How the Queen is set up (`[queen]` in the queen config, plus the shipped prompts).
#[derive(Debug, Clone)]
pub struct QueenSettings {
    /// Session backends to try, in order (e.g. `["grok-acp"]`).
    pub backends: Vec<String>,
    /// The Queen's system prompt.
    pub system_prompt: String,
    /// The system prompt of every bee (the bee's name and id are added).
    pub bee_prompt: String,
    /// Backend for new bees when the Queen names none (default: the first Queen backend).
    pub bee_backend: Option<String>,
    /// Melissa's state for the Queen: `session.json`, the agent's session store.
    pub state_dir: PathBuf,
    /// The Queen agent's working directory; bees work in `<workdir>/../bees/<id>`.
    pub workdir: PathBuf,
    /// Tools the Queen may use (None = all). Bees get at most these.
    pub tools: Option<Vec<String>>,
    /// This comb's id (the Queen acts on the cells of her comb).
    pub comb: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QueenError {
    #[error("still working on the previous message; wait for it or cancel it")]
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
    #[error("no bee `{0}` on this comb")]
    NoSuchBee(String),
    /// Bad input for a bee or cell operation (name taken, bad name, ...).
    #[error("{0}")]
    Invalid(String),
    /// The cluster or the state directory failed.
    #[error("{0}")]
    Failed(String),
}

struct Inner {
    conv: Conversation,
    server: McpServer,
    board: TaskBoard,
    pool: BeePool,
}

/// Handle to the Queen. Cheap to clone.
#[derive(Clone)]
pub struct Queen {
    inner: Arc<Inner>,
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
        let ctx = TaskContext {
            // The Queen acts only on a message from the user, who is in the chat.
            attended: true,
            delegate: true,
            // Every cell on her comb; destructive actions name the owning bee.
            scope: Scope::Queen {
                comb: settings.comb.clone(),
            },
            ..TaskContext::default()
        };
        let spec = ConversationSpec {
            who: "the Queen".into(),
            audit_kind: "queen",
            backends: settings.backends.clone(),
            system_prompt: settings.system_prompt.clone(),
            state_dir: settings.state_dir.clone(),
            workdir: settings.workdir.clone(),
            task,
            ctx,
            allow: allow.clone(),
        };
        let conv = Conversation::new(
            spec,
            backends.clone(),
            link.server.clone(),
            link.base_url.clone(),
            events.clone(),
            board.audit_log().cloned(),
        );
        let workroot = settings
            .workdir
            .parent()
            .map_or_else(|| settings.workdir.join("bees"), |p| p.join("bees"));
        pool.attach(bees::Attached {
            board: board.clone(),
            server: link.server.clone(),
            base_url: link.base_url,
            backends,
            default_backend: settings.bee_backend.clone().unwrap_or(first),
            bee_prompt: settings.bee_prompt.clone(),
            queen_allow: allow,
            queen_task: task,
            workroot,
        });
        forward_confirmations(registry.confirmations(), events);
        Ok(Self {
            inner: Arc::new(Inner {
                conv,
                server: link.server,
                board,
                pool: pool.clone(),
            }),
        })
    }

    pub fn events(&self) -> &EventLog {
        self.inner.conv.events()
    }

    pub fn confirmations(&self) -> &ConfirmationStore {
        self.inner.server.registry().confirmations()
    }

    /// The Queen's task id (audit records, confirmations).
    pub fn task_id(&self) -> TaskId {
        self.inner.conv.task_id()
    }

    /// This comb's bees.
    pub fn bees(&self) -> &BeePool {
        &self.inner.pool
    }

    /// A human approves or denies a pending confirmation (audited, actor `ui`). `None` if
    /// it no longer exists.
    pub fn resolve_confirmation(
        &self,
        id: ConfirmationId,
        verdict: Verdict,
    ) -> Option<Confirmation> {
        let c = self.confirmations().resolve(id, verdict)?;
        if let Some(log) = self.inner.board.audit_log() {
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
        self.inner.conv.status()
    }

    /// The policy context and tool allowlist of the Queen's MCP session.
    #[cfg(test)]
    pub(crate) fn session_policy(&self) -> (TaskContext, Option<Vec<&'static str>>) {
        let spec = self.inner.conv.spec();
        (spec.ctx.clone(), spec.allow.clone())
    }

    /// Send a user message: starts a turn and returns its id and the event sequence number
    /// to follow from (the `user_message` event comes right after it). Must be called
    /// inside a Tokio runtime.
    pub fn send(&self, text: &str) -> Result<(TurnId, u64), QueenError> {
        self.inner.conv.send(text, "ui")
    }

    /// Cancel the running turn. Returns false if no turn runs.
    pub async fn cancel(&self) -> bool {
        self.inner.conv.cancel(None, "ui").await
    }

    /// Close the agent sessions of the Queen and every bee (daemon shutdown).
    pub async fn close(&self) {
        self.inner.pool.close_all().await;
        self.inner.conv.close().await;
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
