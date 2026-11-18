//! Long-lived agent sessions: one conversation with an agent across many prompts.
//!
//! A [`SessionBackend`] opens an [`AgentSession`] in a working directory with a set of MCP
//! servers (Melissa's tools) and a [`PermissionHandler`] that answers the agent's permission
//! requests. The session takes prompts one at a time; while a turn runs, the agent streams
//! typed [`SessionEvent`]s (text, thoughts, tool calls, plans, usage) and the turn ends with
//! a [`StopReason`]. A turn can be cancelled; dropping the session kills the agent.
//!
//! The Queen keeps one session open across many prompts; worker tasks use one prompt per
//! session ([`crate::AcpBackend`]). ACP ([`crate::acp`]) is the first implementation.
//!
//! The traits return [`BoxFuture`]s so they stay object safe (`Arc<dyn AgentSession>`), like
//! [`crate::Backend`]. Methods take `&self` so one task can wait for a turn while another
//! cancels it.

mod permissions;

use std::{path::PathBuf, sync::Arc};

use tokio::sync::mpsc;

use crate::{AgentError, BackendInfo, BoxFuture};

pub use permissions::{PermissionContext, PolicyPermissions};

/// What a session backend and the agent behind it support (from the protocol handshake).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionCapabilities {
    /// Protocol version both sides speak (ACP: 1).
    pub protocol_version: u16,
    /// Agent name and version, if it said.
    pub agent: Option<String>,
    /// Can reopen a stored session and replay its history.
    pub load: bool,
    /// Can reopen a stored session without replaying it.
    pub resume: bool,
    /// Can fork a session (not in stable ACP v1; always false there).
    pub fork: bool,
    /// Can close a session explicitly.
    pub close: bool,
    /// Accepts MCP servers over HTTP (Melissa's endpoint) in the session setup.
    pub mcp_http: bool,
}

/// An MCP server the agent should connect to for this session.
#[derive(Clone, PartialEq, Eq)]
pub struct McpServerSpec {
    /// Server name as the agent shows it (tool names are usually prefixed with it).
    pub name: String,
    /// HTTP endpoint (streamable HTTP, JSON responses).
    pub url: String,
    /// Extra HTTP headers (e.g. an authorization token).
    pub headers: Vec<(String, String)>,
}

impl std::fmt::Debug for McpServerSpec {
    // The URL and headers carry the session token; keep it out of logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServerSpec")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// Everything needed to open a session.
#[derive(Clone)]
pub struct OpenSession {
    /// Working directory for the agent (absolute; created by the caller).
    pub cwd: PathBuf,
    /// MCP servers that give the agent its tools. Empty: no tools.
    pub mcp_servers: Vec<McpServerSpec>,
    /// Answers the agent's permission requests.
    pub permissions: Arc<dyn PermissionHandler>,
}

/// An open session and the stream of its events. The stream ends when the session closes.
pub struct OpenedSession {
    pub session: Box<dyn AgentSession>,
    pub events: mpsc::UnboundedReceiver<SessionEvent>,
}

/// Why a turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
}

impl StopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EndTurn => "end_turn",
            Self::MaxTokens => "max_tokens",
            Self::MaxTurnRequests => "max_turn_requests",
            Self::Refusal => "refusal",
            Self::Cancelled => "cancelled",
        }
    }
}

/// State of a tool call as the agent reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl ToolStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "running",
            Self::Completed => "done",
            Self::Failed => "failed",
        }
    }

    pub fn is_finished(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

/// What kind of action a tool call is (ACP tool kinds). Drives the permission policy for the
/// agent's own built-in tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    SwitchMode,
    Other,
}

impl ToolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Edit => "edit",
            Self::Delete => "delete",
            Self::Move => "move",
            Self::Search => "search",
            Self::Execute => "execute",
            Self::Think => "think",
            Self::Fetch => "fetch",
            Self::SwitchMode => "switch_mode",
            Self::Other => "other",
        }
    }
}

/// One step of the agent's plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanStep {
    pub content: String,
    /// `pending`, `in_progress` or `completed`.
    pub status: &'static str,
}

/// Something that happened in a session. Unknown protocol updates become [`SessionEvent::Other`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// A piece of the agent's reply (not line-buffered).
    Text(String),
    /// A piece of the agent's reasoning.
    Thought(String),
    /// The agent started a tool call.
    ToolStarted {
        id: String,
        title: String,
        kind: ToolKind,
        status: ToolStatus,
    },
    /// A tool call changed (status or title), not yet finished.
    ToolUpdated {
        id: String,
        title: Option<String>,
        status: Option<ToolStatus>,
    },
    /// A tool call finished (completed or failed).
    ToolFinished {
        id: String,
        title: Option<String>,
        status: ToolStatus,
    },
    /// The agent's current plan (replaces the previous one).
    Plan(Vec<PlanStep>),
    /// Context window usage in tokens.
    Usage { used: u64, size: u64 },
    /// Melissa answered a permission request.
    Permission {
        tool_call_id: String,
        title: String,
        decision: PermissionDecision,
    },
    /// The current turn ended.
    TurnFinished(StopReason),
    /// A protocol update Melissa does not interpret (its kind, for logs).
    Other(String),
}

/// A permission request from the agent, in protocol-neutral form.
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRequest {
    pub tool_call_id: String,
    /// What the agent wants to do, as it describes it (e.g. "Execute `ls`").
    pub title: String,
    pub kind: ToolKind,
    /// The tool's name if the agent gave one (MCP tools: usually `<server>__<tool>` or similar).
    pub tool_name: Option<String>,
    /// The tool input as the agent reported it (only hashed into the audit log).
    pub raw_input: serde_json::Value,
}

/// Melissa's answer to a permission request. There is deliberately no "always" answer:
/// persistent grants would live in the agent's own config, outside policy and audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    /// Allow this one call.
    AllowOnce,
    /// Refuse this one call; the agent may continue the turn.
    RejectOnce,
    /// No answer (turn cancelled, confirmation timed out or withdrawn).
    Cancelled,
}

impl PermissionDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AllowOnce => "allow_once",
            Self::RejectOnce => "reject_once",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Decides permission requests for one session. Called off the protocol's dispatch loop,
/// so it may wait (e.g. for a human). If the turn is cancelled meanwhile, the future is
/// dropped and the request is answered `cancelled`.
pub trait PermissionHandler: Send + Sync {
    fn decide<'a>(&'a self, req: &'a PermissionRequest) -> BoxFuture<'a, PermissionDecision>;
}

/// An open session with an agent.
pub trait AgentSession: Send + Sync {
    /// The agent's id for this session (used to resume it later, where supported).
    fn id(&self) -> &str;

    fn capabilities(&self) -> &SessionCapabilities;

    /// Run one turn. Returns when the agent ends the turn. Only one turn runs at a time;
    /// a second call while one runs fails with [`AgentError::Busy`].
    fn prompt<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<StopReason, AgentError>>;

    /// Ask the agent to stop the current turn. Pending permission requests are answered
    /// `cancelled`; the running [`AgentSession::prompt`] then returns
    /// [`StopReason::Cancelled`]. A no-op when no turn runs.
    fn cancel(&self) -> BoxFuture<'_, Result<(), AgentError>>;

    /// End the session and stop the agent process. Later calls fail with
    /// [`AgentError::SessionClosed`]. Dropping the session also stops the agent (by force).
    fn close(&self) -> BoxFuture<'_, Result<(), AgentError>>;
}

/// Something that can open agent sessions (e.g. an ACP agent command).
pub trait SessionBackend: Send + Sync {
    fn info(&self) -> BackendInfo;

    fn open(&self, req: OpenSession) -> BoxFuture<'_, Result<OpenedSession, AgentError>>;
}
