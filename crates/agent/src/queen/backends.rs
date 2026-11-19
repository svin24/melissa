//! Choosing the backend (and model) of a conversation: the user switches the Queen or a bee
//! to another configured session backend at runtime.
//!
//! - The choice is the user's, so it lives in Melissa's state, not in the config file: the
//!   Queen's in `$XDG_STATE_HOME/melissa/queen/backend.json`, a bee's in the bee registry.
//! - It takes effect at the next turn, never mid-turn: the running session is closed before
//!   the next message and a session on the chosen backend opens instead. Context carries
//!   over the way it does after a lost session: the same backend (a model change) resumes
//!   its own session; another backend starts a new session primed with the last messages
//!   of Melissa's transcript.
//! - Models: only what the agents themselves report (ACP `configOptions` of category
//!   `model`, Claude's handshake answer), remembered per backend in
//!   `$XDG_STATE_HOME/melissa/queen/models.json` once a session on it has opened. Melissa
//!   never makes up model ids; a backend whose models are unknown offers none.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};

use crate::{
    comb::write_private,
    session::{ModelList, ModelOption, NotReady},
};

/// The backend (and model) the user chose for a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendChoice {
    pub backend: String,
    /// A model id the backend reported; None: the backend's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// One backend the user can choose, as clients see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackendView {
    pub name: String,
    /// Usable now (installed, logged in where Melissa can tell, sandbox ready).
    pub available: bool,
    /// Why not (`not_installed`, `not_logged_in`, `sandbox`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    /// The conversation runs (or will run on its next message) on this backend.
    pub current: bool,
    /// The model in use or chosen on the current backend, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Models the backend offers, once Melissa has seen them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<ModelOption>>,
}

/// What a switch did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Switched {
    pub backend: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// False: that was already the choice (nothing recorded).
    pub changed: bool,
    /// A turn is running: the switch applies when it has ended.
    pub after_turn: bool,
}

/// Models each backend offered when a session last opened on it. Shared by the Queen and
/// every bee; saved so the list is there after a restart. Cheap to clone.
#[derive(Clone, Default)]
pub struct ModelCache {
    inner: Arc<ModelCacheInner>,
}

#[derive(Default)]
struct ModelCacheInner {
    path: Option<PathBuf>,
    lists: Mutex<BTreeMap<String, Vec<ModelOption>>>,
}

impl ModelCache {
    /// In memory only (tests).
    pub fn new() -> Self {
        Self::default()
    }

    /// Kept in `path` (a JSON object: backend name → `[{id, label}]`, mode 0600).
    pub fn open(path: PathBuf) -> Self {
        let lists = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Self {
            inner: Arc::new(ModelCacheInner {
                path: Some(path),
                lists: Mutex::new(lists),
            }),
        }
    }

    pub fn get(&self, backend: &str) -> Option<Vec<ModelOption>> {
        lock(&self.inner.lists).get(backend).cloned()
    }

    /// Remember what a backend offered (saved if it changed).
    pub fn put(&self, backend: &str, models: &ModelList) {
        if models.options.is_empty() {
            return;
        }
        let mut lists = lock(&self.inner.lists);
        if lists.get(backend) == Some(&models.options) {
            return;
        }
        lists.insert(backend.to_string(), models.options.clone());
        if let Some(path) = &self.inner.path {
            match serde_json::to_vec_pretty(&*lists) {
                Ok(json) => {
                    if let Err(e) = write_private(path, &json) {
                        tracing::warn!(path = %path.display(), "cannot save the model list: {e}");
                    }
                }
                Err(e) => tracing::warn!("cannot save the model list: {e}"),
            }
        }
    }
}

/// The Queen's saved choice, if any.
pub(crate) fn load_choice(path: &Path) -> Option<BackendChoice> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&text) {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!(path = %path.display(), "ignoring the saved backend choice: {e}");
            None
        }
    }
}

pub(crate) fn save_choice(path: &Path, choice: &BackendChoice) -> std::io::Result<()> {
    let json = serde_json::to_vec_pretty(choice).map_err(std::io::Error::other)?;
    write_private(path, &json)
}

pub(crate) fn reason(r: Option<NotReady>) -> Option<&'static str> {
    r.map(NotReady::as_str)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // Poisoning only means another thread panicked mid-update; the data is still usable.
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(ids: &[&str]) -> ModelList {
        ModelList {
            current: None,
            options: ids
                .iter()
                .map(|i| ModelOption {
                    id: (*i).into(),
                    label: i.to_uppercase(),
                })
                .collect(),
        }
    }

    #[test]
    fn model_cache_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("melissa-models-{}", std::process::id()));
        let path = dir.join("models.json");
        let cache = ModelCache::open(path.clone());
        assert_eq!(cache.get("claude-session"), None);
        cache.put("claude-session", &list(&["sonnet", "opus"]));
        // An empty list never replaces a known one.
        cache.put("claude-session", &list(&[]));
        let again = ModelCache::open(path.clone());
        let ids: Vec<String> = again
            .get("claude-session")
            .unwrap_or_default()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, ["sonnet", "opus"]);
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).map(|m| m.permissions().mode() & 0o777);
        assert_eq!(mode.ok(), Some(0o600));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn choice_round_trips_and_bad_files_are_ignored() {
        let dir = std::env::temp_dir().join(format!("melissa-choice-{}", std::process::id()));
        let path = dir.join("backend.json");
        assert_eq!(load_choice(&path), None);
        let c = BackendChoice {
            backend: "claude-session".into(),
            model: Some("sonnet".into()),
        };
        save_choice(&path, &c).ok();
        assert_eq!(load_choice(&path), Some(c));
        std::fs::write(&path, "not json").ok();
        assert_eq!(load_choice(&path), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
