//! This device's Melissa release and container image updates.
//!
//! Melissa runs as a rootful podman container (packaging/container). The lead agent inside it can't
//! run the host's podman, so a small host helper does the privileged part:
//!
//! - The lead agent asks by creating an **empty file** in [`REQUEST_DIR`] (a bind mount of the
//!   host's `/var/lib/melissa-container/request`) named `check`, `apply`, `rollback`, `auto-on`,
//!   `auto-off` or `schedule-<daily|mon..sun>-<00..23>` ([`UpdateAction`], [`UpdateSetting`]). A
//!   host path unit starts `melissa-host-update requests`, which acts only on those names and
//!   never reads file contents. Nothing else travels from the web.
//! - The helper writes `status.json` (running, staged, available and previous image, the
//!   schedule, a held version, the last update), `last.json` (the outcome of its last action)
//!   and, while something runs, `action.json` (its step and download progress) to the host's
//!   `/var/lib/melissa-container/status`, mounted read-only at [`STATE_DIR`].
//! - Automatic updates run on the host at the owner's time (`melissa-auto-update.timer`), and a
//!   daily check (`melissa-check.timer`, no download) runs either way.
//! - Restarting the lead agent itself (to use saved settings) goes through systemd inside the
//!   container: a polkit rule lets the `melissa` user restart `melissa-server.service`, nothing else.
//!
//! Without the helper (a dev container, a from-source run) the UI shows the host commands.
//!
//! A Nix install (`MELISSA_RUNTIME=nix` in the release file, which the Nix package ships in the
//! store and names with [`RELEASE_FILE_ENV`]) updates with the system's Nix configuration: the UI
//! shows the commands and offers no update buttons, only the lead agent's restart.

use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{clean_text, SettingsError};

/// Where the host helper's status arrives (read-only bind mount).
pub const STATE_DIR: &str = "/run/melissa-update";
/// Where the lead agent drops requests for the host helper (bind mount, group `melissa` writable).
pub const REQUEST_DIR: &str = "/run/melissa-update-request";
/// Build facts written into the image.
pub const RELEASE_FILE: &str = "/usr/lib/melissa/release";
/// Overrides [`RELEASE_FILE`]: the Nix packaging sets it to the release file in the store.
pub const RELEASE_FILE_ENV: &str = "MELISSA_RELEASE_FILE";

/// The release file to read: `$MELISSA_RELEASE_FILE` if set, else [`RELEASE_FILE`].
pub fn release_file() -> PathBuf {
    std::env::var_os(RELEASE_FILE_ENV)
        .filter(|v| !v.is_empty())
        .map_or_else(|| PathBuf::from(RELEASE_FILE), PathBuf::from)
}
/// The host helper, for commands typed on the host.
pub const HOST_HELPER: &str = "/usr/local/libexec/melissa/melissa-host-update";

/// Which Melissa this is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Release {
    pub version: String,
    /// The source revision the image was built from, if recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// How it is packaged: `container` or `nix` (from the release file only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// `image` (the release file), `env` (MELISSA_VERSION) or `source` (a cargo build).
    pub source: &'static str,
}

impl Release {
    /// From the release file, else `MELISSA_VERSION`, else this build.
    pub fn read(file: &Path) -> Self {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        if let Ok(text) = std::fs::read_to_string(file) {
            let get = |key: &str| {
                text.lines()
                    .filter_map(|l| l.split_once('='))
                    .find(|(k, _)| k.trim() == key)
                    .map(|(_, v)| clean_text(v.trim().trim_matches('"'), 64))
                    .filter(|v| !v.is_empty())
            };
            if let Some(version) = get("MELISSA_VERSION") {
                return Self {
                    version,
                    revision: get("MELISSA_REVISION"),
                    runtime: get("MELISSA_RUNTIME"),
                    source: "image",
                };
            }
        }
        if let Some(v) = env("MELISSA_VERSION") {
            return Self {
                version: clean_text(&v, 64),
                revision: None,
                runtime: None,
                source: "env",
            };
        }
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            revision: None,
            runtime: None,
            source: "source",
        }
    }
}

/// What kind of host the lead agent runs on, which decides how it is updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    /// The Melissa container image under podman.
    Container,
    /// The Nix package: the NixOS module or the `melissa-host` profile on another distribution.
    Nix,
    /// Anything else, e.g. a developer's machine running from source.
    Other,
}

impl HostKind {
    /// From the release's `runtime` (`nix`), else marker files under `root` (`/` normally).
    pub fn detect(root: &Path, runtime: Option<&str>) -> Self {
        let run = root.join("run");
        if runtime == Some("nix") {
            Self::Nix
        } else if run.join(".containerenv").exists() || run.join("systemd/container").exists() {
            Self::Container
        } else {
            Self::Other
        }
    }
}

/// What podman writes to `/run/.containerenv` for a privileged container: the image name and ID
/// the container was created from (known even without the host helper).
pub fn container_image(root: &Path) -> Option<ImageView> {
    let text = std::fs::read_to_string(root.join("run/.containerenv")).ok()?;
    let get = |key: &str| {
        text.lines()
            .filter_map(|l| l.split_once('='))
            .find(|(k, _)| k.trim() == key)
            .map(|(_, v)| clean_text(v.trim().trim_matches('"'), 200))
            .filter(|v| !v.is_empty())
    };
    let image = get("image")?;
    Some(ImageView {
        image,
        id: get("imageid").map(|i| clean_text(&i, 80)),
        ..ImageView::default()
    })
}

// --- status.json (melissa-host-update) ------------------------------------------------------------

#[derive(Deserialize, Default)]
struct RawStatus {
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    auto_update: Option<String>,
    #[serde(default)]
    running: Option<RawImage>,
    #[serde(default)]
    staged: Option<RawImage>,
    #[serde(default)]
    available: Option<RawImage>,
    #[serde(default)]
    previous: Option<RawImage>,
    #[serde(default)]
    schedule: Option<String>,
    #[serde(default)]
    next_run: Option<u64>,
    #[serde(default)]
    tz_offset: Option<String>,
    #[serde(default)]
    held: Option<RawHeld>,
    #[serde(default)]
    just_updated: Option<RawUpdated>,
}

#[derive(Deserialize, Default)]
struct RawHeld {
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    revision: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    at: Option<u64>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawUpdated {
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    from_revision: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    to_revision: Option<String>,
    #[serde(default)]
    at: Option<u64>,
}

#[derive(Deserialize, Default)]
struct RawImage {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    digest: Option<String>,
    #[serde(default)]
    created: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    revision: Option<String>,
    #[serde(default)]
    size: Option<u64>,
}

/// One container image as the UI shows it (all text cleaned).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ImageView {
    /// e.g. `registry.example.com/melissa:latest`.
    pub image: String,
    /// The local image ID (`sha256:…` or bare hex).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The manifest digest (`sha256:…`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// Build time (RFC 3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// The download size in bytes (an update found by a check, where the helper knows it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

impl ImageView {
    fn from(image: &str, raw: RawImage) -> Self {
        let opt = |v: Option<String>, n| v.map(|v| clean_text(&v, n)).filter(|v| !v.is_empty());
        Self {
            image: image.to_string(),
            id: opt(raw.id, 80),
            digest: opt(raw.digest, 80),
            version: opt(raw.version, 64),
            revision: opt(raw.revision, 64),
            timestamp: opt(raw.created, 40),
            size: raw.size.filter(|s| *s > 0),
        }
    }

    /// The build date (`2026-11-26`), if known.
    pub fn built(&self) -> Option<&str> {
        self.timestamp.as_deref().map(|t| t.get(..10).unwrap_or(t))
    }

    /// A short digest for people: `sha256:0123456789ab` (the image ID if there is no digest).
    pub fn short_digest(&self) -> Option<String> {
        let short = |d: &String| {
            let d = if d.contains(':') {
                d.clone()
            } else {
                format!("sha256:{d}")
            };
            d.chars().take("sha256:".len() + 12).collect()
        };
        self.digest.as_ref().or(self.id.as_ref()).map(short)
    }
}

/// What the host helper reports, reduced to what the UI needs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ImageStatus {
    /// The image name Melissa runs (the Quadlet's `Image=`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// `registry`, `local` or `off`: whether automatic updates install newer images.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_update: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running: Option<ImageView>,
    /// A newer image already downloaded: used at the next start (or Apply).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staged: Option<ImageView>,
    /// A newer image found in the registry by the last check (not downloaded yet).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available: Option<ImageView>,
    /// The image Melissa ran before, which a rollback returns to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<ImageView>,
    /// When automatic updates run (also while they are off: the time they would use).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
    /// The automatic update's next start as systemd planned it (Unix seconds; none while off).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_run: Option<u64>,
    /// The host's UTC offset when the status was written (`+0100`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tz_offset: Option<String>,
    /// A version automatic updates skip: it didn't start, or Melissa was rolled back from it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub held: Option<Held>,
    /// The last successful update (from which version to which, and when).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub just_updated: Option<JustUpdated>,
}

impl ImageStatus {
    /// Automatic updates are on (any policy but `off`).
    pub fn auto_on(&self) -> bool {
        matches!(self.auto_update.as_deref(), Some("registry" | "local"))
    }

    /// Melissa runs an image built or loaded on this host (`localhost/`): nothing to look up,
    /// no schedule to choose.
    pub fn local_image(&self) -> bool {
        self.auto_update.as_deref() == Some("local")
            || self
                .image
                .as_deref()
                .is_some_and(|i| i.starts_with("localhost/"))
    }
}

/// Why a version is held back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HeldReason {
    /// The new version didn't start; Melissa went back by herself.
    Failed,
    /// The owner rolled back from it.
    RolledBack,
}

/// A version automatic updates skip until a newer one comes out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Held {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    pub reason: HeldReason,
    /// Unix seconds.
    pub at: u64,
    /// The helper's reason, for Details only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// The last successful update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JustUpdated {
    pub from: Option<String>,
    pub from_revision: Option<String>,
    pub to: Option<String>,
    pub to_revision: Option<String>,
    /// Unix seconds.
    pub at: u64,
}

/// A day of the automatic update: every day or one weekday.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateDay {
    Daily,
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl UpdateDay {
    /// Every choice, in the order the page lists them.
    pub const ALL: [Self; 8] = [
        Self::Daily,
        Self::Mon,
        Self::Tue,
        Self::Wed,
        Self::Thu,
        Self::Fri,
        Self::Sat,
        Self::Sun,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Mon => "mon",
            Self::Tue => "tue",
            Self::Wed => "wed",
            Self::Thu => "thu",
            Self::Fri => "fri",
            Self::Sat => "sat",
            Self::Sun => "sun",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.as_str() == s)
    }

    /// "Every day", "Every Sunday".
    pub fn label(self) -> &'static str {
        match self {
            Self::Daily => "Every day",
            Self::Mon => "Every Monday",
            Self::Tue => "Every Tuesday",
            Self::Wed => "Every Wednesday",
            Self::Thu => "Every Thursday",
            Self::Fri => "Every Friday",
            Self::Sat => "Every Saturday",
            Self::Sun => "Every Sunday",
        }
    }

    /// The weekday, Monday = 1 … Sunday = 7 (None: every day).
    pub fn weekday(self) -> Option<i8> {
        match self {
            Self::Daily => None,
            Self::Mon => Some(1),
            Self::Tue => Some(2),
            Self::Wed => Some(3),
            Self::Thu => Some(4),
            Self::Fri => Some(5),
            Self::Sat => Some(6),
            Self::Sun => Some(7),
        }
    }
}

/// When automatic updates run: a day and a whole hour in the host's time (the update starts
/// within 15 minutes after it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Schedule {
    pub day: UpdateDay,
    /// 0..=23.
    pub hour: u8,
}

impl Default for Schedule {
    /// Every day at 03:00 (owner, 2026-11-28).
    fn default() -> Self {
        Self {
            day: UpdateDay::Daily,
            hour: 3,
        }
    }
}

impl Schedule {
    /// From `daily-03`, `sun-23`; anything else is refused.
    pub fn parse(s: &str) -> Option<Self> {
        let (day, hour) = s.split_once('-')?;
        let day = UpdateDay::parse(day)?;
        if hour.len() != 2 || !hour.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let hour: u8 = hour.parse().ok()?;
        (hour <= 23).then_some(Self { day, hour })
    }

    /// `daily-03`.
    pub fn as_string(self) -> String {
        format!("{}-{:02}", self.day.as_str(), self.hour)
    }
}

/// Parse the host helper's `status.json`. Unknown fields are ignored.
pub fn parse_status(json: &str) -> Result<ImageStatus, SettingsError> {
    let raw: RawStatus = serde_json::from_str(json).map_err(|e| SettingsError::Parse {
        path: "status.json".into(),
        message: e.to_string(),
    })?;
    let name = raw
        .image
        .map(|i| clean_text(&i, 200))
        .filter(|i| !i.is_empty());
    let img = |r: Option<RawImage>| r.map(|r| ImageView::from(name.as_deref().unwrap_or("?"), r));
    let text = |v: Option<String>, n| v.map(|v| clean_text(&v, n)).filter(|v| !v.is_empty());
    let held = raw.held.and_then(|h| {
        let reason = match h.reason.as_deref() {
            Some("failed") => HeldReason::Failed,
            Some("rolled_back") => HeldReason::RolledBack,
            _ => return None,
        };
        Some(Held {
            version: text(h.version, 64),
            revision: text(h.revision, 64),
            reason,
            at: h.at.unwrap_or(0),
            message: text(h.message, 300),
        })
    });
    let just_updated = raw.just_updated.and_then(|u| {
        Some(JustUpdated {
            at: u.at?,
            from: text(u.from, 64),
            from_revision: text(u.from_revision, 64),
            to: text(u.to, 64),
            to_revision: text(u.to_revision, 64),
        })
    });
    let mut view = ImageStatus {
        auto_update: text(raw.auto_update, 20),
        running: img(raw.running),
        staged: img(raw.staged),
        available: img(raw.available),
        previous: img(raw.previous),
        image: name.clone(),
        schedule: raw.schedule.as_deref().and_then(Schedule::parse),
        next_run: raw.next_run,
        tz_offset: text(raw.tz_offset, 8),
        held,
        just_updated,
    };
    // An update that is already downloaded is shown once, as staged.
    if view.staged.is_some() {
        view.available = None;
    }
    Ok(view)
}

// --- What the host helper wrote ---------------------------------------------------------------------

/// The outcome of the helper's last action (`last.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastAction {
    /// `check`, `apply`, `auto` or `rollback`.
    pub action: String,
    pub ok: bool,
    /// Unix seconds.
    pub at: u64,
    /// The helper's words (Details only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// What happened, as a fixed word: `up_to_date`, `available`, `held`, `updated`,
    /// `rolled_back`, `check` (the registry couldn't be asked), `download`, `no_start`,
    /// `no_previous`, `not_running`, `busy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The version it was about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// What the helper is doing right now (`action.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningAction {
    /// `check`, `apply`, `auto` or `rollback`.
    pub action: String,
    /// `check`, `download`, `restart` or `health`.
    pub step: String,
    /// The version it moves to, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default)]
    pub bytes_done: u64,
    /// 0: unknown.
    #[serde(default)]
    pub bytes_total: u64,
    #[serde(default)]
    pub started: u64,
    /// When the helper last wrote it (Unix seconds).
    #[serde(default)]
    pub updated: u64,
}

/// A running action whose file wasn't touched for this long is left over (the helper died).
pub const RUNNING_STALE_SECS: u64 = 15 * 60;

/// What the page shows while the helper works: one at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Busy {
    Checking,
    Updating,
    RollingBack,
}

impl Busy {
    fn from_action(a: &str) -> Option<Self> {
        match a {
            "check" => Some(Self::Checking),
            "apply" | "auto" => Some(Self::Updating),
            "rollback" => Some(Self::RollingBack),
            _ => None,
        }
    }

    /// Restarts Melissa (no other restart-causing request may start meanwhile).
    pub fn restarts(self) -> bool {
        !matches!(self, Self::Checking)
    }
}

/// Requests placed for the helper that it hasn't picked up yet (names only).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PendingRequests {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<UpdateAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
}

/// Everything the Updates page shows about the image.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct UpdateState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<ImageStatus>,
    /// When the status was written (Unix seconds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last: Option<LastAction>,
    /// Why the status could not be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// What the helper is doing (fresh only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running: Option<RunningAction>,
    /// Requests waiting for the helper.
    pub pending: PendingRequests,
}

impl UpdateState {
    /// What runs or is about to: the helper's running action, else a request it hasn't picked
    /// up yet.
    pub fn busy(&self) -> Option<Busy> {
        self.running
            .as_ref()
            .and_then(|r| Busy::from_action(&r.action))
            .or_else(|| {
                self.pending
                    .action
                    .and_then(|a| Busy::from_action(a.as_str()))
            })
    }

    /// Automatic updates are on, counting a switch the helper hasn't carried out yet.
    pub fn auto_on(&self) -> bool {
        self.pending
            .auto
            .unwrap_or_else(|| self.status.as_ref().is_some_and(ImageStatus::auto_on))
    }

    /// The automatic update's time, counting a change the helper hasn't carried out yet.
    pub fn schedule(&self) -> Schedule {
        self.pending
            .schedule
            .or_else(|| self.status.as_ref().and_then(|s| s.schedule))
            .unwrap_or_default()
    }
}

/// Read the helper's files in `dir`.
pub fn read_state(dir: &Path) -> UpdateState {
    let mut state = UpdateState::default();
    let status_path = dir.join("status.json");
    match std::fs::read_to_string(&status_path) {
        Ok(text) => match parse_status(&text) {
            Ok(v) => state.status = Some(v),
            Err(e) => state.error = Some(clean_text(&e.to_string(), 200)),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => state.error = Some(format!("cannot read {}: {e}", status_path.display())),
    }
    state.status_at = std::fs::metadata(&status_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    state.last = std::fs::read_to_string(dir.join("last.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<LastAction>(&t).ok())
        .map(|mut l| {
            l.action = clean_text(&l.action, 20);
            l.message = l
                .message
                .map(|m| clean_text(&m, 300))
                .filter(|m| !m.is_empty());
            l.kind = l.kind.map(|k| clean_text(&k, 20)).filter(|k| !k.is_empty());
            l.version = l
                .version
                .map(|v| clean_text(&v, 64))
                .filter(|v| !v.is_empty());
            l
        });
    state.running = read_running(dir, now_secs());
    state
}

/// `action.json`, unless it is left over from a helper that stopped.
fn read_running(dir: &Path, now: u64) -> Option<RunningAction> {
    let text = std::fs::read_to_string(dir.join("action.json")).ok()?;
    let mut r: RunningAction = serde_json::from_str(&text).ok()?;
    if now.saturating_sub(r.updated) > RUNNING_STALE_SECS {
        return None;
    }
    r.action = clean_text(&r.action, 20);
    r.step = clean_text(&r.step, 20);
    r.version = r
        .version
        .map(|v| clean_text(&v, 64))
        .filter(|v| !v.is_empty());
    Some(r)
}

/// The requests in `dir` the helper hasn't picked up (only names Melissa places count).
pub fn read_pending(dir: &Path) -> PendingRequests {
    let mut p = PendingRequests::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return p;
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in names {
        if let Some(a) = UpdateAction::parse(&name).filter(|a| a.request_name().is_some()) {
            // Apply and rollback say more than a check.
            if p.action.is_none_or(|old| old == UpdateAction::Check) {
                p.action = Some(a);
            }
        } else if let Some(s) = UpdateSetting::parse(&name) {
            match s {
                UpdateSetting::Auto(on) => p.auto = Some(on),
                UpdateSetting::Schedule(s) => p.schedule = Some(s),
            }
        }
    }
    p
}

// --- Actions -------------------------------------------------------------------------------------

/// The only things the lead agent may ask for. Each maps to one fixed request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateAction {
    /// Look for a newer image.
    Check,
    /// Download the newer image and restart Melissa on it.
    Apply,
    /// Restart Melissa on the previous image.
    Rollback,
    /// Restart Melissa's lead agent (to use saved settings).
    Restart,
}

impl UpdateAction {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "check" => Some(Self::Check),
            "apply" => Some(Self::Apply),
            "rollback" => Some(Self::Rollback),
            "restart" => Some(Self::Restart),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Apply => "apply",
            Self::Rollback => "rollback",
            Self::Restart => "restart",
        }
    }

    /// Restarts all of Melissa: chats in progress and apps pause (needs the owner's yes).
    pub fn restarts_melissa(self) -> bool {
        matches!(self, Self::Apply | Self::Rollback)
    }

    /// The `systemctl` arguments for a restart of the lead agent: a fixed verb and unit.
    pub fn systemctl_args(self) -> Option<[&'static str; 3]> {
        match self {
            Self::Restart => Some(["restart", "--no-block", "melissa-server.service"]),
            _ => None,
        }
    }

    /// The request file name for the host helper (check, apply, rollback).
    pub fn request_name(self) -> Option<&'static str> {
        match self {
            Self::Check | Self::Apply | Self::Rollback => Some(self.as_str()),
            Self::Restart => None,
        }
    }

    /// What to type on the host instead, where the helper is missing.
    pub fn manual_command(self) -> String {
        match self {
            Self::Restart => "sudo podman exec melissa systemctl restart melissa-server".into(),
            _ => format!("sudo {HOST_HELPER} {}", self.as_str()),
        }
    }
}

/// A setting for the helper: automatic updates on or off, and their time. Each is one fixed
/// request name, checked against fixed lists before it is placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateSetting {
    Auto(bool),
    Schedule(Schedule),
}

impl UpdateSetting {
    /// `auto-on`, `auto-off`, `schedule-daily-03`.
    pub fn request_name(self) -> String {
        match self {
            Self::Auto(true) => "auto-on".into(),
            Self::Auto(false) => "auto-off".into(),
            Self::Schedule(s) => format!("schedule-{}", s.as_string()),
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "auto-on" => Some(Self::Auto(true)),
            "auto-off" => Some(Self::Auto(false)),
            _ => name
                .strip_prefix("schedule-")
                .and_then(Schedule::parse)
                .map(Self::Schedule),
        }
    }

    /// Another request of the same kind (an older choice the helper hasn't picked up).
    fn same_kind(self, name: &str) -> bool {
        matches!(
            (self, Self::parse(name)),
            (Self::Auto(_), Some(Self::Auto(_))) | (Self::Schedule(_), Some(Self::Schedule(_)))
        )
    }
}

/// Melissa's polkit rule file and the directories polkit reads it from (relative to `root`).
const POLKIT_RULE: &str = "50-melissa.rules";
const POLKIT_RULE_DIRS: [&str; 3] = [
    "usr/share/polkit-1/rules.d",
    "etc/polkit-1/rules.d",
    "run/current-system/sw/share/polkit-1/rules.d",
];

/// Where the helper writes and reads; `root` is `/` except in tests.
#[derive(Debug, Clone)]
pub struct UpdateEnv {
    pub root: PathBuf,
    pub kind: HostKind,
    pub state_dir: PathBuf,
    pub request_dir: PathBuf,
}

impl UpdateEnv {
    pub fn system() -> Self {
        let root = PathBuf::from("/");
        let release = Release::read(&release_file());
        Self {
            kind: HostKind::detect(&root, release.runtime.as_deref()),
            state_dir: PathBuf::from(STATE_DIR),
            request_dir: PathBuf::from(REQUEST_DIR),
            root,
        }
    }

    /// The host helper's directories are mounted (installed with install.sh).
    pub fn helper_installed(&self) -> bool {
        self.kind == HostKind::Container && self.request_dir.is_dir() && self.state_dir.is_dir()
    }

    /// A Nix install on NixOS (else the `melissa-host` profile on another distribution).
    pub fn nixos(&self) -> bool {
        self.root.join("etc/NIXOS").exists()
    }

    /// The lead agent may restart itself: Melissa's polkit rule (restart the lead agent, nothing else) is
    /// installed, by the container image in /usr, by the non-NixOS Nix bundle in /etc, or by the
    /// NixOS module in the system profile.
    pub fn can_restart(&self) -> bool {
        POLKIT_RULE_DIRS
            .iter()
            .any(|d| self.root.join(d).join(POLKIT_RULE).exists())
    }

    /// Whether the UI may offer `action` as a button.
    pub fn can(&self, action: UpdateAction) -> bool {
        match action {
            UpdateAction::Restart => self.can_restart(),
            _ => self.helper_installed(),
        }
    }

    pub fn state(&self) -> UpdateState {
        let mut s = read_state(&self.state_dir);
        s.pending = read_pending(&self.request_dir);
        s
    }

    /// Ask the helper to change a setting. An older request of the same kind that it hasn't
    /// picked up yet is withdrawn first, so the last choice wins.
    pub fn request_setting(&self, setting: UpdateSetting) -> Result<(), SettingsError> {
        if let Ok(entries) = std::fs::read_dir(&self.request_dir) {
            for e in entries.filter_map(|e| e.ok()) {
                let name = e.file_name().to_string_lossy().into_owned();
                if setting.same_kind(&name) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        place_request(&self.request_dir, &setting.request_name())
    }

    /// The running image from podman's `/run/.containerenv` (fallback when the helper is absent).
    pub fn container_image(&self) -> Option<ImageView> {
        container_image(&self.root)
    }

    /// Ask for `action`: a restart through systemd, anything else through the host helper.
    /// Returns once the request is placed; the outcome lands in `last.json`.
    pub async fn request(&self, action: UpdateAction) -> Result<(), SettingsError> {
        match action.request_name() {
            Some(name) => place_request(&self.request_dir, name),
            None => systemctl(action).await,
        }
    }
}

/// Create the empty request file `dir/name` (the helper only looks at the name).
fn place_request(dir: &Path, name: &str) -> Result<(), SettingsError> {
    let path = dir.join(name);
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .map(|_| ())
        .map_err(|source| SettingsError::Write {
            path: path.display().to_string(),
            source,
        })
}

/// Ask systemd (through polkit) to restart the lead agent. Returns once systemd took the job.
async fn systemctl(action: UpdateAction) -> Result<(), SettingsError> {
    let Some(args) = action.systemctl_args() else {
        return Err(SettingsError::Invalid(format!(
            "{} is not a systemd action",
            action.as_str()
        )));
    };
    let mut cmd = tokio::process::Command::new("systemctl");
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    match tokio::time::timeout(Duration::from_secs(15), cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => Ok(()),
        Ok(Ok(out)) => Err(SettingsError::Failed(format!(
            "systemd refused: {}",
            clean_text(&String::from_utf8_lossy(&out.stderr), 200)
        ))),
        Ok(Err(e)) => Err(SettingsError::Failed(format!("cannot run systemctl: {e}"))),
        Err(_) => Err(SettingsError::Failed(
            "systemctl did not answer in time".into(),
        )),
    }
}

/// Unix seconds now.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// "just now", "5 min ago", "3 h ago", "2 days ago".
pub fn ago(then: u64, now: u64) -> String {
    let s = now.saturating_sub(then);
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        _ if s / 86_400 == 1 => "1 day ago".into(),
        _ => format!("{} days ago", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::scratch;

    fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        std::fs::read_to_string(&path).unwrap()
    }

    #[test]
    fn parses_running_staged_previous_and_found_update() {
        let v = parse_status(&fixture("container-status-running.json")).unwrap();
        let r = v.running.as_ref().unwrap();
        assert_eq!(r.image, "localhost/melissa:latest");
        assert_eq!(r.short_digest().as_deref(), Some("sha256:5b0d3c1e9f2a"));
        assert_eq!(r.version.as_deref(), Some("0.1.0"));
        assert_eq!(v.auto_update.as_deref(), Some("local"));
        assert!(v.staged.is_none() && v.previous.is_none() && v.available.is_none());

        let v = parse_status(&fixture("container-status-staged.json")).unwrap();
        assert_eq!(
            v.staged.as_ref().and_then(|s| s.version.as_deref()),
            Some("0.1.1")
        );
        assert_eq!(
            v.previous.as_ref().map(|p| p.image.as_str()),
            Some("registry.example.com/melissa:latest")
        );
        // Staged wins: the found update is not shown twice.
        assert!(v.available.is_none());
        let v = parse_status(&fixture("container-status-update.json")).unwrap();
        // The registry check knows only that there is something newer.
        assert_eq!(
            v.available,
            Some(ImageView {
                image: "registry.example.com/melissa:latest".into(),
                ..ImageView::default()
            })
        );
        assert!(v.staged.is_none());
    }

    #[test]
    fn bad_or_hostile_status_is_handled() {
        assert!(parse_status("not json").is_err());
        assert_eq!(parse_status("{}").unwrap(), ImageStatus::default());
        let hostile =
            r#"{"image":"<script>\u001b[31mx","running":{"version":"1\u0007"},"previous":null}"#;
        let v = parse_status(hostile).unwrap();
        let r = v.running.unwrap();
        assert_eq!(r.image, "<script>x");
        assert_eq!(r.version.as_deref(), Some("1"));
    }

    #[test]
    fn reads_the_helpers_files() {
        let dir = scratch("update");
        assert_eq!(read_state(&dir), UpdateState::default());
        std::fs::write(
            dir.join("status.json"),
            fixture("container-status-update.json"),
        )
        .unwrap();
        std::fs::write(
            dir.join("last.json"),
            r#"{"action":"check","ok":false,"at":1763500000,"message":"registry unreachable"}"#,
        )
        .unwrap();
        let s = read_state(&dir);
        assert!(s.status.is_some() && s.status_at.is_some() && s.error.is_none());
        let last = s.last.unwrap();
        assert!(!last.ok);
        assert_eq!(last.message.as_deref(), Some("registry unreachable"));
        std::fs::write(dir.join("status.json"), "garbage").unwrap();
        assert!(read_state(&dir).error.is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn host_kind_and_helper_detection() {
        let root = scratch("root");
        std::fs::create_dir_all(root.join("run")).unwrap();
        assert_eq!(HostKind::detect(&root, None), HostKind::Other);
        let env = UpdateEnv {
            root: root.clone(),
            kind: HostKind::Container,
            state_dir: root.join("run/melissa-update"),
            request_dir: root.join("run/melissa-update-request"),
        };
        assert!(!env.helper_installed() && !env.can(UpdateAction::Check));
        assert!(!env.can(UpdateAction::Restart));
        std::fs::create_dir_all(&env.state_dir).unwrap();
        std::fs::create_dir_all(&env.request_dir).unwrap();
        std::fs::create_dir_all(root.join("usr/share/polkit-1/rules.d")).unwrap();
        std::fs::write(root.join("usr/share/polkit-1/rules.d/50-melissa.rules"), "").unwrap();
        assert!(env.can(UpdateAction::Apply) && env.can(UpdateAction::Restart));
        let other = UpdateEnv {
            kind: HostKind::Other,
            ..env.clone()
        };
        assert!(!other.can(UpdateAction::Check));
        std::fs::write(
            root.join("run/.containerenv"),
            "engine=\"podman-6.1.3\"\nname=\"melissa\"\nimage=\"localhost/melissa:latest\"\nimageid=\"5b0d3c1e9f2a77\"\nrootless=0\n",
        )
        .unwrap();
        assert_eq!(HostKind::detect(&root, None), HostKind::Container);
        assert_eq!(HostKind::detect(&root, Some("nix")), HostKind::Nix);
        let img = env.container_image().unwrap();
        assert_eq!(img.image, "localhost/melissa:latest");
        assert_eq!(img.short_digest().as_deref(), Some("sha256:5b0d3c1e9f2a"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn requests_are_empty_files_with_fixed_names() {
        let root = scratch("request");
        let env = UpdateEnv {
            root: root.clone(),
            kind: HostKind::Container,
            state_dir: root.join("state"),
            request_dir: root.join("req"),
        };
        // No mount: the request fails instead of pretending.
        assert!(env.request(UpdateAction::Check).await.is_err());
        std::fs::create_dir_all(&env.request_dir).unwrap();
        env.request(UpdateAction::Apply).await.unwrap();
        let names: Vec<String> = std::fs::read_dir(&env.request_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["apply"]);
        assert_eq!(
            std::fs::metadata(env.request_dir.join("apply"))
                .unwrap()
                .len(),
            0
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn restart_needs_only_the_polkit_rule() {
        // NixOS: the rule comes with the system profile; no host helper, no container.
        let root = scratch("nixos-root");
        let env = UpdateEnv {
            root: root.clone(),
            kind: HostKind::Other,
            state_dir: root.join("run/melissa-update"),
            request_dir: root.join("run/melissa-update-request"),
        };
        assert!(!env.can(UpdateAction::Restart));
        let dir = root.join("run/current-system/sw/share/polkit-1/rules.d");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("50-melissa.rules"), "").unwrap();
        assert!(env.can(UpdateAction::Restart));
        assert!(!env.helper_installed() && !env.can(UpdateAction::Check));
        // Nix: restart yes, never the helper's actions (even with stray mounts).
        let nix = UpdateEnv {
            kind: HostKind::Nix,
            ..env.clone()
        };
        std::fs::create_dir_all(&nix.state_dir).unwrap();
        std::fs::create_dir_all(&nix.request_dir).unwrap();
        assert!(nix.can(UpdateAction::Restart) && !nix.can(UpdateAction::Apply));
        assert!(!nix.nixos());
        std::fs::create_dir_all(root.join("etc")).unwrap();
        std::fs::write(root.join("etc/NIXOS"), "").unwrap();
        assert!(nix.nixos());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn actions_map_to_fixed_requests() {
        for s in ["check", "apply", "rollback"] {
            let a = UpdateAction::parse(s).unwrap();
            assert_eq!(a.as_str(), s);
            assert_eq!(a.request_name(), Some(s));
            assert!(a.systemctl_args().is_none());
            assert!(a
                .manual_command()
                .ends_with(&format!("melissa-host-update {s}")));
        }
        let r = UpdateAction::Restart;
        assert_eq!(r.request_name(), None);
        assert_eq!(r.systemctl_args().unwrap()[2], "melissa-server.service");
        assert_eq!(UpdateAction::parse("reboot; rm -rf /"), None);
        assert!(UpdateAction::Apply.restarts_melissa() && !UpdateAction::Check.restarts_melissa());
    }

    #[test]
    fn release_file_wins_over_env_and_build() {
        let dir = scratch("release");
        let file = dir.join("release");
        std::fs::write(
            &file,
            "MELISSA_VERSION=0.3.1\nMELISSA_REVISION=abc1234\nMELISSA_RUNTIME=container\n",
        )
        .unwrap();
        let r = Release::read(&file);
        assert_eq!(
            (
                r.version.as_str(),
                r.revision.as_deref(),
                r.runtime.as_deref(),
                r.source
            ),
            ("0.3.1", Some("abc1234"), Some("container"), "image")
        );
        std::fs::write(
            &file,
            "MELISSA_VERSION=0.0.1\nMELISSA_REVISION=abc1234\nMELISSA_RUNTIME=nix\n",
        )
        .unwrap();
        assert_eq!(Release::read(&file).runtime.as_deref(), Some("nix"));
        let r = Release::read(&dir.join("missing"));
        assert!(r.source == "source" || r.source == "env");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn schedules_and_settings_are_fixed_names() {
        assert_eq!(Schedule::default().as_string(), "daily-03");
        let s = Schedule::parse("sun-23").unwrap();
        assert_eq!((s.day, s.hour), (UpdateDay::Sun, 23));
        assert_eq!(s.day.weekday(), Some(7));
        for bad in [
            "daily-24",
            "daily-3",
            "monday-03",
            "sun-03x",
            "daily-+3",
            "",
            "-03",
            "daily-",
        ] {
            assert_eq!(Schedule::parse(bad), None, "{bad}");
        }
        assert_eq!(UpdateSetting::Auto(true).request_name(), "auto-on");
        assert_eq!(UpdateSetting::Auto(false).request_name(), "auto-off");
        assert_eq!(
            UpdateSetting::Schedule(Schedule::parse("mon-07").unwrap()).request_name(),
            "schedule-mon-07"
        );
        assert_eq!(UpdateSetting::parse("schedule-x-07"), None);
        assert_eq!(UpdateSetting::parse("auto-maybe"), None);
    }

    #[test]
    fn the_last_setting_wins_and_pending_requests_count() {
        let root = scratch("settings-req");
        let env = UpdateEnv {
            root: root.clone(),
            kind: HostKind::Container,
            state_dir: root.join("state"),
            request_dir: root.join("req"),
        };
        std::fs::create_dir_all(&env.request_dir).unwrap();
        std::fs::create_dir_all(&env.state_dir).unwrap();
        let sun = Schedule::parse("sun-05").unwrap();
        env.request_setting(UpdateSetting::Auto(false)).unwrap();
        env.request_setting(UpdateSetting::Auto(true)).unwrap();
        env.request_setting(UpdateSetting::Schedule(Schedule::default()))
            .unwrap();
        env.request_setting(UpdateSetting::Schedule(sun)).unwrap();
        let mut names: Vec<String> = std::fs::read_dir(&env.request_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["auto-on", "schedule-sun-05"]);
        // A stray name is ignored.
        std::fs::write(env.request_dir.join("reboot"), "").unwrap();
        let st = env.state();
        assert!(st.auto_on());
        assert_eq!(st.schedule(), sun);
        assert_eq!(st.busy(), None);
        place_request(&env.request_dir, "check").unwrap();
        let st = env.state();
        assert_eq!(st.busy(), Some(Busy::Checking));
        std::fs::write(env.request_dir.join("apply"), "").unwrap();
        assert_eq!(env.state().busy(), Some(Busy::Updating));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn running_action_and_new_status_fields() {
        let dir = scratch("running");
        let now = now_secs();
        std::fs::write(
            dir.join("action.json"),
            format!(r#"{{"action":"auto","step":"download","version":"0.0.4","bytes_done":59000000,"bytes_total":141000000,"started":{now},"updated":{now}}}"#),
        )
        .unwrap();
        let st = read_state(&dir);
        let r = st.running.as_ref().unwrap();
        assert_eq!(
            (r.step.as_str(), r.bytes_done, r.bytes_total),
            ("download", 59_000_000, 141_000_000)
        );
        assert_eq!(st.busy(), Some(Busy::Updating));
        // Left over from a helper that stopped: not busy.
        std::fs::write(
            dir.join("action.json"),
            r#"{"action":"apply","step":"restart","started":1,"updated":1}"#,
        )
        .unwrap();
        assert_eq!(read_state(&dir).busy(), None);

        let v = parse_status(
            r#"{"image":"ghcr.io/example/melissa:edge","auto_update":"registry","schedule":"sun-03","next_run":1764385200,"tz_offset":"+0100",
            "running":{"version":"0.0.3"},"available":{"id":"","digest":"sha256:abc","version":"0.0.4","size":141000000},
            "held":{"id":"x","digests":"sha256:def","version":"0.0.5","reason":"failed","at":5,"message":"did not answer"},
            "just_updated":{"from":"0.0.2","to":"0.0.3","at":7}}"#,
        )
        .unwrap();
        assert!(v.auto_on() && !v.local_image());
        assert_eq!(v.schedule, Schedule::parse("sun-03"));
        assert_eq!(v.next_run, Some(1_764_385_200));
        assert_eq!(v.available.as_ref().and_then(|a| a.size), Some(141_000_000));
        let h = v.held.unwrap();
        assert_eq!(
            (h.reason, h.version.as_deref()),
            (HeldReason::Failed, Some("0.0.5"))
        );
        assert_eq!(v.just_updated.unwrap().to.as_deref(), Some("0.0.3"));
        // Unknown reasons and a broken schedule are dropped, not guessed.
        let v = parse_status(r#"{"schedule":"often","held":{"reason":"why"},"auto_update":"local","image":"localhost/melissa:latest"}"#).unwrap();
        assert!(v.schedule.is_none() && v.held.is_none() && v.local_image() && v.auto_on());
        let _ = std::fs::remove_dir_all(dir);
    }
}
