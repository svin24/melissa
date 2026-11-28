//! Ids of the cluster model (docs/AGENT-MODEL.md): devices (devices) and the agents on them.
//!
//! - A **device id** is `d-` plus 8 random hex characters, made once per device and kept in
//!   `$XDG_STATE_HOME/melissa/device.json` ([`DeviceIdentity`]). Random, so two devices that
//!   later join one cluster don't collide.
//! - An **agent id** is `<device id>.a<n>`: the device it was born on and a number the device's agent
//!   registry hands out once (never reused, also after an agent is gone). So an agent id is unique
//!   across the cluster and says where the agent lives, which is what cross-device takeovers need.
//! - A **workspace** is named by its app (`nextcloud`); across devices it is `<device id>/<name>`.
//!
//! Every id fits a Kubernetes label value (`[A-Za-z0-9._-]`, at most 63, alphanumeric at
//! both ends), because workspaces record their device and owning agent as namespace labels.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// `d-` + 8 lowercase hex characters.
pub fn valid_device_id(id: &str) -> bool {
    id.strip_prefix("d-")
        .is_some_and(|h| h.len() == 8 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

/// `<device id>.a<n>`, `n` from 1 to 999999999 without leading zeros.
pub fn valid_agent_id(id: &str) -> bool {
    let Some((device, n)) = id.split_once(".a") else {
        return false;
    };
    valid_device_id(device)
        && (1..=9).contains(&n.len())
        && !n.starts_with('0')
        && n.bytes().all(|b| b.is_ascii_digit())
}

/// The agent id for number `n` on `device`.
pub fn agent_id(device: &str, n: u64) -> String {
    format!("{device}.a{n}")
}

/// The device an agent id belongs to (None for an invalid id).
pub fn device_of(agent: &str) -> Option<&str> {
    valid_agent_id(agent).then(|| agent.split_once(".a").map_or("", |(c, _)| c))
}

/// This device's device: stable id and display name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceIdentity {
    pub id: String,
    /// Shown to the user; they may rename the device (not used anywhere else yet).
    pub name: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("reading {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("{path} is not a valid device file: {reason}")]
    Invalid { path: String, reason: String },
    #[error("writing {path}: {source}")]
    Write {
        path: String,
        source: std::io::Error,
    },
}

impl DeviceIdentity {
    /// Load the device identity from `path`, or make a new one and save it (mode 0600).
    pub fn load_or_create(path: &Path) -> Result<Self, DeviceError> {
        let shown = path.display().to_string();
        match std::fs::read_to_string(path) {
            Ok(text) => {
                // Only written when made: a newer format is read as well as it can be.
                let device: Self =
                    melissa_statefile::parse_json(path, &crate::formats::DEVICE, text.as_bytes())
                        .map_err(|e| DeviceError::Invalid {
                            path: shown.clone(),
                            reason: e.to_string(),
                        })?
                        .value;
                if !valid_device_id(&device.id) {
                    return Err(DeviceError::Invalid {
                        path: shown,
                        reason: format!("`{}` is not a device id", device.id),
                    });
                }
                Ok(device)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let id = format!(
                    "d-{}",
                    random_hex(4).map_err(|source| DeviceError::Write {
                        path: shown.clone(),
                        source,
                    })?
                );
                let device = Self {
                    name: "this device".into(),
                    id,
                };
                melissa_statefile::save_json(path, &crate::formats::DEVICE, &device, true)
                    .map_err(|e| DeviceError::Write {
                        path: shown,
                        source: std::io::Error::other(e),
                    })?;
                Ok(device)
            }
            Err(source) => Err(DeviceError::Read {
                path: shown,
                source,
            }),
        }
    }
}

/// `n` random bytes from the kernel CSPRNG as lowercase hex.
pub(crate) fn random_hex(n: usize) -> std::io::Result<String> {
    use std::io::Read;
    let mut bytes = vec![0u8; n];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Write `data` to `path` atomically (temp file, fsync, rename, directory fsync), mode 0600,
/// creating the parent.
pub(crate) fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    melissa_statefile::write_private(path, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_have_the_documented_shape() {
        assert!(valid_device_id("d-0a1b2c3d"));
        for bad in ["d-0A1B2C3D", "d-0a1b2c3", "x-0a1b2c3d", "d-0a1b2c3d4", ""] {
            assert!(!valid_device_id(bad), "{bad}");
        }
        assert!(valid_agent_id("d-0a1b2c3d.a1"));
        assert!(valid_agent_id("d-0a1b2c3d.a123456789"));
        for bad in [
            "d-0a1b2c3d.a0",
            "d-0a1b2c3d.a01",
            "d-0a1b2c3d.a",
            "d-0a1b2c3d.ax",
            "d-0a1b2c3d.b1",
            "c-0a1b2c3d.a1",
            "d-0a1b2c3d.a1234567890",
            "d-0a1b2c3.a1",
            "job-abc",
            "owner",
        ] {
            assert!(!valid_agent_id(bad), "{bad}");
        }
        assert_eq!(agent_id("d-0a1b2c3d", 7), "d-0a1b2c3d.a7");
        assert_eq!(device_of("d-0a1b2c3d.a7"), Some("d-0a1b2c3d"));
        assert_eq!(device_of("nope"), None);
    }

    #[test]
    fn the_device_identity_is_made_once_and_kept() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::test_support::fresh_path("device");
        let path = dir.join("device.json");
        let a = DeviceIdentity::load_or_create(&path).unwrap();
        assert!(valid_device_id(&a.id), "{}", a.id);
        let b = DeviceIdentity::load_or_create(&path).unwrap();
        assert_eq!(a, b);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::write(&path, r#"{"id": "bogus", "name": "x"}"#).unwrap();
        assert!(DeviceIdentity::load_or_create(&path).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
