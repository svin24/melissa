//! Built-in backend with no LLM. Echoes the prompt back; useful on combs without any
//! agent subscription and as a deterministic backend for tests and demos.

use std::time::Duration;

use crate::{AgentError, Backend, BackendInfo, BoxFuture, EventSink, RunRequest};

#[derive(Debug, Clone, Default)]
pub struct EchoBackend {
    /// Pause between output lines, to make streaming visible in demos.
    delay: Duration,
}

impl EchoBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_delay(delay: Duration) -> Self {
        Self { delay }
    }
}

impl Backend for EchoBackend {
    fn info(&self) -> BackendInfo {
        BackendInfo {
            name: "echo".into(),
            kind: "echo",
            detail: "built-in, no LLM: repeats the prompt".into(),
            available: true,
            tools: false,
        }
    }

    fn run<'a>(
        &'a self,
        req: &'a RunRequest,
        sink: EventSink,
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            for line in req.prompt.lines() {
                if !self.delay.is_zero() {
                    tokio::time::sleep(self.delay).await;
                }
                sink.stdout(format!("echo: {line}"));
            }
            Ok(())
        })
    }
}
