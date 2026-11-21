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
pub mod icon;
pub(crate) mod prompt;
pub mod registry;
pub mod schedules;
pub mod transcript;
pub mod usage;

#[cfg(test)]
mod tests;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use tokio::task::AbortHandle;

use crate::{
    attachments::{AttachmentError, Upload},
    files::FileBoxes,
    mcp::McpServer,
    session::SessionBackend,
    tools::{
        permissions::Principal, Confirmation, ConfirmationEvent, ConfirmationId, ConfirmationStore,
        Scope, TaskContext, Verdict,
    },
    AuditRecord, TaskBoard, TaskId, MAX_PROMPT_BYTES,
};

use agents::AgentPool;
use backends::{BackendChoice, BackendView, ModelCache, ProbeOutcome, Switched};
pub use conversation::{
    CompactPolicy, LeadStatus, QueuedFrom, QueuedMessage, SendMode, Sent, TurnOutcome,
};
use conversation::{Conversation, ConversationSpec};
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
    /// Every agent's file box (the lead agent's and its agents'; see [`crate::files`]).
    pub files: FileBoxes,
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
    /// Refused while something still depends on it (an agent that looks after apps).
    #[error("{0}")]
    InUse(String),
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
    files: FileBoxes,
    /// Runs the scheduled jobs, once [`Lead::start_schedules`] was called.
    scheduler: OnceLock<schedules::Scheduler>,
    /// The background task that removes expired files.
    prune: Mutex<Option<AbortHandle>>,
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
        let files = settings.files.clone();
        migrate_attachments(&settings.state_dir, &files, pool);
        // With the lead agent's current name, read when a session starts (a rename applies
        // from the next session; the open one gets a note, see `AgentPool::rename_lead`).
        let system_prompt = {
            let template = format!(
                "{}\n\n{}",
                settings.system_prompt.trim_end(),
                file_box_words(&files, &Principal::Lead)
            );
            let registry = pool.registry().clone();
            prompt::SystemPrompt::new(move || {
                prompt::with_lead_name(&template, &registry.lead_name())
            })
        };
        let spec = ConversationSpec {
            who: "the lead agent".into(),
            audit_kind: "lead",
            backends: settings.backends.clone(),
            choices: settings.backends.clone(),
            choice,
            failover: true,
            models: models.clone(),
            system_prompt,
            state_dir: settings.state_dir.clone(),
            workdir: settings.workdir.clone(),
            task,
            ctx,
            allow: allow.clone(),
            files: files.clone(),
            owner: Principal::Lead,
            compact: pool.limits().compact,
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
            lead: conv.clone(),
            workroot,
            files: files.clone(),
        });
        forward_confirmations(registry.confirmations(), events, pool.clone());
        let lead = Self {
            inner: Arc::new(Inner {
                conv,
                choice_path,
                server: link.server,
                board,
                pool: pool.clone(),
                backends: all_backends,
                models,
                probe_dir,
                files,
                scheduler: OnceLock::new(),
                prune: Mutex::new(None),
            }),
        };
        // An open chat page shows a box's new files at once (the file panel).
        let weak = Arc::downgrade(&lead.inner);
        lead.inner
            .files
            .on_change(Arc::new(move |owner: &Principal| {
                let Some(inner) = weak.upgrade() else { return };
                let log = match owner {
                    Principal::Lead => Some(inner.conv.events().clone()),
                    Principal::Agent(id) => {
                        inner.pool.conversation_of(id).map(|c| c.events().clone())
                    }
                };
                if let Some(log) = log {
                    log.push(LeadEvent::FilesChanged);
                }
            }));
        Ok(lead)
    }

    /// Every agent's file box.
    pub fn files(&self) -> &FileBoxes {
        &self.inner.files
    }

    /// Remove expired files now and then every `every` (the first run right away). Once;
    /// stopped by [`Lead::close`]. Must be called inside a Tokio runtime.
    pub fn start_file_prune(&self, every: Duration) {
        let mut slot = conversation::lock(&self.inner.prune);
        if slot.is_some() {
            return;
        }
        let files = self.inner.files.clone();
        let task = tokio::spawn(async move {
            loop {
                let f = files.clone();
                let removed = tokio::task::spawn_blocking(move || f.prune().len())
                    .await
                    .unwrap_or(0);
                if removed > 0 {
                    tracing::info!(removed, "removed expired files from the file boxes");
                }
                tokio::time::sleep(every).await;
            }
        });
        *slot = Some(task.abort_handle());
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

    /// Melissa's last turn ended in an error (this daemon run, until the next turn or a fresh
    /// start): the sidebar lists her under "Needs attention".
    pub fn failed(&self) -> bool {
        let conv = &self.inner.conv;
        conv.busy().is_none()
            && conv
                .last_outcome()
                .is_some_and(|o| o.stop_reason == "error" || o.error.is_some())
    }

    pub fn status(&self) -> LeadStatus {
        self.inner.conv.status()
    }

    /// What Melissa's conversation has used (turns, time, approvals, provider reports).
    pub fn usage(&self) -> usage::Usage {
        self.inner.conv.usage()
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

    /// The models (and reasoning levels) each backend reported, shared by every conversation.
    pub fn models(&self) -> &ModelCache {
        &self.inner.models
    }

    /// The user's saved choice of backend for the lead agent, if any.
    pub fn backend_choice(&self) -> Option<BackendChoice> {
        self.inner.conv.choice()
    }

    /// The user switches the lead agent to `backend` (and `model` and reasoning level
    /// `effort`, ones the backend reported). Saved in the state directory, audited
    /// `lead.backend`; applies at the next turn, with the conversation so far. Refused for a
    /// backend that is not usable now.
    pub fn use_backend(
        &self,
        backend: &str,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<Switched, LeadError> {
        self.use_backend_with(backend, model, effort, false)
    }

    /// [`Self::use_backend`] with fast mode (`fast`: only for a model that offers one).
    pub fn use_backend_with(
        &self,
        backend: &str,
        model: Option<&str>,
        effort: Option<&str>,
        fast: bool,
    ) -> Result<Switched, LeadError> {
        let choice = self.inner.conv.check_choice(backend, model, effort, fast)?;
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

    /// A user message for Melissa: starts a turn when none runs, else waits in the queue
    /// or (`mode` [`SendMode::Steer`]) goes into the running turn. See
    /// the conversation module's "One turn at a time, with a queue". Must be called inside a Tokio
    /// runtime.
    pub async fn submit(
        &self,
        text: &str,
        files: Vec<Upload>,
        mode: SendMode,
    ) -> Result<Sent, LeadError> {
        self.inner.conv.submit(text, files, "ui", mode).await
    }

    /// Melissa's queued messages, in order.
    pub fn queued(&self) -> Vec<QueuedMessage> {
        self.inner.conv.queued()
    }

    /// Take queued message `id` back (`edited`: into the composer). None: not queued.
    pub fn unqueue(&self, id: u64, edited: bool) -> Option<QueuedMessage> {
        self.inner.conv.unqueue(id, edited, "ui")
    }

    /// "Send now" for queued message `id` (into the running turn, or next). None: not queued.
    pub async fn steer(&self, id: u64) -> Option<Sent> {
        self.inner.conv.steer(id, "ui").await
    }

    /// Start a turn with a user message now: returns its id and the event sequence number
    /// to follow from (the `user_message` event comes right after it). [`LeadError::Busy`]
    /// while a turn runs ([`Lead::submit`] queues instead). Must be called inside a Tokio
    /// runtime.
    pub fn send(&self, text: &str) -> Result<(TurnId, u64), LeadError> {
        self.inner.conv.send(text, "ui")
    }

    /// [`Lead::send`] with files (validated and stored first; see [`crate::attachments`]).
    /// The text may be empty when there are files.
    pub fn send_with(&self, text: &str, files: Vec<Upload>) -> Result<(TurnId, u64), LeadError> {
        self.inner.conv.send_with(text, files, "ui")
    }

    /// Cancel the running turn. Returns false if no turn runs.
    pub async fn cancel(&self) -> bool {
        self.inner.conv.cancel(None, "ui").await
    }

    /// "Fresh start" for Melissa (the owner-approved way to clear its taint; one chat per
    /// agent since 2026-11-20, so the chat is not archived): withdraw its pending
    /// confirmations (audited `confirmation.withdrawn`), stop a running turn, close its agent
    /// session for good (the next message starts a new one without the earlier conversation)
    /// and clear the taint. The chat goes on, with a "Fresh start" divider. Audited
    /// `lead.fresh_start`. Agents keep their sessions and memory.
    pub async fn fresh_start(&self) -> Result<(), LeadError> {
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
                    format!("confirmation {}: the user cleared Melissa's memory", c.id),
                )
                .by("ui");
                if let Err(e) =
                    log.append(&record.with_tool(&c.tool, &crate::tools::args_hash(&c.args)))
                {
                    tracing::error!(path = %log.path().display(), "audit log write failed: {e}");
                }
            }
        }
        self.inner.conv.fresh_start("ui").await
    }

    /// Condense Melissa's context now, as when it fills up (see
    /// [`conversation::CompactPolicy`]). False if there was nothing to condense.
    pub fn compact(&self) -> bool {
        self.inner.conv.compact("ui")
    }

    /// Chats archived by "New chat" before 2026-11-20 (read-only), newest first.
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

    /// The longest a scheduled run's confirmation can wait on some usable backend that gives
    /// up on tool calls sooner than Melissa would (today only Claude Code; see
    /// [`schedules::effective_wait`]). None: no such backend, the owner's setting applies.
    pub fn scheduled_wait_cap(&self) -> Option<std::time::Duration> {
        self.inner
            .backends
            .iter()
            .filter(|b| b.not_ready().is_none())
            .filter_map(|b| b.tool_call_limit())
            .map(|limit| schedules::effective_wait(std::time::Duration::MAX, Some(limit)))
            .min()
    }

    /// Start running `schedules` in this lead agent's conversations (its own chat and its
    /// agents'). Once: later calls return the scheduler already running. Must be called inside
    /// a Tokio runtime.
    pub fn start_schedules(&self, schedules: schedules::Schedules) -> schedules::Scheduler {
        self.start_schedules_with(schedules, schedules::DEFAULT_GRACE)
    }

    /// [`Lead::start_schedules`] with a grace window for runs missed while Melissa was down.
    pub fn start_schedules_with(
        &self,
        schedules: schedules::Schedules,
        grace: std::time::Duration,
    ) -> schedules::Scheduler {
        self.inner
            .scheduler
            .get_or_init(|| {
                let runner = Arc::new(schedules::LeadRunner {
                    lead: Arc::downgrade(&self.inner),
                });
                // Deleting an agent removes its schedules too.
                self.inner.pool.attach_schedules(schedules.clone());
                let scheduler = schedules::Scheduler::new(schedules, runner, grace);
                scheduler.start();
                scheduler
            })
            .clone()
    }

    /// The scheduler, if schedules run on this device.
    pub fn scheduler(&self) -> Option<&schedules::Scheduler> {
        self.inner.scheduler.get()
    }

    /// Close the agent sessions of the lead agent and every agent (daemon shutdown).
    pub async fn close(&self) {
        if let Some(s) = self.scheduler() {
            s.stop();
        }
        if let Some(prune) = conversation::lock(&self.inner.prune).take() {
            prune.abort();
        }
        self.inner.pool.close_all().await;
        self.inner.conv.close().await;
    }
}

/// What an agent's system prompt says about its file box.
pub(crate) fn file_box_words(files: &FileBoxes, owner: &Principal) -> String {
    let dir = files
        .dir(owner)
        .map(|d| d.display().to_string())
        .unwrap_or_default();
    format!(
        "Your file box is {dir}/ (only you can see it; the user sees it too, in the files \
         panel next to your chat). Files the user sends you land there. Save files you want \
         to keep or hand to the user there: reports, exports, configs. Only plain files \
         directly in it count, no subfolders. Without file tools, use the `my_files` tool. \
         Files are removed after a while (the user's setting), so don't rely on them as \
         long-term storage. When the user writes `[file box: report.pdf]`, they mean that \
         file in your file box."
    )
}

/// Move the files of the old shared attachment store (`<lead state>/attachments`) into the
/// file boxes, once: each into the box of the conversation that sent it, as far as the
/// transcripts tell (the rest into the lead agent's box).
fn migrate_attachments(lead_state: &Path, files: &FileBoxes, pool: &AgentPool) {
    let old = lead_state.join("attachments");
    if !old.is_dir() {
        return;
    }
    let mut whose: HashMap<String, (Principal, String)> = HashMap::new();
    // Agents first: a file found in an agent's chat is that agent's.
    for agent in pool.registry_ids() {
        let dir = pool.agent_dir(&agent);
        collect_attachments(&dir, &Principal::Agent(agent), &mut whose);
    }
    collect_attachments(lead_state, &Principal::Lead, &mut whose);
    match files.migrate_attachments(&old, &whose) {
        Ok(0) => {}
        Ok(n) => tracing::info!(
            files = n,
            "moved earlier chat attachments into the file boxes"
        ),
        Err(e) => tracing::warn!("moving earlier chat attachments into the file boxes: {e}"),
    }
}

/// Every attachment named in the transcripts (`*.jsonl*`) under `dir` (and its `chats/`),
/// recorded for `owner` unless already claimed.
fn collect_attachments(
    dir: &Path,
    owner: &Principal,
    whose: &mut HashMap<String, (Principal, String)>,
) {
    let mut paths = Vec::new();
    for d in [dir.to_path_buf(), dir.join("chats")] {
        if let Ok(entries) = std::fs::read_dir(&d) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.contains(".jsonl") {
                    paths.push(e.path());
                }
            }
        }
    }
    for path in paths {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let Some(list) = v.get("attachments").and_then(|a| a.as_array()) else {
                continue;
            };
            for a in list {
                if let (Some(file), Some(name)) = (
                    a.get("file").and_then(|f| f.as_str()),
                    a.get("name").and_then(|n| n.as_str()),
                ) {
                    whose
                        .entry(file.to_string())
                        .or_insert_with(|| (owner.clone(), name.to_string()));
                }
            }
        }
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
