//! The owner's display preferences for the web UI, in `$XDG_CONFIG_HOME/melissa/ui.toml`
//! (next to `providers.toml`). They change only what the UI shows, so they apply at once
//! (no restart). A missing or unreadable file means the defaults.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};

use crate::{write_private, SettingsError};

pub const FILE_NAME: &str = "ui.toml";

/// The preferences. Every one is off by default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Prefs {
    /// Show the agents' thinking and how long each step took in the chat (Settings →
    /// Developer).
    pub show_steps: bool,
    /// A microphone button in the chat composer; speech becomes text on this device
    /// (Settings → Voice). Turning it on downloads the speech model once.
    pub voice_input: bool,
}

impl Prefs {
    /// Read `path`; a missing file means the defaults.
    pub fn load(path: &Path) -> Result<Self, SettingsError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(SettingsError::Read {
                    path: path.display().to_string(),
                    source,
                })
            }
        };
        toml::from_str(&text).map_err(|e| SettingsError::Parse {
            path: path.display().to_string(),
            message: e.message().to_string(),
        })
    }

    pub fn save(&self, path: &Path) -> Result<(), SettingsError> {
        let mut text = String::from("# Melissa display preferences, written by the web UI.\n\n");
        text.push_str(&toml::to_string(self).map_err(|e| SettingsError::Failed(e.to_string()))?);
        write_private(path, text.as_bytes()).map_err(|source| SettingsError::Write {
            path: path.display().to_string(),
            source,
        })
    }
}

/// The preferences file, shared by the UI's handlers. Cheap to clone.
#[derive(Clone)]
pub struct PrefsStore {
    inner: Arc<Mutex<(Option<PathBuf>, Prefs)>>,
}

impl PrefsStore {
    /// Kept in `path`; starts from what is saved there (the defaults if it can't be read,
    /// with a warning in the log).
    pub fn open(path: PathBuf) -> Self {
        let prefs = Prefs::load(&path).unwrap_or_else(|e| {
            tracing::warn!("{e}; using the default UI preferences");
            Prefs::default()
        });
        Self {
            inner: Arc::new(Mutex::new((Some(path), prefs))),
        }
    }

    /// In memory only (tests, routers without a config directory).
    pub fn in_memory() -> Self {
        Self {
            inner: Arc::new(Mutex::new((None, Prefs::default()))),
        }
    }

    pub fn get(&self) -> Prefs {
        lock(&self.inner).1
    }

    /// Change and save. The file is written before memory changes, so a failed write leaves
    /// both as they were.
    pub fn update(&self, change: impl FnOnce(&mut Prefs)) -> Result<Prefs, SettingsError> {
        let mut inner = lock(&self.inner);
        let mut next = inner.1;
        change(&mut next);
        if let Some(path) = &inner.0 {
            next.save(path)?;
        }
        inner.1 = next;
        Ok(next)
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // Poisoning only means another thread panicked mid-update; the data is still usable.
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_preferences_come_back_and_default_to_off() {
        let dir = std::env::temp_dir().join(format!("melissa-prefs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(FILE_NAME);
        let store = PrefsStore::open(path.clone());
        assert!(!store.get().show_steps);
        store.update(|p| p.show_steps = true).unwrap();
        assert!(PrefsStore::open(path.clone()).get().show_steps);
        // Files written before a preference existed still load (serde default).
        assert!(!PrefsStore::open(path.clone()).get().voice_input);
        store.update(|p| p.voice_input = true).unwrap();
        let back = PrefsStore::open(path.clone()).get();
        assert!(back.voice_input && back.show_steps);
        store.update(|p| p.voice_input = false).unwrap();
        assert!(!PrefsStore::open(path.clone()).get().voice_input);
        // Unknown or broken content: the defaults.
        std::fs::write(&path, "nope = 1\n").unwrap();
        assert_eq!(PrefsStore::open(path).get(), Prefs::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
