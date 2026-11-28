//! Factory reset, Melissa only (owner decisions 2026-11-21; docs/DECISIONS.md "Factory reset",
//! PLAN 6d). It erases everything Melissa keeps on this device and starts her again unclaimed,
//! with a new device id: people and sessions, agents, chats, files, schedules, permissions, the
//! audit log, every app and its data (the whole k3s datastore), provider sign-ins and API keys
//! (the server user's home), the UI and app access settings, Melissa's own Tailscale (logged out
//! first) and the journal. Kept: the install (`server.toml`; the host's `install.env`, image,
//! Quadlet file and drop-ins are outside the container anyway), k3s's node password and the
//! voice model.
//!
//! How (container install; the marker directory exists only there):
//! 1. The server (the owner, password entered again) writes the marker [`MARKER_NAME`] in
//!    [`MARKER_DIR`] with [`Step::Requested`] and an audit line ([`Requester::request`]),
//!    answers "Resetting…", then tidies up while it still runs: pauses the scheduler, denies
//!    open requests, logs Tailscale out, deletes the apps, closes the agent sessions, and marks
//!    [`Step::OnlineDone`].
//! 2. `melissa-factory-reset.path` sees the marker and starts `melissa-factory-reset.service`
//!    (root), which runs [`run`]: wait (bounded) for step 1, stop the server, Tailscale and k3s,
//!    kill what k3s leaves behind, unmount, remove [`WIPE`] and empty [`EMPTY`], write the one
//!    audit line, delete the marker LAST, then start everything again. Each step is recorded in
//!    the marker.
//! 3. The same service runs at boot while a marker exists (before the server and k3s): a stale
//!    marker always means "finish the wipe". Every step is safe to run again.
//!
//! What goes is these constants and nothing else: no request, file or argument names a path.
//! Removal never follows a symlink, never descends into a mount (mounts below a target are
//! unmounted first, and a target that stays mounted is left alone and retried), and refuses a
//! target whose parent directory isn't where the list says (a symlinked parent).

use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use melissa_agent::{AuditLog, AuditRecord};
use serde::{Deserialize, Serialize};

/// Where the request marker lives: on the `/var` volume (it survives a crash or a restart of
/// the container), in a directory the server's user may write (root:melissa 0770, tmpfiles).
pub const MARKER_DIR: &str = "/var/lib/melissa-container/reset";
/// The marker: `{"requested_at", "step", "by", "online"}` (0600).
pub const MARKER_NAME: &str = "factory-reset.json";
/// The word typed to confirm (there is no device name setting yet; when one comes, the page
/// asks for that instead).
pub const CONFIRM_WORD: &str = "melissa";
/// The root unit that does the wipe (started by `melissa-factory-reset.path`).
pub const UNIT: &str = "melissa-factory-reset.service";
/// The server's unit.
pub const SERVER_UNIT: &str = "melissa-server.service";
/// The audit log, which starts again with one line.
pub const AUDIT_FILE: &str = "/var/lib/melissa/audit.jsonl";
/// How long the root side waits for the running server to finish its part.
pub const ONLINE_WAIT: Duration = Duration::from_secs(90);
/// After a failed run the unit waits this long before it exits (the path unit then starts it
/// again while the marker exists: a slow retry, never a tight loop).
pub const RETRY_AFTER: Duration = Duration::from_secs(60);

/// Removed entirely (file, directory or symlink). Absolute paths inside the container.
pub const WIPE: &[&str] = &[
    // The server's state (`$XDG_STATE_HOME/melissa`); its data dir is the same directory and
    // keeps only `models/` (the voice model).
    "/var/lib/melissa/auth",
    "/var/lib/melissa/device.json",
    "/var/lib/melissa/agents",
    "/var/lib/melissa/lead",
    "/var/lib/melissa/work",
    "/var/lib/melissa/files",
    "/var/lib/melissa/schedules.json",
    "/var/lib/melissa/permissions.json",
    "/var/lib/melissa/audit.jsonl",
    "/var/lib/melissa/models.json",
    "/var/lib/melissa/backend.json",
    "/var/lib/melissa/usage.json",
    "/var/lib/melissa/acp-registry.json",
    "/var/lib/melissa/egress",
    "/var/lib/melissa/tmp",
    // Old names, in case a migration left them behind.
    "/var/lib/melissa/bees",
    "/var/lib/melissa/queen",
    "/var/lib/melissa/comb.json",
    // The server user's home: the provider CLIs, their sign-ins and caches.
    "/var/lib/melissa/home",
    // Settings (/etc/melissa); server.toml stays.
    "/var/lib/melissa-container/etc-melissa/providers.toml",
    "/var/lib/melissa-container/etc-melissa/providers.toml.broken",
    "/var/lib/melissa-container/etc-melissa/ui.toml",
    "/var/lib/melissa-container/etc-melissa/app-access.toml",
    "/var/lib/melissa-container/etc-melissa/tailscale.toml",
    "/var/lib/melissa-container/etc-melissa/cluster.toml",
    "/var/lib/melissa-container/etc-melissa/cluster-join.token",
    "/var/lib/melissa-container/etc-melissa/cluster-invitation.token",
    "/var/lib/melissa-container/etc-melissa/cluster-request.json",
    "/var/lib/melissa-container/etc-melissa/cluster-answer.json",
    // Melissa's own Tailscale (never the host's: that one isn't in this container).
    "/var/lib/melissa-tailscale",
    // k3s: the datastore, every app's volumes, the pods' and the network's leftovers.
    "/var/lib/rancher/k3s",
    // Multi-device (crate::cluster): root's cluster state (pinned node name, agent token) and
    // the k3s data kept across role changes.
    "/var/lib/melissa-container/cluster",
    "/var/lib/rancher/k3s.single-node-backup",
    "/var/lib/rancher/k3s.agent-backup",
    "/run/melissa-cluster",
    "/var/lib/kubelet",
    "/var/lib/cni",
    "/run/k3s",
    // Backups (melissa_backup): root's settings and the device's copy of the recovery key, and
    // what the server reads. The backups on the drive stay; the owner's recovery key opens them.
    "/var/lib/melissa-container/backup",
    "/var/lib/melissa-container/backup-status",
];

/// Emptied (the directory itself stays, with its owner and mode).
pub const EMPTY: &[&str] = &[
    // The journal (detached while it is emptied).
    "/var/log/journal",
    // The server's cluster credentials for the old cluster.
    "/run/melissa-kube",
    // Backup requests not yet handled.
    "/var/lib/melissa-container/backup-request",
];

/// Swept: everything inside goes except the [`KEEP`] entries directly in it. A catch-all behind
/// [`WIPE`], so a state or settings file added later can't survive a reset by being missed.
pub const SWEEP: &[&str] = &["/var/lib/melissa", "/var/lib/melissa-container/etc-melissa"];

/// Never removed, and nothing in [`WIPE`] or [`EMPTY`] may be one of these or above one.
pub const KEEP: &[&str] = &[
    "/var/lib/melissa/models",
    "/var/lib/melissa-container/etc-melissa/server.toml",
    "/var/lib/melissa-container/rancher-node",
    "/var/lib/melissa-container/install.env",
    "/var/lib/melissa-container/status",
    "/var/lib/melissa-container/request",
    MARKER_DIR,
    "/run/melissa-update",
    "/run/melissa-update-request",
    "/usr",
    "/etc",
    "/home",
    "/root",
];

/// Stopped before the wipe; it doesn't go on unless they stopped.
pub const STOP: &[&str] = &[
    SERVER_UNIT,
    "melissa-tailscaled.service",
    "melissa-kubeconfig.service",
    "k3s.service",
];

/// Also stopped, best effort: install and sign-in jobs that would write to the home (globs,
/// which match nothing when none runs).
pub const STOP_JOBS: &[&str] = &[
    "melissa-agent-install@*.service",
    "melissa-agent-login@*.service",
    "melissa-backup.service",
];

/// Started again afterwards (`restart --no-block`: the app filter goes back to its default,
/// k3s comes up empty, the server's credentials are written for the new cluster).
pub const START: &[&str] = &[
    "melissa-app-access.service",
    "k3s.service",
    "melissa-kubeconfig.service",
    SERVER_UNIT,
];

/// Melissa's tmpfiles.d files, applied again so wiped directories come back with their owner
/// and mode (the home, /run/melissa-kube).
pub const TMPFILES: &[&str] = &[
    "melissa-config.conf",
    "melissa-home.conf",
    "melissa-kube.conf",
    "melissa-container.conf",
    "melissa-tailscale.conf",
    "melissa-agent-install.conf",
];

// --- what a backup of Melissa's own state holds (melissa_backup) -----------------------------
//
// One inventory for both: a backup copies what a reset erases, minus what is regenerated or
// bound to this device. Every [`WIPE`] entry is either inside a [`BACKUP_STATE`] directory or
// listed in [`BACKUP_ELSEWHERE`] (checked by the tests), so a new state file is covered by
// both or neither.

/// Melissa's state in a backup: the swept directories (the server's state and settings), whole,
/// without [`BACKUP_EXCLUDE`] and [`KEEP`].
pub const BACKUP_STATE: &[&str] = SWEEP;

/// Inside [`BACKUP_STATE`] but never backed up: temporary files, the voice model (downloaded
/// again), the server user's home (the provider CLIs, their sign-ins and caches: never in a
/// backup, owner decision 2026-11-28; after a restore the agents are set up and signed in
/// again), the install's own `server.toml`, and the cluster's tokens and requests (a restored
/// device gets a fresh cluster identity).
pub const BACKUP_EXCLUDE: &[&str] = &[
    "/var/lib/melissa/tmp",
    "/var/lib/melissa/models",
    "/var/lib/melissa/home",
    "/var/lib/melissa-container/etc-melissa/server.toml",
    "/var/lib/melissa-container/etc-melissa/providers.toml.broken",
    "/var/lib/melissa-container/etc-melissa/cluster.toml",
    "/var/lib/melissa-container/etc-melissa/cluster-join.token",
    "/var/lib/melissa-container/etc-melissa/cluster-invitation.token",
    "/var/lib/melissa-container/etc-melissa/cluster-request.json",
    "/var/lib/melissa-container/etc-melissa/cluster-answer.json",
];

/// Erased by a reset, not part of the state backup: k3s (backed up as its own parts: a raw
/// snapshot of its datastore and each app's export), Melissa's Tailscale node key, the cluster
/// identity, runtime leftovers and the backup settings themselves.
pub const BACKUP_ELSEWHERE: &[&str] = &[
    "/var/lib/melissa-tailscale",
    "/var/lib/rancher/k3s",
    "/var/lib/melissa-container/cluster",
    "/var/lib/rancher/k3s.single-node-backup",
    "/var/lib/rancher/k3s.agent-backup",
    "/run/melissa-cluster",
    "/var/lib/kubelet",
    "/var/lib/cni",
    "/run/k3s",
    "/var/lib/melissa-container/backup",
    "/var/lib/melissa-container/backup-status",
];

/// What the owner reads before confirming (short; the page and the CLI use the same words).
pub const ERASED: &str =
    "People, helpers, chats, files, apps and their data, provider sign-ins and keys, settings, logs";
pub const KEPT: &str = "Melissa's version, install settings, the voice model";

/// How far a reset got (recorded in the marker; later is greater).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// Asked for; the server tidies up if it runs.
    Requested,
    /// The server's part is done (or it didn't run).
    OnlineDone,
    /// Stopping the server, Tailscale and k3s.
    Stopping,
    /// Removing.
    Wiping,
    /// Directories back, the audit line written; the marker goes next.
    Finishing,
}

impl Step {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::OnlineDone => "online_done",
            Self::Stopping => "stopping",
            Self::Wiping => "wiping",
            Self::Finishing => "finishing",
        }
    }
}

/// The marker's contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    /// Unix seconds.
    pub requested_at: u64,
    pub step: Step,
    /// Who asked ("ui", "host").
    #[serde(default)]
    pub by: String,
    /// A running server tidies up first (false when root asked directly).
    #[serde(default)]
    pub online: bool,
}

impl Marker {
    /// A new request by `by`; `online`: a running server tidies up first.
    pub fn new(by: &str, online: bool) -> Self {
        Self {
            requested_at: now(),
            step: Step::Requested,
            by: by.into(),
            online,
        }
    }

    /// A marker that can't be read still means "finish the wipe".
    fn unreadable() -> Self {
        Self {
            requested_at: 0,
            step: Step::Requested,
            by: String::new(),
            online: false,
        }
    }
}

/// Errors of a request or a run.
#[derive(Debug, thiserror::Error)]
pub enum ResetError {
    #[error("{what}: {source}")]
    Io {
        what: String,
        #[source]
        source: std::io::Error,
    },
    /// Another run holds the lock.
    #[error("a factory reset is already running")]
    Busy,
    /// Some targets could not be removed (each with why); the marker stays, so it is retried.
    #[error("not everything could be removed: {0}")]
    Incomplete(String),
    #[error("{0}")]
    Failed(String),
}

fn io(what: impl Into<String>) -> impl FnOnce(std::io::Error) -> ResetError {
    let what = what.into();
    move |source| ResetError::Io { what, source }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `abs` (one of the constants) under `root` (`/` on the device, a temporary tree in tests).
pub fn at(root: &Path, abs: &str) -> PathBuf {
    root.join(abs.trim_start_matches('/'))
}

/// The typed confirmation matches (surrounding spaces ignored, nothing else).
pub fn confirm_matches(typed: &str, word: &str) -> bool {
    !word.is_empty() && typed.trim() == word
}

/// Parse a marker; anything unreadable is a marker all the same.
pub fn parse_marker(text: &str) -> Marker {
    serde_json::from_str(text).unwrap_or_else(|_| Marker::unreadable())
}

/// Read the marker at `path`: `None` when there is none. Never follows a symlink (one there is
/// treated as an unreadable marker).
pub fn read_marker(path: &Path) -> Result<Option<Marker>, ResetError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path);
    let file = match file {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) if path.symlink_metadata().is_ok() => return Ok(Some(Marker::unreadable())),
        Err(e) => return Err(io(format!("reading {}", path.display()))(e)),
    };
    let mut text = String::new();
    // A marker is a few dozen bytes; anything bigger is not one of ours.
    if file.take(4096).read_to_string(&mut text).is_err() {
        return Ok(Some(Marker::unreadable()));
    }
    Ok(Some(parse_marker(&text)))
}

/// Write the marker atomically: a new temporary file (0600, never through a symlink), fsync,
/// rename over the marker, fsync the directory.
pub fn write_marker(path: &Path, marker: &Marker) -> Result<(), ResetError> {
    let dir = path
        .parent()
        .ok_or_else(|| ResetError::Failed("the marker has no directory".into()))?;
    let tmp = dir.join(format!(".{MARKER_NAME}.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let text = serde_json::to_string(marker).map_err(|e| ResetError::Failed(e.to_string()))?;
    let write = || -> std::io::Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        File::open(dir)?.sync_all()
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io(format!("writing {}", path.display()))(e)
    })
}

/// Remove the marker and make that durable.
fn remove_marker(path: &Path) -> Result<(), ResetError> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io(format!("removing {}", path.display()))(e)),
    }
    if let Some(dir) = path.parent() {
        File::open(dir)
            .and_then(|d| d.sync_all())
            .map_err(io(format!("syncing {}", dir.display())))?;
    }
    Ok(())
}

// --- the server's side ---------------------------------------------------------------------

/// The server's side: ask for a reset (the root unit does it). Cheap to clone.
#[derive(Clone, Debug)]
pub struct Requester {
    dir: PathBuf,
    audit: Option<AuditLog>,
}

impl Requester {
    /// Markers in `dir`.
    pub fn new(dir: impl Into<PathBuf>, audit: Option<AuditLog>) -> Self {
        Self {
            dir: dir.into(),
            audit,
        }
    }

    /// This install: `Some` only where the root unit is (the container's marker directory).
    pub fn system(audit: Option<AuditLog>) -> Option<Self> {
        Path::new(MARKER_DIR)
            .is_dir()
            .then(|| Self::new(MARKER_DIR, audit))
    }

    pub fn marker_path(&self) -> PathBuf {
        self.dir.join(MARKER_NAME)
    }

    /// A reset was asked for and isn't finished.
    pub fn pending(&self) -> bool {
        self.marker_path().symlink_metadata().is_ok()
    }

    /// Ask for a reset (audited `factory_reset.requested`). Asking again while one is pending
    /// changes nothing.
    pub fn request(&self, actor: &str) -> Result<(), ResetError> {
        if self.pending() {
            return Ok(());
        }
        write_marker(&self.marker_path(), &Marker::new(actor, true))?;
        if let Some(log) = &self.audit {
            let record =
                AuditRecord::now("factory_reset.requested", 0, "", "factory reset requested")
                    .by(actor);
            if let Err(e) = log.append(&record) {
                tracing::error!(path = %log.path().display(), "audit log write failed: {e}");
            }
        }
        Ok(())
    }

    /// The server's part is done: the root unit goes on without waiting further.
    pub fn online_done(&self) -> Result<(), ResetError> {
        let path = self.marker_path();
        match read_marker(&path)? {
            Some(mut m) if m.step == Step::Requested => {
                m.step = Step::OnlineDone;
                write_marker(&path, &m)
            }
            _ => Ok(()),
        }
    }
}

// --- the root side ---------------------------------------------------------------------------

/// What the root side does to the system (injected so the sequence is tested on a temporary
/// tree with a fake cluster).
pub trait Host {
    /// The unit is active now.
    fn unit_active(&self, unit: &str) -> bool;
    /// Log Melissa's own Tailscale out if it runs (never the host's).
    fn tailscale_logout(&self) -> Result<(), String>;
    /// Stop these units and wait for them.
    fn stop_units(&self, units: &[&str]) -> Result<(), String>;
    /// The caller's own cgroup (v2, as in `/proc/self/cgroup`: `/system.slice/k3s.service`),
    /// `None` when unknown.
    fn own_cgroup(&self) -> Option<String>;
    /// Kill every process in a cgroup (`cgroup.kill`; a missing cgroup is fine). `cgroup` as
    /// [`Host::own_cgroup`] gives it. Only through [`kill_leftovers`], which never names the
    /// caller's own cgroup or one above it.
    fn kill_cgroup(&self, cgroup: &str);
    /// Kill k3s's containerd shims by name and drop k3s's network leftovers (pods' network
    /// namespaces, flannel's and CNI's interfaces).
    fn kill_shims(&self);
    /// Every mount point now.
    fn mounts(&self) -> Result<Vec<PathBuf>, String>;
    fn unmount(&self, path: &Path) -> Result<(), String>;
    /// Stop writing the journal to /var/log/journal (so it can be emptied) ...
    fn journal_detach(&self) -> Result<(), String>;
    /// ... and go back to it.
    fn journal_attach(&self);
    /// Make Melissa's directories again (tmpfiles).
    fn restore_layout(&self) -> Result<(), String>;
    /// Start (restart) these units without waiting.
    fn start_units(&self, units: &[&str]) -> Result<(), String>;
    fn sleep(&self, d: Duration);
}

/// Where k3s's leftovers run after its unit stops: the pods (kubepods, cgroupfs or systemd
/// driver) and the shims (k3s.service's own cgroup, `KillMode=process`).
pub const LEFTOVER_CGROUPS: [&str; 3] = ["/kubepods", "/kubepods.slice", K3S_CGROUP];
/// k3s.service's cgroup. `melissa-server cluster prepare` runs in it (k3s-prepare is an
/// `ExecStartPre`; with `Delegate=yes` in its `.control` child).
pub const K3S_CGROUP: &str = "/system.slice/k3s.service";

/// `cgroup` is `own` or above it (killing it would kill the caller).
fn holds(cgroup: &str, own: &str) -> bool {
    let cgroup = cgroup.trim_end_matches('/');
    cgroup.is_empty() || own == cgroup || own.starts_with(&format!("{cgroup}/"))
}

/// Kill what k3s leaves running after its unit stops (pods, shims) and drop its network
/// leftovers, never the caller: a cgroup that holds the caller is skipped (its shims go by
/// name, [`Host::kill_shims`]). An unknown own cgroup counts as k3s.service's (the cluster
/// switch runs there).
pub fn kill_leftovers(host: &dyn Host) {
    let own = host.own_cgroup().unwrap_or_else(|| K3S_CGROUP.to_string());
    for cg in LEFTOVER_CGROUPS {
        if !holds(cg, &own) {
            host.kill_cgroup(cg);
        }
    }
    host.kill_shims();
}

/// What [`run`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// No marker: nothing asked for.
    NothingToDo,
    /// Done; the marker is gone and the units are starting.
    Done,
}

/// Do (or finish) the reset asked for by the marker under `root`. `say` gets one line per
/// step for the journal. An error leaves the marker in place: the next run goes on.
pub fn run(root: &Path, host: &dyn Host, say: &mut dyn FnMut(&str)) -> Result<Outcome, ResetError> {
    run_with(root, host, ONLINE_WAIT, say)
}

/// [`run`] with the wait for the server's part.
pub fn run_with(
    root: &Path,
    host: &dyn Host,
    online_wait: Duration,
    say: &mut dyn FnMut(&str),
) -> Result<Outcome, ResetError> {
    check_lists().map_err(ResetError::Failed)?;
    let root = root
        .canonicalize()
        .map_err(io(format!("resolving {}", root.display())))?;
    let dir = at(&root, MARKER_DIR);
    let marker_path = dir.join(MARKER_NAME);
    let Some(mut marker) = read_marker(&marker_path)? else {
        return Ok(Outcome::NothingToDo);
    };
    // One run at a time (the path unit, a boot, someone by hand).
    let lock = File::open(&dir).map_err(io(format!("opening {}", dir.display())))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Err(ResetError::Busy),
        Err(std::fs::TryLockError::Error(e)) => {
            return Err(io(format!("locking {}", dir.display()))(e))
        }
    }
    say(&format!(
        "factory reset: requested at {} by {}, at step {}",
        marker.requested_at,
        if marker.by.is_empty() {
            "?"
        } else {
            &marker.by
        },
        marker.step.as_str()
    ));

    // 1. The running server tidies up first (bounded).
    if marker.step == Step::Requested && marker.online && host.unit_active(SERVER_UNIT) {
        say("factory reset: waiting for the server to finish its part");
        let until = Instant::now() + online_wait;
        while Instant::now() < until {
            host.sleep(Duration::from_secs(1));
            match read_marker(&marker_path)? {
                Some(m) if m.step >= Step::OnlineDone => {
                    marker = m;
                    break;
                }
                Some(_) => {}
                None => return Ok(Outcome::NothingToDo),
            }
        }
    }

    // 2. Stop everything that writes what goes. Always (a resumed run may find them started).
    advance(&marker_path, &mut marker, Step::Stopping)?;
    if let Err(e) = host.tailscale_logout() {
        say(&format!("factory reset: Tailscale not logged out: {e}"));
    }
    host.stop_units(STOP)
        .map_err(|e| ResetError::Failed(format!("stopping Melissa: {e}")))?;
    if let Err(e) = host.stop_units(STOP_JOBS) {
        say(&format!("factory reset: install jobs: {e}"));
    }
    kill_leftovers(host);

    // 3. Remove.
    advance(&marker_path, &mut marker, Step::Wiping)?;
    if let Err(e) = host.journal_detach() {
        say(&format!("factory reset: journal not detached: {e}"));
    }
    let wiped = wipe(&root, host);
    host.journal_attach();
    wiped?;
    say("factory reset: Melissa's data removed");

    // 4. Directories back, one audit line, the marker LAST, then start.
    advance(&marker_path, &mut marker, Step::Finishing)?;
    if let Err(e) = host.restore_layout() {
        say(&format!("factory reset: directories not restored: {e}"));
    }
    if let Err(e) = write_audit_line(&root, &marker) {
        say(&format!("factory reset: audit line not written: {e}"));
    }
    if let Err(e) = ask_host_to_forget_snapshots(&root) {
        say(&format!(
            "factory reset: the host's datastore snapshots stay ({e}); a rollback refuses them"
        ));
    }
    remove_marker(&marker_path)?;
    if let Err(e) = host.start_units(START) {
        say(&format!("factory reset: starting Melissa again: {e}"));
    }
    say("factory reset: done; Melissa starts unclaimed");
    Ok(Outcome::Done)
}

/// The host helper's request directory (container only) and the request that deletes its copies
/// of k3s's datastore (`melissa-host-update`, taken before each update): they hold the data and the
/// Secrets key this reset removed. An empty file with a fixed name, like Settings → Updates' requests.
pub const UPDATE_REQUEST_DIR: &str = "/run/melissa-update-request";
pub const FORGET_SNAPSHOTS: &str = "forget-snapshots";

/// Place the [`FORGET_SNAPSHOTS`] request where the host helper is (nothing to do without it).
fn ask_host_to_forget_snapshots(root: &Path) -> Result<(), String> {
    let dir = at(root, UPDATE_REQUEST_DIR);
    if !dir.is_dir() {
        return Ok(());
    }
    std::fs::write(dir.join(FORGET_SNAPSHOTS), b"").map_err(|e| e.to_string())
}

/// Record `step` if it is further than the marker says.
fn advance(path: &Path, marker: &mut Marker, step: Step) -> Result<(), ResetError> {
    if step > marker.step {
        marker.step = step;
        write_marker(path, marker)?;
    }
    Ok(())
}

/// Remove every target; go on past failures and report them all.
fn wipe(root: &Path, host: &dyn Host) -> Result<(), ResetError> {
    let mut problems = Vec::new();
    for (list, whole) in [(WIPE, true), (EMPTY, false), (SWEEP, false)] {
        for target in list {
            if let Err(e) = clear(root, host, target, whole) {
                problems.push(format!("{target}: {e}"));
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(ResetError::Incomplete(problems.join("; ")))
    }
}

/// Remove `target` (`whole`) or what is in it.
fn clear(root: &Path, host: &dyn Host, target: &str, whole: bool) -> Result<(), String> {
    let path = at(root, target);
    let meta = match path.symlink_metadata() {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    // The parent must be the real directory the list names (no symlink on the way).
    let parent = path.parent().ok_or("no parent directory")?;
    match parent.canonicalize() {
        Ok(real) if real == parent => {}
        _ => return Err("its directory is not where it should be (a symlink?)".into()),
    }
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return std::fs::remove_file(&path).map_err(|e| e.to_string());
    }
    unmount_below(host, &path, whole)?;
    if whole {
        return std::fs::remove_dir_all(&path).map_err(|e| e.to_string());
    }
    let entries = std::fs::read_dir(&path).map_err(|e| e.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let p = entry.path();
        if KEEP.iter().any(|k| at(root, k) == p) {
            continue;
        }
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        let removed = if kind.is_dir() {
            std::fs::remove_dir_all(&p)
        } else {
            std::fs::remove_file(&p)
        };
        removed.map_err(|e| format!("{}: {e}", p.display()))?;
    }
    Ok(())
}

/// Unmount everything mounted at (when `including`) or below `path`, deepest first; fail if
/// anything stays mounted there (removing would reach into another file system).
pub(crate) fn unmount_below(host: &dyn Host, path: &Path, including: bool) -> Result<(), String> {
    let under = |mounts: Vec<PathBuf>| -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = mounts
            .into_iter()
            .filter(|m| m.starts_with(path) && (including || m != path))
            .collect();
        v.sort_by_key(|m| std::cmp::Reverse(m.components().count()));
        v.dedup();
        v
    };
    let mounted = under(host.mounts()?);
    if mounted.is_empty() {
        return Ok(());
    }
    for m in &mounted {
        if let Err(e) = host.unmount(m) {
            tracing::warn!(mount = %m.display(), "unmount: {e}");
        }
    }
    let left = under(host.mounts()?);
    match left.first() {
        None => Ok(()),
        Some(m) => Err(format!(
            "{} still mounted ({} in all)",
            m.display(),
            left.len()
        )),
    }
}

/// The audit log starts again with this one line, owned like its directory.
fn write_audit_line(root: &Path, marker: &Marker) -> Result<(), ResetError> {
    let path = at(root, AUDIT_FILE);
    let dir = path
        .parent()
        .ok_or_else(|| ResetError::Failed("no audit directory".into()))?;
    let owner = dir
        .metadata()
        .map_err(io(format!("reading {}", dir.display())))?;
    let by = if marker.by.is_empty() {
        "?"
    } else {
        &marker.by
    };
    let record = AuditRecord::now(
        "factory_reset",
        0,
        "",
        format!(
            "factory reset: Melissa's data, apps, sign-ins and logs erased (asked by {by} at {})",
            marker.requested_at
        ),
    )
    .by("host");
    let mut line = melissa_statefile::encode_line(&melissa_agent::formats::AUDIT_LINE, &record)
        .map_err(|e| ResetError::Failed(e.to_string()))?;
    line.push('\n');
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o640)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(&path)
        .map_err(io(format!("writing {}", path.display())))?;
    f.write_all(line.as_bytes())
        .and_then(|()| f.sync_all())
        .map_err(io(format!("writing {}", path.display())))?;
    std::os::unix::fs::chown(&path, Some(owner.uid()), Some(owner.gid()))
        .map_err(io(format!("chown {}", path.display())))
}

/// Mount points from `/proc/self/mountinfo` text (field 5, octal escapes undone).
pub fn parse_mountinfo(text: &str) -> Vec<PathBuf> {
    text.lines()
        .filter_map(|l| l.split(' ').nth(4))
        .map(|m| PathBuf::from(unescape_octal(m)))
        .collect()
}

fn unescape_octal(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 4 <= b.len() {
            let digits = &b[i + 1..i + 4];
            if digits.iter().all(|d| (b'0'..=b'7').contains(d)) {
                let v = digits
                    .iter()
                    .fold(0u32, |acc, d| acc * 8 + u32::from(d - b'0'));
                if let Ok(byte) = u8::try_from(v) {
                    out.push(byte);
                    i += 4;
                    continue;
                }
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The list is sane: absolute, plain (no `.`/`..`), at least two levels deep, and never one of
/// [`KEEP`] or above one (checked by the tests; also at run time before anything goes).
pub fn check_lists() -> Result<(), String> {
    // A swept directory may hold kept entries only directly inside it (those are skipped).
    for t in SWEEP {
        let p = Path::new(t);
        if p.components().count() < 4 {
            return Err(format!("{t} is too close to /"));
        }
        for k in KEEP {
            let k = Path::new(k);
            if p.starts_with(k) || (k.starts_with(p) && k.parent() != Some(p)) {
                return Err(format!("{t} would remove {}", k.display()));
            }
        }
    }
    for t in WIPE.iter().chain(EMPTY) {
        let p = Path::new(t);
        let plain = p.is_absolute()
            && p.components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
            && !t.ends_with('/')
            && !t.contains("//");
        if !plain {
            return Err(format!("{t} is not a plain absolute path"));
        }
        if p.components().count() < 3 {
            return Err(format!("{t} is too close to /"));
        }
        for k in KEEP {
            if Path::new(k).starts_with(p) {
                return Err(format!("{t} would remove {k}"));
            }
        }
    }
    Ok(())
}

// --- the real system -------------------------------------------------------------------------

/// The container's systemd, mounts, journal and Tailscale.
pub struct SystemHost {
    /// `MELISSA_HOST_TAILSCALE` (the host runs Tailscale: Melissa never touches it).
    pub host_tailscale: Option<String>,
}

impl SystemHost {
    pub fn from_env() -> Self {
        Self {
            host_tailscale: std::env::var(crate::tailscale::HOST_ENV).ok(),
        }
    }
}

/// Run `program args`, waiting at most `timeout` (then kill it). Output goes to the journal
/// (`quiet`: nowhere).
fn command(program: &str, args: &[&str], timeout: Duration, quiet: bool) -> Result<(), String> {
    let mut cmd = Command::new(program);
    cmd.args(args).stdin(Stdio::null());
    if quiet {
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot run {program}: {}", e.kind()))?;
    let until = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(format!("`{program} {}` failed ({status})", args.join(" ")))
            }
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(100)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "`{program} {}` did not finish in time",
                    args.join(" ")
                ));
            }
            Err(e) => return Err(format!("waiting for {program}: {e}")),
        }
    }
}

impl Host for SystemHost {
    fn unit_active(&self, unit: &str) -> bool {
        command(
            "systemctl",
            &["is-active", "--quiet", unit],
            Duration::from_secs(10),
            true,
        )
        .is_ok()
    }

    fn tailscale_logout(&self) -> Result<(), String> {
        use crate::tailscale as ts;
        if ts::parse_host_flag(self.host_tailscale.as_deref()) == Some(true) {
            return Ok(());
        }
        if !Path::new(ts::SOCKET).exists() {
            return Ok(());
        }
        let socket = format!("--socket={}", ts::SOCKET);
        command(ts::CLI, &[&socket, "logout"], Duration::from_secs(20), true)
    }

    fn stop_units(&self, units: &[&str]) -> Result<(), String> {
        let mut args = vec!["stop", "--"];
        args.extend_from_slice(units);
        command("systemctl", &args, Duration::from_secs(300), false)
    }

    fn own_cgroup(&self) -> Option<String> {
        let text = std::fs::read_to_string("/proc/self/cgroup").ok()?;
        text.lines()
            .find_map(|l| l.strip_prefix("0::"))
            .map(|p| p.trim().to_string())
            .filter(|p| p.starts_with('/'))
    }

    fn kill_cgroup(&self, cgroup: &str) {
        let kill = Path::new("/sys/fs/cgroup")
            .join(cgroup.trim_start_matches('/'))
            .join("cgroup.kill");
        if kill.exists() {
            if let Err(e) = std::fs::write(&kill, "1") {
                tracing::warn!(cgroup, "cgroup.kill: {e}");
            }
        }
    }

    fn kill_shims(&self) {
        let _ = command(
            "pkill",
            &["-KILL", "-f", "/k3s/data/[^/]+/bin/containerd-shim"],
            Duration::from_secs(10),
            true,
        );
        std::thread::sleep(Duration::from_secs(2));
        // The pods' network namespaces and k3s's interfaces (k3s makes them again).
        if let Ok(entries) = std::fs::read_dir("/run/netns") {
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().starts_with("cni-") {
                    let p = e.path();
                    let _ = self.unmount(&p);
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
        for link in [
            "cni0",
            "flannel.1",
            "flannel-v6.1",
            "flannel-wg",
            "flannel-wg-v6",
            "kube-ipvs0",
        ] {
            let _ = command(
                "ip",
                &["link", "delete", link],
                Duration::from_secs(5),
                true,
            );
        }
    }

    fn mounts(&self) -> Result<Vec<PathBuf>, String> {
        std::fs::read_to_string("/proc/self/mountinfo")
            .map(|t| parse_mountinfo(&t))
            .map_err(|e| format!("reading the mount table: {e}"))
    }

    fn unmount(&self, path: &Path) -> Result<(), String> {
        let p = path.to_string_lossy();
        command("umount", &["-l", "--", &p], Duration::from_secs(30), true)
    }

    fn journal_detach(&self) -> Result<(), String> {
        command(
            "journalctl",
            &["--relinquish-var"],
            Duration::from_secs(60),
            true,
        )
    }

    fn journal_attach(&self) {
        let _ = command("journalctl", &["--flush"], Duration::from_secs(60), true);
    }

    fn restore_layout(&self) -> Result<(), String> {
        let mut args = vec!["--create"];
        args.extend_from_slice(TMPFILES);
        command("systemd-tmpfiles", &args, Duration::from_secs(60), false)
    }

    fn start_units(&self, units: &[&str]) -> Result<(), String> {
        let mut args = vec!["restart", "--no-block", "--"];
        args.extend_from_slice(units);
        command("systemctl", &args, Duration::from_secs(60), false)
    }

    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

#[cfg(test)]
mod tests;
