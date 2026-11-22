//! Built-in tools that need nothing but the local device (this device).

use std::path::PathBuf;

use serde_json::{json, Value};

use super::{Labels, Tool, ToolError, ToolSpec};
use crate::BoxFuture;

/// Filesystems whose space `device_info` reports, relative to the root: `/`, `/var` (apps'
/// data and images) and k3s's data directory when it exists.
const DISKS: &[&str] = &["", "var", "var/lib/rancher/k3s"];

/// `device_info`: OS, kernel, Melissa release, uptime, memory and disk space of this device.
/// Read-only; disk space comes from `statvfs` (no privileges, no shelling out).
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// Filesystem root to read from (`/` in production; a fixture dir in tests).
    root: PathBuf,
}

impl DeviceInfo {
    pub fn new() -> Self {
        Self {
            root: PathBuf::from("/"),
        }
    }

    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.root.join(rel)).ok()
    }
}

impl DeviceInfo {
    /// Space on the filesystem holding `rel` (None if it does not exist or can't be read).
    fn disk(&self, rel: &str) -> Option<Value> {
        let path = self.root.join(rel);
        if !path.is_dir() {
            return None;
        }
        let st = rustix::fs::statvfs(&path).ok()?;
        let unit = st.f_frsize;
        let total = st.f_blocks.saturating_mul(unit);
        let free = st.f_bfree.saturating_mul(unit);
        let available = st.f_bavail.saturating_mul(unit);
        Some(json!({
            "path": format!("/{rel}"),
            "total_bytes": total,
            "available_bytes": available,
            "total": crate::attachments::gib_size(total),
            "available": crate::attachments::gib_size(available),
            "used_percent": used_percent(total, free, available),
        }))
    }
}

/// Percent used as `df` shows it: used / (used + available to users), rounded up.
fn used_percent(total: u64, free: u64, available: u64) -> u64 {
    let used = total.saturating_sub(free);
    let usable = used.saturating_add(available);
    if usable == 0 {
        return 0;
    }
    (used.saturating_mul(100)).div_ceil(usable)
}

impl Default for DeviceInfo {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for DeviceInfo {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "device_info",
            description: "Describe this device (the device Melissa runs on): OS, kernel, Melissa release, uptime, memory and disk space (`disks`: total/available bytes and percent used of /, /var and the k3s data directory). Quote sizes as `total`, `available` and `mem_total` give them (GiB, as Melissa's pages show them).",
            labels: Labels::READ_ONLY,
            params: json!({"type": "object", "properties": {}, "additionalProperties": false}),
        }
    }

    fn call<'a>(&'a self, _args: Value) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(async move {
            let os = self
                .read("etc/os-release")
                .and_then(|t| key_value(&t, "PRETTY_NAME"));
            let release = self.read("usr/lib/melissa/release");
            let kernel = self
                .read("proc/sys/kernel/osrelease")
                .map(|s| s.trim().to_string());
            let uptime_secs = self
                .read("proc/uptime")
                .and_then(|t| {
                    t.split_whitespace()
                        .next()
                        .and_then(|s| s.parse::<f64>().ok())
                })
                .map(|s| s as u64);
            let meminfo = self.read("proc/meminfo").unwrap_or_default();
            let disks: Vec<Value> = DISKS.iter().filter_map(|rel| self.disk(rel)).collect();
            Ok(json!({
                "os": os,
                "kernel": kernel,
                "melissa_version": release.as_deref().and_then(|t| key_value(t, "MELISSA_VERSION")),
                "melissa_variant": release.as_deref().and_then(|t| key_value(t, "MELISSA_VARIANT")),
                "uptime_secs": uptime_secs,
                "mem_total_kib": meminfo_kib(&meminfo, "MemTotal"),
                "mem_total": meminfo_kib(&meminfo, "MemTotal").map(|k| crate::attachments::gib_size(k.saturating_mul(1024))),
                "mem_available": meminfo_kib(&meminfo, "MemAvailable").map(|k| crate::attachments::gib_size(k.saturating_mul(1024))),
                "mem_available_kib": meminfo_kib(&meminfo, "MemAvailable"),
                "disks": disks,
            }))
        })
    }
}

/// `KEY=value` or `KEY="value"` lookup (os-release / release files).
fn key_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
    })
}

fn meminfo_kib(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let rest = line.strip_prefix(key)?.strip_prefix(':')?;
        rest.split_whitespace().next()?.parse().ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_fixture_root() {
        let dir = std::env::temp_dir().join(format!("melissa-beeinfo-{}", std::process::id()));
        for (rel, body) in [
            (
                "etc/os-release",
                "NAME=Fedora\nPRETTY_NAME=\"Fedora Linux 45\"\n",
            ),
            (
                "usr/lib/melissa/release",
                "MELISSA_VERSION=abc123\nMELISSA_VARIANT=headless\n",
            ),
            ("proc/sys/kernel/osrelease", "7.3.4\n"),
            ("proc/uptime", "123.45 678.9\n"),
            (
                "proc/meminfo",
                "MemTotal:  2867000 kB\nMemFree: 1 kB\nMemAvailable:  2400000 kB\n",
            ),
        ] {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        let out = DeviceInfo::with_root(&dir).call(Value::Null).await.unwrap();
        assert_eq!(out["os"], "Fedora Linux 45");
        assert_eq!(out["melissa_variant"], "headless");
        assert_eq!(out["kernel"], "7.3.4");
        assert_eq!(out["uptime_secs"], 123);
        assert_eq!(out["mem_available_kib"], 2_400_000);
        // Disk space of the fixture's / and /var (a real filesystem under the temp dir); no
        // k3s data directory, so no entry for it.
        std::fs::create_dir_all(dir.join("var")).unwrap();
        let out = DeviceInfo::with_root(&dir).call(Value::Null).await.unwrap();
        let disks = out["disks"].as_array().unwrap();
        let paths: Vec<&str> = disks.iter().map(|d| d["path"].as_str().unwrap()).collect();
        assert_eq!(paths, ["/", "/var"]);
        for d in disks {
            let total = d["total_bytes"].as_u64().unwrap();
            assert!(total > 0);
            assert!(d["available_bytes"].as_u64().unwrap() <= total);
            assert!(d["used_percent"].as_u64().unwrap() <= 100);
        }
        std::fs::create_dir_all(dir.join("var/lib/rancher/k3s")).unwrap();
        let out = DeviceInfo::with_root(&dir).call(Value::Null).await.unwrap();
        assert_eq!(out["disks"][2]["path"], "/var/lib/rancher/k3s");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn used_percent_matches_df() {
        // 100 blocks, 20 free, 15 available to users (5 reserved): 80 / 95 used.
        assert_eq!(used_percent(100, 20, 15), 85);
        assert_eq!(used_percent(100, 100, 100), 0);
        assert_eq!(used_percent(0, 0, 0), 0);
        assert_eq!(used_percent(100, 0, 0), 100);
    }

    #[tokio::test]
    async fn missing_files_give_nulls_not_errors() {
        let out = DeviceInfo::with_root("/nonexistent-melissa")
            .call(Value::Null)
            .await
            .unwrap();
        assert!(out["os"].is_null());
        assert_eq!(out["disks"], serde_json::json!([]));
    }
}
