//! Worker agents: sandboxed agents the Queen starts for concrete jobs.
//!
//! A worker is a task on the [`TaskBoard`] whose parent is the Queen. It runs unattended
//! (reversible actions need a human), with at most the Queen's own tools (or a subset the
//! Queen names), never the worker tools themselves (depth 1), under a time limit no longer
//! than the configured maximum. At most `max_concurrent` workers run at once. The tools here
//! are Queen-only ([`Tool::queen_only`]): workers and UI tasks never see them.
//!
//! Taint flows both ways: a worker started by a tainted Queen starts tainted, and the Queen
//! becomes tainted when it reads the output of a worker that read untrusted content.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use serde_json::{json, Value};

use super::{
    events::{EventLog, QueenEvent},
    transcript::shorten,
};
use crate::{
    tools::{check_tool_set, Effect, Labels, TaskContext, Tool, ToolError, ToolSpec},
    BoxFuture, DispatchOptions, Stream, TaskBoard, TaskId, TaskSnapshot, TaskStatus,
    MAX_PROMPT_BYTES,
};

/// Limits for worker agents (`[queen]` in the queen config).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerLimits {
    /// Workers running at the same time.
    pub max_concurrent: usize,
    /// Time limit when the Queen gives none.
    pub default_timeout: Duration,
    /// Largest time limit the Queen may ask for.
    pub max_timeout: Duration,
}

impl Default for WorkerLimits {
    fn default() -> Self {
        Self {
            max_concurrent: 2,
            default_timeout: Duration::from_secs(300),
            max_timeout: Duration::from_secs(900),
        }
    }
}

/// Longest `wait_secs` for `worker_status` (agents' MCP clients time out tool calls).
const MAX_WAIT_SECS: u64 = 50;
/// Output lines returned by `worker_status`.
const TAIL_LINES: usize = 30;
/// Characters of a finished worker's reply returned by `worker_status`.
const RESULT_CHARS: usize = 8000;

struct Attached {
    board: TaskBoard,
    default_backend: String,
    preamble: String,
    /// The Queen's own tool allowlist (None = every tool).
    queen_allow: Option<Vec<&'static str>>,
}

struct PoolInner {
    limits: WorkerLimits,
    events: EventLog,
    attached: OnceLock<Attached>,
    /// Goal text per worker (the board keeps the full prompt with the preamble).
    goals: Mutex<HashMap<TaskId, String>>,
    /// Serialises the concurrency check and the dispatch.
    start: Mutex<()>,
}

/// Starts and tracks worker agents. Cheap to clone.
#[derive(Clone)]
pub struct WorkerPool {
    inner: Arc<PoolInner>,
}

impl WorkerPool {
    pub fn new(limits: WorkerLimits, events: EventLog) -> Self {
        Self {
            inner: Arc::new(PoolInner {
                limits,
                events,
                attached: OnceLock::new(),
                goals: Mutex::new(HashMap::new()),
                start: Mutex::new(()),
            }),
        }
    }

    pub fn limits(&self) -> &WorkerLimits {
        &self.inner.limits
    }

    /// The four worker tools, to register in the [`crate::tools::ToolRegistry`].
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(StartWorker(self.clone())),
            Arc::new(WorkerStatus(self.clone())),
            Arc::new(ListWorkers(self.clone())),
            Arc::new(CancelWorker(self.clone())),
        ]
    }

    /// Connect the pool to the board (done by [`super::Queen::new`]; the board needs the tool
    /// registry first, so this cannot be a constructor argument). Later calls are ignored.
    pub(crate) fn attach(
        &self,
        board: TaskBoard,
        default_backend: String,
        preamble: String,
        queen_allow: Option<Vec<&'static str>>,
    ) {
        let _ = self.inner.attached.set(Attached {
            board,
            default_backend,
            preamble,
            queen_allow,
        });
    }

    fn attached(&self) -> Result<&Attached, ToolError> {
        self.inner
            .attached
            .get()
            .ok_or_else(|| ToolError::Failed("workers are not available on this bee".into()))
    }

    /// Tools a worker may get: the Queen's own, without the Queen-only ones.
    fn handable(&self, a: &Attached) -> Vec<(&'static str, Labels)> {
        let Some(link) = a.board.mcp_link() else {
            return Vec::new();
        };
        link.server
            .registry()
            .visible_to(&TaskContext::default(), a.queen_allow.as_deref())
            .into_iter()
            .map(|s| (s.name, s.labels))
            .collect()
    }

    /// Workers of `parent`, newest first.
    pub fn list(&self, parent: TaskId) -> Vec<TaskSnapshot> {
        match self.inner.attached.get() {
            Some(a) => a
                .board
                .list()
                .into_iter()
                .filter(|t| t.parent == Some(parent))
                .collect(),
            None => Vec::new(),
        }
    }

    pub fn goal(&self, worker: TaskId) -> Option<String> {
        lock(&self.inner.goals).get(&worker).cloned()
    }

    fn worker(&self, a: &Attached, parent: TaskId, id: TaskId) -> Result<TaskSnapshot, ToolError> {
        a.board
            .get(id)
            .filter(|t| t.parent == Some(parent))
            .ok_or_else(|| ToolError::BadArgs(format!("no worker {id}")))
    }

    fn start(&self, parent: TaskId, ctx: &TaskContext, args: &Value) -> Result<Value, ToolError> {
        let a = self.attached()?;
        let args = object(args, &["goal", "backend", "tools", "timeout_secs"])?;
        let goal = args
            .get("goal")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .ok_or_else(|| ToolError::BadArgs("`goal` must be a non-empty string".into()))?;
        let prompt = format!("{}\n\n## Your goal\n\n{goal}", a.preamble.trim_end());
        if prompt.len() > MAX_PROMPT_BYTES {
            return Err(ToolError::BadArgs(format!(
                "`goal` is too long (at most about {} bytes)",
                MAX_PROMPT_BYTES.saturating_sub(a.preamble.len() + 20)
            )));
        }

        let limits = &self.inner.limits;
        let max = limits.max_timeout.as_secs();
        let timeout = match args.get("timeout_secs") {
            None | Some(Value::Null) => limits.default_timeout.min(limits.max_timeout),
            Some(v) => match v.as_u64() {
                Some(s) if (1..=max).contains(&s) => Duration::from_secs(s),
                _ => {
                    return Err(ToolError::BadArgs(format!(
                        "`timeout_secs` must be a whole number from 1 to {max}"
                    )))
                }
            },
        };

        let handable = self.handable(a);
        let tools: Vec<(&'static str, Labels)> = match args.get("tools") {
            None | Some(Value::Null) => handable.clone(),
            Some(Value::Array(names)) => {
                let mut out: Vec<(&'static str, Labels)> = Vec::new();
                for n in names {
                    let n = n.as_str().ok_or_else(|| {
                        ToolError::BadArgs("`tools` must be a list of tool names".into())
                    })?;
                    let Some(t) = handable.iter().find(|(h, _)| *h == n) else {
                        let allowed: Vec<&str> = handable.iter().map(|(h, _)| *h).collect();
                        return Err(ToolError::BadArgs(format!(
                            "tool `{n}` cannot be given to a worker; allowed: {}",
                            allowed.join(", ")
                        )));
                    };
                    if !out.iter().any(|(o, _)| *o == t.0) {
                        out.push(*t);
                    }
                }
                out
            }
            Some(_) => {
                return Err(ToolError::BadArgs(
                    "`tools` must be a list of tool names".into(),
                ))
            }
        };
        check_tool_set(tools.iter().map(|(_, l)| l), false).map_err(ToolError::BadArgs)?;

        let backend = match args.get("backend") {
            None | Some(Value::Null) => a.default_backend.clone(),
            Some(Value::String(b)) => b.clone(),
            Some(_) => return Err(ToolError::BadArgs("`backend` must be a string".into())),
        };
        let info = a
            .board
            .backend(&backend)
            .ok_or_else(|| ToolError::BadArgs(format!("unknown backend `{backend}`")))?;
        if !info.available {
            return Err(ToolError::Failed(format!(
                "backend `{backend}` is not available on this bee"
            )));
        }

        let id = {
            let _start = lock(&self.inner.start);
            let running = a
                .board
                .list()
                .iter()
                .filter(|t| t.parent.is_some() && t.status == TaskStatus::Running)
                .count();
            if running >= limits.max_concurrent {
                return Err(ToolError::Failed(format!(
                    "{running} workers are already running (at most {}); wait for one to finish \
                     or cancel one",
                    limits.max_concurrent
                )));
            }
            let opts = DispatchOptions {
                ctx: TaskContext {
                    attended: false,
                    tainted: ctx.tainted,
                    ..TaskContext::default()
                },
                allow: Some(tools.iter().map(|(n, _)| *n).collect()),
                timeout: Some(timeout),
                parent: Some(parent),
                actor: "queen".into(),
            };
            a.board
                .dispatch_with(&backend, &prompt, opts)
                .map_err(|e| ToolError::Failed(e.to_string()))?
        };
        lock(&self.inner.goals).insert(id, goal.to_string());
        self.inner.events.push(QueenEvent::WorkerStarted {
            worker: id,
            backend: backend.clone(),
            goal: shorten(goal, 200),
        });
        let (board, events) = (a.board.clone(), self.inner.events.clone());
        tokio::spawn(async move {
            loop {
                match board.wait(id, Duration::from_secs(3600)).await {
                    Some(snap) if snap.status.is_finished() => {
                        events.push(QueenEvent::WorkerFinished {
                            worker: id,
                            status: snap.status.as_str().into(),
                            error: snap.error,
                        });
                        break;
                    }
                    Some(_) => continue,
                    None => break,
                }
            }
        });
        Ok(json!({
            "worker": id,
            "backend": backend,
            "tools": tools.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            "timeout_secs": timeout.as_secs(),
            "status": "running",
        }))
    }

    async fn status(
        &self,
        parent: TaskId,
        ctx: &mut TaskContext,
        args: &Value,
    ) -> Result<Value, ToolError> {
        let a = self.attached()?;
        let args = object(args, &["worker", "wait_secs"])?;
        let id = worker_id(&args)?;
        let wait = match args.get("wait_secs") {
            None | Some(Value::Null) => 0,
            Some(v) => v.as_u64().filter(|w| *w <= MAX_WAIT_SECS).ok_or_else(|| {
                ToolError::BadArgs(format!(
                    "`wait_secs` must be a whole number from 0 to {MAX_WAIT_SECS}"
                ))
            })?,
        };
        self.worker(a, parent, id)?;
        let snap = if wait > 0 {
            a.board.wait(id, Duration::from_secs(wait)).await
        } else {
            a.board.get(id)
        }
        .ok_or_else(|| ToolError::BadArgs(format!("no worker {id}")))?;
        if snap.tainted {
            // The worker read untrusted content; so does whoever reads its output.
            ctx.tainted = true;
        }
        let tail: Vec<String> = snap
            .output
            .iter()
            .rev()
            .take(TAIL_LINES)
            .rev()
            .map(|l| shorten(&l.text, 300))
            .collect();
        let result = snap.status.is_finished().then(|| {
            let reply: Vec<&str> = snap
                .output
                .iter()
                .filter(|l| l.stream == Stream::Stdout)
                .map(|l| l.text.as_str())
                .collect();
            let reply = reply.join("\n");
            let skip = reply.chars().count().saturating_sub(RESULT_CHARS);
            reply.chars().skip(skip).collect::<String>()
        });
        Ok(json!({
            "worker": id,
            "backend": snap.backend,
            "goal": self.goal(id),
            "status": snap.status.as_str(),
            "elapsed_secs": snap.elapsed.as_secs(),
            "error": snap.error,
            "output_tail": tail,
            "output_truncated": snap.truncated,
            "result": result,
            "read_untrusted_content": snap.tainted,
        }))
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The arguments as an object with only the `known` keys (null = `{}`).
fn object(args: &Value, known: &[&str]) -> Result<serde_json::Map<String, Value>, ToolError> {
    let map = match args {
        Value::Null => serde_json::Map::new(),
        Value::Object(m) => m.clone(),
        _ => return Err(ToolError::BadArgs("arguments must be an object".into())),
    };
    if let Some(k) = map.keys().find(|k| !known.contains(&k.as_str())) {
        return Err(ToolError::BadArgs(format!("unknown argument `{k}`")));
    }
    Ok(map)
}

fn worker_id(args: &serde_json::Map<String, Value>) -> Result<TaskId, ToolError> {
    args.get("worker")
        .and_then(Value::as_u64)
        .ok_or_else(|| ToolError::BadArgs("`worker` must be a worker id".into()))
}

/// Starting a worker spends money and lets an agent act (unattended): reversible. Attended
/// Queen sessions may do it without asking; a tainted Queen needs a human.
const START_LABELS: Labels = Labels {
    effect: Effect::Reversible,
    ..Labels::READ_ONLY
};

struct StartWorker(WorkerPool);
struct WorkerStatus(WorkerPool);
struct ListWorkers(WorkerPool);
struct CancelWorker(WorkerPool);

fn needs_context<'a>() -> BoxFuture<'a, Result<Value, ToolError>> {
    Box::pin(async { Err(ToolError::Failed("worker tools need a caller".into())) })
}

impl Tool for StartWorker {
    fn spec(&self) -> ToolSpec {
        let l = self.0.limits();
        ToolSpec {
            name: "start_worker",
            description: "Start a sandboxed worker agent for one concrete job (a multi-step check, \
                          a longer investigation). It works unattended with your tools or the subset \
                          in `tools`, cannot start workers itself, and is stopped after \
                          `timeout_secs`. Returns its worker id; then use worker_status.",
            labels: START_LABELS,
            params: json!({
                "type": "object",
                "properties": {
                    "goal": {"type": "string", "description": "What the worker should do and report back. Self-contained: the worker does not see this conversation."},
                    "backend": {"type": "string", "description": "Agent backend (default: the configured worker backend)."},
                    "tools": {"type": "array", "items": {"type": "string"}, "description": "Tools the worker may use (default: all of yours except the worker tools)."},
                    "timeout_secs": {"type": "integer", "minimum": 1, "maximum": l.max_timeout.as_secs(), "description": format!("Time limit (default {}).", l.default_timeout.min(l.max_timeout).as_secs())},
                },
                "required": ["goal"],
                "additionalProperties": false,
            }),
        }
    }
    fn call<'a>(&'a self, _: Value) -> BoxFuture<'a, Result<Value, ToolError>> {
        needs_context()
    }
    fn call_in<'a>(
        &'a self,
        task: TaskId,
        ctx: &'a mut TaskContext,
        args: Value,
    ) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(async move { self.0.start(task, ctx, &args) })
    }
    fn queen_only(&self) -> bool {
        true
    }
}

impl Tool for WorkerStatus {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "worker_status",
            description: "Status of one of your workers: running/succeeded/failed/cancelled, its last \
                          output lines, and its final reply (`result`) once finished. `wait_secs` waits \
                          up to that long for it to finish.",
            // Output of a worker that read untrusted content taints the caller (see `status`).
            labels: Labels::READ_ONLY,
            params: json!({
                "type": "object",
                "properties": {
                    "worker": {"type": "integer"},
                    "wait_secs": {"type": "integer", "minimum": 0, "maximum": MAX_WAIT_SECS},
                },
                "required": ["worker"],
                "additionalProperties": false,
            }),
        }
    }
    fn call<'a>(&'a self, _: Value) -> BoxFuture<'a, Result<Value, ToolError>> {
        needs_context()
    }
    fn call_in<'a>(
        &'a self,
        task: TaskId,
        ctx: &'a mut TaskContext,
        args: Value,
    ) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(async move { self.0.status(task, ctx, &args).await })
    }
    fn queen_only(&self) -> bool {
        true
    }
}

impl Tool for ListWorkers {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_workers",
            description: "Your workers, newest first: id, backend, status and goal.",
            labels: Labels::READ_ONLY,
            params: json!({"type": "object", "properties": {}, "additionalProperties": false}),
        }
    }
    fn call<'a>(&'a self, _: Value) -> BoxFuture<'a, Result<Value, ToolError>> {
        needs_context()
    }
    fn call_in<'a>(
        &'a self,
        task: TaskId,
        _ctx: &'a mut TaskContext,
        args: Value,
    ) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(async move {
            object(&args, &[])?;
            let workers: Vec<Value> = self
                .0
                .list(task)
                .into_iter()
                .map(|t| {
                    json!({
                        "worker": t.id,
                        "backend": t.backend,
                        "status": t.status.as_str(),
                        "elapsed_secs": t.elapsed.as_secs(),
                        "goal": self.0.goal(t.id).map(|g| shorten(&g, 200)),
                    })
                })
                .collect();
            Ok(json!({"workers": workers}))
        })
    }
    fn queen_only(&self) -> bool {
        true
    }
}

impl Tool for CancelWorker {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "cancel_worker",
            description: "Stop one of your running workers.",
            // Stops work in progress: a change of state, harmless to undo by starting again.
            labels: START_LABELS,
            params: json!({
                "type": "object",
                "properties": {"worker": {"type": "integer"}},
                "required": ["worker"],
                "additionalProperties": false,
            }),
        }
    }
    fn call<'a>(&'a self, _: Value) -> BoxFuture<'a, Result<Value, ToolError>> {
        needs_context()
    }
    fn call_in<'a>(
        &'a self,
        task: TaskId,
        _ctx: &'a mut TaskContext,
        args: Value,
    ) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(async move {
            let a = self.0.attached()?;
            let args = object(&args, &["worker"])?;
            let id = worker_id(&args)?;
            self.0.worker(a, task, id)?;
            a.board
                .cancel_by(id, "queen")
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            let status = a
                .board
                .get(id)
                .map(|t| t.status.as_str())
                .unwrap_or("unknown");
            Ok(json!({"worker": id, "status": status}))
        })
    }
    fn queen_only(&self) -> bool {
        true
    }
}
