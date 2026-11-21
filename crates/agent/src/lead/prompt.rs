//! A conversation's system prompt, built when a session needs it.
//!
//! The prompt names the agent and the lead agent, and the user can rename either at any time.
//! So the conversation asks for the text each time it primes a new session (the first message
//! of a session, after a fresh start, a condensed context or a backend switch) instead of
//! keeping one string from when it was made: a rename reaches the model with its next
//! session. The open session is told with a note on its next turn (see
//! [`super::agents::AgentPool::rename_lead`]).

use std::sync::Arc;

/// Builds the system prompt. Cheap to clone.
#[derive(Clone)]
pub(crate) struct SystemPrompt(Arc<dyn Fn() -> String + Send + Sync>);

impl SystemPrompt {
    pub fn new(build: impl Fn() -> String + Send + Sync + 'static) -> Self {
        Self(Arc::new(build))
    }

    /// The prompt as it is now.
    pub fn text(&self) -> String {
        (self.0)()
    }
}

/// The lead agent's prompt with its current name (`{lead_name}` in `prompts/lead.md`).
pub(crate) fn with_lead_name(template: &str, name: &str) -> String {
    template.replace("{lead_name}", name)
}
