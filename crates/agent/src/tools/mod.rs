//! The narrow tool API agents act through.
//!
//! Agents never get a shell. They call named tools with JSON arguments; each tool carries
//! [`Labels`] and the [`policy`] decides per call whether it runs now or waits for a human
//! ([`ConfirmationStore`]). Every call, confirmation and refusal is audited with a hash of
//! its arguments.

mod builtin;
mod confirm;
pub mod policy;

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use serde_json::Value;
use sha2::{Digest, Sha256};

pub use builtin::BeeInfo;
pub use confirm::{Confirmation, ConfirmationId, ConfirmationStore, Verdict};
pub use policy::{check_tool_set, decide, Decision, Effect, Labels, TaskContext};

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
    #[error("confirmation timed out")]
    ConfirmationTimeout,
    #[error("tool failed: {0}")]
    Failed(String),
}

/// A tool implementation. Validate `args` strictly; never shell out with them.
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn call<'a>(&'a self, args: Value) -> BoxFuture<'a, Result<Value, ToolError>>;
}

/// All tools available on this bee, plus the confirmation inbox.
#[derive(Clone)]
pub struct ToolRegistry {
    tools: Arc<BTreeMap<&'static str, Arc<dyn Tool>>>,
    confirmations: ConfirmationStore,
    audit: Option<AuditLog>,
    confirm_timeout: Duration,
}

impl ToolRegistry {
    pub fn new(tools: Vec<Arc<dyn Tool>>, audit: Option<AuditLog>) -> Self {
        let tools = tools.into_iter().map(|t| (t.spec().name, t)).collect();
        Self {
            tools: Arc::new(tools),
            confirmations: ConfirmationStore::new(),
            audit,
            confirm_timeout: Duration::from_secs(15 * 60),
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

    /// Tool specs an agent may see. `allow` limits the set (None = all); hidden tools are
    /// not merely refused, the agent never learns they exist.
    pub fn visible(&self, allow: Option<&[&str]>) -> Vec<ToolSpec> {
        self.tools
            .values()
            .map(|t| t.spec())
            .filter(|s| allow.is_none_or(|a| a.contains(&s.name)))
            .collect()
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
        if !args.is_object() && !args.is_null() {
            return Err(ToolError::BadArgs("arguments must be a JSON object".into()));
        }
        let spec = tool.spec();
        let hash = args_hash(&args);

        if let Decision::Confirm(reason) = decide(name, &spec.labels, ctx) {
            self.confirm(task, name, &args, &reason).await?;
        }

        let result = tool.call(args).await;
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
fn args_hash(args: &Value) -> String {
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
