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

/// The preferences. Every one is off (or unset) by default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Prefs {
    /// Show the agents' thinking and how long each step took in the chat (Settings →
    /// Developer).
    pub show_steps: bool,
    /// A microphone button in the chat composer; speech becomes text on this device
    /// (Settings → Voice). Turning it on downloads the speech model once.
    pub voice_input: bool,
    /// While an agent works, Enter in the chat composer steers the message into the running
    /// turn (double Enter queues it); off: Enter queues it (double Enter steers).
    pub enter_steers: bool,
    /// The name the UI shows for this device (first-run setup). Display only: it does not
    /// rename the Tailscale or cluster node. `None`: the cluster's node name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    /// First-run setup: the steps the owner put off, and whether the checklist is hidden.
    pub setup: SetupPrefs,
}

/// What first-run setup keeps (its progress otherwise comes from the real state).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SetupPrefs {
    /// Steps the owner chose "Later" for (`device`, `provider`, `model`, `extras`).
    pub skipped: Vec<String>,
    /// The owner hid the setup checklist in the chat.
    pub dismissed: bool,
}

/// Longest device name kept (characters).
pub const DEVICE_NAME_MAX: usize = 40;

/// A device name as typed, cleaned: control characters dropped, spaces trimmed, at most
/// [`DEVICE_NAME_MAX`] characters. `None` when nothing is left.
pub fn clean_device_name(name: &str) -> Option<String> {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(DEVICE_NAME_MAX)
        .collect();
    let cleaned = cleaned.trim().to_string();
    (!cleaned.is_empty()).then_some(cleaned)
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
        lock(&self.inner).1.clone()
    }

    /// Change and save. The file is written before memory changes, so a failed write leaves
    /// both as they were.
    pub fn update(&self, change: impl FnOnce(&mut Prefs)) -> Result<Prefs, SettingsError> {
        let mut inner = lock(&self.inner);
        let mut next = inner.1.clone();
        change(&mut next);
        if let Some(path) = &inner.0 {
            next.save(path)?;
        }
        inner.1 = next.clone();
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

    #[test]
    fn device_name_and_setup_progress_round_trip() {
        let dir = std::env::temp_dir().join(format!("melissa-prefs-dev-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(FILE_NAME);
        let store = PrefsStore::open(path.clone());
        assert_eq!(store.get().device_name, None);
        store
            .update(|p| {
                p.device_name = clean_device_name("  Living room box \u{7} ");
                p.setup.skipped.push("extras".into());
            })
            .unwrap();
        let back = PrefsStore::open(path.clone()).get();
        assert_eq!(back.device_name.as_deref(), Some("Living room box"));
        assert_eq!(back.setup.skipped, vec!["extras".to_string()]);
        assert!(!back.setup.dismissed);
        // Unset again: the key leaves the file.
        store.update(|p| p.device_name = None).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("device_name"), "{text}");
        assert_eq!(PrefsStore::open(path).get().device_name, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn device_names_are_cleaned() {
        assert_eq!(clean_device_name("   "), None);
        assert_eq!(clean_device_name("a\nb"), Some("ab".into()));
        assert_eq!(
            clean_device_name(&"x".repeat(100)).map(|n| n.chars().count()),
            Some(DEVICE_NAME_MAX)
        );
    }
}
