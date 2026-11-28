//! The owner's display preferences for the web UI, in `$XDG_CONFIG_HOME/melissa/ui.toml`
//! (next to `providers.toml`). They change only what the UI shows, so they apply at once
//! (no restart). A missing or unreadable file means the defaults.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};

use crate::SettingsError;

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
    /// The name the UI shows for this device (first-run setup). It does not rename the
    /// Tailscale node. The k3s node takes it (sanitized) while that is harmless
    /// (`crate::cluster::decide_name`). `None`: the cluster's node name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    /// First-run setup: the steps the owner put off, and whether the checklist is hidden.
    pub setup: SetupPrefs,
    /// Icons people picked for apps (app name → shape and colour, names as for helpers'
    /// icons); apps without one get an icon derived from their name.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub app_icons: std::collections::BTreeMap<String, AppIconPref>,
    /// How the pages show times of day (Settings → About → This device).
    #[serde(skip_serializing_if = "TimeFormat::is_default")]
    pub time_format: TimeFormat,
}

/// 24-hour ("15:00"), 12-hour ("3:00 PM"), or whatever the browser's language uses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeFormat {
    /// The browser's language decides (its `Accept-Language`).
    #[default]
    Browser,
    H24,
    H12,
}

impl TimeFormat {
    pub const ALL: [Self; 3] = [Self::Browser, Self::H24, Self::H12];

    fn is_default(&self) -> bool {
        *self == Self::Browser
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::H24 => "h24",
            Self::H12 => "h12",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == s)
    }
}

/// An app's icon as picked in the UI (names checked by the UI when read).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppIconPref {
    pub shape: String,
    pub color: String,
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
        Self::parse(path, &text)
    }

    /// Parse the text of `path` (its format checked and migrated).
    pub fn parse(path: &Path, text: &str) -> Result<Self, SettingsError> {
        crate::parse_versioned(path, &crate::formats::PREFS, text, |t| {
            toml::from_str(t).map_err(|e| SettingsError::Parse {
                path: path.display().to_string(),
                message: e.message().to_string(),
            })
        })
    }

    /// Save to `path`; refused for a newer Melissa's file ([`SettingsError::Newer`]).
    pub fn save(&self, path: &Path) -> Result<(), SettingsError> {
        crate::save_versioned(
            path,
            &crate::formats::PREFS,
            "# Melissa display preferences, written by the web UI.\n\n",
            self,
        )
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
        let dir = crate::test_support::scratch("prefs");
        let path = dir.join(FILE_NAME);
        let store = PrefsStore::open(path.clone());
        assert_eq!(store.get(), Prefs::default());
        assert!(!store.get().show_steps && !store.get().voice_input);
        store
            .update(|p| {
                p.show_steps = true;
                p.voice_input = true;
                p.device_name = clean_device_name("  Living room box \u{7} ");
                p.setup.skipped.push("extras".into());
            })
            .unwrap();
        let back = PrefsStore::open(path.clone()).get();
        assert!(back.voice_input && back.show_steps);
        assert_eq!(back.device_name.as_deref(), Some("Living room box"));
        assert_eq!(back.setup.skipped, vec!["extras".to_string()]);
        assert!(!back.setup.dismissed);
        // Turned off or unset again: an unset key leaves the file.
        store
            .update(|p| {
                p.voice_input = false;
                p.device_name = None;
            })
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("device_name"), "{text}");
        let back = PrefsStore::open(path.clone()).get();
        assert!(!back.voice_input && back.show_steps && back.device_name.is_none());
        // Files written before a preference existed still load (serde default); unknown or
        // broken content: the defaults.
        std::fs::write(&path, "show_steps = true\n").unwrap();
        assert!(PrefsStore::open(path.clone()).get().show_steps);
        std::fs::write(&path, "nope = 1\n").unwrap();
        assert_eq!(PrefsStore::open(path).get(), Prefs::default());
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
