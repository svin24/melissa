//! Built-in tools that need nothing but the local comb (this device).

use std::path::PathBuf;

use serde_json::{json, Value};

use super::{Labels, Tool, ToolError, ToolSpec};
use crate::BoxFuture;

/// `comb_info`: OS, kernel, Melissa release, uptime and memory of this comb. Read-only.
#[derive(Debug, Clone)]
pub struct CombInfo {
    /// Filesystem root to read from (`/` in production; a fixture dir in tests).
    root: PathBuf,
}

impl CombInfo {
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

impl Default for CombInfo {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for CombInfo {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "comb_info",
            description: "Describe this comb (the device Melissa runs on): OS, kernel, Melissa release, uptime and memory.",
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
            Ok(json!({
                "os": os,
                "kernel": kernel,
                "melissa_version": release.as_deref().and_then(|t| key_value(t, "MELISSA_VERSION")),
                "melissa_variant": release.as_deref().and_then(|t| key_value(t, "MELISSA_VARIANT")),
                "uptime_secs": uptime_secs,
                "mem_total_kib": meminfo_kib(&meminfo, "MemTotal"),
                "mem_available_kib": meminfo_kib(&meminfo, "MemAvailable"),
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
        let out = CombInfo::with_root(&dir).call(Value::Null).await.unwrap();
        assert_eq!(out["os"], "Fedora Linux 45");
        assert_eq!(out["melissa_variant"], "headless");
        assert_eq!(out["kernel"], "7.3.4");
        assert_eq!(out["uptime_secs"], 123);
        assert_eq!(out["mem_available_kib"], 2_400_000);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn missing_files_give_nulls_not_errors() {
        let out = CombInfo::with_root("/nonexistent-melissa")
            .call(Value::Null)
            .await
            .unwrap();
        assert!(out["os"].is_null());
    }
}
