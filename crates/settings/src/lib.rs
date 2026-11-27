#![forbid(unsafe_code)]
//! Melissa's settings that the owner changes at runtime (the web UI's Settings pages and
//! `melissa-server provider|update`), kept apart from the hand-written `server.toml`:
//!
//! - [`providers`]: per agent CLI (claude, grok, opencode, echo, ...) overrides in
//!   `$XDG_CONFIG_HOME/melissa/providers.toml`, merged over `server.toml` at startup.
//! - [`app_access`]: who may open the apps on this device (`app-access.toml`), the nftables
//!   filter that enforces it in the container, and the agents' `set_app_access` tool.
//! - [`cluster`]: this device's place in a multi-device k3s cluster (`cluster.toml`: role,
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
}

/// Write `data` to `path` atomically (temp file + rename), mode 0600, creating the parent.
pub(crate) fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    // A leftover temp file may have another mode; start fresh so 0600 applies.
    let _ = std::fs::remove_file(&tmp);
    {
        let mut f = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
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
