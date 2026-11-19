//! Built-in backend with no LLM. Echoes the prompt back; useful on devices without any
//! agent subscription and as a deterministic backend for tests and demos.

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use tokio::sync::mpsc;

use crate::{
    session::{
        AgentSession, OpenSession, OpenedSession, SessionBackend, SessionCapabilities,
        SessionEvent, StopReason,
    },
    AgentError, Backend, BackendInfo, BoxFuture, EventSink, RunRequest,
};

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

/// Configuration of an echo session backend (`kind = "echo"` in the lead agent config): a
/// conversation partner with no LLM, for demos and for trying the chat (and switching
/// between backends) without any agent subscription.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EchoSpec {
    /// Unique backend name (not `echo`, which is the built-in one-shot backend).
    pub name: String,
}

/// An echo session backend: each session repeats the user's messages. It says how much
/// earlier conversation Melissa handed it (after a switch or a lost session), which makes
/// the context hand-over visible. Never resumes; has no models and no tools.
#[derive(Debug, Clone)]
pub struct EchoSessionBackend {
    name: String,
    next: Arc<AtomicU64>,
}

impl EchoSessionBackend {
    pub fn new(spec: &EchoSpec) -> Self {
        Self {
            name: spec.name.clone(),
            next: Arc::new(AtomicU64::new(1)),
        }
    }
}

impl Backend for EchoSessionBackend {
    fn info(&self) -> BackendInfo {
        SessionBackend::info(self)
    }

    fn run<'a>(
        &'a self,
        req: &'a RunRequest,
        sink: EventSink,
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(async move { EchoBackend::new().run(req, sink).await })
    }
}

impl SessionBackend for EchoSessionBackend {
    fn info(&self) -> BackendInfo {
        BackendInfo {
            name: self.name.clone(),
            kind: "echo",
            detail: "built-in, no LLM: repeats your messages".into(),
            available: true,
            tools: false,
        }
    }

    fn open(&self, _req: OpenSession) -> BoxFuture<'_, Result<OpenedSession, AgentError>> {
        Box::pin(async move {
            let n = self.next.fetch_add(1, Ordering::SeqCst);
            let (tx, rx) = mpsc::unbounded_channel();
            Ok(OpenedSession {
                session: Box::new(EchoSession {
                    id: format!("{}-{n}", self.name),
                    caps: SessionCapabilities::default(),
                    events: Mutex::new(Some(tx)),
                }),
                events: rx,
                resumed: false,
                models: None,
            })
        })
    }
}

struct EchoSession {
    id: String,
    caps: SessionCapabilities,
    events: Mutex<Option<mpsc::UnboundedSender<SessionEvent>>>,
}

/// The user's message inside a Melissa prompt (after the system prompt and any recap), and
/// how many lines of earlier conversation came with it.
fn echo_reply(prompt: &str) -> String {
    let (head, message) = match prompt.rsplit_once("The message:\n\n") {
        Some((head, message)) => (head, message),
        None => ("", prompt),
    };
    let earlier = head
        .split_once("<earlier-conversation>\n")
        .and_then(|(_, rest)| rest.split_once("</earlier-conversation>"))
        .map_or(0, |(lines, _)| {
            lines
                .lines()
                .filter(|l| l.starts_with("user: ") || l.starts_with("you: "))
                .count()
        });
    let mut reply = format!("echo: {}", message.trim());
    if earlier > 0 {
        reply.push_str(&format!(
            "\n\n(Melissa handed me {earlier} earlier messages of this chat.)"
        ));
    }
    reply
}

impl AgentSession for EchoSession {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &SessionCapabilities {
        &self.caps
    }

    fn prompt<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<StopReason, AgentError>> {
        Box::pin(async move {
            let guard = self
                .events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(tx) = guard.as_ref() else {
                return Err(AgentError::SessionClosed);
            };
            let _ = tx.send(SessionEvent::Text(echo_reply(text)));
            let _ = tx.send(SessionEvent::TurnFinished(StopReason::EndTurn));
            Ok(StopReason::EndTurn)
        })
    }

    fn cancel(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async { Ok(()) })
    }

    fn close(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async move {
            self.events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::echo_reply;

    #[test]
    fn echo_reply_repeats_the_message_and_counts_the_recap() {
        assert_eq!(echo_reply("hi"), "echo: hi");
        let p = "<melissa-system-prompt>\nx\n</melissa-system-prompt>\n\nEarlier conversation, \
                 restored:\n<earlier-conversation>\nuser: a\nyou: b\n</earlier-conversation>\n\n\
                 The message:\n\nhello";
        assert_eq!(
            echo_reply(p),
            "echo: hello\n\n(Melissa handed me 2 earlier messages of this chat.)"
        );
    }
}
