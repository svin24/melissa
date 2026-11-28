#![forbid(unsafe_code)]
//! Melissa's settings that the owner changes at runtime (the web UI's Settings pages and
//! `melissa-server provider|update`), kept apart from the hand-written `server.toml`:
//!
//! - [`providers`]: per agent CLI (claude, grok, opencode, echo, ...) overrides in
//!   `$XDG_CONFIG_HOME/melissa/providers.toml`, merged over `server.toml` at startup.
//! - [`app_access`]: who may open the apps on this device (`app-access.toml`), the nftables
//!   filter that enforces it in the container, and the agents' `set_app_access` tool.
//! - [`cluster`]: this device's place in a multi-device k3s cluster (root's own setting: role,
//!   peers), the node name, root's k3s config rendering and the owner's join/leave actions.
//! - [`tailscale`]: Melissa's own Tailscale in the container (off by default, `tailscale.toml`),
//!   its state, enable/disable and sign-in, and the agents' `tailscale_status` and
//!   `enable_tailscale` tools.
//! - [`factory_reset`]: erase Melissa's data and start her again unclaimed (the marker the
//!   server writes, the fixed list of what goes, the root unit's sequence).
//! - [`prefs`]: the owner's display preferences for the web UI (`ui.toml`), applied at once.
//! - [`registry`]: the public ACP agent registry (parse, search, cache, install a release).
//! - [`version`]: the installed CLIs' `--version`, cached and timed out.
//! - [`updates`]: this device's Melissa release and its container image state, read from what
//!   the host helper writes; the fixed list of actions the lead agent may ask it for.

pub mod app_access;
pub mod cluster;
pub mod factory_reset;
pub mod formats;
pub mod install;
pub mod prefs;
pub mod providers;
pub mod registry;
pub mod tailscale;
pub mod updates;
pub mod version;

#[cfg(test)]
mod test_support;

/// Errors from reading, checking or saving settings. Messages never contain a variable's
/// value (they may be shown in the UI and the log).
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("{0}")]
    Invalid(String),
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("{path} is not valid: {message}")]
    Parse { path: String, message: String },
    #[error("cannot save {path}: {source}")]
    Write {
        path: String,
        source: std::io::Error,
    },
    #[error("{0}")]
    Failed(String),
    /// The file was written by a newer Melissa (after a rollback): left as it is, not saved
    /// over (melissa-statefile).
    #[error("{0}")]
    Newer(String),
}

impl SettingsError {
    pub fn is_newer(&self) -> bool {
        matches!(self, Self::Newer(_))
    }
}

/// Write `data` to `path` atomically (a fresh temp file, fsync, rename, directory fsync),
/// mode 0600, creating the parent.
pub(crate) fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    melissa_statefile::write_private(path, data)
}

/// Read the TOML settings file `text` (from `path`) of format `fmt`: its format checked and
/// migrated, then parsed by `parse` (each file's own parser keeps its own error messages,
/// which never quote a value). A newer Melissa's file: [`SettingsError::Newer`].
pub(crate) fn parse_versioned<T>(
    path: &std::path::Path,
    fmt: &melissa_statefile::TomlFormat,
    text: &str,
    parse: impl FnOnce(&str) -> Result<T, SettingsError>,
) -> Result<T, SettingsError> {
    let Ok(mut table) = text.parse::<toml::Table>() else {
        // Not TOML at all: the file's own parser says where.
        return parse(text);
    };
    let found = match melissa_statefile::Doc::version(&table) {
        Ok(v) => v,
        Err(message) => {
            return Err(SettingsError::Parse {
                path: path.display().to_string(),
                message,
            })
        }
    };
    if found > fmt.current {
        return Err(SettingsError::Newer(
            melissa_statefile::FormatError::Newer {
                path: path.display().to_string(),
                name: fmt.name,
                found,
                supported: fmt.current,
            }
            .to_string(),
        ));
    }
    if found == fmt.current && !table.contains_key(melissa_statefile::KEY) {
        return parse(text);
    }
    for from in found..fmt.current {
        let step = usize::try_from(from - 1)
            .ok()
            .and_then(|i| fmt.migrations.get(i))
            .ok_or_else(|| SettingsError::Failed(format!("no migration for {}", fmt.name)))?;
        step(&mut table).map_err(|message| SettingsError::Parse {
            path: path.display().to_string(),
            message: format!(
                "cannot bring it from format {from} to {}: {message}",
                from + 1
            ),
        })?;
    }
    melissa_statefile::Doc::strip_version(&mut table);
    let text = toml::to_string(&table).map_err(|e| SettingsError::Failed(e.to_string()))?;
    parse(&text)
}

/// Save TOML settings text `body` to `path` (0600) in format `fmt` after `header`, unless a
/// newer Melissa wrote the file ([`SettingsError::Newer`]).
pub(crate) fn save_versioned<T: serde::Serialize>(
    path: &std::path::Path,
    fmt: &melissa_statefile::TomlFormat,
    header: &str,
    value: &T,
) -> Result<(), SettingsError> {
    match melissa_statefile::save_toml(path, fmt, header, value) {
        Ok(()) => Ok(()),
        Err(e @ melissa_statefile::FormatError::Newer { .. }) => {
            Err(SettingsError::Newer(e.to_string()))
        }
        Err(melissa_statefile::FormatError::Write { source, .. }) => Err(SettingsError::Write {
            path: path.display().to_string(),
            source,
        }),
        Err(e) => Err(SettingsError::Failed(e.to_string())),
    }
}

/// SHA-256 of `data` in hex (audit records carry hashes of values, never values).
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Text from a CLI or a root helper, made safe to show: control characters and escape
/// sequences dropped, cut to `max` characters. (The UI escapes it again.)
pub fn clean_text(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // CSI: ESC [ ... final byte in @..~
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        if c.is_control() {
            if c == '\n' || c == '\t' {
                out.push(' ');
            }
            continue;
        }
        out.push(c);
    }
    let out = out.trim();
    let mut cut: String = out.chars().take(max).collect();
    if out.chars().count() > max {
        cut.push('…');
    }
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_text_drops_escapes_and_cuts() {
        assert_eq!(
            clean_text("\u{1b}[1;32m2.1.3\u{1b}[0m (Claude Code)\n", 80),
            "2.1.3 (Claude Code)"
        );
        assert_eq!(clean_text("abcdef", 3), "abc…");
        assert_eq!(clean_text("a\u{7}b", 10), "ab");
    }

    #[test]
    fn private_files_are_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::test_support::scratch("settings");
        let path = dir.join("x.toml");
        write_private(&path, b"a").unwrap();
        write_private(&path, b"b").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "b");
        let _ = std::fs::remove_dir_all(dir);
    }
}
