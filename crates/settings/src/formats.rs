//! The format of every settings file this crate writes (melissa-statefile; inventory in
//! docs/design/STATE-INVENTORY-2026-11-28.md). Bump `current` and add a migration whenever an
//! older Melissa would misread what this one writes; with `deny_unknown_fields` that is any
//! new key.
//!
//! All are read with `deny_unknown_fields`, so format 1 is written without the `format` key:
//! the images from before formats existed still read these files after a rollback.

use melissa_statefile::{Format, TomlFormat};

const fn v1(name: &'static str) -> TomlFormat {
    Format {
        name,
        current: 1,
        migrations: &[],
        v1_marker: false,
    }
}

/// `<config>/ui.toml`.
pub static PREFS: TomlFormat = v1("display preferences");
/// `<config>/providers.toml`.
pub static PROVIDERS: TomlFormat = v1("provider settings");
/// `/etc/melissa/tailscale.toml`.
pub static TAILSCALE: TomlFormat = v1("Tailscale setting");
/// `/etc/melissa/app-access.toml`.
pub static APP_ACCESS: TomlFormat = v1("app access setting");
/// Root's `/var/lib/melissa-container/cluster/setting.toml`.
pub static CLUSTER: TomlFormat = v1("cluster setting");

#[cfg(test)]
mod tests {
    use crate::{
        prefs::{Prefs, PrefsStore},
        providers::ProviderFile,
        test_support::scratch,
    };

    /// Files from before formats existed load, and format 1 is still written without the key
    /// (the images before formats reject unknown keys).
    #[test]
    fn old_settings_load_and_format_1_keeps_their_shape() {
        let dir = scratch("formats-old");
        let prefs = dir.join("ui.toml");
        std::fs::write(&prefs, "# old\nshow_steps = true\n").unwrap();
        let store = PrefsStore::open(prefs.clone());
        assert!(store.get().show_steps);
        store.update(|p| p.voice_input = true).unwrap();
        let text = std::fs::read_to_string(&prefs).unwrap();
        assert!(!text.contains("format"), "{text}");
        assert!(text.starts_with("# Melissa display preferences"), "{text}");
        let providers = dir.join("providers.toml");
        std::fs::write(&providers, "[provider.claude]\nenabled = false\n").unwrap();
        let file = ProviderFile::load(&providers).unwrap();
        assert_eq!(file.get("claude").enabled, Some(false));
        file.save(&providers).unwrap();
        assert!(!std::fs::read_to_string(&providers)
            .unwrap()
            .contains("format"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// After a rollback: a newer Melissa's settings are refused, never saved over or moved.
    #[test]
    fn newer_settings_are_left_alone() {
        let dir = scratch("formats-newer");
        let prefs = dir.join("ui.toml");
        let newer_prefs = "format = 2\nshow_steps = true\ncolour = \"teal\"\n";
        std::fs::write(&prefs, newer_prefs).unwrap();
        let err = Prefs::load(&prefs).unwrap_err();
        assert!(err.is_newer(), "{err}");
        assert!(
            err.to_string().contains("newer version of Melissa"),
            "{err}"
        );
        // The UI starts from the defaults and its changes are refused.
        let store = PrefsStore::open(prefs.clone());
        assert_eq!(store.get(), Prefs::default());
        assert!(store
            .update(|p| p.show_steps = false)
            .unwrap_err()
            .is_newer());
        assert_eq!(std::fs::read_to_string(&prefs).unwrap(), newer_prefs);

        let providers = dir.join("providers.toml");
        let newer_providers = "format = 3\n[provider.claude]\nenabled = false\nwild = 1\n";
        std::fs::write(&providers, newer_providers).unwrap();
        assert!(ProviderFile::load(&providers).unwrap_err().is_newer());
        assert!(ProviderFile::default()
            .save(&providers)
            .unwrap_err()
            .is_newer());
        assert_eq!(
            std::fs::read_to_string(&providers).unwrap(),
            newer_providers
        );

        let access = dir.join("app-access.toml");
        std::fs::write(&access, "format = 2\napps = \"tailnet\"\n").unwrap();
        let err = crate::app_access::read_setting(&access).unwrap_err();
        assert!(err.contains("newer version of Melissa"), "{err}");
        let ts = dir.join("tailscale.toml");
        std::fs::write(&ts, "format = 2\nenabled = true\n").unwrap();
        let err = crate::tailscale::read_setting(&ts).unwrap_err();
        assert!(err.contains("newer version of Melissa"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
