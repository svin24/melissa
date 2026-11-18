//! What the UI shows about apps and devices, read with Melissa's read-only tools
//! (`list_apps`, `cluster_health`, `comb_info`) through the same registry the agents use, so
//! the calls are policy-checked and audited (`tool.call`, task 0, read-only scope). The answer is cached for
//! [`HiveCache::TTL`] so open pages don't fill the audit log; a stale answer is served
//! while a fresh one is fetched in the background.
//!
//! Who looks after each app (cell) comes from the Queen's bee pool (`cells()`: the
//! `melissa.io/owner-bee` labels with the bees' names).
//!
//! Like `feed.rs`, this module holds the names of tools and phases the UI depends on.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use melissa_agent::{
    mcp::McpServer,
    queen::{bees::CellView, Queen},
    tools::{Scope, TaskContext},
};
use serde_json::{json, Value};

use crate::{components::Health, feed::display_app};

/// One installed app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    /// Catalog name (`nextcloud`).
    pub name: String,
    /// Phase as `app_status` reports it.
    pub phase: String,
    pub url: Option<String>,
    /// Who looks after it.
    pub keeper: Option<Keeper>,
}

/// Who looks after an app (cell).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Keeper {
    /// A bee: id and display name.
    Bee { id: String, name: String },
    /// The owner (a cell from before bees, `melissa.io/owner=owner`).
    Owner,
}

/// Who looks after each cell, from the bee pool's cell list.
pub fn keepers(cells: &[CellView]) -> Vec<(String, Keeper)> {
    cells
        .iter()
        .filter_map(|c| {
            let keeper = match (&c.bee, &c.bee_name, c.legacy_owner.as_deref()) {
                (Some(id), name, _) => Keeper::Bee {
                    id: id.clone(),
                    name: name.clone().unwrap_or_else(|| id.clone()),
                },
                (None, _, Some("owner")) => Keeper::Owner,
                _ => return None,
            };
            Some((c.cell.clone(), keeper))
        })
        .collect()
}

impl App {
    pub fn display_name(&self) -> String {
        display_app(&self.name)
    }
}

/// What is known about the apps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Apps {
    /// No answer yet (or the tool is not available).
    Unknown,
    /// The app platform (k3s) did not answer.
    Unreachable,
    Known {
        installed: Vec<App>,
        /// Catalog apps that are not installed.
        available: Vec<String>,
    },
}

/// One device.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Device {
    pub name: String,
    /// The node is ready (None: not known, e.g. no cluster).
    pub ready: Option<bool>,
    pub os: Option<String>,
    pub kernel: Option<String>,
    pub cpus: Option<String>,
    pub mem_total_kib: Option<u64>,
    pub mem_available_kib: Option<u64>,
    pub uptime_secs: Option<u64>,
    /// Space where apps keep their data (`/var`, else `/`), from `comb_info`'s `disks`.
    pub disk: Option<Disk>,
}

/// Space on one filesystem.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Disk {
    pub path: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_percent: u64,
}

impl Disk {
    /// Used at or above this percent: warn (the app strip and the device card).
    pub const WARN_PERCENT: u64 = 85;

    pub fn nearly_full(&self) -> bool {
        self.used_percent >= Self::WARN_PERCENT
    }
}

/// Where apps keep their data: `/var` if reported, else `/`.
fn data_disk(info: &Value) -> Option<Disk> {
    let disks = info["disks"].as_array()?;
    let pick = |p: &str| disks.iter().find(|d| d["path"] == p);
    let d = pick("/var").or_else(|| pick("/"))?;
    Some(Disk {
        path: d["path"].as_str()?.to_string(),
        total_bytes: d["total_bytes"].as_u64()?,
        available_bytes: d["available_bytes"].as_u64()?,
        used_percent: d["used_percent"].as_u64()?,
    })
}

/// Apps and devices at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HiveSnapshot {
    pub apps: Apps,
    pub devices: Vec<Device>,
}

impl Default for HiveSnapshot {
    fn default() -> Self {
        Self {
            apps: Apps::Unknown,
            devices: Vec::new(),
        }
    }
}

/// Plain words and a colour for an app phase (see `app_status`).
pub fn phase_words(phase: &str) -> (Health, &'static str) {
    match phase {
        "ready" => (Health::Ok, "Running"),
        "starting" => (Health::Busy, "Installing"),
        "failing" => (Health::Problem, "Needs attention"),
        "deleting" => (Health::Busy, "Removing"),
        "uninstalled_data_kept" => (Health::Idle, "Removed, data kept"),
        _ => (Health::Idle, "Unknown"),
    }
}

impl HiveSnapshot {
    /// Build from the tools' answers (None: tool missing or failed).
    pub fn from_tools(
        list_apps: Option<&Value>,
        cluster: Option<&Value>,
        bee: Option<&Value>,
    ) -> Self {
        let apps = match list_apps {
            None => Apps::Unknown,
            Some(v) if v["k3s_reachable"] == false => Apps::Unreachable,
            Some(v) => Apps::Known {
                installed: v["apps"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|a| {
                        Some(App {
                            name: a["app"].as_str()?.to_string(),
                            phase: a["phase"].as_str().unwrap_or("unknown").to_string(),
                            url: a["url"].as_str().map(str::to_string),
                            keeper: None,
                        })
                    })
                    .collect(),
                available: v["not_installed"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|n| n.as_str().map(str::to_string))
                    .collect(),
            },
        };

        let mut devices: Vec<Device> = cluster
            .and_then(|c| c["nodes"].as_array())
            .into_iter()
            .flatten()
            .map(|n| Device {
                name: n["name"].as_str().unwrap_or("Device").to_string(),
                ready: n["ready"].as_bool(),
                cpus: n["allocatable"]["cpu"].as_str().map(str::to_string),
                ..Device::default()
            })
            .collect();
        if let Some(b) = bee {
            // comb_info describes the device this daemon runs on. With one node that is it;
            // with several we can't tell which, so it becomes its own entry.
            if devices.len() != 1 {
                devices.insert(
                    0,
                    Device {
                        name: "This device".into(),
                        ..Device::default()
                    },
                );
            }
            let d = &mut devices[0];
            d.os = b["os"].as_str().map(str::to_string);
            d.kernel = b["kernel"].as_str().map(str::to_string);
            d.mem_total_kib = b["mem_total_kib"].as_u64();
            d.mem_available_kib = b["mem_available_kib"].as_u64();
            d.uptime_secs = b["uptime_secs"].as_u64();
            d.disk = data_disk(b);
        }
        Self { apps, devices }
    }

    /// Fill in who looks after each app.
    pub fn with_keepers(mut self, keepers: Vec<(String, Keeper)>) -> Self {
        if let Apps::Known { installed, .. } = &mut self.apps {
            for app in installed {
                app.keeper = keepers
                    .iter()
                    .find(|(cell, _)| *cell == app.name)
                    .map(|(_, k)| k.clone());
            }
        }
        self
    }
}

struct Cached {
    snap: Option<Arc<HiveSnapshot>>,
    at: Option<Instant>,
    refreshing: bool,
}

/// The cached snapshot. Cheap to clone.
#[derive(Clone)]
pub struct HiveCache {
    mcp: McpServer,
    queen: Option<Queen>,
    cached: Arc<Mutex<Cached>>,
}

impl HiveCache {
    /// How long an answer is fresh.
    pub const TTL: Duration = Duration::from_secs(30);
    /// Longest wait for one tool.
    const CALL_TIMEOUT: Duration = Duration::from_secs(8);

    /// `queen`: for who looks after which app (None: not shown).
    pub fn new(mcp: McpServer, queen: Option<Queen>) -> Self {
        Self {
            mcp,
            queen,
            cached: Arc::new(Mutex::new(Cached {
                snap: None,
                at: None,
                refreshing: false,
            })),
        }
    }

    /// The newest snapshot. The first call waits for the tools; later calls return the
    /// cached answer and refresh it in the background once it is stale.
    pub async fn get(&self) -> Arc<HiveSnapshot> {
        let (snap, stale) = {
            let mut c = lock(&self.cached);
            let stale = c.at.is_none_or(|at| at.elapsed() > Self::TTL);
            let start = stale && !c.refreshing && c.snap.is_some();
            if start {
                c.refreshing = true;
            }
            (c.snap.clone(), start)
        };
        match snap {
            Some(snap) => {
                if stale {
                    let me = self.clone();
                    tokio::spawn(async move { me.refresh().await });
                }
                snap
            }
            None => self.refresh().await,
        }
    }

    async fn refresh(&self) -> Arc<HiveSnapshot> {
        let (apps, cluster, bee, cells) = tokio::join!(
            self.call("list_apps"),
            self.call("cluster_health"),
            self.call("comb_info"),
            self.cells(),
        );
        let snap = Arc::new(
            HiveSnapshot::from_tools(apps.as_ref(), cluster.as_ref(), bee.as_ref())
                .with_keepers(keepers(&cells)),
        );
        let mut c = lock(&self.cached);
        c.snap = Some(snap.clone());
        c.at = Some(Instant::now());
        c.refreshing = false;
        snap
    }

    async fn cells(&self) -> Vec<CellView> {
        let Some(queen) = &self.queen else {
            return Vec::new();
        };
        match tokio::time::timeout(Self::CALL_TIMEOUT, queen.bees().cells()).await {
            Ok(Ok(cells)) => cells,
            Ok(Err(e)) => {
                tracing::debug!("UI cell read failed: {e}");
                Vec::new()
            }
            Err(_) => Vec::new(),
        }
    }

    async fn call(&self, tool: &str) -> Option<Value> {
        // The UI only reads: no scope (reads only), unattended, read-only tools.
        let mut ctx = TaskContext {
            scope: Scope::Nothing,
            ..TaskContext::default()
        };
        let call = self.mcp.registry().call(0, &mut ctx, tool, json!({}));
        match tokio::time::timeout(Self::CALL_TIMEOUT, call).await {
            Ok(Ok(v)) => Some(v),
            Ok(Err(e)) => {
                tracing::debug!(tool, "UI status read failed: {e}");
                None
            }
            Err(_) => {
                tracing::debug!(tool, "UI status read timed out");
                None
            }
        }
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
    fn the_data_disk_is_var_and_warns_from_85_percent() {
        let info = json!({"disks": [
            {"path": "/", "total_bytes": 100, "available_bytes": 50, "used_percent": 50},
            {"path": "/var", "total_bytes": 100, "available_bytes": 14, "used_percent": 86},
        ]});
        let snap = HiveSnapshot::from_tools(None, None, Some(&info));
        let disk = snap.devices[0].disk.clone().unwrap();
        assert_eq!(disk.path, "/var");
        assert!(disk.nearly_full());
        // Without /var, / counts; below 85% no warning.
        let info = json!({"disks": [
            {"path": "/", "total_bytes": 100, "available_bytes": 16, "used_percent": 84},
        ]});
        let snap = HiveSnapshot::from_tools(None, None, Some(&info));
        let disk = snap.devices[0].disk.clone().unwrap();
        assert_eq!(disk.path, "/");
        assert!(!disk.nearly_full());
        // Older comb_info without disks.
        let snap = HiveSnapshot::from_tools(None, None, Some(&json!({})));
        assert_eq!(snap.devices[0].disk, None);
    }

    #[test]
    fn snapshot_from_tool_answers() {
        let apps = json!({
            "apps": [
                {"app": "nextcloud", "phase": "ready", "url": "http://nextcloud.example.com"},
                {"app": "immich", "phase": "starting", "url": null},
            ],
            "not_installed": ["jellyfin"],
        });
        let cluster =
            json!({"nodes": [{"name": "comb-1", "ready": true, "allocatable": {"cpu": "4"}}]});
        let bee = json!({"os": "Fedora Linux 45", "mem_total_kib": 8_000_000, "mem_available_kib": 2_000_000});
        let s = HiveSnapshot::from_tools(Some(&apps), Some(&cluster), Some(&bee));
        let Apps::Known {
            installed,
            available,
        } = &s.apps
        else {
            panic!()
        };
        assert_eq!(installed.len(), 2);
        assert_eq!(installed[0].display_name(), "Nextcloud");
        assert_eq!(available, &["jellyfin"]);
        assert_eq!(s.devices.len(), 1);
        assert_eq!(s.devices[0].name, "comb-1");
        assert_eq!(s.devices[0].os.as_deref(), Some("Fedora Linux 45"));
        assert_eq!(s.devices[0].cpus.as_deref(), Some("4"));

        let s = HiveSnapshot::from_tools(Some(&json!({"k3s_reachable": false})), None, Some(&bee));
        assert_eq!(s.apps, Apps::Unreachable);
        assert_eq!(s.devices[0].name, "This device");
        assert_eq!(
            HiveSnapshot::from_tools(None, None, None),
            HiveSnapshot::default()
        );

        let cell = |name: &str, bee: Option<&str>, legacy: Option<&str>| CellView {
            cell: name.into(),
            bee: bee.map(str::to_string),
            bee_name: bee.map(|_| "Ada".to_string()),
            comb: None,
            assigned_by: None,
            assigned_at: None,
            legacy_owner: legacy.map(str::to_string),
        };
        let k = keepers(&[
            cell("nextcloud", Some("c.b1"), None),
            cell("immich", None, Some("owner")),
            cell("old", None, Some("job-1")),
        ]);
        let s = HiveSnapshot::from_tools(Some(&apps), None, None).with_keepers(k);
        let Apps::Known { installed, .. } = &s.apps else {
            panic!()
        };
        assert_eq!(
            installed[0].keeper,
            Some(Keeper::Bee {
                id: "c.b1".into(),
                name: "Ada".into()
            })
        );
        assert_eq!(installed[1].keeper, Some(Keeper::Owner));
    }
}
