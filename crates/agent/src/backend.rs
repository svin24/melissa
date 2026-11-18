//! The backend trait and the types backends use to report output.

use std::{future::Future, pin::Pin, time::Duration};

use tokio::sync::mpsc;

/// A boxed, sendable future. Used so [`Backend`] stays object safe (`Arc<dyn Backend>`).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Static description of a backend, shown in the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendInfo {
    /// Unique name used to address the backend (e.g. `claude`).
    pub name: String,
    /// Backend family (e.g. `echo`, `cli`).
    pub kind: &'static str,
    /// Human-readable detail, such as the command line template.
    pub detail: String,
    /// Whether the backend can run on this bee right now (e.g. its binary was found).
    pub available: bool,
}

/// Errors a backend can report while running a task.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("backend is not available on this bee: {0}")]
    Unavailable(String),
    #[error("prompt rejected: {0}")]
    BadPrompt(&'static str),
    #[error("failed to start `{command}`: {source}")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("i/o error while running backend: {0}")]
    Io(#[from] std::io::Error),
    #[error("backend exited unsuccessfully ({0})")]
    Exit(String),
    #[error("backend timed out after {0:?}")]
    Timeout(Duration),
}

/// Which output stream a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// One line of backend output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputLine {
    pub stream: Stream,
    pub text: String,
}

/// Where a running backend sends its output. Cheap to clone.
#[derive(Debug, Clone)]
pub struct EventSink {
    tx: mpsc::UnboundedSender<OutputLine>,
}

impl EventSink {
    /// Create a sink and the receiver that collects its output.
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<OutputLine>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }

    pub fn stdout(&self, text: impl Into<String>) {
        self.send(Stream::Stdout, text.into());
    }

    pub fn stderr(&self, text: impl Into<String>) {
        self.send(Stream::Stderr, text.into());
    }

    fn send(&self, stream: Stream, text: String) {
        // A closed receiver means the task was cancelled or finished; dropping output is fine.
        let _ = self.tx.send(OutputLine { stream, text });
    }
}

/// An agent backend. Implementations must be cancel safe: dropping the future returned
/// by [`Backend::run`] must stop the work (e.g. kill the child process).
pub trait Backend: Send + Sync {
    fn info(&self) -> BackendInfo;

    /// Run one prompt to completion, streaming output into `sink`.
    fn run<'a>(&'a self, prompt: &'a str, sink: EventSink)
        -> BoxFuture<'a, Result<(), AgentError>>;
}
