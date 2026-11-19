//! This device's Melissa release and container image updates.
//!
//! Melissa runs as a rootful podman container (packaging/container). The queen inside it can't
//! run the host's podman, so a small host helper does the privileged part:
//!
//! - The queen asks by creating an **empty file** named `check`, `apply` or `rollback` in
//!   [`REQUEST_DIR`] (a bind mount of the host's `/var/lib/melissa-container/request`). A host
//!   path unit starts `melissa-host-update requests`, which acts only on those three names and
//!   never reads file contents. Nothing else travels from the web.
//! - The helper writes `status.json` (running, staged, available and previous image) and
//!   `last.json` (the outcome of its last action) to the host's
//!   `/var/lib/melissa-container/status`, mounted read-only at [`STATE_DIR`].
//! - Restarting the queen itself (to use saved settings) goes through systemd inside the
//!   container: a polkit rule lets the `melissa` user restart `melissa-queen.service`, nothing else.
//!
//! Without the helper (a dev container, a from-source run) the UI shows the host commands.

use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{clean_text, SettingsError};

/// Where the host helper's status arrives (read-only bind mount).
pub const STATE_DIR: &str = "/run/melissa-update";
/// Where the queen drops requests for the host helper (bind mount, group `melissa` writable).
pub const REQUEST_DIR: &str = "/run/melissa-update-request";
/// Build facts written into the image.
pub const RELEASE_FILE: &str = "/usr/lib/melissa/release";
/// The host helper, for commands typed on the host.
pub const HOST_HELPER: &str = "/usr/local/libexec/melissa/melissa-host-update";

/// Which Melissa this is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Release {
    pub version: String,
    /// The source revision the image was built from, if recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// How it is packaged, e.g. `container` (image builds only).
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

/// What kind of host the queen runs on, which decides how it is updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    /// The Melissa container image under podman.
    Container,
    /// Anything else, e.g. a developer's machine running from source.
    Other,
}

impl HostKind {
    /// Look at marker files under `root` (`/` normally).
    pub fn detect(root: &Path) -> Self {
        let run = root.join("run");
        if run.join(".containerenv").exists() || run.join("systemd/container").exists() {
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
        }
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
    /// `registry`, `local` or `off`: podman-auto-update's policy for the container.
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
    let mut view = ImageStatus {
        auto_update: raw
            .auto_update
            .map(|a| clean_text(&a, 20))
            .filter(|a| !a.is_empty()),
        running: img(raw.running),
        staged: img(raw.staged),
        available: img(raw.available),
        previous: img(raw.previous),
        image: name.clone(),
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
    pub action: String,
    pub ok: bool,
    /// Unix seconds.
    pub at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
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
            l.message = l.message.map(|m| clean_text(&m, 300));
            l
        });
    state
}

// --- Actions -------------------------------------------------------------------------------------

/// The only things the queen may ask for. Each maps to one fixed request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateAction {
    /// Look for a newer image.
    Check,
    /// Download the newer image and restart Melissa on it.
    Apply,
    /// Restart Melissa on the previous image.
    Rollback,
    /// Restart Melissa's queen (to use saved settings).
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

    /// The `systemctl` arguments for a restart of the queen: a fixed verb and unit.
    pub fn systemctl_args(self) -> Option<[&'static str; 3]> {
        match self {
            Self::Restart => Some(["restart", "--no-block", "melissa-queen.service"]),
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
            Self::Restart => "sudo podman exec melissa systemctl restart melissa-queen".into(),
            _ => format!("sudo {HOST_HELPER} {}", self.as_str()),
        }
    }
}

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
        Self {
            kind: HostKind::detect(&root),
            state_dir: PathBuf::from(STATE_DIR),
            request_dir: PathBuf::from(REQUEST_DIR),
            root,
        }
    }

    /// The host helper's directories are mounted (installed with install.sh).
    pub fn helper_installed(&self) -> bool {
        self.kind == HostKind::Container && self.request_dir.is_dir() && self.state_dir.is_dir()
    }

    /// The queen may restart itself (polkit rule from packaging/common).
    pub fn can_restart(&self) -> bool {
        self.root
            .join("usr/share/polkit-1/rules.d/50-melissa.rules")
            .exists()
    }

    /// Whether the UI may offer `action` as a button.
    pub fn can(&self, action: UpdateAction) -> bool {
        match action {
            UpdateAction::Restart => self.can_restart(),
            _ => self.helper_installed(),
        }
    }

    pub fn state(&self) -> UpdateState {
        read_state(&self.state_dir)
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

/// Ask systemd (through polkit) to restart the queen. Returns once systemd took the job.
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

    fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        std::fs::read_to_string(&path).unwrap()
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("melissa-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_a_running_only_status() {
        let v = parse_status(&fixture("container-status-running.json")).unwrap();
        let r = v.running.as_ref().unwrap();
        assert_eq!(r.image, "localhost/melissa:latest");
        assert_eq!(r.short_digest().as_deref(), Some("sha256:5b0d3c1e9f2a"));
        assert_eq!(r.version.as_deref(), Some("0.1.0"));
        assert_eq!(v.auto_update.as_deref(), Some("local"));
        assert!(v.staged.is_none() && v.previous.is_none() && v.available.is_none());
    }

    #[test]
    fn parses_staged_previous_and_found_update() {
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
        let dir = temp("update");
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
        let root = temp("root");
        std::fs::create_dir_all(root.join("run")).unwrap();
        assert_eq!(HostKind::detect(&root), HostKind::Other);
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
        assert_eq!(HostKind::detect(&root), HostKind::Container);
        let img = env.container_image().unwrap();
        assert_eq!(img.image, "localhost/melissa:latest");
        assert_eq!(img.short_digest().as_deref(), Some("sha256:5b0d3c1e9f2a"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn requests_are_empty_files_with_fixed_names() {
        let root = temp("request");
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
        assert_eq!(r.systemctl_args().unwrap()[2], "melissa-queen.service");
        assert_eq!(UpdateAction::parse("reboot; rm -rf /"), None);
        assert!(UpdateAction::Apply.restarts_melissa() && !UpdateAction::Check.restarts_melissa());
    }

    #[test]
    fn release_file_wins_over_env_and_build() {
        let dir = temp("release");
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
        let r = Release::read(&dir.join("missing"));
        assert!(r.source == "source" || r.source == "env");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn ago_reads_well() {
        assert_eq!(ago(100, 110), "just now");
        assert_eq!(ago(0, 300), "5 min ago");
        assert_eq!(ago(0, 7200), "2 h ago");
        assert_eq!(ago(0, 90_000), "1 day ago");
        assert_eq!(ago(500, 100), "just now");
    }
}
