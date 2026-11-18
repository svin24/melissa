#![forbid(unsafe_code)]
//! Agent layer for Melissa.
//!
//! One interface ([`Backend`]) over pluggable agent backends: a built-in [`EchoBackend`]
//! (no LLM, for "dumb box" bees and tests) and a generic [`CliBackend`] that drives
//! headless agent CLIs such as Claude Code, Grok or OpenCode. The [`TaskBoard`]
//! dispatches prompts to backends, collects their output, supports cancellation and
//! timeouts, and writes every mutating action to an [`AuditLog`]. Agents act on the hive only
//! through the narrow [`tools`] API, whose policy decides when a human must confirm.

mod audit;
mod backend;
mod board;
mod cli;
mod echo;
pub mod tools;

pub use audit::{AuditLog, AuditRecord};
pub use backend::{AgentError, Backend, BackendInfo, BoxFuture, EventSink, OutputLine, Stream};
pub use board::{DispatchError, TaskBoard, TaskId, TaskSnapshot, TaskStatus, MAX_PROMPT_BYTES};
pub use cli::{CliBackend, CliSpec};
pub use echo::EchoBackend;
