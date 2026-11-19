//! This device's Melissa release and OS image updates.
//!
//! The queen runs as the unprivileged `melissa` user; `bootc` needs root even to read its
//! status. So a small root helper does the privileged part (image `rootfs`):
//!
//! - `/usr/libexec/melissa/melissa-update <verb>` with a fixed verb, run only by four
//!   oneshot units: `melissa-update-status.service` (at boot), `melissa-update-check.service`
//!   (`bootc upgrade --check`), `melissa-update-apply.service` (`bootc upgrade --apply`:
//!   downloads, stages and reboots) and `melissa-update-rollback.service` (`bootc rollback`,
//!   then reboot). Each writes `bootc status --format=json` to
//!   `/run/melissa-update/status.json` and the outcome to `last.json` (root:melissa 0640).
//! - A polkit rule lets the `melissa` user start exactly those units (and restart
//!   `melissa-queen.service` to apply settings). No arguments travel from the web: the
//!   queen picks a unit from [`UpdateAction`], `systemctl` asks systemd over D-Bus.
//!
//! On a host without the helper (a dev machine, container mode) the UI shows the commands.

use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{clean_text, SettingsError};

/// Where the root helper writes (tmpfiles: root:melissa 0750).
pub const STATE_DIR: &str = "/run/melissa-update";
/// Build facts written into the image.
pub const RELEASE_FILE: &str = "/usr/lib/melissa/release";

/// Which Melissa this is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Release {
    pub version: String,
    /// `headless` or `desktop` (image builds only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    /// `image` (the release file), `env` (MELISSA_VERSION) or `source` (a cargo build).
    pub source: &'static str,
}

impl Release {
    /// From the release file, else `MELISSA_VERSION`/`MELISSA_VARIANT`, else this build.
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
                    variant: get("MELISSA_VARIANT"),
                    source: "image",
                };
            }
        }
        if let Some(v) = env("MELISSA_VERSION") {
            return Self {
                version: clean_text(&v, 64),
                variant: env("MELISSA_VARIANT").map(|v| clean_text(&v, 32)),
                source: "env",
            };
        }
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            variant: None,
            source: "source",
        }
    }
}

/// What kind of host the queen runs on, which decides how it is updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    /// A Melissa bootc image (`bootc upgrade`, rollback).
    Bootc,
    /// Container mode: the image under podman; updated by rebuilding the container.
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
        } else if run.join("ostree-booted").exists() {
            Self::Bootc
        } else {
            Self::Other
        }
    }
}

// --- bootc status --format=json --------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Host {
    #[serde(default)]
    status: Option<HostStatus>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct HostStatus {
    #[serde(default)]
    staged: Option<BootEntry>,
    #[serde(default)]
    booted: Option<BootEntry>,
    #[serde(default)]
    rollback: Option<BootEntry>,
    #[serde(default)]
    rollback_queued: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BootEntry {
    #[serde(default)]
    image: Option<ImageStatus>,
    #[serde(default)]
    cached_update: Option<ImageStatus>,
    #[serde(default)]
    incompatible: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImageStatus {
    image: ImageReference,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    image_digest: Option<String>,
}

#[derive(Deserialize)]
struct ImageReference {
    image: String,
    #[serde(default)]
    transport: Option<String>,
}

/// One OS image as the UI shows it (all text cleaned).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImageView {
    /// e.g. `ghcr.io/example/melissa-headless:latest`.
    pub image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Build time as bootc reports it (RFC 3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

impl ImageView {
    fn from(s: ImageStatus) -> Self {
        let opt = |v: Option<String>, n| v.map(|v| clean_text(&v, n)).filter(|v| !v.is_empty());
        Self {
            image: clean_text(&s.image.image, 200),
            transport: opt(s.image.transport, 40),
            digest: opt(s.image_digest, 80),
            version: opt(s.version, 64),
            timestamp: opt(s.timestamp, 40),
        }
    }

    /// A short digest for people: `sha256:0123456789ab`.
    pub fn short_digest(&self) -> Option<String> {
        self.digest
            .as_ref()
            .map(|d| d.chars().take("sha256:".len() + 12).collect())
    }
}

/// What `bootc status` says, reduced to what the UI needs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct BootcView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub booted: Option<ImageView>,
    /// Downloaded and ready: used at the next boot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staged: Option<ImageView>,
    /// The previous image, which a rollback boots.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rollback: Option<ImageView>,
    /// A newer image found by the last check (not downloaded yet).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available: Option<ImageView>,
    /// The next boot goes to the rollback image.
    pub rollback_queued: bool,
    /// The booted image can't be updated by bootc (e.g. not a container image).
    pub incompatible: bool,
}

/// Parse `bootc status --format=json` (`org.containers.bootc/v1` `BootcHost`). Unknown
/// fields are ignored; a missing `status` is an empty view.
pub fn parse_bootc_status(json: &str) -> Result<BootcView, SettingsError> {
    let host: Host = serde_json::from_str(json).map_err(|e| SettingsError::Parse {
        path: "bootc status".into(),
        message: e.to_string(),
    })?;
    let status = host.status.unwrap_or_default();
    let mut view = BootcView {
        rollback_queued: status.rollback_queued,
        ..BootcView::default()
    };
    if let Some(b) = status.booted {
        view.incompatible = b.incompatible;
        view.available = b.cached_update.map(ImageView::from);
        view.booted = b.image.map(ImageView::from);
    }
    view.staged = status.staged.and_then(|s| s.image).map(ImageView::from);
    view.rollback = status.rollback.and_then(|r| r.image).map(ImageView::from);
    // A found update that is already staged, or the image already running, is not news.
    if let Some(a) = &view.available {
        let same = |i: &Option<ImageView>| {
            i.as_ref()
                .is_some_and(|i| i.digest.is_some() && i.digest == a.digest)
        };
        if same(&view.staged) || same(&view.booted) {
            view.available = None;
        }
    }
    Ok(view)
}

// --- What the root helper wrote ----------------------------------------------------------------

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

/// Everything the Updates page shows about the OS image.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct UpdateState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<BootcView>,
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
        Ok(text) => match parse_bootc_status(&text) {
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

/// The only things the queen may ask root for. Each maps to one fixed unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateAction {
    /// Look for a newer image (`bootc upgrade --check`).
    Check,
    /// Download, stage and reboot into the newer image.
    Apply,
    /// Boot the previous image.
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

    /// Reboots the device (needs the owner's explicit yes).
    pub fn reboots(self) -> bool {
        matches!(self, Self::Apply | Self::Rollback)
    }

    /// The `systemctl` arguments: a fixed verb and unit, nothing from the request.
    pub fn systemctl_args(self) -> [&'static str; 3] {
        match self {
            Self::Check => ["start", "--no-block", "melissa-update-check.service"],
            Self::Apply => ["start", "--no-block", "melissa-update-apply.service"],
            Self::Rollback => ["start", "--no-block", "melissa-update-rollback.service"],
            Self::Restart => ["restart", "--no-block", "melissa-queen.service"],
        }
    }

    /// What to type instead, where the helper is missing.
    pub fn manual_command(self) -> &'static str {
        match self {
            Self::Check => "sudo bootc upgrade --check",
            Self::Apply => "sudo bootc upgrade --apply",
            Self::Rollback => "sudo bootc rollback && sudo systemctl reboot",
            Self::Restart => "sudo systemctl restart melissa-queen",
        }
    }
}

/// Where the helper is installed and writes; `root` is `/` except in tests.
#[derive(Debug, Clone)]
pub struct UpdateEnv {
    pub root: PathBuf,
    pub kind: HostKind,
    pub state_dir: PathBuf,
}

impl UpdateEnv {
    pub fn system() -> Self {
        let root = PathBuf::from("/");
        Self {
            kind: HostKind::detect(&root),
            state_dir: PathBuf::from(STATE_DIR),
            root,
        }
    }

    /// The root helper and its polkit rule are installed (Melissa images).
    pub fn helper_installed(&self) -> bool {
        self.root
            .join("usr/lib/systemd/system/melissa-update-check.service")
            .exists()
            && self
                .root
                .join("usr/share/polkit-1/rules.d/50-melissa.rules")
                .exists()
    }

    /// Whether the UI may offer `action` as a button.
    pub fn can(&self, action: UpdateAction) -> bool {
        match action {
            UpdateAction::Restart => self.helper_installed(),
            _ => self.kind == HostKind::Bootc && self.helper_installed(),
        }
    }

    pub fn state(&self) -> UpdateState {
        read_state(&self.state_dir)
    }
}

/// Ask systemd (through polkit) to start the action's unit. Returns once systemd took the
/// job (`--no-block`); the unit's outcome lands in `last.json`.
pub async fn request(action: UpdateAction) -> Result<(), SettingsError> {
    let mut cmd = tokio::process::Command::new("systemctl");
    cmd.args(action.systemctl_args())
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

    #[test]
    fn parses_a_booted_only_host() {
        let v = parse_bootc_status(&fixture("bootc-status-booted.json")).unwrap();
        let b = v.booted.as_ref().unwrap();
        assert_eq!(b.image, "localhost/melissa-headless:dev");
        assert_eq!(b.transport.as_deref(), Some("containers-storage"));
        assert_eq!(b.short_digest().as_deref(), Some("sha256:5b0d3c1e9f2a"));
        assert_eq!(b.version.as_deref(), Some("43.20261118.0"));
        assert!(v.staged.is_none() && v.rollback.is_none() && v.available.is_none());
        assert!(!v.rollback_queued);
    }

    #[test]
    fn parses_staged_rollback_and_found_update() {
        let v = parse_bootc_status(&fixture("bootc-status-staged.json")).unwrap();
        assert_eq!(
            v.staged.as_ref().and_then(|s| s.version.as_deref()),
            Some("43.20261119.0")
        );
        assert_eq!(
            v.rollback.as_ref().map(|r| r.image.as_str()),
            Some("ghcr.io/example/melissa-headless:latest")
        );
        // The found update is the staged one: not shown twice.
        assert!(v.available.is_none());
        let v = parse_bootc_status(&fixture("bootc-status-update.json")).unwrap();
        assert_eq!(
            v.available.as_ref().and_then(|a| a.version.as_deref()),
            Some("43.20261120.0")
        );
        assert!(v.staged.is_none());
    }

    #[test]
    fn bad_or_hostile_status_is_handled() {
        assert!(parse_bootc_status("not json").is_err());
        assert_eq!(parse_bootc_status("{}").unwrap(), BootcView::default());
        let hostile = r#"{"status":{"booted":{"image":{"image":{"image":"<script>\u001b[31mx","transport":"registry"}}}}}"#;
        let v = parse_bootc_status(hostile).unwrap();
        assert_eq!(v.booted.unwrap().image, "<script>x");
    }

    #[test]
    fn reads_the_helpers_files() {
        let dir = std::env::temp_dir().join(format!("melissa-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read_state(&dir), UpdateState::default());
        std::fs::write(dir.join("status.json"), fixture("bootc-status-update.json")).unwrap();
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
        let root = std::env::temp_dir().join(format!("melissa-root-{}", std::process::id()));
        std::fs::create_dir_all(root.join("run")).unwrap();
        assert_eq!(HostKind::detect(&root), HostKind::Other);
        std::fs::write(root.join("run/ostree-booted"), "").unwrap();
        assert_eq!(HostKind::detect(&root), HostKind::Bootc);
        let env = UpdateEnv {
            root: root.clone(),
            kind: HostKind::Bootc,
            state_dir: root.join("run/melissa-update"),
        };
        assert!(!env.helper_installed() && !env.can(UpdateAction::Check));
        std::fs::create_dir_all(root.join("usr/lib/systemd/system")).unwrap();
        std::fs::create_dir_all(root.join("usr/share/polkit-1/rules.d")).unwrap();
        std::fs::write(
            root.join("usr/lib/systemd/system/melissa-update-check.service"),
            "",
        )
        .unwrap();
        std::fs::write(root.join("usr/share/polkit-1/rules.d/50-melissa.rules"), "").unwrap();
        assert!(env.can(UpdateAction::Apply) && env.can(UpdateAction::Restart));
        std::fs::write(root.join("run/.containerenv"), "").unwrap();
        assert_eq!(HostKind::detect(&root), HostKind::Container);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn actions_map_to_fixed_units() {
        for (s, unit) in [
            ("check", "melissa-update-check.service"),
            ("apply", "melissa-update-apply.service"),
            ("rollback", "melissa-update-rollback.service"),
            ("restart", "melissa-queen.service"),
        ] {
            let a = UpdateAction::parse(s).unwrap();
            assert_eq!(a.as_str(), s);
            assert_eq!(a.systemctl_args()[2], unit);
        }
        assert_eq!(UpdateAction::parse("reboot; rm -rf /"), None);
        assert!(UpdateAction::Apply.reboots() && !UpdateAction::Check.reboots());
    }

    #[test]
    fn release_file_wins_over_env_and_build() {
        let dir = std::env::temp_dir().join(format!("melissa-release-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("release");
        std::fs::write(&file, "MELISSA_VERSION=0.3.1\nMELISSA_VARIANT=headless\n").unwrap();
        let r = Release::read(&file);
        assert_eq!(
            (r.version.as_str(), r.variant.as_deref(), r.source),
            ("0.3.1", Some("headless"), "image")
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
