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
//! Fetches ([`ToolKind::Fetch`]) depend on how the agent reaches the web ([`WebAccess`], set
//! by the backend when the agent starts): through Melissa's proxy, web reading is read-only
//! and public-only, so a fetch is not an outbound channel the policy has to stop (the proxy
//! is), but it does read untrusted content: every allowed fetch taints the task. A fetch the
//! proxy would refuse (a private address, a local name, another port, web reading off, no
//! network) is refused here with the reason, instead of failing later with a misleading
//! error inside the CLI.
//!
//! Every answer is audited as `agent.permission` (actor `agent`, input hashed). Answers are
//! always one-off; "always" grants are never given to the CLI (see [`PermissionDecision`]):
//! the owner's "Always allow" rules live in Melissa and are checked on every request
//! ([`ToolRegistry::confirm`]).

use std::sync::{Arc, Mutex};

use serde_json::Value;

use super::{PermissionDecision, PermissionHandler, PermissionRequest, ToolKind};
use crate::{
    egress,
    mcp::{McpServer, TaintSource},
    tools::{
        policy::{self, Decision, Effect, Labels, TaskContext},
        RuleOffer, ToolError, ToolRegistry,
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

/// How an agent reaches the web, for its fetch requests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum WebAccess {
    /// Not told (a backend that never fetches): only the URL checks, fetches labelled as
    /// outbound calls.
    #[default]
    Unknown,
    /// Through Melissa's egress proxy with web reading on: public hosts on these ports,
    /// read-only, audited.
    ReadOnly {
        https_ports: Vec<u16>,
        http_ports: Vec<u16>,
    },
    /// Through the egress proxy with web reading off: only the backend's own service hosts.
    Off,
    /// No network at all (`network = "none"`).
    NoNetwork,
    /// The host's network, unfiltered (`network = "host"`, or no sandbox).
    Unfiltered,
}

impl WebAccess {
    /// What a sandbox with `network` and the proxy's `web` settings gets.
    pub fn for_network(
        sandboxed: bool,
        network: egress::NetworkMode,
        web: Option<&egress::WebRead>,
    ) -> Self {
        use egress::NetworkMode;
        match (sandboxed, network) {
            (false, _) | (true, NetworkMode::Host) => Self::Unfiltered,
            (true, NetworkMode::None) => Self::NoNetwork,
            // Learn mode lets every public host through (a diagnostic mode): no port rule.
            (true, NetworkMode::Learn) => Self::Unknown,
            (true, NetworkMode::Proxy) => match web {
                Some(w) if w.enabled => Self::ReadOnly {
                    https_ports: w.https_ports.clone(),
                    http_ports: w.http_ports.clone(),
                },
                _ => Self::Off,
            },
        }
    }
}

/// [`PermissionHandler`] backed by Melissa's policy, confirmation inbox and audit log.
#[derive(Clone)]
pub struct PolicyPermissions {
    registry: ToolRegistry,
    ctx: PermissionContext,
    web: Arc<Mutex<WebAccess>>,
}

impl PolicyPermissions {
    pub fn new(registry: ToolRegistry, ctx: PermissionContext) -> Self {
        Self {
            registry,
            ctx,
            web: Arc::new(Mutex::new(WebAccess::default())),
        }
    }

    fn web_access(&self) -> WebAccess {
        self.web.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The task read (or is about to read) untrusted content from the web.
    fn taint(&self, req: &PermissionRequest) {
        if let Some((server, token)) = &self.ctx.mcp {
            let host = fetch_url(req)
                .and_then(|u| egress::url_target(&u))
                .map(|t| t.1);
            server.taint_session_with(
                token,
                TaintSource {
                    tool: Some("web".into()),
                    subject: host,
                },
            );
        }
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
        let Some(mut labels) = labels_for(req.kind) else {
            return (
                PermissionDecision::RejectOnce,
                "agents may not change their own permission mode".into(),
            );
        };
        if req.kind == ToolKind::Fetch {
            let web = self.web_access();
            if let Some(reason) = fetch_blocked(&web, fetch_url(req).as_deref()) {
                return (
                    PermissionDecision::RejectOnce,
                    format!("blocked by Melissa's network policy: {reason}"),
                );
            }
            if matches!(web, WebAccess::ReadOnly { .. }) {
                // The proxy keeps it read-only (GET/HEAD, upload-capped tunnels, public
                // hosts): reading on in a tainted task needs no OK; its changes still do.
                labels.egress = false;
            }
        }
        let ctx = self.task_context();
        let name = format!("agent:{}", req.kind.as_str());
        let fetch = req.kind == ToolKind::Fetch;
        match policy::decide(&name, &labels, &ctx) {
            Decision::Allow => {
                if fetch {
                    self.taint(req);
                }
                (PermissionDecision::AllowOnce, "policy: allowed".into())
            }
            Decision::Confirm(reason) => {
                let args = serde_json::json!({"title": req.title, "input": req.raw_input});
                // "Always allow" for an agent's own tool covers that kind for that agent
                // (e.g. Ada may run commands in her sandbox).
                let rule = RuleOffer::new(&ctx.scope, &name, None);
                match self
                    .registry
                    .confirm_within(self.ctx.task, rule, &name, &args, &reason, ctx.confirm_wait)
                    .await
                {
                    Ok(()) => {
                        if fetch {
                            self.taint(req);
                        }
                        (PermissionDecision::AllowOnce, format!("approved: {reason}"))
                    }
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

    fn set_web_access(&self, web: WebAccess) {
        *self.web.lock().unwrap_or_else(|p| p.into_inner()) = web;
    }
}

/// The URL a fetch request is for: the `url` (or `uri`, `href`) argument, else any
/// argument or title word that looks like an http(s) URL. `None` for e.g. a web search.
fn fetch_url(req: &PermissionRequest) -> Option<String> {
    let looks = |s: &str| {
        let l = s.trim().to_ascii_lowercase();
        l.starts_with("http://") || l.starts_with("https://")
    };
    if let Value::Object(args) = &req.raw_input {
        for key in ["url", "uri", "href"] {
            if let Some(u) = args.get(key).and_then(Value::as_str) {
                return Some(u.trim().to_string());
            }
        }
        if let Some(u) = args.values().filter_map(Value::as_str).find(|s| looks(s)) {
            return Some(u.trim().to_string());
        }
    }
    req.title
        .split_whitespace()
        .find(|w| looks(w))
        .map(str::to_string)
}

/// Why a fetch of `url` cannot succeed with this web access, if it cannot.
fn fetch_blocked(web: &WebAccess, url: Option<&str>) -> Option<String> {
    match web {
        WebAccess::Unfiltered => return None,
        WebAccess::NoNetwork => {
            return Some("this agent has no network access (network = \"none\")".into())
        }
        WebAccess::Off => {
            return Some(
                "web reading is off (server.toml: [egress] web_read = false); only the \
                 agent's own service is reachable"
                    .into(),
            )
        }
        WebAccess::Unknown | WebAccess::ReadOnly { .. } => {}
    }
    let url = url?;
    let Some((scheme, host, port)) = egress::url_target(url) else {
        // Not an absolute URL: let the CLI say what it makes of it.
        return None;
    };
    if scheme != "http" && scheme != "https" {
        return Some(format!(
            "{url}: only http:// and https:// pages can be read"
        ));
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if !egress::is_public(ip) {
            return Some(format!(
                "{host} is a private or local address; web reading reaches public hosts only"
            ));
        }
    } else if egress::local_name(&host) {
        return Some(format!(
            "{host} is a local name; web reading reaches public hosts only"
        ));
    }
    if let WebAccess::ReadOnly {
        https_ports,
        http_ports,
    } = web
    {
        let ports = if scheme == "https" {
            https_ports
        } else {
            http_ports
        };
        if !ports.contains(&port) {
            return Some(format!(
                "{host}:{port}: port {port} is not open for web reading ({scheme} uses {})",
                ports
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    None
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
/// different ways (`melissa__device_info`, `mcp__melissa__device_info`, `melissa/device_info`,
/// `device_info (melissa)`, OpenCode's `melissa_device_info`), so accept the server name and the
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
    use crate::tools::{DeviceInfo, Tool, Verdict};

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
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(DeviceInfo::new())];
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
            "melissa__device_info",
            "mcp__melissa__device_info",
            "melissa/device_info",
            "device_info (melissa)",
            "MCP: melissa.device_info",
            "melissa_device_info",
        ] {
            assert!(
                names_mcp_tool(&label.to_lowercase(), "melissa", "device_info"),
                "{label}"
            );
        }
        for label in [
            "device_info",
            "melissa__device_infox",
            "other__device_info",
            "melissa_device_infox",
            "xmelissa_device_info",
        ] {
            assert!(!names_mcp_tool(label, "melissa", "device_info"), "{label}");
        }
    }

    #[tokio::test]
    async fn melissa_tools_and_reads_are_allowed_once() {
        let p = perms(None, Duration::from_secs(5));
        let mcp = request(ToolKind::Other, "melissa__device_info", None);
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
        let exec = request(ToolKind::Execute, "Execute `melissa__device_info`", None);
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
    fn fetch(url: &str) -> PermissionRequest {
        PermissionRequest {
            tool_call_id: "f1".into(),
            title: format!("Fetch: {url}"),
            kind: ToolKind::Fetch,
            tool_name: Some("web_fetch".into()),
            raw_input: serde_json::json!({"url": url}),
        }
    }

    fn read_only() -> WebAccess {
        WebAccess::ReadOnly {
            https_ports: vec![443],
            http_ports: vec![80],
        }
    }

    #[test]
    fn web_access_follows_the_sandbox_network() {
        use egress::{NetworkMode, WebRead};
        let on = WebRead::on();
        let off = WebRead::default();
        assert_eq!(
            WebAccess::for_network(true, NetworkMode::Proxy, Some(&on)),
            read_only()
        );
        assert_eq!(
            WebAccess::for_network(true, NetworkMode::Proxy, Some(&off)),
            WebAccess::Off
        );
        assert_eq!(
            WebAccess::for_network(true, NetworkMode::Proxy, None),
            WebAccess::Off
        );
        assert_eq!(
            WebAccess::for_network(true, NetworkMode::None, Some(&on)),
            WebAccess::NoNetwork
        );
        assert_eq!(
            WebAccess::for_network(true, NetworkMode::Host, Some(&on)),
            WebAccess::Unfiltered
        );
        assert_eq!(
            WebAccess::for_network(false, NetworkMode::Proxy, Some(&on)),
            WebAccess::Unfiltered
        );
    }

    #[test]
    fn fetches_that_cannot_succeed_are_named() {
        let ro = read_only();
        let blocked = |web: &WebAccess, url: &str| fetch_blocked(web, Some(url));
        assert_eq!(blocked(&ro, "https://example.com/a"), None);
        assert_eq!(blocked(&ro, "http://example.com/"), None);
        for (url, want) in [
            ("http://10.0.0.1/", "10.0.0.1 is a private or local address"),
            ("https://[::1]/", "::1 is a private or local address"),
            (
                "https://[::ffff:192.168.1.1]/",
                "is a private or local address",
            ),
            (
                "https://169.254.169.254/latest",
                "is a private or local address",
            ),
            ("https://100.100.100.100/", "is a private or local address"),
            ("https://localhost/", "localhost is a local name"),
            ("https://nas.local/", "nas.local is a local name"),
            (
                "https://example.com:8443/",
                "port 8443 is not open for web reading",
            ),
            (
                "http://example.com:8080/",
                "port 8080 is not open for web reading",
            ),
            ("ftp://example.com/", "only http:// and https://"),
        ] {
            let why = blocked(&ro, url).unwrap_or_default();
            assert!(why.contains(want), "{url}: {why}");
        }
        // Unknown access: same address rules, no port rule.
        assert_eq!(
            blocked(&WebAccess::Unknown, "https://example.com:8443/"),
            None
        );
        assert!(blocked(&WebAccess::Unknown, "https://10.1.2.3/").is_some());
        // The host's network: Melissa does not filter.
        assert_eq!(blocked(&WebAccess::Unfiltered, "http://10.0.0.1/"), None);
        assert!(fetch_blocked(&WebAccess::Off, None)
            .unwrap()
            .contains("web reading is off"));
        assert!(fetch_blocked(&WebAccess::NoNetwork, None)
            .unwrap()
            .contains("no network access"));
        // No URL (a web search): nothing to check.
        assert_eq!(fetch_blocked(&ro, None), None);
    }

    #[test]
    fn the_fetch_url_is_found_in_arguments_or_title() {
        let mut r = fetch("https://a.example/x");
        assert_eq!(fetch_url(&r).as_deref(), Some("https://a.example/x"));
        r.raw_input = serde_json::json!({"format": "markdown", "target": "http://b.example/"});
        assert_eq!(fetch_url(&r).as_deref(), Some("http://b.example/"));
        r.raw_input = serde_json::json!({"query": "rust async"});
        r.title = "Search: rust async".into();
        assert_eq!(fetch_url(&r), None);
        r.title = "Fetch: https://c.example/".into();
        assert_eq!(fetch_url(&r).as_deref(), Some("https://c.example/"));
    }

    #[tokio::test]
    async fn read_only_fetches_taint_and_stay_allowed_after() {
        let path =
            std::env::temp_dir().join(format!("melissa-perm-web-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let p = perms(Some(crate::AuditLog::new(&path)), Duration::from_millis(20));
        p.set_web_access(read_only());
        let (server, token) = p.ctx.mcp.clone().unwrap();
        assert!(!server.session_context(&token).unwrap().tainted);

        assert_eq!(
            p.decide(&fetch("https://example.com/")).await,
            PermissionDecision::AllowOnce
        );
        assert!(server.session_context(&token).unwrap().tainted);
        assert_eq!(
            server.taint_source(&token),
            Some(TaintSource {
                tool: Some("web".into()),
                subject: Some("example.com".into()),
            })
        );
        // Reading on is fine; changing things now needs a human (times out here).
        assert_eq!(
            p.decide(&fetch("https://docs.example.org/")).await,
            PermissionDecision::AllowOnce
        );
        let edit = request(ToolKind::Edit, "Edit notes.txt", None);
        assert_eq!(p.decide(&edit).await, PermissionDecision::Cancelled);
        // The first source is kept.
        assert_eq!(
            server.taint_source(&token).unwrap().subject.as_deref(),
            Some("example.com")
        );

        // Refused with the reason, audited.
        assert_eq!(
            p.decide(&fetch("http://192.168.1.1/admin")).await,
            PermissionDecision::RejectOnce
        );
        let log = std::fs::read_to_string(&path).unwrap();
        assert!(log.contains(
            "reject_once (fetch): blocked by Melissa's network policy: 192.168.1.1 is a private or local address"
        ));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn without_proxied_web_reading_a_tainted_fetch_still_needs_a_human() {
        let p = perms(None, Duration::from_millis(20));
        // Not told (Unknown): the old labels; the first fetch taints.
        assert_eq!(
            p.decide(&fetch("https://example.com/")).await,
            PermissionDecision::AllowOnce
        );
        assert_eq!(
            p.decide(&fetch("https://example.com/2")).await,
            PermissionDecision::Cancelled
        );
        p.set_web_access(WebAccess::Off);
        assert_eq!(
            p.decide(&fetch("https://example.com/")).await,
            PermissionDecision::RejectOnce
        );
    }
}
