//! The root unit's work: set up a drive, back up, apply retention, restore one app, and the
//! parts of a whole-device restore ([`Device`]). Everything here runs as root, one run at a
//! time ([`Device::lock`]).

use std::{
    fs::File,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use jiff::Zoned;
use melissa_appspec::{BackupMode, RestoreHook};

use crate::{
    apps::{secret_names, AppExport, Cluster, SecretData, EXPORT_FILE, SECRETS_FILE},
    at,
    config::Config,
    engine::{BackupEngine, Repo, Snapshot, Source},
    fsutil, inventory, io, k3s,
    key::RecoveryKey,
    marker,
    request::{self, Request},
    status::{self, ItemResult, Report, Running, Status},
    BackupError, CONFIG_DIR, DRIVES_DIR, REQUEST_DIR, STATUS_DIR,
};

/// The device's copy of the recovery key, in [`CONFIG_DIR`] (root 0600).
pub const KEY_FILE: &str = "key";
/// Tags: one item per snapshot group.
pub const STATE_TAG: &str = "item:state";
pub const K3S_TAG: &str = "item:k3s";
pub const FILES_TAG: &str = "part:files";
pub const DUMP_TAG: &str = "part:dump";

pub fn app_tag(app: &str) -> String {
    format!("app:{app}")
}

/// One device's backups: where things are, the engine, and the cluster (none on a device
/// without k3s, or in tests of the state part).
pub struct Device<'a> {
    /// `/` on the device; a temporary tree in tests.
    pub root: PathBuf,
    pub engine: &'a dyn BackupEngine,
    pub cluster: Option<&'a dyn Cluster>,
    /// The sqlite3 CLI (k3s's datastore copy).
    pub sqlite: PathBuf,
    /// This device's k3s version (recorded with the raw snapshot, checked on a rollback).
    pub k3s_version: Option<String>,
    /// How long an app may take to stop, and to be ready again for its restore hook.
    pub stop_timeout: Duration,
    pub ready_timeout: Duration,
}

impl<'a> Device<'a> {
    pub fn new(root: impl Into<PathBuf>, engine: &'a dyn BackupEngine) -> Self {
        Self {
            root: root.into(),
            engine,
            cluster: None,
            sqlite: crate::SQLITE_BIN.into(),
            k3s_version: None,
            stop_timeout: Duration::from_secs(180),
            ready_timeout: Duration::from_secs(300),
        }
    }

    pub fn config_dir(&self) -> PathBuf {
        at(&self.root, CONFIG_DIR)
    }

    pub fn status_dir(&self) -> PathBuf {
        at(&self.root, STATUS_DIR)
    }

    pub fn request_dir(&self) -> PathBuf {
        at(&self.root, REQUEST_DIR)
    }

    fn key_path(&self) -> PathBuf {
        self.config_dir().join(KEY_FILE)
    }

    fn staging(&self) -> PathBuf {
        self.config_dir().join("staging")
    }

    fn k3s_staging(&self) -> PathBuf {
        self.staging().join("k3s")
    }

    fn app_meta_dir(&self, app: &str) -> PathBuf {
        self.staging().join("apps").join(app)
    }

    fn k3s_server(&self) -> PathBuf {
        at(&self.root, k3s::SERVER_DIR)
    }

    /// One run at a time: hold the returned file for the run. [`BackupError::Busy`] when
    /// another holds it.
    pub fn lock(&self) -> Result<File, BackupError> {
        let dir = self.config_dir();
        fsutil::ensure_dir(&dir, 0o700)?;
        let path = dir.join("lock");
        let f = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(io(format!("opening {}", path.display())))?;
        match f.try_lock() {
            Ok(()) => Ok(f),
            Err(std::fs::TryLockError::WouldBlock) => Err(BackupError::Busy),
            Err(std::fs::TryLockError::Error(e)) => {
                Err(io(format!("locking {}", path.display()))(e))
            }
        }
    }

    pub fn config(&self) -> Result<Config, BackupError> {
        Config::load(&self.config_dir())
    }

    /// The repository, after checking the drive's marker (before every write).
    fn repo(&self, config: &Config) -> Result<Repo, BackupError> {
        let (target, id) = config.ready()?;
        marker::check_target(target, id)?;
        let password_file = if config.encrypted {
            let k = self.key_path();
            if !k.is_file() {
                return Err(BackupError::Failed(
                    "the recovery key is not on this device; enter it again".into(),
                ));
            }
            Some(k)
        } else {
            None
        };
        Ok(Repo {
            path: marker::repo_path(target),
            password_file,
        })
    }

    /// [`Device::repo`], and restic agrees it is that repository (the key opens it).
    pub fn open(&self, config: &Config) -> Result<Repo, BackupError> {
        let repo = self.repo(config)?;
        let (_, id) = config.ready()?;
        let actual = self.engine.repo_id(&repo)?;
        if actual != id {
            return Err(BackupError::WrongRepository(format!(
                "the repository's id is {actual}"
            )));
        }
        Ok(repo)
    }

    // --- settings and setup ------------------------------------------------------------

    /// Set up the configured target: a new repository in an empty directory, the marker, and
    /// with encryption a new recovery key (also left for the owner in the status directory).
    /// Already set up: nothing to do.
    pub fn init(&self, config: &mut Config) -> Result<Option<RecoveryKey>, BackupError> {
        let target = config.target.clone().ok_or(BackupError::NotConfigured)?;
        if let Some(id) = &config.repo_id {
            marker::check_target(&target, id)?;
            return Ok(None);
        }
        marker::check_empty(&target)?;
        fsutil::ensure_dir(&self.config_dir(), 0o700)?;
        let key = if config.encrypted {
            let k = RecoveryKey::generate()?;
            k.save(&self.key_path())?;
            Some(k)
        } else {
            fsutil::remove_any(&self.key_path())?;
            None
        };
        let repo = Repo {
            path: marker::repo_path(&target),
            password_file: key.as_ref().map(|_| self.key_path()),
        };
        let id = self.engine.init(&repo)?;
        marker::write(&target, &marker::Marker::new(&id, config.encrypted))?;
        config.repo_id = Some(id);
        config.save(&self.config_dir())?;
        if let Some(k) = &key {
            fsutil::ensure_dir(&self.status_dir(), 0o750)?;
            status::offer_key(&self.status_dir(), k)?;
        }
        Ok(key)
    }

    /// Use the backups already on the configured target (a new device, or this one after a
    /// factory reset): its marker says whether a key is needed; restic must open it.
    pub fn adopt(&self, config: &mut Config, key: Option<RecoveryKey>) -> Result<(), BackupError> {
        let target = config.target.clone().ok_or(BackupError::NotConfigured)?;
        let m = marker::read(&target)?.ok_or_else(|| {
            BackupError::DriveMissing(format!("no Melissa backups in {}", target.display()))
        })?;
        fsutil::ensure_dir(&self.config_dir(), 0o700)?;
        let password_file = match (m.encrypted, key) {
            (true, Some(k)) => {
                k.save(&self.key_path())?;
                Some(self.key_path())
            }
            (true, None) => {
                return Err(BackupError::Invalid(
                    "these backups are encrypted: the recovery key is needed".into(),
                ))
            }
            (false, _) => {
                fsutil::remove_any(&self.key_path())?;
                None
            }
        };
        let repo = Repo {
            path: marker::repo_path(&target),
            password_file,
        };
        let id = self.engine.repo_id(&repo)?;
        if id != m.repo_id {
            return Err(BackupError::WrongRepository(format!(
                "the marker names {}, the repository is {id}",
                m.repo_id
            )));
        }
        config.repo_id = Some(id);
        config.encrypted = m.encrypted;
        config.save(&self.config_dir())
    }

    /// Every snapshot of this device's backups.
    pub fn snapshots(&self) -> Result<Vec<Snapshot>, BackupError> {
        let config = self.config()?;
        let repo = self.repo(&config)?;
        self.engine.snapshots(&repo, &[])
    }

    // --- status --------------------------------------------------------------------------

    /// Load, change and write the status (with the current settings filled in).
    pub fn update_status(&self, change: impl FnOnce(&mut Status)) -> Result<(), BackupError> {
        let dir = self.status_dir();
        fsutil::ensure_dir(&dir, 0o750)?;
        let config = self.config()?;
        let mut s = Status::load(&dir).unwrap_or_default().with_config(&config);
        change(&mut s);
        s.recovery_key_pending = status::pending_key(&dir).ok().flatten().is_some();
        s.next_due = config
            .schedule
            .next_slot(&Zoned::now())
            .map(|t| t.as_second());
        s.save(&dir)
    }

    fn running(&self, action: &str, started: i64, step: &str) {
        let r = Running {
            action: action.into(),
            started,
            step: step.into(),
        };
        if let Err(e) = self.update_status(|s| s.running = Some(r)) {
            tracing::warn!("backup status: {e}");
        }
    }

    // --- backup --------------------------------------------------------------------------

    /// Back up everything now: Melissa's state, k3s, every app, then retention. `trigger`:
    /// `scheduled`, `owner` or `host`.
    pub fn backup(&self, trigger: &str) -> Report {
        let started = crate::now();
        let mut report = Report {
            trigger: trigger.into(),
            started,
            ..Report::default()
        };
        let config = match self.config() {
            Ok(c) => c,
            Err(e) => return fail(report, e),
        };
        let repo = match self.open(&config) {
            Ok(r) => r,
            Err(e) => return fail(report, e),
        };
        let time = started;

        self.running("backup", started, "Melissa's settings and data");
        report
            .items
            .push(item(STATE_TAG, self.backup_state(&repo, time)));

        self.running("backup", started, "the cluster");
        report
            .items
            .push(item(K3S_TAG, self.backup_k3s(&repo, time)));

        if let Some(cluster) = self.cluster {
            match cluster.apps() {
                Ok(apps) => {
                    for app in apps {
                        self.running("backup", started, &format!("app {app}"));
                        report
                            .items
                            .push(self.backup_app(cluster, &repo, &app, time));
                    }
                }
                Err(e) => report.items.push(ItemResult::failed("apps", e.to_string())),
            }
        }

        // Retention only after a run that reached the drive (the marker was checked again
        // by every item's write; check once more before deleting anything).
        self.running("backup", started, "removing old backups");
        let forget =
            marker::check_target(&repo_target(&repo), config.repo_id.as_deref().unwrap_or(""))
                .and_then(|_| self.engine.forget(&repo, &config.retention));
        report.items.push(match forget {
            Ok(n) => ItemResult {
                message: (n > 0).then(|| format!("{n} old backups removed")),
                ..ItemResult::ok("retention")
            },
            Err(e) => ItemResult::failed("retention", e.to_string()),
        });

        report.finished = crate::now();
        report.ok = report.items.iter().all(|i| i.ok);
        report
    }

    fn backup_state(&self, repo: &Repo, time: i64) -> Result<ItemResult, BackupError> {
        let paths = inventory::state_paths(&self.root);
        if paths.is_empty() {
            return Ok(ItemResult {
                message: Some("no Melissa data on this device".into()),
                ..ItemResult::ok(STATE_TAG)
            });
        }
        let tags = [STATE_TAG.to_string()];
        let added = self.engine.backup_paths(
            repo,
            &Source { tags: &tags, time },
            &paths,
            &inventory::state_excludes(&self.root),
        )?;
        Ok(added_item(STATE_TAG, None, &[added]))
    }

    fn backup_k3s(&self, repo: &Repo, time: i64) -> Result<ItemResult, BackupError> {
        let server = self.k3s_server();
        if !k3s::present(&server) {
            return Ok(ItemResult {
                message: Some("no cluster datastore on this device".into()),
                ..ItemResult::ok(K3S_TAG)
            });
        }
        let staging = self.k3s_staging();
        let result = k3s::stage(&server, &staging, &self.sqlite, self.k3s_version.as_deref())
            .and_then(|()| {
                let tags = [K3S_TAG.to_string()];
                self.engine.backup_paths(
                    repo,
                    &Source { tags: &tags, time },
                    std::slice::from_ref(&staging),
                    &[],
                )
            });
        let _ = fsutil::remove_any(&staging);
        Ok(added_item(K3S_TAG, None, &[result?]))
    }

    /// One app: its export, a dump if its hook has one, then its volumes (stopped around the
    /// copy for mode `stop`).
    fn backup_app(&self, cluster: &dyn Cluster, repo: &Repo, app: &str, time: i64) -> ItemResult {
        let name = app_tag(app);
        let meta = self.app_meta_dir(app);
        let result = self.backup_app_inner(cluster, repo, app, time, &meta);
        let _ = fsutil::remove_any(&meta);
        match result {
            Ok(r) => r,
            Err(e) => ItemResult::failed(name, e.to_string()),
        }
    }

    fn backup_app_inner(
        &self,
        cluster: &dyn Cluster,
        repo: &Repo,
        app: &str,
        time: i64,
        meta: &Path,
    ) -> Result<ItemResult, BackupError> {
        let tag = app_tag(app);
        let Some(info) = cluster.app(app)? else {
            return Ok(ItemResult {
                message: Some("removed before its turn".into()),
                ..ItemResult::ok(tag)
            });
        };
        let spec = info.spec();
        let mode = spec.as_ref().map_or(BackupMode::Stop, |s| s.backup_mode());
        let hook = spec.as_ref().and_then(|s| s.backup.clone());
        let running = info.replicas > 0;
        let mut export = AppExport::of(&info, mode.as_str());
        let mut added = Vec::new();
        let mut notes = Vec::new();

        fsutil::fresh_dir(meta)?;
        let secrets = cluster.secrets(app, &secret_names(app, spec.as_ref()))?;
        if !secrets.is_empty() {
            let text =
                serde_json::to_vec(&secrets).map_err(|e| BackupError::Failed(e.to_string()))?;
            fsutil::write_atomic(&meta.join(SECRETS_FILE), &text, 0o600, false)?;
        }

        // Which volumes, and what of them. A stopped app's files are consistent as they are:
        // a dump hook's whole-volume excludes don't apply then.
        let dump_now = mode == BackupMode::Dump && running;
        let excludes = hook.as_ref().map(|b| b.excludes()).unwrap_or_default();
        let mut paths = vec![meta.to_path_buf()];
        let mut exclude_paths = Vec::new();
        for v in &info.volumes {
            let whole = excludes
                .iter()
                .any(|e| e.volume == v.volume && e.path.is_none());
            if mode == BackupMode::Skip || (whole && (dump_now || mode != BackupMode::Dump)) {
                export.volumes_left_out.push(v.volume.clone());
                continue;
            }
            paths.push(v.path.clone());
            for e in excludes.iter().filter(|e| e.volume == v.volume) {
                if let Some(rel) = e.path {
                    exclude_paths.push(v.path.join(rel));
                }
            }
        }

        if dump_now {
            if let Some(d) = hook.as_ref().and_then(|b| b.dump.as_ref()) {
                let pod = cluster
                    .ready_pod(app, &d.container, Duration::ZERO)?
                    .ok_or_else(|| {
                        BackupError::Cluster(format!(
                            "container {} is not ready, so there is no dump",
                            d.container
                        ))
                    })?;
                let argv = cluster.exec_argv(app, &pod, &d.container, &d.command, false);
                let tags = [tag.clone(), DUMP_TAG.to_string()];
                added.push(self.engine.backup_command(
                    repo,
                    &Source { tags: &tags, time },
                    &d.file,
                    &argv,
                )?);
                export.dump = Some(d.file.clone());
            }
        } else if mode == BackupMode::Dump {
            notes.push("stopped: files copied as they are, no dump".to_string());
        }

        let text =
            serde_json::to_vec_pretty(&export).map_err(|e| BackupError::Failed(e.to_string()))?;
        fsutil::write_atomic(&meta.join(EXPORT_FILE), &text, 0o600, false)?;

        let stop = mode == BackupMode::Stop && running && paths.len() > 1;
        if stop {
            cluster.scale(app, 0)?;
            if let Err(e) = cluster.wait_stopped(app, self.stop_timeout) {
                let again = cluster.scale(app, info.replicas);
                return Err(match again {
                    Ok(()) => e,
                    Err(e2) => BackupError::Cluster(format!("{e}; starting it again failed: {e2}")),
                });
            }
        }
        let tags = [tag.clone(), FILES_TAG.to_string()];
        let copied =
            self.engine
                .backup_paths(repo, &Source { tags: &tags, time }, &paths, &exclude_paths);
        let restarted = if stop {
            cluster.scale(app, info.replicas)
        } else {
            Ok(())
        };
        added.push(copied?);
        if let Err(e) = restarted {
            return Err(BackupError::Cluster(format!(
                "backed up, but starting it again failed: {e}"
            )));
        }
        if stop {
            notes.push("stopped for the copy".into());
        }
        let mut r = added_item(&tag, Some(mode), &added);
        if !notes.is_empty() {
            r.message = Some(notes.join("; "));
        }
        Ok(r)
    }

    // --- restore -------------------------------------------------------------------------

    /// The snapshot of `tags` at `at` (a run's time), or the latest.
    fn pick(
        &self,
        repo: &Repo,
        tags: &[String],
        at_time: Option<i64>,
    ) -> Result<Snapshot, BackupError> {
        let snaps = self.engine.snapshots(repo, tags)?;
        let found = match at_time {
            Some(t) => snaps.into_iter().find(|s| s.time == t),
            None => snaps.into_iter().last(),
        };
        found.ok_or_else(|| {
            BackupError::Invalid(match at_time {
                Some(t) => format!("no backup of {} from {t}", tags.join(" ")),
                None => format!("no backup of {}", tags.join(" ")),
            })
        })
    }

    /// Restore one app from its backup at `at` (latest when `None`): its Secret values, then
    /// its volumes (each swapped in, the replaced data kept as `<volume>.pre-restore`), then
    /// its dump through the restore hook. The app must be deployed (deploy it again from the
    /// spec in its backup first); it is stopped around the swap and keeps its replicas.
    pub fn restore_app(&self, app: &str, at_time: Option<i64>, trigger: &str) -> Report {
        let mut report = Report {
            trigger: trigger.into(),
            started: crate::now(),
            ..Report::default()
        };
        self.running(
            &format!("restore {app}"),
            report.started,
            "reading the backup",
        );
        let staging = self.staging().join(format!("restore-{app}"));
        let result = self.restore_app_inner(app, at_time, &staging, &mut report.items);
        let _ = fsutil::remove_any(&staging);
        report.finished = crate::now();
        match result {
            Ok(()) => {
                report.ok = report.items.iter().all(|i| i.ok);
                report
            }
            Err(e) => fail(report, e),
        }
    }

    fn restore_app_inner(
        &self,
        app: &str,
        at_time: Option<i64>,
        staging: &Path,
        items: &mut Vec<ItemResult>,
    ) -> Result<(), BackupError> {
        if !melissa_appspec::valid_app_name(app) {
            return Err(BackupError::Invalid(format!("`{app}` is not an app name")));
        }
        let cluster = self
            .cluster
            .ok_or_else(|| BackupError::Cluster("no cluster on this device".into()))?;
        let config = self.config()?;
        let repo = self.open(&config)?;
        let tag = app_tag(app);
        let files = self.pick(&repo, &[tag.clone(), FILES_TAG.into()], at_time)?;
        let dump = self
            .engine
            .snapshots(&repo, &[tag.clone(), DUMP_TAG.into()])?
            .into_iter()
            .find(|s| s.time == files.time);
        let info = cluster.app(app)?.ok_or_else(|| {
            BackupError::Invalid(format!(
                "{app} is not installed: deploy it again (its spec is in the backup), then restore"
            ))
        })?;

        fsutil::fresh_dir(staging)?;
        self.engine.restore(&repo, &files.id, staging)?;
        let meta = at(staging, self.app_meta_dir(app));
        let export_text = fsutil::read_small(&meta.join(EXPORT_FILE), 1 << 20)?
            .ok_or_else(|| BackupError::Invalid("the app backup has no app.json".into()))?;
        let export = AppExport::parse(&export_text)?;
        if export.app != app {
            return Err(BackupError::Invalid(format!(
                "the backup is of {}, not {app}",
                export.app
            )));
        }
        let secrets: Vec<SecretData> = match fsutil::read_small(&meta.join(SECRETS_FILE), 1 << 20)?
        {
            Some(t) => serde_json::from_str(&t)
                .map_err(|e| BackupError::Invalid(format!("secrets in the backup: {e}")))?,
            None => Vec::new(),
        };

        // What goes where: a volume of the backup into the same volume of the app now.
        let mut swaps = Vec::new();
        for v in &export.volumes {
            if export.volumes_left_out.contains(&v.volume) {
                continue;
            }
            let src = at(staging, &v.path);
            if src.symlink_metadata().is_err() {
                continue;
            }
            match info.volume(&v.volume) {
                Some(now) => swaps.push((v.volume.clone(), src, now.path.clone())),
                None => items.push(ItemResult::failed(
                    format!("volume:{}", v.volume),
                    "the app has no such volume now",
                )),
            }
        }

        self.running(&format!("restore {app}"), crate::now(), "stopping the app");
        if info.replicas > 0 {
            cluster.scale(app, 0)?;
            if let Err(e) = cluster.wait_stopped(app, self.stop_timeout) {
                let _ = cluster.scale(app, info.replicas);
                return Err(e);
            }
        }
        let swapped = (|| -> Result<(), BackupError> {
            if !secrets.is_empty() {
                cluster.apply_secrets(app, &secrets)?;
                items.push(ItemResult {
                    message: Some(format!("{} secrets", secrets.len())),
                    ..ItemResult::ok("secrets")
                });
            }
            for (volume, src, dst) in &swaps {
                fsutil::swap_in(src, dst, &[])?;
                items.push(ItemResult {
                    message: Some(format!(
                        "the replaced data is kept in {}",
                        fsutil::pre_restore_path(dst).display()
                    )),
                    ..ItemResult::ok(format!("volume:{volume}"))
                });
            }
            Ok(())
        })();
        if info.replicas > 0 {
            cluster.scale(app, info.replicas)?;
        }
        swapped?;

        let hook: Option<RestoreHook> = export
            .spec
            .as_deref()
            .and_then(|t| melissa_appspec::AppSpec::from_text(t).ok())
            .and_then(|s| s.backup)
            .and_then(|b| b.restore);
        match (dump, hook) {
            (Some(d), Some(h)) if info.replicas > 0 => {
                self.running(&format!("restore {app}"), crate::now(), "loading the dump");
                let pod = cluster
                    .ready_pod(app, &h.container, self.ready_timeout)?
                    .ok_or_else(|| {
                        BackupError::Cluster(format!(
                            "{} was not ready within {} s for the dump",
                            h.container,
                            self.ready_timeout.as_secs()
                        ))
                    })?;
                let argv = cluster.exec_argv(app, &pod, &h.container, &h.command, true);
                let mut cmd = Command::new(&argv[0]);
                cmd.args(&argv[1..]);
                let loaded = self.engine.dump_into(&repo, &d.id, &h.stdin, &mut cmd);
                items.push(match loaded {
                    Ok(()) => ItemResult::ok("dump"),
                    Err(e) => ItemResult::failed("dump", e.to_string()),
                });
            }
            (Some(_), Some(_)) => items.push(ItemResult::failed(
                "dump",
                "the app is stopped: start it and restore again to load its dump",
            )),
            _ => {}
        }
        Ok(())
    }

    /// Whole-device restore, Melissa's part: put the state directories of the backup at `at`
    /// (latest when `None`) in place under `into` (`/` on the device), keeping this device's
    /// excluded entries (voice model, `server.toml`) and the replaced directories as
    /// `*.pre-restore`. Run with the server stopped (the first-run flow, later).
    pub fn restore_state(
        &self,
        at_time: Option<i64>,
        into: &Path,
    ) -> Result<Vec<PathBuf>, BackupError> {
        let config = self.config()?;
        let repo = self.open(&config)?;
        let snap = self.pick(&repo, &[STATE_TAG.into()], at_time)?;
        let staging = self.staging().join("restore-state");
        let result = (|| {
            fsutil::fresh_dir(&staging)?;
            self.engine.restore(&repo, &snap.id, &staging)?;
            let mut placed = Vec::new();
            for dir in inventory::BACKUP_STATE {
                let src = at(&staging, at(&self.root, dir));
                if src.symlink_metadata().is_err() {
                    continue;
                }
                let dst = at(into, dir);
                fsutil::swap_in(&src, &dst, &inventory::kept_names(dir))?;
                placed.push(dst);
            }
            Ok(placed)
        })();
        let _ = fsutil::remove_any(&staging);
        result
    }

    /// Whole-device restore, the raw cluster part (a same-version rollback, k3s stopped):
    /// put k3s's datastore, token, TLS and secrets key of the backup at `at` in place under
    /// `server` (k3s's server directory).
    pub fn restore_k3s(
        &self,
        at_time: Option<i64>,
        server: &Path,
    ) -> Result<Vec<PathBuf>, BackupError> {
        let config = self.config()?;
        let repo = self.open(&config)?;
        let snap = self.pick(&repo, &[K3S_TAG.into()], at_time)?;
        let staging = self.staging().join("restore-k3s");
        let result = (|| {
            fsutil::fresh_dir(&staging)?;
            self.engine.restore(&repo, &snap.id, &staging)?;
            k3s::place(
                &at(&staging, self.k3s_staging()),
                server,
                self.k3s_version.as_deref(),
            )
        })();
        let _ = fsutil::remove_any(&staging);
        result
    }

    // --- the unit ------------------------------------------------------------------------

    /// What `melissa-backup.service` does each time it starts: handle the server's requests,
    /// then run the scheduled backup if it is due. Another run holding the lock: nothing
    /// (that run's unit start will see new requests on its next start).
    pub fn unit(&self, now: &Zoned) -> Result<(), BackupError> {
        let _lock = match self.lock() {
            Ok(l) => l,
            Err(BackupError::Busy) => return Ok(()),
            Err(e) => return Err(e),
        };
        let (requests, problems) = request::take(&self.request_dir())?;
        let mut problem = (!problems.is_empty()).then(|| problems.join("; "));
        for r in requests {
            tracing::info!("backup request: {}", r.describe());
            if let Err(e) = self.handle(&r) {
                tracing::warn!("backup request {}: {e}", r.file_name());
                problem = Some(e.to_string());
            }
        }
        let config = self.config()?;
        let s = Status::load(&self.status_dir()).unwrap_or_default();
        let last_attempt = s.last_backup.as_ref().map(|r| r.started);
        if config.schedule.is_due(now, last_attempt, s.last_ok) && config.target.is_some() {
            self.record_backup(self.backup("scheduled"))?;
        }
        self.update_status(|s| {
            s.running = None;
            if problem.is_some() {
                s.problem = problem;
            }
        })
    }

    /// Handle one request (the lock held).
    pub fn handle(&self, r: &Request) -> Result<(), BackupError> {
        match r {
            Request::Config(c) => {
                let mut config = self.config()?;
                let roots = [at(&self.root, DRIVES_DIR)];
                let roots: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
                c.apply(&mut config, &roots)?;
                config.save(&self.config_dir())?;
                self.update_status(|s| s.problem = None)
            }
            Request::Init => {
                let mut config = self.config()?;
                self.running("init", crate::now(), "setting up the drive");
                self.init(&mut config)?;
                self.update_status(|s| {
                    s.running = None;
                    s.problem = None;
                })
            }
            Request::AckKey => {
                status::withdraw_key(&self.status_dir())?;
                self.update_status(|_| {})
            }
            Request::Run => self.record_backup(self.backup("owner")),
            Request::RestoreApp { app, at } => {
                let report = self.restore_app(app, *at, "owner");
                let failed = report.error.clone();
                self.update_status(|s| {
                    s.running = None;
                    s.last_restore = Some(report);
                })?;
                failed.map_or(Ok(()), |e| Err(BackupError::Failed(e)))
            }
        }
    }

    /// Record a finished backup in the status.
    pub fn record_backup(&self, report: Report) -> Result<(), BackupError> {
        let failed = report.error.clone();
        self.update_status(|s| {
            s.running = None;
            if report.ok {
                s.last_ok = Some(report.finished);
            }
            s.last_backup = Some(report);
        })?;
        failed.map_or(Ok(()), |e| Err(BackupError::Failed(e)))
    }
}

fn repo_target(repo: &Repo) -> PathBuf {
    repo.path
        .parent()
        .map_or_else(|| repo.path.clone(), Path::to_path_buf)
}

fn fail(mut report: Report, e: BackupError) -> Report {
    report.ok = false;
    report.error = Some(e.to_string());
    report.finished = crate::now();
    report
}

fn item(name: &str, r: Result<ItemResult, BackupError>) -> ItemResult {
    r.unwrap_or_else(|e| ItemResult::failed(name, e.to_string()))
}

fn added_item(name: &str, mode: Option<BackupMode>, added: &[crate::engine::Added]) -> ItemResult {
    let incomplete = added.iter().any(|a| a.incomplete);
    ItemResult {
        item: name.into(),
        ok: !incomplete,
        mode: mode.map(|m| m.as_str().to_string()),
        snapshots: added.iter().filter_map(|a| a.snapshot.clone()).collect(),
        bytes_added: added.iter().map(|a| a.bytes_added).sum(),
        message: incomplete.then(|| "some files could not be read".to_string()),
    }
}

/// The times of the runs that hold a backup of `app` (what a restore can pick), oldest first.
pub fn run_times(snapshots: &[Snapshot], app: &str) -> Vec<i64> {
    let tags = [app_tag(app), FILES_TAG.to_string()];
    let mut times: Vec<i64> = snapshots
        .iter()
        .filter(|s| s.has_tags(&tags))
        .map(|s| s.time)
        .collect();
    times.sort_unstable();
    times.dedup();
    times
}
