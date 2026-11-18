//! Minimal MCP (Model Context Protocol) server exposing the [`ToolRegistry`] to agent CLIs.
//!
//! Transport: JSON-RPC 2.0 over HTTP POST (the MCP "streamable HTTP" transport, JSON responses
//! only, no server-initiated streams). Each task gets its own session token, embedded in the
//! URL the agent CLI is given; the token selects the task's policy context. Methods handled:
//! `initialize`, `ping`, `tools/list`, `tools/call`; of the notifications only
//! `notifications/cancelled` does something (it stops that session's call with the given
//! request id, which withdraws a confirmation it waits for); the others are accepted and
//! ignored.

use std::{
    collections::HashMap,
    io::Read,
    sync::{Arc, Mutex},
};

use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::{
    tools::{TaskContext, ToolRegistry},
    TaskId,
};

/// MCP protocol revision we implement.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

struct Session {
    task: TaskId,
    ctx: TaskContext,
    /// Tools this task may see (None = all).
    allow: Option<Vec<&'static str>>,
}

/// Shared MCP server state. Cheap to clone.
#[derive(Clone)]
pub struct McpServer {
    registry: ToolRegistry,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    /// `tools/call`s in flight by (session token, JSON-RPC id as JSON text), so a client's
    /// `notifications/cancelled` can stop one.
    inflight: Arc<InflightMap>,
}

/// Cancel handles of calls in flight, by (session token, JSON-RPC id as JSON text).
type InflightMap = Mutex<HashMap<(String, String), oneshot::Sender<()>>>;

/// JSON-RPC error code MCP uses for a request the client cancelled.
const REQUEST_CANCELLED: i64 = -32800;

/// Removes a call's cancel handle when the call ends (finished, failed or dropped).
struct Inflight<'a> {
    map: &'a InflightMap,
    key: (String, String),
}

impl Drop for Inflight<'_> {
    fn drop(&mut self) {
        lock(self.map).remove(&self.key);
    }
}

impl McpServer {
    pub fn new(registry: ToolRegistry) -> Self {
        Self {
            registry,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            inflight: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    /// Open a session for `task`; returns the secret token for its URL.
    pub fn open_session(
        &self,
        task: TaskId,
        ctx: TaskContext,
        allow: Option<Vec<&'static str>>,
    ) -> std::io::Result<String> {
        let token = random_token()?;
        lock(&self.sessions).insert(token.clone(), Session { task, ctx, allow });
        Ok(token)
    }

    pub fn close_session(&self, token: &str) {
        lock(&self.sessions).remove(token);
    }

    pub fn has_session(&self, token: &str) -> bool {
        lock(&self.sessions).contains_key(token)
    }

    /// The current policy context of a session (it changes, e.g. once the task is tainted).
    pub fn session_context(&self, token: &str) -> Option<TaskContext> {
        lock(&self.sessions).get(token).map(|s| s.ctx.clone())
    }

    /// Mark a session tainted (it was handed untrusted content outside a tool call, e.g. a
    /// message from a tainted Queen). Returns false for an unknown session.
    pub fn taint_session(&self, token: &str) -> bool {
        match lock(&self.sessions).get_mut(token) {
            Some(s) => {
                s.ctx.tainted = true;
                true
            }
            None => false,
        }
    }

    /// The tool allowlist of a session (None = no limit beyond the policy context).
    pub fn session_allow(&self, token: &str) -> Option<Option<Vec<&'static str>>> {
        lock(&self.sessions).get(token).map(|s| s.allow.clone())
    }

    /// Handle one JSON-RPC message (or batch). Returns `None` when there is nothing to send
    /// back (notifications only), and `Err(UnknownSession)` for an unknown session token.
    pub async fn handle(
        &self,
        token: &str,
        message: Value,
    ) -> Result<Option<Value>, UnknownSession> {
        if !self.has_session(token) {
            return Err(UnknownSession);
        }
        if let Value::Array(batch) = message {
            let mut out = Vec::new();
            for m in batch {
                if let Some(r) = self.handle_one(token, m).await {
                    out.push(r);
                }
            }
            return Ok((!out.is_empty()).then_some(Value::Array(out)));
        }
        Ok(self.handle_one(token, message).await)
    }

    async fn handle_one(&self, token: &str, msg: Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        if method == "notifications/cancelled" && id.is_none() {
            // An agent's MCP client gave up on a call (e.g. its own request timeout). Stop
            // it, so a confirmation it waits for leaves the inbox instead of running later
            // for nobody.
            if let Some(request) = params.get("requestId") {
                let key = (token.to_string(), request.to_string());
                if let Some(cancel) = lock(&self.inflight).remove(&key) {
                    let _ = cancel.send(());
                }
            }
            return None;
        }
        // Notifications (no id) never get a response.
        let id = id?;
        let result = match method.as_str() {
            "initialize" => Ok(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "melissa", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "Tools to inspect and operate this Melissa hive. Some calls wait for a human to approve them."
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(self.tools_list(token)),
            "tools/call" => self.cancellable_call(token, &id, &params).await,
            _ => Err((-32601, format!("method not found: {method}"))),
        };
        Some(match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, message)) => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
            }
        })
    }

    fn tools_list(&self, token: &str) -> Value {
        let (ctx, allow) = match lock(&self.sessions).get(token) {
            Some(s) => (s.ctx.clone(), s.allow.clone()),
            None => (TaskContext::default(), Some(Vec::new())),
        };
        let tools: Vec<Value> = self
            .registry
            .visible_to(&ctx, allow.as_deref())
            .into_iter()
            .map(|s| json!({"name": s.name, "description": s.description, "inputSchema": s.params}))
            .collect();
        json!({"tools": tools})
    }

    /// `tools/call` that stops when the client sends `notifications/cancelled` for `id`.
    async fn cancellable_call(
        &self,
        token: &str,
        id: &Value,
        params: &Value,
    ) -> Result<Value, (i64, String)> {
        let (cancel, cancelled) = oneshot::channel();
        let key = (token.to_string(), id.to_string());
        lock(&self.inflight).insert(key.clone(), cancel);
        let _inflight = Inflight {
            map: &self.inflight,
            key,
        };
        tokio::select! {
            result = self.tools_call(token, params) => result,
            Ok(()) = cancelled => {
                // The call's future is dropped here: a pending confirmation is withdrawn.
                let task = lock(&self.sessions).get(token).map(|s| s.task);
                let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
                let args = params.get("arguments").cloned().unwrap_or(Value::Null);
                if let Some(task) = task {
                    self.registry.audit_agent(
                        task,
                        "",
                        "tool.cancelled",
                        name,
                        &args,
                        "the agent's MCP client cancelled the call",
                    );
                }
                tracing::info!(tool = name, "MCP client cancelled a tool call");
                Err((REQUEST_CANCELLED, "request cancelled by the client".into()))
            }
        }
    }

    async fn tools_call(&self, token: &str, params: &Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((-32602, "missing tool name".to_string()))?;
        let args = params.get("arguments").cloned().unwrap_or(Value::Null);
        // Copy the context out: a call may wait minutes for a confirmation, and the lock must
        // not be held across that await.
        let (task, mut ctx, allowed) = {
            let sessions = lock(&self.sessions);
            let s = sessions
                .get(token)
                .ok_or((-32001, "session closed".to_string()))?;
            let allowed = s.allow.as_ref().is_none_or(|a| a.contains(&name));
            (s.task, s.ctx.clone(), allowed)
        };
        if !allowed {
            // Same answer as for a nonexistent tool: hidden tools stay invisible.
            return Ok(tool_error(&format!("unknown tool `{name}`")));
        }
        let result = self.registry.call(task, &mut ctx, name, args).await;
        if ctx.tainted {
            if let Some(s) = lock(&self.sessions).get_mut(token) {
                s.ctx.tainted = true;
            }
        }
        Ok(match result {
            Ok(value) => {
                let text = serde_json::to_string_pretty(&value).unwrap_or_default();
                json!({"content": [{"type": "text", "text": text}], "structuredContent": value, "isError": false})
            }
            // Tool-level failures are results with isError, so the agent can react to them.
            Err(e) => tool_error(&e.to_string()),
        })
    }
}

/// The token in the URL does not match an open session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownSession;

fn tool_error(message: &str) -> Value {
    json!({"content": [{"type": "text", "text": message}], "isError": true})
}

/// 128-bit random token, hex encoded, from the kernel CSPRNG.
fn random_token() -> std::io::Result<String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> (McpServer, String) {
        let server = McpServer::new(ToolRegistry::builtin(None));
        let token = server
            .open_session(1, TaskContext::default(), None)
            .unwrap();
        (server, token)
    }

    async fn rpc(server: &McpServer, token: &str, method: &str, params: Value) -> Value {
        server
            .handle(
                token,
                json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}),
            )
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn initialize_and_list_tools() {
        let (server, token) = server();
        let init = rpc(
            &server,
            &token,
            "initialize",
            json!({"protocolVersion": PROTOCOL_VERSION}),
        )
        .await;
        assert_eq!(init["result"]["serverInfo"]["name"], "melissa");
        assert!(init["result"]["capabilities"]["tools"].is_object());
        let list = rpc(&server, &token, "tools/list", json!({})).await;
        assert_eq!(list["result"]["tools"][0]["name"], "bee_info");
        assert!(list["result"]["tools"][0]["inputSchema"].is_object());
    }

    #[tokio::test]
    async fn calls_a_tool() {
        let (server, token) = server();
        let out = rpc(
            &server,
            &token,
            "tools/call",
            json!({"name": "bee_info", "arguments": {}}),
        )
        .await;
        assert_eq!(out["result"]["isError"], false);
        assert!(out["result"]["structuredContent"].is_object());
        let bad = rpc(
            &server,
            &token,
            "tools/call",
            json!({"name": "nope", "arguments": {}}),
        )
        .await;
        assert_eq!(bad["result"]["isError"], true);
    }

    #[tokio::test]
    async fn notifications_unknown_methods_and_tokens() {
        let (server, token) = server();
        let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        assert_eq!(server.handle(&token, note).await, Ok(None));
        let unknown = rpc(&server, &token, "resources/list", json!({})).await;
        assert_eq!(unknown["error"]["code"], -32601);
        assert_eq!(server.handle("wrong", json!({})).await, Err(UnknownSession));
        server.close_session(&token);
        assert!(!server.has_session(&token));
    }

    #[tokio::test]
    async fn allow_list_hides_tools() {
        let server = McpServer::new(ToolRegistry::builtin(None));
        let token = server
            .open_session(1, TaskContext::default(), Some(vec![]))
            .unwrap();
        let list = rpc(&server, &token, "tools/list", json!({})).await;
        assert_eq!(list["result"]["tools"], json!([]));
        let call = rpc(&server, &token, "tools/call", json!({"name": "bee_info"})).await;
        assert_eq!(call["result"]["isError"], true);
    }

    #[test]
    fn tokens_are_random_hex() {
        let (a, b) = (random_token().unwrap(), random_token().unwrap());
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    struct Destructive;
    impl crate::tools::Tool for Destructive {
        fn spec(&self) -> crate::tools::ToolSpec {
            crate::tools::ToolSpec {
                name: "wipe",
                description: "test",
                labels: crate::tools::Labels {
                    effect: crate::tools::Effect::Destructive,
                    ..crate::tools::Labels::READ_ONLY
                },
                params: json!({"type": "object"}),
            }
        }
        fn call<'a>(
            &'a self,
            _args: Value,
        ) -> crate::BoxFuture<'a, Result<Value, crate::tools::ToolError>> {
            Box::pin(async { Ok(json!({"wiped": true})) })
        }
    }

    #[tokio::test]
    async fn cancelled_notification_stops_the_call_and_withdraws_its_confirmation() {
        let dir = std::env::temp_dir().join(format!("melissa-mcp-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let audit = crate::AuditLog::new(dir.join("audit.jsonl"));
        let server = McpServer::new(ToolRegistry::new(
            vec![std::sync::Arc::new(Destructive)],
            Some(audit.clone()),
        ));
        let token = server
            .open_session(5, TaskContext::default(), None)
            .unwrap();
        let inbox = server.registry().confirmations().clone();
        let call = {
            let (server, token) = (server.clone(), token.clone());
            tokio::spawn(async move {
                server
                    .handle(
                        &token,
                        json!({"jsonrpc": "2.0", "id": "req-7", "method": "tools/call",
                            "params": {"name": "wipe", "arguments": {}}}),
                    )
                    .await
            })
        };
        while inbox.pending().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        // Another session's cancel, or one for another id, changes nothing.
        let (other_server, other) = (
            server.clone(),
            server
                .open_session(6, TaskContext::default(), None)
                .unwrap(),
        );
        for (t, id) in [(other.as_str(), json!("req-7")), (token.as_str(), json!(7))] {
            let none = other_server
                .handle(
                    t,
                    json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
                        "params": {"requestId": id, "reason": "timeout"}}),
                )
                .await
                .unwrap();
            assert!(none.is_none(), "notifications get no answer");
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(inbox.pending().len(), 1);
        server
            .handle(
                &token,
                json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
                    "params": {"requestId": "req-7", "reason": "timeout"}}),
            )
            .await
            .unwrap();
        let answer = call.await.unwrap().unwrap().unwrap();
        assert_eq!(answer["id"], "req-7");
        assert_eq!(answer["error"]["code"], REQUEST_CANCELLED);
        assert!(inbox.pending().is_empty(), "confirmation withdrawn");
        assert!(lock(&server.inflight).is_empty());
        let log = std::fs::read_to_string(audit.path()).unwrap();
        assert!(log.contains("tool.cancelled"), "{log}");
        assert!(!log.contains("tool.call"), "the tool never ran: {log}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
