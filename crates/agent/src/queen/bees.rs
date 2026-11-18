//! Bee agents: sandboxed agents the Queen starts for concrete jobs.
//!
//! A bee is a task on the [`TaskBoard`] whose parent is the Queen. It runs unattended
//! (reversible actions need a human), with at most the Queen's own tools (or a subset the
//! Queen names), never the bee tools themselves (depth 1), under a time limit no longer
//! than the configured maximum. At most `max_concurrent` bees run at once. The tools here
//! are Queen-only ([`Tool::queen_only`]): bees and UI tasks never see them.
//!
//! Scope: a bee may change only the apps the Queen names in `apps` (each one the Queen
//! may change itself; the owner's apps only after the owner approves the grant) and, with
//! `create_apps` (default), the apps it creates, which belong to its job (or to the owner
//! with `for_owner`, approved by the owner). See [`crate::tools::scope`]. The bee tools
//! only reach the caller's own bees; bees never see them.
//!
//! Taint flows both ways: a bee started by a tainted Queen starts tainted, and the Queen
//! becomes tainted when it reads the output of a bee that read untrusted content.

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
    tools::{
        check_tool_set, Effect, JobScope, Labels, Owner, ResourceKind, Scope, TaskContext, Tool,
        ToolError, ToolSpec,
    },
    BoxFuture, DispatchOptions, Stream, TaskBoard, TaskId, TaskSnapshot, TaskStatus,
    MAX_PROMPT_BYTES,
};

/// Limits for bee agents (`[queen]` in the queen config).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeeLimits {
    /// Bees running at the same time.
    pub max_concurrent: usize,
    /// Time limit when the Queen gives none.
    pub default_timeout: Duration,
    /// Largest time limit the Queen may ask for.
    pub max_timeout: Duration,
}

impl Default for BeeLimits {
    fn default() -> Self {
        Self {
            max_concurrent: 2,
            default_timeout: Duration::from_secs(300),
            max_timeout: Duration::from_secs(900),
        }
    }
}

/// Longest `wait_secs` for `bee_status` (agents' MCP clients time out tool calls).
const MAX_WAIT_SECS: u64 = 50;
/// Output lines returned by `bee_status`.
const TAIL_LINES: usize = 30;
/// Characters of a finished bee's reply returned by `bee_status`.
const RESULT_CHARS: usize = 8000;

struct Attached {
    board: TaskBoard,
    default_backend: String,
    preamble: String,
    /// The Queen's own tool allowlist (None = every tool).
    queen_allow: Option<Vec<&'static str>>,
}

struct PoolInner {
    limits: BeeLimits,
    events: EventLog,
    attached: OnceLock<Attached>,
    /// Goal text per bee (the board keeps the full prompt with the preamble).
    goals: Mutex<HashMap<TaskId, String>>,
    /// Serialises the concurrency check and the dispatch.
    start: Mutex<()>,
}

/// Starts and tracks bee agents. Cheap to clone.
#[derive(Clone)]
pub struct BeePool {
    inner: Arc<PoolInner>,
}

impl BeePool {
    pub fn new(limits: BeeLimits, events: EventLog) -> Self {
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

    pub fn limits(&self) -> &BeeLimits {
        &self.inner.limits
    }

    /// The four bee tools, to register in the [`crate::tools::ToolRegistry`].
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(StartBee(self.clone())),
            Arc::new(BeeStatus(self.clone())),
            Arc::new(ListBees(self.clone())),
            Arc::new(StopBee(self.clone())),
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
            .ok_or_else(|| ToolError::Failed("bees are not available on this comb".into()))
    }

    /// Tools a bee may get: the Queen's own, without the Queen-only ones. (What a bee
    /// may do with them is limited by its scope, not by hiding them here.)
    fn handable(&self, a: &Attached) -> Vec<(&'static str, Labels)> {
        let Some(link) = a.board.mcp_link() else {
            return Vec::new();
        };
        let registry = link.server.registry();
        registry
            .visible(a.queen_allow.as_deref())
            .into_iter()
            .filter(|s| !registry.is_queen_only(s.name))
            .map(|s| (s.name, s.labels))
            .collect()
    }

    /// The bee's scope from `apps`, `create_apps` and `for_owner`: never more than the
    /// caller's own (`ctx.scope`). Grants the owner must approve are asked for here, in one
    /// confirmation that names them.
    async fn bee_scope(
        &self,
        a: &Attached,
        parent: TaskId,
        ctx: &TaskContext,
        args: &serde_json::Map<String, Value>,
    ) -> Result<Scope, ToolError> {
        let mut apps = std::collections::BTreeSet::new();
        match args.get("apps") {
            None | Some(Value::Null) => {}
            Some(Value::Array(names)) => {
                for n in names {
                    match n.as_str() {
                        Some(n) if plausible_name(n) => {
                            apps.insert(n.to_string());
                        }
                        _ => {
                            return Err(ToolError::BadArgs(
                                "`apps` must be a list of app names".into(),
                            ))
                        }
                    }
                }
            }
            Some(_) => {
                return Err(ToolError::BadArgs(
                    "`apps` must be a list of app names".into(),
                ))
            }
        }
        let flag = |name: &str, default: bool| match args.get(name) {
            None | Some(Value::Null) => Ok(default),
            Some(Value::Bool(b)) => Ok(*b),
            Some(_) => Err(ToolError::BadArgs(format!("`{name}` must be a boolean"))),
        };
        let create = flag("create_apps", true)?;
        let for_owner = flag("for_owner", false)?;

        let agent = match &ctx.scope {
            Scope::Owner => None,
            Scope::ForOwner { agent } => Some(agent.clone()),
            // Only sessions acting for the owner hand out scopes (bees can't start bees).
            Scope::Nothing | Scope::Job(_) => {
                return Err(ToolError::Refused(
                    "not yours: this session may not give apps to bees".into(),
                ))
            }
        };
        let mut reasons = Vec::new();
        if let Some(agent) = &agent {
            let registry = a
                .board
                .mcp_link()
                .map(|l| l.server.registry().clone())
                .ok_or_else(|| ToolError::Failed("bees are not available on this comb".into()))?;
            let mut owners_apps = Vec::new();
            for app in &apps {
                match registry.owner_of(ResourceKind::App, app).await {
                    Ok(Some(Owner::Human)) => owners_apps.push(format!("`{app}`")),
                    Ok(_) => {}
                    Err(e) => {
                        return Err(ToolError::Failed(format!(
                            "could not check who owns `{app}`: {e}"
                        )))
                    }
                }
            }
            if !owners_apps.is_empty() {
                reasons.push(format!(
                    "{} belong{} to you; {agent} wants to let a bee change {}",
                    owners_apps.join(", "),
                    if owners_apps.len() == 1 { "s" } else { "" },
                    if owners_apps.len() == 1 { "it" } else { "them" },
                ));
            }
            if for_owner && create {
                reasons.push(format!(
                    "{agent} wants the apps a bee creates to belong to you"
                ));
            }
            if !reasons.is_empty() {
                let shown = serde_json::json!({
                    "goal": args.get("goal"),
                    "apps": apps,
                    "create_apps": create,
                    "for_owner": for_owner,
                });
                registry
                    .confirm(parent, "start_bee", &shown, &reasons.join("; "))
                    .await?;
            }
        }
        Ok(Scope::Job(JobScope {
            key: job_key().map_err(|e| ToolError::Failed(format!("job key: {e}")))?,
            apps,
            create,
            for_owner: for_owner && create,
        }))
    }

    /// Bees of `parent`, newest first.
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

    pub fn goal(&self, bee: TaskId) -> Option<String> {
        lock(&self.inner.goals).get(&bee).cloned()
    }

    fn bee(&self, a: &Attached, parent: TaskId, id: TaskId) -> Result<TaskSnapshot, ToolError> {
        a.board
            .get(id)
            .filter(|t| t.parent == Some(parent))
            .ok_or_else(|| ToolError::BadArgs(format!("no bee {id}")))
    }

    async fn start(
        &self,
        parent: TaskId,
        ctx: &TaskContext,
        args: &Value,
    ) -> Result<Value, ToolError> {
        let a = self.attached()?;
        let args = object(
            args,
            &[
                "goal",
                "backend",
                "tools",
                "timeout_secs",
                "apps",
                "create_apps",
                "for_owner",
            ],
        )?;
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
                            "tool `{n}` cannot be given to a bee; allowed: {}",
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
                "backend `{backend}` is not available on this comb"
            )));
        }

        // Last, after every cheap check: it may wait for the owner.
        let scope = self.bee_scope(a, parent, ctx, &args).await?;
        let scope_json = match &scope {
            Scope::Job(j) => json!({
                "job": j.key,
                "apps": j.apps,
                "create_apps": j.create,
                "for_owner": j.for_owner,
            }),
            _ => Value::Null,
        };

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
                    "{running} bees are already running (at most {}); wait for one to finish \
                     or cancel one",
                    limits.max_concurrent
                )));
            }
            let opts = DispatchOptions {
                ctx: TaskContext {
                    attended: false,
                    tainted: ctx.tainted,
                    scope,
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
        self.inner.events.push(QueenEvent::BeeStarted {
            bee: id,
            backend: backend.clone(),
            goal: shorten(goal, 200),
        });
        let (board, events) = (a.board.clone(), self.inner.events.clone());
        tokio::spawn(async move {
            loop {
                match board.wait(id, Duration::from_secs(3600)).await {
                    Some(snap) if snap.status.is_finished() => {
                        events.push(QueenEvent::BeeFinished {
                            bee: id,
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
            "bee": id,
            "backend": backend,
            "tools": tools.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            "timeout_secs": timeout.as_secs(),
            "scope": scope_json,
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
        let args = object(args, &["bee", "wait_secs"])?;
        let id = bee_id(&args)?;
        let wait = match args.get("wait_secs") {
            None | Some(Value::Null) => 0,
            Some(v) => v.as_u64().filter(|w| *w <= MAX_WAIT_SECS).ok_or_else(|| {
                ToolError::BadArgs(format!(
                    "`wait_secs` must be a whole number from 0 to {MAX_WAIT_SECS}"
                ))
            })?,
        };
        self.bee(a, parent, id)?;
        let snap = if wait > 0 {
            a.board.wait(id, Duration::from_secs(wait)).await
        } else {
            a.board.get(id)
        }
        .ok_or_else(|| ToolError::BadArgs(format!("no bee {id}")))?;
        if snap.tainted {
            // The bee read untrusted content; so does whoever reads its output.
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
            "bee": id,
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

/// Cheap shape check for app names in `apps` (the app tools validate names fully).
fn plausible_name(n: &str) -> bool {
    (1..=63).contains(&n.len())
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A new job key: 12 hex characters from the kernel CSPRNG. Unique across daemon restarts
/// (task ids are not), so ownership labels never point at a different, later job.
fn job_key() -> std::io::Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 6];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
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

fn bee_id(args: &serde_json::Map<String, Value>) -> Result<TaskId, ToolError> {
    args.get("bee")
        .and_then(Value::as_u64)
        .ok_or_else(|| ToolError::BadArgs("`bee` must be a bee id".into()))
}

/// Starting a bee spends money and lets an agent act (unattended): reversible. Attended
/// Queen sessions may do it without asking; a tainted Queen needs a human.
const START_LABELS: Labels = Labels {
    effect: Effect::Reversible,
    ..Labels::READ_ONLY
};

struct StartBee(BeePool);
struct BeeStatus(BeePool);
struct ListBees(BeePool);
struct StopBee(BeePool);

fn needs_context<'a>() -> BoxFuture<'a, Result<Value, ToolError>> {
    Box::pin(async { Err(ToolError::Failed("bee tools need a caller".into())) })
}

impl Tool for StartBee {
    fn spec(&self) -> ToolSpec {
        let l = self.0.limits();
        ToolSpec {
            name: "start_bee",
            description: "Start a sandboxed bee agent for one concrete job (a multi-step check, \
                          a longer investigation). It works unattended with your tools or the subset \
                          in `tools`, cannot start bees itself, and is stopped after \
                          `timeout_secs`. It may change only the apps in `apps` and the ones it \
                          creates. Returns its bee id; then use bee_status.",
            labels: START_LABELS,
            params: json!({
                "type": "object",
                "properties": {
                    "goal": {"type": "string", "description": "What the bee should do and report back. Self-contained: the bee does not see this conversation."},
                    "backend": {"type": "string", "description": "Agent backend (default: the configured bee backend)."},
                    "tools": {"type": "array", "items": {"type": "string"}, "description": "Tools the bee may use, by their plain names such as `bee_info` (no `mcp__melissa__` or other prefix). Default: all of yours except the bee tools."},
                    "timeout_secs": {"type": "integer", "minimum": 1, "maximum": l.max_timeout.as_secs(), "description": format!("Time limit (default {}).", l.default_timeout.min(l.max_timeout).as_secs())},
                    "apps": {"type": "array", "items": {"type": "string"}, "description": "Existing apps the bee may change (install over, uninstall, read logs). Default: none. Granting an app that belongs to the owner asks the owner first. Any other app is refused to the bee."},
                    "create_apps": {"type": "boolean", "default": true, "description": "The bee may install new apps; they belong to its job (it and you may change them later)."},
                    "for_owner": {"type": "boolean", "default": false, "description": "Apps the bee creates belong to the owner instead of the job (asks the owner first)."},
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
        Box::pin(async move { self.0.start(task, ctx, &args).await })
    }
    fn queen_only(&self) -> bool {
        true
    }
}

impl Tool for BeeStatus {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bee_status",
            description: "Status of one of your bees: running/succeeded/failed/cancelled, its last \
                          output lines, and its final reply (`result`) once finished. `wait_secs` waits \
                          up to that long for it to finish.",
            // Output of a bee that read untrusted content taints the caller (see `status`).
            labels: Labels::READ_ONLY,
            params: json!({
                "type": "object",
                "properties": {
                    "bee": {"type": "integer"},
                    "wait_secs": {"type": "integer", "minimum": 0, "maximum": MAX_WAIT_SECS},
                },
                "required": ["bee"],
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

impl Tool for ListBees {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_bees",
            description: "Your bees, newest first: id, backend, status and goal.",
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
            let bees: Vec<Value> = self
                .0
                .list(task)
                .into_iter()
                .map(|t| {
                    json!({
                        "bee": t.id,
                        "backend": t.backend,
                        "status": t.status.as_str(),
                        "elapsed_secs": t.elapsed.as_secs(),
                        "goal": self.0.goal(t.id).map(|g| shorten(&g, 200)),
                    })
                })
                .collect();
            Ok(json!({"bees": bees}))
        })
    }
    fn queen_only(&self) -> bool {
        true
    }
}

impl Tool for StopBee {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "stop_bee",
            description: "Stop one of your running bees.",
            // Stops work in progress: a change of state, harmless to undo by starting again.
            labels: START_LABELS,
            params: json!({
                "type": "object",
                "properties": {"bee": {"type": "integer"}},
                "required": ["bee"],
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
            let args = object(&args, &["bee"])?;
            let id = bee_id(&args)?;
            self.0.bee(a, task, id)?;
            a.board
                .cancel_by(id, "queen")
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            let status = a
                .board
                .get(id)
                .map(|t| t.status.as_str())
                .unwrap_or("unknown");
            Ok(json!({"bee": id, "status": status}))
        })
    }
    fn queen_only(&self) -> bool {
        true
    }
}
