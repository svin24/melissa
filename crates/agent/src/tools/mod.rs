//! The narrow tool API agents act through.
//!
//! Agents never get a shell. They call named tools with JSON arguments; each tool carries
//! [`Labels`] and the [`policy`] decides per call whether it runs now or waits for a human
//! ([`ConfirmationStore`]). Every call, confirmation and refusal is audited with a hash of
//! its arguments. Before the policy, the registry checks the task's [`Scope`]: agents
//! change only resources that belong to them ([`scope`]).

mod builtin;
mod confirm;
pub mod policy;
pub mod scope;

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use serde_json::Value;
use sha2::{Digest, Sha256};

pub use builtin::BeeInfo;
pub use confirm::{Confirmation, ConfirmationEvent, ConfirmationId, ConfirmationStore, Verdict};
pub use policy::{check_tool_set, decide, Decision, Effect, Labels, TaskContext};
pub use scope::{
    Access, JobScope, Owner, ResourceKind, ResourceOwners, Scope, ScopeDecision, Target,
};

use crate::{AuditLog, AuditRecord, BoxFuture, TaskId};

/// Static description of a tool, also what agents see (name, description, JSON schema).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub labels: Labels,
    /// JSON Schema for the arguments object.
    pub params: Value,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ToolError {
    #[error("unknown tool `{0}`")]
    UnknownTool(String),
    #[error("invalid arguments: {0}")]
    BadArgs(String),
    #[error("denied by a human")]
    Denied,
    /// Out of the task's scope (not its resource): refused without asking anyone.
    #[error("refused: {0}")]
    Refused(String),
    #[error("confirmation timed out: nobody answered in time, so the request was withdrawn")]
    ConfirmationTimeout,
    #[error("tool failed: {0}")]
    Failed(String),
}

/// A tool implementation. Validate `args` strictly; never shell out with them.
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn call<'a>(&'a self, args: Value) -> BoxFuture<'a, Result<Value, ToolError>>;

    /// Like [`Tool::call`], for tools that need to know who is calling: the task and its
    /// policy context. A tool may set `ctx.tainted` when it hands back untrusted content
    /// (e.g. the output of a tainted bee). Default: [`Tool::call`].
    fn call_in<'a>(
        &'a self,
        _task: TaskId,
        _ctx: &'a mut TaskContext,
        args: Value,
    ) -> BoxFuture<'a, Result<Value, ToolError>> {
        self.call(args)
    }

    /// Only sessions that may delegate ([`TaskContext::delegate`], the Queen) see and may
    /// call this tool (e.g. `start_bee`). Default: false.
    fn queen_only(&self) -> bool {
        false
    }

    /// The resource this tool works on, named by one of its arguments. The registry checks
    /// the caller's [`Scope`] against it before the label policy (see [`scope`]). Every tool
    /// that changes or reads into a resource with an owner must declare it. Default: none.
    fn target(&self) -> Option<Target> {
        None
    }
}

/// All tools available on this comb, plus the confirmation inbox.
#[derive(Clone)]
pub struct ToolRegistry {
    tools: Arc<BTreeMap<&'static str, Arc<dyn Tool>>>,
    confirmations: ConfirmationStore,
    audit: Option<AuditLog>,
    confirm_timeout: Duration,
    /// Who owns resources (None: every existing resource counts as the human's).
    owners: Option<Arc<dyn ResourceOwners>>,
}

impl ToolRegistry {
    pub fn new(tools: Vec<Arc<dyn Tool>>, audit: Option<AuditLog>) -> Self {
        let tools = tools.into_iter().map(|t| (t.spec().name, t)).collect();
        Self {
            tools: Arc::new(tools),
            confirmations: ConfirmationStore::new(),
            audit,
            confirm_timeout: Duration::from_secs(15 * 60),
            owners: None,
        }
    }

    /// Where the scope check learns who owns a resource. Without it, every resource is
    /// treated as existing and owned by the human (the strictest answer).
    pub fn with_owners(mut self, owners: Arc<dyn ResourceOwners>) -> Self {
        self.owners = Some(owners);
        self
    }

    /// Who owns resource `name` of `kind` now (`Ok(None)`: it does not exist).
    pub async fn owner_of(&self, kind: ResourceKind, name: &str) -> Result<Option<Owner>, String> {
        match &self.owners {
            Some(o) => o.owner(kind, name).await,
            None => Ok(Some(Owner::Human)),
        }
    }

    pub fn with_confirm_timeout(mut self, timeout: Duration) -> Self {
        self.confirm_timeout = timeout;
        self
    }

    /// Every built-in tool that works on any bee.
    pub fn builtin(audit: Option<AuditLog>) -> Self {
        Self::new(vec![Arc::new(BeeInfo::new())], audit)
    }

    pub fn confirmations(&self) -> &ConfirmationStore {
        &self.confirmations
    }

    /// Every tool spec, including Queen-only tools, limited by `allow` (None = all). For
    /// recognising Melissa's tool names; what an agent sees is [`ToolRegistry::visible_to`].
    pub fn visible(&self, allow: Option<&[&str]>) -> Vec<ToolSpec> {
        self.tools
            .values()
            .map(|t| t.spec())
            .filter(|s| allow.is_none_or(|a| a.contains(&s.name)))
            .collect()
    }

    /// Tool specs an agent with policy context `ctx` may see. `allow` limits the set
    /// (None = all); Queen-only tools need `ctx.delegate`. Hidden tools are not merely
    /// refused, the agent never learns they exist.
    /// Tools that change resources outside `ctx.scope` are hidden too, and a targeted tool's
    /// argument description lists the resources in scope.
    pub fn visible_to(&self, ctx: &TaskContext, allow: Option<&[&str]>) -> Vec<ToolSpec> {
        self.tools
            .values()
            .filter(|t| ctx.delegate || !t.queen_only())
            .filter_map(|t| {
                let mut spec = t.spec();
                match t.target() {
                    Some(target) if !scope::present(&ctx.scope, &target, &mut spec.params) => None,
                    _ => Some(spec),
                }
            })
            .filter(|s| allow.is_none_or(|a| a.contains(&s.name)))
            .collect()
    }

    /// The registry's own `&'static` name for `name`, if such a tool exists.
    pub fn name(&self, name: &str) -> Option<&'static str> {
        self.tools.get_key_value(name).map(|(k, _)| *k)
    }

    /// Is `name` a Queen-only tool?
    pub fn is_queen_only(&self, name: &str) -> bool {
        self.tools.get(name).is_some_and(|t| t.queen_only())
    }

    /// Call a tool for `task`, applying the policy. May wait for a human confirmation.
    /// Updates `ctx.tainted` when the tool returns untrusted content.
    pub async fn call(
        &self,
        task: TaskId,
        ctx: &mut TaskContext,
        name: &str,
        args: Value,
    ) -> Result<Value, ToolError> {
        let tool = self
            .tools
            .get(name)
            .cloned()
            .ok_or_else(|| ToolError::UnknownTool(name.to_string()))?;
        if tool.queen_only() && !ctx.delegate {
            // Same answer as for a nonexistent tool: bees never learn these exist.
            return Err(ToolError::UnknownTool(name.to_string()));
        }
        if !args.is_object() && !args.is_null() {
            return Err(ToolError::BadArgs("arguments must be a JSON object".into()));
        }
        let spec = tool.spec();
        let hash = args_hash(&args);

        let mut reasons = Vec::new();
        if let Some(target) = tool.target() {
            if let Some(reason) = self
                .check_scope(task, ctx, name, &target, &args, &hash)
                .await?
            {
                reasons.push(reason);
            }
        }
        if let Decision::Confirm(reason) = decide(name, &spec.labels, ctx) {
            reasons.push(reason);
        }
        if !reasons.is_empty() {
            self.confirm(task, name, &args, &reasons.join("; ")).await?;
        }

        let result = tool.call_in(task, ctx, args).await;
        if spec.labels.reads_untrusted && result.is_ok() {
            ctx.tainted = true;
        }
        let outcome = match &result {
            Ok(_) => "ok".to_string(),
            Err(e) => format!("error: {e}"),
        };
        self.audit(task, "tool.call", name, &hash, &outcome);
        result
    }

    /// The scope check for a targeted call: `Ok(Some(reason))` = needs a confirmation,
    /// `Err(Refused)` = out of scope (audited `tool.refused`).
    async fn check_scope(
        &self,
        task: TaskId,
        ctx: &TaskContext,
        name: &str,
        target: &Target,
        args: &Value,
        hash: &str,
    ) -> Result<Option<String>, ToolError> {
        let resource = match args.get(target.arg) {
            Some(Value::String(s)) => s.as_str(),
            _ => {
                return Err(ToolError::BadArgs(format!(
                    "`{}` (string) is required",
                    target.arg
                )))
            }
        };
        let owner = if ctx.scope.needs_owner(target.access, resource) {
            match self.owner_of(target.kind, resource).await {
                Ok(owner) => owner,
                Err(e) => {
                    // Fail closed: without knowing the owner, nothing is changed.
                    let reason = format!("could not check who owns `{resource}`: {e}");
                    self.audit(task, "tool.refused", name, hash, &reason);
                    return Err(ToolError::Failed(reason));
                }
            }
        } else {
            None
        };
        match scope::check(&ctx.scope, target, resource, owner.as_ref()) {
            ScopeDecision::Allow => Ok(None),
            ScopeDecision::Confirm(reason) => Ok(Some(reason)),
            ScopeDecision::Refuse(reason) => {
                self.audit(task, "tool.refused", name, hash, &reason);
                Err(ToolError::Refused(reason))
            }
        }
    }

    /// Ask a human to approve `name` with `args` for `task` and wait for the answer, at most
    /// the confirmation timeout. Audited (`tool.confirm_requested`, then `tool.approved`,
    /// `tool.denied` or `tool.confirm_timeout`). If the returned future is dropped (task
    /// cancelled), the confirmation is withdrawn from the inbox.
    pub async fn confirm(
        &self,
        task: TaskId,
        name: &str,
        args: &Value,
        reason: &str,
    ) -> Result<(), ToolError> {
        let hash = args_hash(args);
        let (info, rx) = self.confirmations.request(task, name, args, reason);
        let _withdraw = Withdraw {
            store: &self.confirmations,
            id: info.id,
        };
        self.audit(task, "tool.confirm_requested", name, &hash, reason);
        let verdict = match tokio::time::timeout(self.confirm_timeout, rx).await {
            Ok(Ok(v)) => v,
            // Store dropped the sender (shouldn't happen): treat as no.
            Ok(Err(_)) => Verdict::Denied,
            Err(_) => {
                self.audit(task, "tool.confirm_timeout", name, &hash, "");
                return Err(ToolError::ConfirmationTimeout);
            }
        };
        if verdict == Verdict::Denied {
            self.audit(task, "tool.denied", name, &hash, reason);
            return Err(ToolError::Denied);
        }
        self.audit(task, "tool.approved", name, &hash, reason);
        Ok(())
    }

    /// Write an agent action to the audit log (actor `agent`, arguments hashed, not logged).
    pub fn audit_agent(
        &self,
        task: TaskId,
        backend: &str,
        action: &str,
        tool: &str,
        args: &Value,
        detail: &str,
    ) {
        self.write_audit(task, backend, action, tool, &args_hash(args), detail);
    }

    /// The audit log, if any (shared with the task board and the Queen).
    pub fn audit_log(&self) -> Option<&AuditLog> {
        self.audit.as_ref()
    }

    fn audit(&self, task: TaskId, action: &str, tool: &str, hash: &str, detail: &str) {
        self.write_audit(task, "", action, tool, hash, detail);
    }

    fn write_audit(
        &self,
        task: TaskId,
        backend: &str,
        action: &str,
        tool: &str,
        hash: &str,
        detail: &str,
    ) {
        if let Some(log) = &self.audit {
            let mut record = AuditRecord::now(action, task, backend, detail).with_tool(tool, hash);
            // Tool calls are made by the task's agent; the person who started the task is
            // recorded on the `task.dispatch` line.
            record.actor = "agent".into();
            if let Err(e) = log.append(&record) {
                tracing::error!(path = %log.path().display(), "audit log write failed: {e}");
            }
        }
    }
}

/// Removes a confirmation from the inbox when its waiter goes away (answered, timed out or
/// dropped). Removing an already resolved one is a no-op.
struct Withdraw<'a> {
    store: &'a ConfirmationStore,
    id: ConfirmationId,
}

impl Drop for Withdraw<'_> {
    fn drop(&mut self) {
        self.store.withdraw(self.id);
    }
}

/// SHA-256 of the canonical (compact, key-sorted) JSON arguments.
pub(crate) fn args_hash(args: &Value) -> String {
    // Sort explicitly: serde_json's `preserve_order` feature (enabled by a dependency
    // anywhere in the build) makes `Map` keep insertion order instead of sorting.
    let bytes = serde_json::to_vec(&canonical(args)).unwrap_or_default();
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `value` with every object's keys in sorted order.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|k| (k.clone(), canonical(&map[k])))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    struct Fake {
        name: &'static str,
        labels: Labels,
    }
    impl Tool for Fake {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name,
                description: "test",
                labels: self.labels,
                params: json!({"type": "object"}),
            }
        }
        fn call<'a>(&'a self, args: Value) -> BoxFuture<'a, Result<Value, ToolError>> {
            Box::pin(async move { Ok(json!({"echo": args})) })
        }
    }

    fn registry(audit: Option<AuditLog>) -> ToolRegistry {
        ToolRegistry::new(
            vec![
                Arc::new(Fake {
                    name: "status",
                    labels: Labels::READ_ONLY,
                }),
                Arc::new(Fake {
                    name: "read_logs",
                    labels: Labels {
                        reads_untrusted: true,
                        ..Labels::READ_ONLY
                    },
                }),
                Arc::new(Fake {
                    name: "remove_app",
                    labels: Labels {
                        effect: Effect::Destructive,
                        ..Labels::READ_ONLY
                    },
                }),
                Arc::new(Fake {
                    name: "scale",
                    labels: Labels {
                        effect: Effect::Reversible,
                        ..Labels::READ_ONLY
                    },
                }),
            ],
            audit,
        )
    }

    fn attended() -> TaskContext {
        TaskContext {
            attended: true,
            ..TaskContext::default()
        }
    }

    #[tokio::test]
    async fn read_only_runs_without_confirmation() {
        let reg = registry(None);
        let out = reg
            .call(1, &mut attended(), "status", json!({"a": 1}))
            .await
            .unwrap();
        assert_eq!(out["echo"]["a"], 1);
        assert!(reg.confirmations().pending().is_empty());
    }

    #[tokio::test]
    async fn unknown_tools_and_bad_args_are_rejected() {
        let reg = registry(None);
        assert_eq!(
            reg.call(1, &mut attended(), "rm_rf", json!({})).await,
            Err(ToolError::UnknownTool("rm_rf".into()))
        );
        assert!(matches!(
            reg.call(1, &mut attended(), "status", json!([1])).await,
            Err(ToolError::BadArgs(_))
        ));
    }

    #[tokio::test]
    async fn destructive_waits_for_approval() {
        let reg = registry(None);
        let waiter = {
            let reg = reg.clone();
            tokio::spawn(async move {
                reg.call(1, &mut attended(), "remove_app", json!({"app": "x"}))
                    .await
            })
        };
        let pending = loop {
            let p = reg.confirmations().pending();
            if !p.is_empty() {
                break p;
            }
            tokio::task::yield_now().await;
        };
        assert_eq!(pending[0].tool, "remove_app");
        assert_eq!(pending[0].reason, "destructive action");
        reg.confirmations()
            .resolve(pending[0].id, Verdict::Approved);
        assert!(waiter.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn denial_and_timeout_stop_the_call() {
        let reg = registry(None).with_confirm_timeout(Duration::from_millis(30));
        assert_eq!(
            reg.call(1, &mut attended(), "remove_app", json!({})).await,
            Err(ToolError::ConfirmationTimeout)
        );
        assert!(reg.confirmations().pending().is_empty());

        let reg = registry(None);
        let waiter = {
            let reg = reg.clone();
            tokio::spawn(async move { reg.call(2, &mut attended(), "remove_app", json!({})).await })
        };
        let id = loop {
            if let Some(c) = reg.confirmations().pending().first() {
                break c.id;
            }
            tokio::task::yield_now().await;
        };
        reg.confirmations().resolve(id, Verdict::Denied);
        assert_eq!(waiter.await.unwrap(), Err(ToolError::Denied));
    }

    #[tokio::test]
    async fn untrusted_output_taints_the_task() {
        let reg = registry(None).with_confirm_timeout(Duration::from_millis(30));
        let mut ctx = attended();
        reg.call(1, &mut ctx, "scale", json!({})).await.unwrap();
        reg.call(1, &mut ctx, "read_logs", json!({})).await.unwrap();
        assert!(ctx.tainted);
        // After reading untrusted logs, even an attended reversible action needs approval.
        assert_eq!(
            reg.call(1, &mut ctx, "scale", json!({})).await,
            Err(ToolError::ConfirmationTimeout)
        );
    }

    struct QueenOnly;
    impl Tool for QueenOnly {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "start_bee",
                description: "test",
                labels: Labels::READ_ONLY,
                params: json!({"type": "object"}),
            }
        }
        fn call<'a>(&'a self, _: Value) -> BoxFuture<'a, Result<Value, ToolError>> {
            Box::pin(async { Ok(json!("started")) })
        }
        fn queen_only(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn queen_only_tools_need_delegation() {
        let reg = ToolRegistry::new(vec![Arc::new(QueenOnly)], None);
        let mut bee = attended();
        assert!(reg.visible_to(&bee, None).is_empty());
        assert_eq!(
            reg.call(1, &mut bee, "start_bee", json!({})).await,
            Err(ToolError::UnknownTool("start_bee".into()))
        );
        let mut queen = TaskContext {
            delegate: true,
            ..attended()
        };
        assert_eq!(reg.visible_to(&queen, None).len(), 1);
        assert_eq!(
            reg.call(1, &mut queen, "start_bee", json!({})).await,
            Ok(json!("started"))
        );
        // Name matching for permission requests still knows the tool.
        assert_eq!(reg.visible(None).len(), 1);
        assert!(reg.is_queen_only("start_bee"));
    }

    #[test]
    fn visibility_filter_hides_tools() {
        let reg = registry(None);
        assert_eq!(reg.visible(None).len(), 4);
        let names: Vec<_> = reg
            .visible(Some(&["status"]))
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, ["status"]);
    }

    #[tokio::test]
    async fn calls_are_audited_with_arg_hash() {
        let path =
            std::env::temp_dir().join(format!("melissa-tools-audit-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let reg = registry(Some(AuditLog::new(&path)));
        reg.call(5, &mut attended(), "status", json!({"b": 2, "a": 1}))
            .await
            .unwrap();
        let line: Value =
            serde_json::from_str(std::fs::read_to_string(&path).unwrap().trim()).unwrap();
        assert_eq!(line["action"], "tool.call");
        assert_eq!(line["actor"], "agent");
        assert_eq!(line["tool"], "status");
        assert_eq!(line["args_sha256"], args_hash(&json!({"a": 1, "b": 2})));
        // Canonical form is sorted keys, whatever serde_json's map ordering feature says.
        let expected: String = Sha256::digest(br#"{"a":1,"b":2}"#)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(line["args_sha256"], expected.as_str());
        assert_eq!(
            args_hash(&json!({"z": {"y": 1, "x": [{"b": 1, "a": 2}]}})),
            args_hash(&json!({"z": {"x": [{"a": 2, "b": 1}], "y": 1}}))
        );
        std::fs::remove_file(&path).unwrap();
    }
}
