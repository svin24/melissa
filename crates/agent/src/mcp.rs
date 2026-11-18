//! Minimal MCP (Model Context Protocol) server exposing the [`ToolRegistry`] to agent CLIs.
//!
//! Transport: JSON-RPC 2.0 over HTTP POST (the MCP "streamable HTTP" transport, JSON responses
//! only, no server-initiated streams). Each task gets its own session token, embedded in the
//! URL the agent CLI is given; the token selects the task's policy context. Methods handled:
//! `initialize`, `ping`, `tools/list`, `tools/call`; notifications are accepted and ignored.

use std::{
    collections::HashMap,
    io::Read,
    sync::{Arc, Mutex},
};

use serde_json::{json, Value};

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
}

impl McpServer {
    pub fn new(registry: ToolRegistry) -> Self {
        Self {
            registry,
            sessions: Arc::new(Mutex::new(HashMap::new())),
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
            "tools/call" => self.tools_call(token, &params).await,
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
}
