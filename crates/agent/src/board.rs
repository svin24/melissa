//! The task board: dispatches prompts to backends and tracks their progress.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use tokio::{sync::Notify, task::AbortHandle};

use crate::{
    backend::McpEndpoint,
    mcp::McpServer,
    tools::{Scope, TaskContext},
    AgentError, AuditLog, AuditRecord, Backend, BackendInfo, EventSink, OutputLine, RunRequest,
};

pub type TaskId = u64;

/// Prompts larger than this are rejected.
pub const MAX_PROMPT_BYTES: usize = 16 * 1024;
/// Output kept per task; later output is dropped and the task is marked truncated.
const MAX_OUTPUT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_finished(self) -> bool {
        self != Self::Running
    }
}

/// A point-in-time copy of a task, safe to render.
#[derive(Debug, Clone)]
pub struct TaskSnapshot {
    pub id: TaskId,
    pub backend: String,
    pub prompt: String,
    pub status: TaskStatus,
    pub output: Vec<OutputLine>,
    pub truncated: bool,
    pub error: Option<String>,
    pub elapsed: Duration,
    /// The task that started this one (workers: the Queen).
    pub parent: Option<TaskId>,
    /// The task's MCP session read untrusted content (known once the task finished).
    pub tainted: bool,
}

/// How to run a task. [`Default`]: started by a person in the UI, who watches it.
#[derive(Debug, Clone)]
pub struct DispatchOptions {
    /// Policy context of the task's MCP session.
    pub ctx: TaskContext,
    /// Tools the task may see (None = every tool its context allows).
    pub allow: Option<Vec<&'static str>>,
    /// Run time limit; None = the board's default.
    pub timeout: Option<Duration>,
    /// The task that started this one.
    pub parent: Option<TaskId>,
    /// Who started it, for the audit log (`ui`, `queen`).
    pub actor: String,
}

impl Default for DispatchOptions {
    fn default() -> Self {
        Self {
            ctx: TaskContext {
                attended: true,
                // Started and watched by the owner: acts for them; changing their apps
                // still needs a confirmation that says so.
                scope: Scope::ForOwner {
                    agent: "an agent task".into(),
                },
                ..TaskContext::default()
            },
            allow: None,
            timeout: None,
            parent: None,
            actor: "ui".into(),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DispatchError {
    #[error("unknown backend `{0}`")]
    UnknownBackend(String),
    #[error("backend `{0}` is not available on this comb")]
    Unavailable(String),
    #[error("prompt is empty")]
    EmptyPrompt,
    #[error("prompt is longer than {MAX_PROMPT_BYTES} bytes")]
    PromptTooLong,
    #[error("no such task")]
    NoSuchTask,
    #[error("could not prepare the task: {0}")]
    Setup(String),
}

/// Gives tool-capable backends Melissa's tools through the MCP endpoint.
#[derive(Clone)]
pub struct McpLink {
    pub server: McpServer,
    /// URL prefix of the MCP endpoint; the session token is appended (e.g. `http://127.0.0.1:8080/mcp/`).
    pub base_url: String,
    /// Directory under which each task gets its own working directory (`task-<id>`).
    pub workroot: PathBuf,
}

struct Task {
    backend: String,
    prompt: String,
    status: TaskStatus,
    output: Vec<OutputLine>,
    output_bytes: usize,
    truncated: bool,
    error: Option<String>,
    started: Instant,
    finished: Option<Instant>,
    abort: Option<AbortHandle>,
    /// Open MCP session for this task, closed when it finishes or is cancelled.
    mcp_token: Option<String>,
    parent: Option<TaskId>,
    tainted: bool,
    /// Who started the task (audit actor of its records).
    actor: String,
}

struct Inner {
    backends: Vec<Arc<dyn Backend>>,
    tasks: Mutex<BTreeMap<TaskId, Task>>,
    next_id: Mutex<TaskId>,
    timeout: Duration,
    audit: Option<AuditLog>,
    mcp: Option<McpLink>,
    /// Woken whenever a task finishes or is cancelled.
    finished: Notify,
}

/// Shared handle to the task board. Cheap to clone.
#[derive(Clone)]
pub struct TaskBoard {
    inner: Arc<Inner>,
}

impl TaskBoard {
    pub fn new(
        backends: Vec<Arc<dyn Backend>>,
        timeout: Duration,
        audit: Option<AuditLog>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                backends,
                tasks: Mutex::new(BTreeMap::new()),
                next_id: Mutex::new(1),
                timeout,
                audit,
                mcp: None,
                finished: Notify::new(),
            }),
        }
    }

    /// Like [`TaskBoard::new`], with Melissa's tools for backends that support them.
    pub fn with_tools(
        backends: Vec<Arc<dyn Backend>>,
        timeout: Duration,
        audit: Option<AuditLog>,
        mcp: McpLink,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                backends,
                tasks: Mutex::new(BTreeMap::new()),
                next_id: Mutex::new(1),
                timeout,
                audit,
                mcp: Some(mcp),
                finished: Notify::new(),
            }),
        }
    }

    pub fn backends(&self) -> Vec<BackendInfo> {
        self.inner.backends.iter().map(|b| b.info()).collect()
    }

    pub fn backend(&self, name: &str) -> Option<BackendInfo> {
        self.inner
            .backends
            .iter()
            .map(|b| b.info())
            .find(|i| i.name == name)
    }

    /// Melissa's MCP endpoint, if this board gives backends tools.
    pub fn mcp_link(&self) -> Option<&McpLink> {
        self.inner.mcp.as_ref()
    }

    pub fn audit_log(&self) -> Option<&AuditLog> {
        self.inner.audit.as_ref()
    }

    /// Take a task id without starting a task (e.g. for the Queen's session, so its audit
    /// records and confirmations have an id no board task shares).
    pub fn reserve_id(&self) -> TaskId {
        let mut next = lock(&self.inner.next_id);
        let id = *next;
        *next += 1;
        id
    }

    /// Start a task started by a person in the UI. Must be called from within a Tokio runtime.
    pub fn dispatch(&self, backend: &str, prompt: &str) -> Result<TaskId, DispatchError> {
        self.dispatch_with(backend, prompt, DispatchOptions::default())
    }

    /// Start a task with explicit options. Must be called from within a Tokio runtime.
    pub fn dispatch_with(
        &self,
        backend: &str,
        prompt: &str,
        opts: DispatchOptions,
    ) -> Result<TaskId, DispatchError> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err(DispatchError::EmptyPrompt);
        }
        if prompt.len() > MAX_PROMPT_BYTES {
            return Err(DispatchError::PromptTooLong);
        }
        let backend_impl = self
            .inner
            .backends
            .iter()
            .find(|b| b.info().name == backend)
            .cloned()
            .ok_or_else(|| DispatchError::UnknownBackend(backend.to_string()))?;
        let info = backend_impl.info();
        if !info.available {
            return Err(DispatchError::Unavailable(backend.to_string()));
        }

        let id = self.reserve_id();
        lock(&self.inner.tasks).insert(
            id,
            Task {
                backend: backend.to_string(),
                prompt: prompt.to_string(),
                status: TaskStatus::Running,
                output: Vec::new(),
                output_bytes: 0,
                truncated: false,
                error: None,
                started: Instant::now(),
                finished: None,
                abort: None,
                mcp_token: None,
                parent: opts.parent,
                tainted: false,
                actor: opts.actor.clone(),
            },
        );
        let mut record = AuditRecord::now("task.dispatch", id, backend, prompt).by(&opts.actor);
        record.parent = opts.parent;
        self.audit(record);

        let mut req = RunRequest::prompt(prompt);
        req.task = Some(id);
        if let (Some(link), true) = (&self.inner.mcp, info.tools) {
            match self.prepare_tools(link, id, opts.ctx, opts.allow) {
                Ok((token, workdir, config)) => {
                    if let Some(task) = lock(&self.inner.tasks).get_mut(&id) {
                        task.mcp_token = Some(token.clone());
                    }
                    req.workdir = Some(workdir);
                    req.mcp_config = Some(config);
                    req.mcp = Some(McpEndpoint {
                        url: format!("{}{token}", link.base_url),
                        token,
                    });
                }
                Err(e) => {
                    let err = DispatchError::Setup(e.to_string());
                    self.finish(id, Err(AgentError::Io(e)));
                    return Err(err);
                }
            }
        }

        let board = self.clone();
        let timeout = opts.timeout.unwrap_or(self.inner.timeout);
        let handle = tokio::spawn(async move {
            let (sink, mut rx) = EventSink::channel();
            let run = async {
                // The sink is moved in so the channel closes when the backend finishes.
                let sink = sink;
                tokio::time::timeout(timeout, backend_impl.run(&req, sink))
                    .await
                    .unwrap_or(Err(AgentError::Timeout(timeout)))
            };
            let collect = async {
                while let Some(line) = rx.recv().await {
                    board.append(id, line);
                }
            };
            let (result, ()) = tokio::join!(run, collect);
            board.finish(id, result);
        });
        if let Some(task) = lock(&self.inner.tasks).get_mut(&id) {
            if task.status == TaskStatus::Running {
                task.abort = Some(handle.abort_handle());
            }
        }
        Ok(id)
    }

    /// Cancel a running task (from the UI). Cancelling a finished task is a no-op.
    pub fn cancel(&self, id: TaskId) -> Result<(), DispatchError> {
        self.cancel_by(id, "ui")
    }

    /// Cancel a running task on behalf of `actor` (audited as such).
    pub fn cancel_by(&self, id: TaskId, actor: &str) -> Result<(), DispatchError> {
        let backend = {
            let mut tasks = lock(&self.inner.tasks);
            let task = tasks.get_mut(&id).ok_or(DispatchError::NoSuchTask)?;
            if task.status.is_finished() {
                return Ok(());
            }
            if let Some(abort) = task.abort.take() {
                abort.abort();
            }
            task.status = TaskStatus::Cancelled;
            task.finished = Some(Instant::now());
            self.close_tools(task);
            task.backend.clone()
        };
        self.inner.finished.notify_waiters();
        self.audit(AuditRecord::now("task.cancel", id, &backend, "").by(actor));
        Ok(())
    }

    /// Wait until task `id` has finished, at most `timeout`. Returns its snapshot (with
    /// output) either way, or `None` for an unknown task.
    pub async fn wait(&self, id: TaskId, timeout: Duration) -> Option<TaskSnapshot> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Register for the wake-up before checking, so a finish in between is not missed.
            let notified = self.inner.finished.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let snap = self.get(id)?;
            if snap.status.is_finished() {
                return Some(snap);
            }
            tokio::select! {
                () = notified => {}
                () = tokio::time::sleep_until(deadline) => return self.get(id),
            }
        }
    }

    /// All tasks, newest first, without their output.
    pub fn list(&self) -> Vec<TaskSnapshot> {
        lock(&self.inner.tasks)
            .iter()
            .rev()
            .map(|(id, t)| snapshot(*id, t, false))
            .collect()
    }

    pub fn get(&self, id: TaskId) -> Option<TaskSnapshot> {
        lock(&self.inner.tasks)
            .get(&id)
            .map(|t| snapshot(id, t, true))
    }

    fn append(&self, id: TaskId, line: OutputLine) {
        let mut tasks = lock(&self.inner.tasks);
        let Some(task) = tasks.get_mut(&id) else {
            return;
        };
        if task.status.is_finished() || task.truncated {
            return;
        }
        let len = line.text.len() + 1;
        if task.output_bytes + len > MAX_OUTPUT_BYTES {
            task.truncated = true;
            return;
        }
        task.output_bytes += len;
        task.output.push(line);
    }

    fn finish(&self, id: TaskId, result: Result<(), AgentError>) {
        let (backend, status, actor) = {
            let mut tasks = lock(&self.inner.tasks);
            let Some(task) = tasks.get_mut(&id) else {
                return;
            };
            if task.status.is_finished() {
                return; // already cancelled
            }
            task.status = match &result {
                Ok(()) => TaskStatus::Succeeded,
                Err(_) => TaskStatus::Failed,
            };
            task.error = result.as_ref().err().map(ToString::to_string);
            task.finished = Some(Instant::now());
            task.abort = None;
            self.close_tools(task);
            (task.backend.clone(), task.status, task.actor.clone())
        };
        self.inner.finished.notify_waiters();
        let detail = match &result {
            Ok(()) => status.as_str().to_string(),
            Err(e) => format!("{}: {e}", status.as_str()),
        };
        self.audit(AuditRecord::now("task.finish", id, &backend, detail).by(&actor));
    }

    /// Open an MCP session for task `id` and write its per-task workdir and MCP config.
    /// The config holds the session token, so it is created mode 0600.
    fn prepare_tools(
        &self,
        link: &McpLink,
        id: TaskId,
        ctx: TaskContext,
        allow: Option<Vec<&'static str>>,
    ) -> std::io::Result<(String, PathBuf, PathBuf)> {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let workdir = link.workroot.join(format!("task-{id}"));
        std::fs::create_dir_all(&workdir)?;
        let token = link.server.open_session(id, ctx, allow)?;
        let config = workdir.join("mcp.json");
        let body = serde_json::json!({
            "mcpServers": {"melissa": {"type": "http", "url": format!("{}{token}", link.base_url)}}
        });
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&config)?;
        file.write_all(body.to_string().as_bytes())?;
        Ok((token, workdir, config))
    }

    /// Close the task's MCP session (its token stops working). Caller holds the tasks lock.
    fn close_tools(&self, task: &mut Task) {
        if let (Some(link), Some(token)) = (&self.inner.mcp, task.mcp_token.take()) {
            task.tainted |= link
                .server
                .session_context(&token)
                .is_some_and(|c| c.tainted);
            link.server.close_session(&token);
        }
    }

    fn audit(&self, record: AuditRecord) {
        if let Some(log) = &self.inner.audit {
            if let Err(e) = log.append(&record) {
                tracing::error!(path = %log.path().display(), "audit log write failed: {e}");
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // Poisoning only means another thread panicked mid-update; the data is still usable.
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn snapshot(id: TaskId, t: &Task, with_output: bool) -> TaskSnapshot {
    let end = t.finished.unwrap_or_else(Instant::now);
    TaskSnapshot {
        id,
        backend: t.backend.clone(),
        prompt: t.prompt.clone(),
        status: t.status,
        output: if with_output {
            t.output.clone()
        } else {
            Vec::new()
        },
        truncated: t.truncated,
        error: t.error.clone(),
        elapsed: end.saturating_duration_since(t.started),
        parent: t.parent,
        tainted: t.tainted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BoxFuture, EchoBackend, Stream};

    /// A backend that never finishes on its own.
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
        fn run<'a>(
            &'a self,
            _: &'a RunRequest,
            sink: EventSink,
        ) -> BoxFuture<'a, Result<(), AgentError>> {
            Box::pin(async move {
                sink.stdout("started");
                std::future::pending().await
            })
        }
    }

    fn board(timeout: Duration) -> TaskBoard {
        TaskBoard::new(
            vec![Arc::new(EchoBackend::new()), Arc::new(Hang)],
            timeout,
            None,
        )
    }

    async fn wait_until(
        board: &TaskBoard,
        id: TaskId,
        pred: impl Fn(&TaskSnapshot) -> bool,
    ) -> TaskSnapshot {
        for _ in 0..200 {
            let snap = board.get(id).unwrap();
            if pred(&snap) {
                return snap;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("condition not reached: {:?}", board.get(id));
    }

    #[tokio::test]
    async fn echo_task_succeeds_with_output() {
        let board = board(Duration::from_secs(5));
        let id = board.dispatch("echo", "  hello\nhive ").unwrap();
        let snap = wait_until(&board, id, |s| s.status.is_finished()).await;
        assert_eq!(snap.status, TaskStatus::Succeeded);
        let text: Vec<_> = snap.output.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(text, ["echo: hello", "echo: hive"]);
        assert!(snap.output.iter().all(|l| l.stream == Stream::Stdout));
    }

    #[tokio::test]
    async fn rejects_bad_dispatches() {
        let board = board(Duration::from_secs(5));
        assert_eq!(
            board.dispatch("echo", "   "),
            Err(DispatchError::EmptyPrompt)
        );
        assert_eq!(
            board.dispatch("nope", "hi"),
            Err(DispatchError::UnknownBackend("nope".into()))
        );
        let long = "x".repeat(MAX_PROMPT_BYTES + 1);
        assert_eq!(
            board.dispatch("echo", &long),
            Err(DispatchError::PromptTooLong)
        );
        assert!(board.list().is_empty());
    }

    #[tokio::test]
    async fn cancel_stops_a_running_task() {
        let board = board(Duration::from_secs(60));
        let id = board.dispatch("hang", "wait").unwrap();
        wait_until(&board, id, |s| !s.output.is_empty()).await;
        board.cancel(id).unwrap();
        let snap = board.get(id).unwrap();
        assert_eq!(snap.status, TaskStatus::Cancelled);
        assert_eq!(board.cancel(999), Err(DispatchError::NoSuchTask));
    }

    #[tokio::test]
    async fn tool_capable_tasks_get_an_mcp_session_until_cancelled() {
        use std::os::unix::fs::PermissionsExt;
        let workroot =
            std::env::temp_dir().join(format!("melissa-board-mcp-{}", std::process::id()));
        let server = McpServer::new(crate::tools::ToolRegistry::builtin(None));
        let board = TaskBoard::with_tools(
            vec![Arc::new(Hang), Arc::new(EchoBackend::new())],
            Duration::from_secs(60),
            None,
            McpLink {
                server: server.clone(),
                base_url: "http://127.0.0.1:8080/mcp/".into(),
                workroot: workroot.clone(),
            },
        );
        let id = board.dispatch("hang", "use tools").unwrap();
        let config = workroot.join(format!("task-{id}/mcp.json"));
        let body: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        let url = body["mcpServers"]["melissa"]["url"]
            .as_str()
            .unwrap()
            .to_string();
        let token = url.strip_prefix("http://127.0.0.1:8080/mcp/").unwrap();
        assert!(server.has_session(token));
        assert_eq!(
            std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
            0o600
        );
        board.cancel(id).unwrap();
        assert!(
            !server.has_session(token),
            "cancel must close the MCP session"
        );

        // Backends without tool support get no session or per-task dir.
        let echo = board.dispatch("echo", "plain").unwrap();
        assert!(!workroot.join(format!("task-{echo}")).exists());
        std::fs::remove_dir_all(workroot).unwrap();
    }

    #[tokio::test]
    async fn dispatch_options_and_wait() {
        let path =
            std::env::temp_dir().join(format!("melissa-board-opts-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let board = TaskBoard::new(
            vec![Arc::new(EchoBackend::new()), Arc::new(Hang)],
            Duration::from_secs(60),
            Some(AuditLog::new(&path)),
        );
        let queen = board.reserve_id();
        let opts = DispatchOptions {
            timeout: Some(Duration::from_millis(50)),
            parent: Some(queen),
            actor: "queen".into(),
            ..DispatchOptions::default()
        };
        let id = board.dispatch_with("hang", "wait", opts).unwrap();
        assert!(id > queen);
        // Not finished within 1 ms; then the per-task timeout (50 ms) ends it.
        let snap = board.wait(id, Duration::from_millis(1)).await.unwrap();
        assert_eq!(snap.status, TaskStatus::Running);
        let snap = board.wait(id, Duration::from_secs(5)).await.unwrap();
        assert_eq!(snap.status, TaskStatus::Failed);
        assert_eq!(snap.parent, Some(queen));
        assert!(board.wait(999, Duration::ZERO).await.is_none());
        let audit = std::fs::read_to_string(&path).unwrap();
        let line: serde_json::Value = serde_json::from_str(audit.lines().next().unwrap()).unwrap();
        assert_eq!(line["action"], "task.dispatch");
        assert_eq!(line["actor"], "queen");
        assert_eq!(line["parent"], queen);
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn timeout_fails_the_task() {
        let board = board(Duration::from_millis(50));
        let id = board.dispatch("hang", "wait").unwrap();
        let snap = wait_until(&board, id, |s| s.status.is_finished()).await;
        assert_eq!(snap.status, TaskStatus::Failed);
        assert!(snap.error.unwrap().contains("timed out"));
    }

    #[tokio::test]
    async fn list_is_newest_first_and_audited() {
        let path =
            std::env::temp_dir().join(format!("melissa-board-audit-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let board = TaskBoard::new(
            vec![Arc::new(EchoBackend::new())],
            Duration::from_secs(5),
            Some(AuditLog::new(&path)),
        );
        let a = board.dispatch("echo", "a").unwrap();
        let b = board.dispatch("echo", "b").unwrap();
        wait_until(&board, b, |s| s.status.is_finished()).await;
        wait_until(&board, a, |s| s.status.is_finished()).await;
        let ids: Vec<_> = board.list().iter().map(|t| t.id).collect();
        assert_eq!(ids, [b, a]);
        let audit = std::fs::read_to_string(&path).unwrap();
        assert_eq!(audit.matches("task.dispatch").count(), 2);
        assert_eq!(audit.matches("task.finish").count(), 2);
        std::fs::remove_file(&path).unwrap();
    }
}
