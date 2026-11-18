//! Answers an agent's permission requests with Melissa's tool policy.
//!
//! Two kinds of requests arrive:
//! - **Melissa's own tools** (called through our MCP endpoint): allowed here, because the MCP
//!   server applies the policy, waits for confirmations and audits the call when it arrives.
//!   Asking twice would only double the prompts.
//! - **The agent's built-in tools** (shell, file edits, fetch, ...): mapped onto policy
//!   [`Labels`] by their ACP kind and decided with [`policy::decide`]. Anything that needs a
//!   human becomes a confirmation in the inbox ([`ToolRegistry::confirm`]).
//!
//! Every answer is audited as `agent.permission` (actor `agent`, input hashed). Answers are
//! always one-off; "always" grants are never given (see [`PermissionDecision`]).

use serde_json::Value;

use super::{PermissionDecision, PermissionHandler, PermissionRequest, ToolKind};
use crate::{
    mcp::McpServer,
    tools::{
        policy::{self, Decision, Effect, Labels, TaskContext},
        ToolError, ToolRegistry,
    },
    BoxFuture, TaskId,
};

/// Who is asking: the task, its backend, and its MCP session (for the policy context).
#[derive(Clone)]
pub struct PermissionContext {
    pub task: TaskId,
    pub backend: String,
    /// Name under which Melissa's MCP server was given to the agent (e.g. `melissa`).
    pub mcp_name: String,
    /// The task's MCP session: its policy context (attended, tainted) is read per request.
    pub mcp: Option<(McpServer, String)>,
}

/// [`PermissionHandler`] backed by Melissa's policy, confirmation inbox and audit log.
#[derive(Clone)]
pub struct PolicyPermissions {
    registry: ToolRegistry,
    ctx: PermissionContext,
}

impl PolicyPermissions {
    pub fn new(registry: ToolRegistry, ctx: PermissionContext) -> Self {
        Self { registry, ctx }
    }

    /// Current policy context of the task. Without an MCP session: unattended, untainted.
    fn task_context(&self) -> TaskContext {
        self.ctx
            .mcp
            .as_ref()
            .and_then(|(server, token)| server.session_context(token))
            .unwrap_or_default()
    }

    /// True if the request is a call to one of Melissa's MCP tools.
    fn is_melissa_tool(&self, req: &PermissionRequest) -> bool {
        // Built-in actions are never treated as MCP calls, whatever their title says.
        if !matches!(
            req.kind,
            ToolKind::Other | ToolKind::Read | ToolKind::Search | ToolKind::Think
        ) {
            return false;
        }
        let names = self.registry.visible(None);
        let server = self.ctx.mcp_name.to_ascii_lowercase();
        [req.tool_name.as_deref(), Some(req.title.as_str())]
            .into_iter()
            .flatten()
            .any(|label| {
                let label = label.to_ascii_lowercase();
                names
                    .iter()
                    .any(|spec| names_mcp_tool(&label, &server, spec.name))
            })
    }

    async fn decide_inner(&self, req: &PermissionRequest) -> (PermissionDecision, String) {
        if self.is_melissa_tool(req) {
            return (
                PermissionDecision::AllowOnce,
                "melissa tool: policy applies to the MCP call".into(),
            );
        }
        let Some(labels) = labels_for(req.kind) else {
            return (
                PermissionDecision::RejectOnce,
                "agents may not change their own permission mode".into(),
            );
        };
        let ctx = self.task_context();
        let name = format!("agent:{}", req.kind.as_str());
        match policy::decide(&name, &labels, &ctx) {
            Decision::Allow => (PermissionDecision::AllowOnce, "policy: allowed".into()),
            Decision::Confirm(reason) => {
                let args = serde_json::json!({"title": req.title, "input": req.raw_input});
                match self
                    .registry
                    .confirm(self.ctx.task, &name, &args, &reason)
                    .await
                {
                    Ok(()) => (PermissionDecision::AllowOnce, format!("approved: {reason}")),
                    Err(ToolError::Denied) => {
                        (PermissionDecision::RejectOnce, format!("denied: {reason}"))
                    }
                    Err(e) => (PermissionDecision::Cancelled, format!("{e}: {reason}")),
                }
            }
        }
    }
}

impl PermissionHandler for PolicyPermissions {
    fn decide<'a>(&'a self, req: &'a PermissionRequest) -> BoxFuture<'a, PermissionDecision> {
        Box::pin(async move {
            let audit = Audit {
                perms: self,
                req,
                done: false,
            };
            let (decision, why) = self.decide_inner(req).await;
            audit.finish(decision, &why);
            decision
        })
    }
}

/// Audits the answer; if the decision future is dropped first (turn cancelled), audits that.
struct Audit<'a> {
    perms: &'a PolicyPermissions,
    req: &'a PermissionRequest,
    done: bool,
}

impl Audit<'_> {
    fn finish(mut self, decision: PermissionDecision, why: &str) {
        self.write(decision, why);
        self.done = true;
    }

    fn write(&self, decision: PermissionDecision, why: &str) {
        let ctx = &self.perms.ctx;
        let tool = self.req.tool_name.as_deref().unwrap_or(&self.req.title);
        let input: &Value = &self.req.raw_input;
        let detail = format!(
            "{} ({}): {why}; {}",
            decision.as_str(),
            self.req.kind.as_str(),
            self.req.title
        );
        self.perms.registry.audit_agent(
            ctx.task,
            &ctx.backend,
            "agent.permission",
            tool,
            input,
            &detail,
        );
    }
}

impl Drop for Audit<'_> {
    fn drop(&mut self) {
        if !self.done {
            self.write(PermissionDecision::Cancelled, "turn cancelled");
        }
    }
}

/// Policy labels for the agent's built-in tool kinds. `None`: never allowed.
fn labels_for(kind: ToolKind) -> Option<Labels> {
    let labels = match kind {
        // Inside the sandbox the agent sees only its workdir; reading it changes nothing.
        ToolKind::Read | ToolKind::Search | ToolKind::Think => Labels::READ_ONLY,
        // Edits stay in the per-task workdir.
        ToolKind::Edit | ToolKind::Move => Labels {
            effect: Effect::Reversible,
            ..Labels::READ_ONLY
        },
        // Arbitrary commands and unknown actions: treat as destructive (always a human).
        ToolKind::Delete | ToolKind::Execute | ToolKind::Other => Labels {
            effect: Effect::Destructive,
            ..Labels::READ_ONLY
        },
        // Fetching the web brings in untrusted content and can carry data out.
        ToolKind::Fetch => Labels {
            reads_untrusted: true,
            egress: true,
            ..Labels::READ_ONLY
        },
        ToolKind::SwitchMode => return None,
    };
    Some(labels)
}

/// Does `label` name tool `tool` of MCP server `server`? Agents spell MCP tool names in
/// different ways (`melissa__comb_info`, `mcp__melissa__comb_info`, `melissa/comb_info`,
/// `comb_info (melissa)`, OpenCode's `melissa_comb_info`), so accept the server name and the
/// exact tool name as separate words of the label, or one word `<server>_<tool>`.
fn names_mcp_tool(label: &str, server: &str, tool: &str) -> bool {
    let words: Vec<&str> = label
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .flat_map(|w| w.split("__"))
        .filter(|w| !w.is_empty())
        .collect();
    let joined = format!("{server}_{tool}");
    (words.contains(&server) && words.contains(&tool)) || words.contains(&joined.as_str())
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use super::*;
    use crate::tools::{CombInfo, Tool, Verdict};

    fn request(kind: ToolKind, title: &str, tool_name: Option<&str>) -> PermissionRequest {
        PermissionRequest {
            tool_call_id: "c1".into(),
            title: title.into(),
            kind,
            tool_name: tool_name.map(Into::into),
            raw_input: serde_json::json!({"cmd": "x"}),
        }
    }

    fn perms(audit: Option<crate::AuditLog>, timeout: Duration) -> PolicyPermissions {
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(CombInfo::new())];
        let registry = ToolRegistry::new(tools, audit).with_confirm_timeout(timeout);
        let server = McpServer::new(registry.clone());
        let token = server
            .open_session(
                3,
                TaskContext {
                    attended: true,
                    ..TaskContext::default()
                },
                None,
            )
            .unwrap();
        PolicyPermissions::new(
            registry,
            PermissionContext {
                task: 3,
                backend: "grok-acp".into(),
                mcp_name: "melissa".into(),
                mcp: Some((server, token)),
            },
        )
    }

    #[test]
    fn mcp_tool_names_are_recognised() {
        for label in [
            "melissa__comb_info",
            "mcp__melissa__comb_info",
            "melissa/comb_info",
            "comb_info (melissa)",
            "MCP: melissa.comb_info",
            "melissa_comb_info",
        ] {
            assert!(
                names_mcp_tool(&label.to_lowercase(), "melissa", "comb_info"),
                "{label}"
            );
        }
        for label in [
            "comb_info",
            "melissa__comb_infox",
            "other__comb_info",
            "melissa_comb_infox",
            "xmelissa_comb_info",
        ] {
            assert!(!names_mcp_tool(label, "melissa", "comb_info"), "{label}");
        }
    }

    #[tokio::test]
    async fn melissa_tools_and_reads_are_allowed_once() {
        let p = perms(None, Duration::from_secs(5));
        let mcp = request(ToolKind::Other, "melissa__comb_info", None);
        assert_eq!(p.decide(&mcp).await, PermissionDecision::AllowOnce);
        let read = request(ToolKind::Read, "Read file", None);
        assert_eq!(p.decide(&read).await, PermissionDecision::AllowOnce);
        // Attended task: edits in the workdir are allowed.
        let edit = request(ToolKind::Edit, "Edit notes.txt", None);
        assert_eq!(p.decide(&edit).await, PermissionDecision::AllowOnce);
    }

    #[tokio::test]
    async fn spoofed_titles_and_mode_switches_are_not_trusted() {
        let p = perms(None, Duration::from_millis(20));
        // A shell command that mentions the tool still needs a human (times out here).
        let exec = request(ToolKind::Execute, "Execute `melissa__comb_info`", None);
        assert_eq!(p.decide(&exec).await, PermissionDecision::Cancelled);
        let mode = request(ToolKind::SwitchMode, "Switch to bypassPermissions", None);
        assert_eq!(p.decide(&mode).await, PermissionDecision::RejectOnce);
    }

    #[tokio::test]
    async fn execute_waits_for_a_human() {
        let path =
            std::env::temp_dir().join(format!("melissa-perm-audit-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let p = perms(Some(crate::AuditLog::new(&path)), Duration::from_secs(5));
        let store = p.registry.confirmations().clone();
        let exec = request(ToolKind::Execute, "Execute `ls`", None);
        let waiter = {
            let p = p.clone();
            let exec = exec.clone();
            tokio::spawn(async move { p.decide(&exec).await })
        };
        let pending = loop {
            if let Some(c) = store.pending().first().cloned() {
                break c;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert_eq!(pending.tool, "agent:execute");
        assert_eq!(pending.task, 3);
        store.resolve(pending.id, Verdict::Denied);
        assert_eq!(waiter.await.unwrap(), PermissionDecision::RejectOnce);

        // A dropped decision (cancelled turn) withdraws the confirmation and is audited.
        let fut = {
            let p = p.clone();
            tokio::spawn(async move { p.decide(&exec).await })
        };
        while store.pending().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        fut.abort();
        let _ = fut.await;
        assert!(store.pending().is_empty());

        let log = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<Value> = log
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let perms: Vec<&Value> = lines
            .iter()
            .filter(|l| l["action"] == "agent.permission")
            .collect();
        assert_eq!(perms.len(), 2, "{log}");
        assert!(perms[0]["detail"]
            .as_str()
            .unwrap()
            .starts_with("reject_once"));
        assert!(perms[1]["detail"]
            .as_str()
            .unwrap()
            .starts_with("cancelled"));
        assert!(perms.iter().all(|l| l["actor"] == "agent"));
        assert!(lines.iter().any(|l| l["action"] == "tool.denied"));
        std::fs::remove_file(&path).unwrap();
    }
}
