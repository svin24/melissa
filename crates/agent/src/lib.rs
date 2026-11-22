#![forbid(unsafe_code)]
//! Agent layer for Melissa.
//!
//! One interface ([`Backend`]) over pluggable agent backends: a built-in [`EchoBackend`]
//! (no LLM, for "dumb box" devices and tests) and a generic [`CliBackend`] that drives
//! headless agent CLIs such as Claude Code, Grok or OpenCode. The [`TaskBoard`]
//! dispatches prompts to backends, collects their output, supports cancellation and
//! timeouts, and writes every mutating action to an [`AuditLog`]. Long-lived agent sessions
//! ([`session`]) run over ACP ([`acp`], e.g. `grok agent stdio`, `opencode acp`) or Claude
//! Code's stream-json protocol ([`claude`]); both also serve one-prompt tasks on the board. Agents act on the cluster only
//! through the narrow [`tools`] API, whose policy decides when a human must confirm.

pub mod acp;
pub mod attachments;
mod audit;
mod backend;
mod board;
pub mod claude;
mod cli;
pub mod device;
mod echo;
pub mod egress;
pub mod files;
pub mod lead;
pub mod mcp;
pub mod migrate;
pub mod sandbox;
pub mod session;
pub mod tools;

pub use acp::{AcpBackend, AcpSpec};
pub use audit::{requester, AuditLog, AuditRecord, PERSON as AUDIT_PERSON, PERSON_ID};
pub use backend::{
    AgentError, Backend, BackendInfo, BoxFuture, EventSink, McpEndpoint, OutputLine, RunRequest,
    Stream,
};
pub use board::{
    DispatchError, DispatchOptions, McpLink, TaskBoard, TaskId, TaskSnapshot, TaskStatus,
    MAX_PROMPT_BYTES,
};
pub use claude::{ClaudeBackend, ClaudeSpec};
pub use cli::{CliBackend, CliSpec, MCP_CONFIG_PLACEHOLDER};
pub use echo::{EchoBackend, EchoSessionBackend, EchoSpec};
pub use egress::{EgressProxy, NetworkMode};
pub use sandbox::SandboxSpec;
