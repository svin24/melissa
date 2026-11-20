//! The lead agent: the agent in charge of this device. One long-lived agent conversation per
//! daemon that the user talks to; it plans, uses Melissa's tools, and creates, names and
//! directs this device's **agents** ([`agents`]): persistent agents that each look after their own
//! workspaces.
//!
//! The conversation machinery (lazy session, ordered backends, one turn at a time, cancel,
//! agent death, resume, sticky taint, system prompt) is shared with every agent
//! ([`conversation`]). Everything is recorded in the [`events::EventLog`] (clients follow it
//! with a cursor) and the JSONL [`transcript::Transcript`].

pub mod agents;
pub mod backends;
pub(crate) mod conversation;
pub mod events;
pub mod registry;
pub mod transcript;

#[cfg(test)]
mod tests;

use std::{path::PathBuf, sync::Arc};

use crate::{
    attachments::{AttachmentError, AttachmentStore, Upload},
    mcp::McpServer,
    session::SessionBackend,
    tools::{
        Confirmation, ConfirmationEvent, ConfirmationId, ConfirmationStore, Scope, TaskContext,
        Verdict,
    },
    AuditRecord, TaskBoard, TaskId, MAX_PROMPT_BYTES,
};

use agents::AgentPool;
use backends::{BackendChoice, BackendView, ModelCache, ProbeOutcome, Switched};
use conversation::{Conversation, ConversationSpec};
pub use conversation::{LeadStatus, TurnOutcome};
use events::{EventLog, LeadEvent, TurnId};

/// How the lead agent is set up (`[lead]` in the lead agent config, plus the shipped prompts).
#[derive(Debug, Clone)]
pub struct LeadSettings {
    /// Session backends to try, in order (e.g. `["grok-acp"]`).
    pub backends: Vec<String>,
    /// The lead agent's system prompt.
    pub system_prompt: String,
    /// The system prompt of every agent (the agent's name and id are added).
    pub agent_prompt: String,
    /// Backend for new agents when the lead agent names none (default: the first lead agent backend).
    /// Once the user switches the lead agent, new agents follow the lead agent's backend instead.
    pub agent_backend: Option<String>,
    /// Melissa's state for the lead agent: `session.json`, the agent's session store.
    pub state_dir: PathBuf,
    /// The lead agent's working directory; agents work in `<workdir>/../agents/<id>`.
    pub workdir: PathBuf,
    /// Tools the lead agent may use (None = all). Agents get at most these.
    pub tools: Option<Vec<String>>,
    /// This device's id (the lead agent acts on the workspaces of its device).
    pub device: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LeadError {
    #[error("still working on the previous message; wait for it or cancel it")]
    Busy,
    #[error("message is empty")]
    Empty,
    #[error("message is longer than {MAX_PROMPT_BYTES} bytes")]
    TooLong,
    #[error("the lead agent needs a task board with Melissa's tool endpoint")]
    NoTools,
    #[error("unknown tool `{0}` in the lead agent's tool list")]
    UnknownTool(String),
    #[error("no Lead backends are configured")]
    NoBackends,
    #[error("no agent `{0}` on this device")]
    NoSuchAgent(String),
    /// The backend asked for cannot run here now (not installed, not logged in).
    #[error("{0}")]
    Unavailable(String),
    /// Bad input for an agent or workspace operation (name taken, bad name, ...).
    #[error("{0}")]
    Invalid(String),
    /// The cluster or the state directory failed.
    #[error("{0}")]
    Failed(String),
    /// The message's files were refused (too large, too many) or could not be stored.
    #[error(transparent)]
    Attachment(#[from] AttachmentError),
}

struct Inner {
    conv: Conversation,
    /// Where the user's backend choice is saved.
    choice_path: PathBuf,
    server: McpServer,
    board: TaskBoard,
    pool: AgentPool,
    /// Every session backend (the lead agent's and the agents'), for model probes.
    backends: Vec<Arc<dyn SessionBackend>>,
    models: ModelCache,
    /// Model probes run in `<probe_dir>/<backend>` (empty directories).
    probe_dir: PathBuf,
    attachments: AttachmentStore,
}

/// Handle to the lead agent. Cheap to clone.
#[derive(Clone)]
pub struct Lead {
    inner: Arc<Inner>,
}

impl Lead {
    /// Set up the lead agent. `board` must have Melissa's MCP endpoint ([`TaskBoard::with_tools`])
    /// whose registry includes `pool`'s tools. Must be called inside a Tokio runtime.
    pub fn new(
        settings: LeadSettings,
        backends: Vec<Arc<dyn SessionBackend>>,
        board: TaskBoard,
        events: EventLog,
        pool: &AgentPool,
    ) -> Result<Self, LeadError> {
        let first = settings
            .backends
            .first()
            .cloned()
            .ok_or(LeadError::NoBackends)?;
        let link = board.mcp_link().cloned().ok_or(LeadError::NoTools)?;
        let registry = link.server.registry().clone();
        let allow = match &settings.tools {
            None => None,
            Some(names) => Some(
                names
                    .iter()
                    .map(|n| {
                        registry
                            .name(n)
                            .ok_or_else(|| LeadError::UnknownTool(n.clone()))
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        };
        let task = board.reserve_id();
        let ctx = TaskContext {
            // The lead agent acts only on a message from the user, who is in the chat.
            attended: true,
            delegate: true,
            // Every workspace on its device; destructive actions name the owning agent.
            scope: Scope::Lead {
                device: settings.device.clone(),
            },
            ..TaskContext::default()
        };
        let models = ModelCache::open(settings.state_dir.join("models.json"));
        let choice_path = settings.state_dir.join("backend.json");
        let choice = backends::load_choice(&choice_path).filter(|c| {
            let known = settings.backends.contains(&c.backend);
            if !known {
                tracing::warn!(
                    backend = %c.backend,
                    "the lead agent's chosen backend is no longer in `[lead] backends`; ignoring it"
                );
            }
            known
        });
        let choice_backend = choice.as_ref().map(|c| c.backend.clone());
        // One store for every conversation's files (served by one route).
        let attachments = AttachmentStore::new(settings.state_dir.join("attachments"));
        let spec = ConversationSpec {
            who: "the lead agent".into(),
            audit_kind: "lead",
            backends: settings.backends.clone(),
            choices: settings.backends.clone(),
            choice,
            failover: true,
            models: models.clone(),
            system_prompt: settings.system_prompt.clone(),
            state_dir: settings.state_dir.clone(),
            workdir: settings.workdir.clone(),
            task,
            ctx,
            allow: allow.clone(),
            attachments: attachments.clone(),
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
            .map_or_else(|| settings.workdir.join("agents"), |p| p.join("agents"));
        let probe_dir = workroot.with_file_name("probe");
        let all_backends = backends.clone();
        pool.attach(agents::Attached {
            board: board.clone(),
            server: link.server.clone(),
            base_url: link.base_url,
            backends,
            default_backend: settings.agent_backend.clone().unwrap_or(first),
            lead_choice: std::sync::Mutex::new(choice_backend),
            models: models.clone(),
            agent_prompt: settings.agent_prompt.clone(),
            lead_allow: allow,
            lead_task: task,
            workroot,
            attachments: attachments.clone(),
        });
        forward_confirmations(registry.confirmations(), events, pool.clone());
        Ok(Self {
            inner: Arc::new(Inner {
                conv,
                choice_path,
                server: link.server,
                board,
                pool: pool.clone(),
                backends: all_backends,
                models,
                probe_dir,
                attachments,
            }),
        })
    }

    pub fn events(&self) -> &EventLog {
        self.inner.conv.events()
    }

    pub fn confirmations(&self) -> &ConfirmationStore {
        self.inner.server.registry().confirmations()
    }

    /// The lead agent's task id (audit records, confirmations).
    pub fn task_id(&self) -> TaskId {
        self.inner.conv.task_id()
    }

    /// This device's agents.
    pub fn agents(&self) -> &AgentPool {
        &self.inner.pool
    }

    /// The user force-deletes a workspace ([`AgentPool::user_force_delete`]: removed, audited, its
    /// agent told). The lead agent learns it with its next message (a note, never a turn of its
    /// own, so nothing runs behind the user's back).
    pub async fn user_force_delete(
        &self,
        workspace: &str,
    ) -> Result<agents::ForceDeleted, LeadError> {
        let done = self.inner.pool.user_force_delete(workspace).await?;
        self.inner
            .conv
            .note(&agents::force_deleted_text(&done.workspace));
        Ok(done)
    }

    /// The MCP server the lead agent's tools run on (the web UI's own reads and the user's
    /// direct actions go through its registry, policy-checked and audited).
    pub fn server(&self) -> &McpServer {
        &self.inner.server
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

    pub fn status(&self) -> LeadStatus {
        self.inner.conv.status()
    }

    /// The session backends the user can switch the lead agent to (`[lead] backends`), with
    /// what is usable now, the current one and the models each offers (once known).
    pub fn backends(&self) -> Vec<BackendView> {
        self.inner.conv.backends()
    }

    /// Ask the usable session backends which models they offer, without a prompt (see
    /// [`backends::probe`]). Without `force` only backends whose list is missing or stale,
    /// at most once per backend per [`backends::PROBE_INTERVAL`].
    pub async fn refresh_models(&self, force: bool) -> Vec<ProbeOutcome> {
        let i = &self.inner;
        i.models.refresh(&i.backends, &i.probe_dir, force).await
    }

    /// [`Lead::refresh_models`] in the background, not forced (e.g. when a client shows
    /// the agent menu). Must be called inside a Tokio runtime.
    pub fn refresh_models_soon(&self) {
        let lead = self.clone();
        tokio::spawn(async move {
            lead.refresh_models(false).await;
        });
    }

    /// The user's saved choice of backend for the lead agent, if any.
    pub fn backend_choice(&self) -> Option<BackendChoice> {
        self.inner.conv.choice()
    }

    /// The user switches the lead agent to `backend` (and `model`, one the backend reported).
    /// Saved in the state directory, audited `lead.backend`; applies at the next turn,
    /// with the conversation so far. Refused for a backend that is not usable now.
    pub fn use_backend(&self, backend: &str, model: Option<&str>) -> Result<Switched, LeadError> {
        let choice = self.inner.conv.check_choice(backend, model)?;
        backends::save_choice(&self.inner.choice_path, &choice).map_err(|e| {
            LeadError::Failed(format!(
                "cannot save the choice ({}): {e}",
                self.inner.choice_path.display()
            ))
        })?;
        self.inner.pool.follow_lead(&choice.backend);
        Ok(self.inner.conv.switch(choice, "ui"))
    }

    /// The policy context and tool allowlist of the lead agent's MCP session.
    #[cfg(test)]
    pub(crate) fn session_policy(&self) -> (TaskContext, Option<Vec<&'static str>>) {
        let spec = self.inner.conv.spec();
        (spec.ctx.clone(), spec.allow.clone())
    }

    /// Send a user message: starts a turn and returns its id and the event sequence number
    /// to follow from (the `user_message` event comes right after it). Must be called
    /// inside a Tokio runtime.
    pub fn send(&self, text: &str) -> Result<(TurnId, u64), LeadError> {
        self.inner.conv.send(text, "ui")
    }

    /// [`Lead::send`] with files (validated and stored first; see [`crate::attachments`]).
    /// The text may be empty when there are files.
    pub fn send_with(&self, text: &str, files: Vec<Upload>) -> Result<(TurnId, u64), LeadError> {
        self.inner.conv.send_with(text, files, "ui")
    }

    /// Where every conversation's attachments are stored (to serve them).
    pub fn attachments(&self) -> &AttachmentStore {
        &self.inner.attachments
    }

    /// Cancel the running turn. Returns false if no turn runs.
    pub async fn cancel(&self) -> bool {
        self.inner.conv.cancel(None, "ui").await
    }

    /// Start a new chat with the lead agent (the owner-approved way to clear its taint): withdraw
    /// its pending confirmations (audited `confirmation.withdrawn`), stop a running turn,
    /// close its agent session for good (the next message starts a new one, no resume),
    /// archive the transcript (readable under [`Lead::past_chats`]) and clear the taint.
    /// Agents' confirmations that still wait are announced again in the new chat.
    /// Audited `lead.new_chat`. Agents keep their sessions and memory. Returns the old chat's
    /// archive id, if it had anything in it.
    pub async fn new_chat(&self) -> Result<Option<u64>, LeadError> {
        let task = self.task_id();
        for c in self.confirmations().pending() {
            if c.task != task {
                continue;
            }
            self.confirmations().withdraw(c.id);
            if let Some(log) = self.inner.board.audit_log() {
                let record = AuditRecord::now(
                    "confirmation.withdrawn",
                    c.task,
                    "",
                    format!("confirmation {}: the user started a new chat", c.id),
                )
                .by("ui");
                if let Err(e) =
                    log.append(&record.with_tool(&c.tool, &crate::tools::args_hash(&c.args)))
                {
                    tracing::error!(path = %log.path().display(), "audit log write failed: {e}");
                }
            }
        }
        let archived = self.inner.conv.new_chat("ui").await?;
        // Agents' confirmations still wait: they stay in the new chat too.
        for c in self.confirmations().pending() {
            self.events().push(LeadEvent::ConfirmationNeeded {
                id: c.id,
                task: c.task,
                tool: c.tool,
                reason: c.reason,
                agent: self.inner.pool.agent_of_task(c.task),
            });
        }
        Ok(archived)
    }

    /// Chats archived by [`Lead::new_chat`], newest first.
    pub fn past_chats(&self) -> Vec<transcript::PastChat> {
        self.events()
            .transcript()
            .map(|t| t.past_chats())
            .unwrap_or_default()
    }

    /// The events of an archived chat (read-only), None if there is no such chat.
    pub fn past_chat(&self, id: u64) -> Option<Vec<events::EventRecord>> {
        self.events().transcript()?.past_chat(id)
    }

    /// Close the agent sessions of the lead agent and every agent (daemon shutdown).
    pub async fn close(&self) {
        self.inner.pool.close_all().await;
        self.inner.conv.close().await;
    }
}

/// Publishes confirmation inbox changes as events: every one in the lead agent's log (the
/// main chat, labelled with the agent that asked), and an agent's own in its log too, so the
/// approval shows in the chat where that agent works.
fn forward_confirmations(store: &ConfirmationStore, events: EventLog, pool: AgentPool) {
    let mut rx = store.subscribe();
    let store = store.clone();
    tokio::spawn(async move {
        // Pending confirmations of agents → that agent's log (for the outcome).
        let mut agent_logs: std::collections::HashMap<ConfirmationId, EventLog> =
            std::collections::HashMap::new();
        loop {
            match rx.recv().await {
                Ok(ConfirmationEvent::Requested(c)) => {
                    let asker = pool.conversation_of_task(c.task);
                    let event = LeadEvent::ConfirmationNeeded {
                        id: c.id,
                        task: c.task,
                        tool: c.tool,
                        reason: c.reason,
                        agent: asker.as_ref().map(|(id, _)| id.clone()),
                    };
                    if let Some((_, log)) = asker {
                        log.push(event.clone());
                        agent_logs.insert(c.id, log);
                    }
                    events.push(event);
                }
                Ok(ConfirmationEvent::Resolved { id, verdict }) => {
                    let event = LeadEvent::ConfirmationResolved {
                        id,
                        outcome: verdict.map_or("withdrawn", |v| v.as_str()).into(),
                    };
                    if let Some(log) = agent_logs.remove(&id) {
                        log.push(event.clone());
                    }
                    events.push(event);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("Lead missed {n} confirmation events");
                    agent_logs.retain(|id, _| store.get(*id).is_some());
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}
