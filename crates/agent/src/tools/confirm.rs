//! Pending confirmations: tool calls waiting for a human to approve or deny them.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::SystemTime,
};

use serde_json::Value;
use tokio::sync::oneshot;

use crate::TaskId;

pub type ConfirmationId = u64;

/// A tool call waiting for a human decision. Shown in the UI's confirmation inbox.
#[derive(Debug, Clone, PartialEq)]
pub struct Confirmation {
    pub id: ConfirmationId,
    pub task: TaskId,
    pub tool: String,
    pub args: Value,
    /// Why the policy asked (e.g. "destructive action").
    pub reason: String,
    pub created: SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Approved,
    Denied,
}

struct Pending {
    info: Confirmation,
    tx: oneshot::Sender<Verdict>,
}

#[derive(Default)]
struct Inner {
    next_id: ConfirmationId,
    pending: BTreeMap<ConfirmationId, Pending>,
}

/// Shared store of pending confirmations. Cheap to clone.
#[derive(Clone, Default)]
pub struct ConfirmationStore {
    inner: Arc<Mutex<Inner>>,
}

impl ConfirmationStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a confirmation; the receiver resolves when a human decides.
    pub fn request(
        &self,
        task: TaskId,
        tool: &str,
        args: &Value,
        reason: &str,
    ) -> (Confirmation, oneshot::Receiver<Verdict>) {
        let (tx, rx) = oneshot::channel();
        let mut inner = lock(&self.inner);
        inner.next_id += 1;
        let info = Confirmation {
            id: inner.next_id,
            task,
            tool: tool.into(),
            args: args.clone(),
            reason: reason.into(),
            created: SystemTime::now(),
        };
        inner.pending.insert(
            info.id,
            Pending {
                info: info.clone(),
                tx,
            },
        );
        (info, rx)
    }

    /// Pending confirmations, oldest first.
    pub fn pending(&self) -> Vec<Confirmation> {
        lock(&self.inner)
            .pending
            .values()
            .map(|p| p.info.clone())
            .collect()
    }

    /// Record a human decision. Returns the confirmation, or `None` if it no longer exists.
    pub fn resolve(&self, id: ConfirmationId, verdict: Verdict) -> Option<Confirmation> {
        let pending = lock(&self.inner).pending.remove(&id)?;
        // The waiting call may have timed out or been cancelled; then nobody listens.
        let _ = pending.tx.send(verdict);
        Some(pending.info)
    }

    /// Drop a confirmation without a decision (timeout or cancelled task).
    pub fn withdraw(&self, id: ConfirmationId) {
        lock(&self.inner).pending.remove(&id);
    }
}

fn lock(m: &Mutex<Inner>) -> std::sync::MutexGuard<'_, Inner> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn approve_wakes_the_waiter() {
        let store = ConfirmationStore::new();
        let (info, rx) = store.request(
            7,
            "remove_app",
            &serde_json::json!({"app": "x"}),
            "destructive",
        );
        assert_eq!(store.pending().len(), 1);
        assert_eq!(
            store.resolve(info.id, Verdict::Approved).unwrap().tool,
            "remove_app"
        );
        assert_eq!(rx.await.unwrap(), Verdict::Approved);
        assert!(store.pending().is_empty());
        assert!(store.resolve(info.id, Verdict::Denied).is_none());
    }

    #[test]
    fn withdraw_removes_without_decision() {
        let store = ConfirmationStore::new();
        let (info, _rx) = store.request(1, "t", &Value::Null, "r");
        store.withdraw(info.id);
        assert!(store.pending().is_empty());
    }
}
