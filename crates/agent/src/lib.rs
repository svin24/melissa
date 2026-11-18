#![forbid(unsafe_code)]
//! Agent layer for Melissa.
//!
//! One interface ([`Backend`]) over pluggable agent backends: a built-in [`EchoBackend`]
//! (no LLM, for "dumb box" bees and tests) and a generic [`CliBackend`] that drives
//! headless agent CLIs such as Claude Code, Grok or OpenCode. The [`TaskBoard`]
//! dispatches prompts to backends, collects their output, supports cancellation and
//! timeouts, and writes every mutating action to an [`AuditLog`]. Long-lived agent sessions
//! ([`session`]) run over ACP ([`acp`], e.g. `grok agent stdio`); [`AcpBackend`] also serves
//! one-prompt tasks on the board. Agents act on the hive only
//! through the narrow [`tools`] API, whose policy decides when a human must confirm.

pub mod acp;
mod audit;
mod backend;
mod board;
mod cli;
mod echo;
pub mod mcp;
pub mod sandbox;
pub mod session;
pub mod tools;

pub use acp::{AcpBackend, AcpSpec};
pub use audit::{AuditLog, AuditRecord};
pub use backend::{
    AgentError, Backend, BackendInfo, BoxFuture, EventSink, McpEndpoint, OutputLine, RunRequest,
    Stream,
};
pub use board::{
    DispatchError, DispatchOptions, McpLink, TaskBoard, TaskId, TaskSnapshot, TaskStatus,
    MAX_PROMPT_BYTES,
};
pub use cli::{CliBackend, CliSpec, MCP_CONFIG_PLACEHOLDER};
pub use echo::EchoBackend;
pub use sandbox::SandboxSpec;
