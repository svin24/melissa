//! Setup password notices: an app install generated an app's password, so the owner gets a
//! setup password card in the chat where the install ran (docs/DECISIONS.md, 2026-11-21).
//!
//! The install pipeline sends a [`SetupPasswordCreated`] here; the lead's forwarder turns it
//! into a chat event. It carries the app's name and login name only, never the password, and it
//! never reaches a model: it is not part of a tool result, and agents' context (recap, agent
//! status) is built from an allowlist of other events.

use crate::TaskId;

/// An install generated an app's password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupPasswordCreated {
    /// The task the install ran in (its chat gets the card).
    pub task: TaskId,
    pub app: String,
    /// The login name, if the app has one.
    pub login: Option<String>,
}

/// One-way channel from the install pipeline to the chats.
#[derive(Clone)]
pub struct SetupNotices {
    tx: tokio::sync::broadcast::Sender<SetupPasswordCreated>,
}

impl Default for SetupNotices {
    fn default() -> Self {
        Self::new()
    }
}

impl SetupNotices {
    pub fn new() -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(64);
        Self { tx }
    }

    /// Tell the chats. Nobody listening (no web UI) is fine.
    pub fn send(&self, notice: SetupPasswordCreated) {
        let _ = self.tx.send(notice);
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<SetupPasswordCreated> {
        self.tx.subscribe()
    }
}
