//! Backups against the real restic binary: set up a drive, back up a fixture of Melissa's
//! state, k3s and two apps (through a fake cluster whose "containers" are local commands),
//! restore, compare. Skipped (with the reason) when no restic is found: set
//! `MELISSA_TEST_RESTIC=/path/to/restic` or put `restic` (0.17.1 or newer) on PATH.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    cell::RefCell,
    collections::BTreeMap,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use melissa_backup::{
    apps::{AppInfo, Cluster, SecretData, Volume},
    at,
    config::{ConfigRequest, Schedule},
    engine::{BackupEngine, Restic},
    marker,
    request::Request,
    run::{Device, STATE_TAG},
    status::{self, Status},
    BackupError, DRIVES_DIR,
};

fn restic() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("MELISSA_TEST_RESTIC") {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join("restic"))
            .find(|p| p.is_file())
    })
}

macro_rules! need_restic {
    () => {
        match restic() {
            Some(r) => Restic::new(r),
            None => {
                eprintln!(
                    "SKIPPED: no restic binary (set MELISSA_TEST_RESTIC or put restic on PATH)"
                );
                return;
            }
        }
    };
}

fn scratch(name: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "melissa-backup-it-{name}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Melissa's state as a device has it, with things that must not be backed up.
fn state_fixture(root: &Path) {
    let m = root.join("var/lib/melissa");
    write(&m.join("device.json"), r#"{"id":"dev-1"}"#);
    write(&m.join("auth/users.json"), "people");
    write(&m.join("lead/transcript.jsonl"), "chat");
    write(&m.join("home/.claude/.credentials.json"), "signed in");
    write(&m.join("home/.cache/big"), "cache");
    write(&m.join("tmp/scratch"), "tmp");
    write(&m.join("models/ggml-base.bin"), "model");
    let etc = root.join("var/lib/melissa-container/etc-melissa");
    write(&etc.join("providers.toml"), "[claude]");
    write(&etc.join("server.toml"), "[web]");
    write(&etc.join("cluster-join.token"), "K10::join");
}

/// A cluster with apps whose "containers" are this machine: exec runs the command here.
#[derive(Default)]
struct FakeCluster {
    apps: BTreeMap<String, AppInfo>,
    secrets: RefCell<BTreeMap<String, Vec<SecretData>>>,
    calls: RefCell<Vec<String>>,
}

impl Cluster for FakeCluster {
    fn apps(&self) -> Result<Vec<String>, BackupError> {
        Ok(self.apps.keys().cloned().collect())
    }
    fn app(&self, app: &str) -> Result<Option<AppInfo>, BackupError> {
        Ok(self.apps.get(app).cloned())
    }
    fn secrets(&self, app: &str, names: &[String]) -> Result<Vec<SecretData>, BackupError> {
        Ok(self
            .secrets
            .borrow()
            .get(app)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|s| names.contains(&s.name))
            .collect())
    }
    fn apply_secrets(&self, app: &str, secrets: &[SecretData]) -> Result<(), BackupError> {
        self.calls.borrow_mut().push(format!("apply_secrets {app}"));
        self.secrets
            .borrow_mut()
            .insert(app.into(), secrets.to_vec());
        Ok(())
    }
    fn scale(&self, app: &str, replicas: i32) -> Result<(), BackupError> {
        self.calls
            .borrow_mut()
            .push(format!("scale {app} {replicas}"));
        Ok(())
    }
    fn wait_stopped(&self, _app: &str, _t: Duration) -> Result<(), BackupError> {
        Ok(())
    }
    fn ready_pod(&self, app: &str, _c: &str, _t: Duration) -> Result<Option<String>, BackupError> {
        Ok(Some(format!("{app}-pod")))
    }
    fn exec_argv(&self, _a: &str, _p: &str, _c: &str, command: &[String], _s: bool) -> Vec<String> {
        command.to_vec()
    }
}

fn app(name: &str, spec: serde_json::Value, volumes: Vec<(&str, PathBuf)>) -> AppInfo {
    AppInfo {
        app: name.into(),
        spec_text: Some(spec.to_string()),
        labels: BTreeMap::from([("melissa.io/workspace".into(), "ws".into())]),
        annotations: BTreeMap::new(),
        replicas: 1,
        volumes: volumes
            .into_iter()
            .map(|(v, path)| Volume {
                volume: v.into(),
                pvc: format!("{name}-{v}"),
                path,
            })
            .collect(),
    }
}

/// A stand-in for sqlite3 that copies the database (`VACUUM INTO '<path>'`).
fn fake_sqlite(dir: &Path) -> PathBuf {
    let p = dir.join("sqlite3");
    write(
        &p,
        "#!/bin/sh\nout=$(printf '%s' \"$5\" | sed \"s/^VACUUM INTO '\\(.*\\)'$/\\1/\")\nexec cp \"$4\" \"$out\"\n",
    );
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// A device rooted at `root` whose drives are plain directories (tests can't mount).
fn test_device<'a>(root: &Path, engine: &'a Restic) -> Device<'a> {
    let mut d = Device::new(root, engine);
    d.require_mounted = false;
    d
}

/// Where local-path keeps volumes, under the test root.
fn storage(root: &Path) -> PathBuf {
    at(root, melissa_backup::apps::LOCAL_PATH_ROOT)
}

/// The safety copies a restore left next to `dst`.
fn safety_copies(dst: &Path) -> Vec<PathBuf> {
    let name = dst.file_name().unwrap().to_string_lossy().into_owned();
    let mut found: Vec<PathBuf> = std::fs::read_dir(dst.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(&format!("{name}.pre-restore-"))
        })
        .map(|e| e.path())
        .collect();
    found.sort();
    found
}

fn set_up(device: &Device<'_>, root: &Path, encrypted: bool) -> PathBuf {
    let drive = at(root, DRIVES_DIR).join("usb1");
    std::fs::create_dir_all(drive.join("lost+found")).unwrap();
    device
        .handle(&Request::Config(ConfigRequest {
            target: Some(drive.clone()),
            encrypted: Some(encrypted),
            schedule: Some(Schedule::Daily { hour: 3 }),
            retention: None,
        }))
        .unwrap();
    device.handle(&Request::Init).unwrap();
    drive
}

#[test]
fn back_up_and_restore_state_k3s_and_apps() {
    let engine = need_restic!();
    let root = scratch("full");
    state_fixture(&root);
    // k3s's server state.
    let server = root.join("var/lib/rancher/k3s/server");
    write(&server.join("db/state.db"), "datastore-1");
    write(&server.join("token"), "K10::server");
    write(&server.join("cred/encryption-config.json"), "{\"keys\":1}");
    write(&server.join("tls/server-ca.key"), "ca");

    // Two apps: one with a dump hook (its database volume left out), one stopped for the copy.
    let storage = storage(&root);
    write(&storage.join("pvc-notes-data/notes/a.txt"), "v1");
    write(&storage.join("pvc-notes-data/cache/c.bin"), "cache");
    write(&storage.join("pvc-notes-db/pg/PG_VERSION"), "17");
    write(&storage.join("pvc-wiki-data/page.md"), "hello");
    let loaded = root.join("loaded.sql");
    let notes_spec = serde_json::json!({
        "description": "Notes",
        "containers": [
            {"name": "web", "image": "ghcr.io/example/notes:1.0",
             "mounts": [{"volume": "data", "path": "/data"}],
             "env": {"ADMIN": {"secret": "admin_password"}}},
            {"name": "db", "image": "postgres:17",
             "mounts": [{"volume": "db", "path": "/var/lib/postgresql/data"}]}
        ],
        "volumes": [{"name": "data", "size": "1Gi"}, {"name": "db", "size": "1Gi"}],
        "secrets": [{"name": "admin_password"}],
        "backup": {
            "mode": "dump",
            "dump": {"container": "db", "command": ["sh", "-c", "printf 'dump-v1'"], "file": "db.sql"},
            "restore": {"container": "db", "command": ["sh", "-c", format!("cat > {}", loaded.display())], "stdin": "db.sql"},
            "exclude": ["db", "data/cache"]
        }
    });
    let wiki_spec = serde_json::json!({
        "description": "Wiki",
        "containers": [{"name": "wiki", "image": "ghcr.io/example/wiki:2.0",
                        "mounts": [{"volume": "data", "path": "/data"}]}],
        "volumes": [{"name": "data", "size": "1Gi"}],
    });
    let mut cluster = FakeCluster::default();
    cluster.apps.insert(
        "notes".into(),
        app(
            "notes",
            notes_spec,
            vec![
                ("data", storage.join("pvc-notes-data")),
                ("db", storage.join("pvc-notes-db")),
            ],
        ),
    );
    cluster.apps.insert(
        "wiki".into(),
        app(
            "wiki",
            wiki_spec,
            vec![("data", storage.join("pvc-wiki-data"))],
        ),
    );
    cluster.secrets.borrow_mut().insert(
        "notes".into(),
        vec![SecretData {
            name: "notes-secret-admin-password".into(),
            data: BTreeMap::from([("admin_password".into(), "czNjcmV0".into())]),
        }],
    );

    let mut device = test_device(&root, &engine);
    device.cluster = Some(&cluster);
    device.sqlite = fake_sqlite(&root);
    device.k3s_version = Some("v1.37.2+k3s1".into());
    let drive = set_up(&device, &root, true);
    assert!(drive.join(marker::MARKER_FILE).is_file());
    let key = status::pending_key(&device.status_dir())
        .unwrap()
        .expect("a key for the owner");
    device.handle(&Request::AckKey).unwrap();
    assert_eq!(status::pending_key(&device.status_dir()).unwrap(), None);

    device.handle(&Request::Run).unwrap();
    let s = Status::load(&device.status_dir()).unwrap();
    let run = s.last_backup.expect("a backup report");
    assert!(run.ok, "{run:#?}");
    let names: Vec<&str> = run.items.iter().map(|i| i.item.as_str()).collect();
    assert_eq!(
        names,
        [
            "item:state",
            "item:k3s",
            "app:notes",
            "app:wiki",
            "retention"
        ]
    );
    assert_eq!(run.items[2].mode.as_deref(), Some("dump"));
    assert_eq!(run.items[2].snapshots.len(), 2, "dump + files");
    assert_eq!(run.items[3].mode.as_deref(), Some("stop"));
    assert!(!run.secrets_left_out, "encrypted: the secrets are in");
    assert!(s.last_ok.is_some());
    // The stop-mode app was stopped and started again; the dump app never was.
    assert_eq!(*cluster.calls.borrow(), ["scale wiki 0", "scale wiki 1"]);
    cluster.calls.borrow_mut().clear();

    // Things change after the backup.
    write(&storage.join("pvc-notes-data/notes/a.txt"), "v2");
    cluster.secrets.borrow_mut().insert("notes".into(), vec![]);

    // Restore one app.
    let report = device.restore_app("notes", None, "test");
    assert!(report.ok, "{report:#?}");
    assert_eq!(read(&storage.join("pvc-notes-data/notes/a.txt")), "v1");
    let copies = safety_copies(&storage.join("pvc-notes-data"));
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(read(&copies[0].join("notes/a.txt")), "v2");
    // Excluded: not copied, so not restored; the whole excluded volume was left alone.
    assert!(!storage.join("pvc-notes-data/cache").exists());
    assert_eq!(read(&storage.join("pvc-notes-db/pg/PG_VERSION")), "17");
    assert!(safety_copies(&storage.join("pvc-notes-db")).is_empty());
    // The app runs again and nothing is left to put right.
    assert!(!device.config_dir().join("scaled.json").exists());
    // The dump went to the restore hook, the Secret values came back.
    assert_eq!(read(&loaded), "dump-v1");
    assert_eq!(
        cluster.secrets.borrow()["notes"][0].data["admin_password"],
        "czNjcmV0"
    );
    assert_eq!(
        *cluster.calls.borrow(),
        ["scale notes 0", "apply_secrets notes", "scale notes 1"]
    );
    // A second restore keeps the first one's safety copy too.
    write(&storage.join("pvc-notes-data/notes/a.txt"), "v3");
    std::thread::sleep(Duration::from_millis(1100));
    assert!(device.restore_app("notes", None, "test").ok);
    let copies = safety_copies(&storage.join("pvc-notes-data"));
    assert_eq!(copies.len(), 2, "{copies:?}");
    assert_eq!(read(&copies[0].join("notes/a.txt")), "v2");
    assert_eq!(read(&copies[1].join("notes/a.txt")), "v3");

    // Whole-device parts: Melissa's state into a fresh device, the raw k3s rollback.
    let fresh = scratch("fresh");
    write(
        &fresh.join("var/lib/melissa/models/ggml-base.bin"),
        "fresh model",
    );
    write(
        &fresh.join("var/lib/melissa-container/etc-melissa/server.toml"),
        "[fresh]",
    );
    let placed = device.restore_state(None, &fresh).unwrap();
    assert_eq!(placed.len(), 2);
    let m = fresh.join("var/lib/melissa");
    assert_eq!(read(&m.join("device.json")), r#"{"id":"dev-1"}"#);
    assert_eq!(read(&m.join("auth/users.json")), "people");
    // Provider sign-ins are never in a backup: the agents sign in again.
    assert!(!m.join("home").exists());
    assert!(!m.join("tmp").exists());
    // This device's own: kept.
    assert_eq!(read(&m.join("models/ggml-base.bin")), "fresh model");
    let etc = fresh.join("var/lib/melissa-container/etc-melissa");
    assert_eq!(read(&etc.join("server.toml")), "[fresh]");
    assert_eq!(read(&etc.join("providers.toml")), "[claude]");
    assert!(!etc.join("cluster-join.token").exists());

    write(&server.join("db/state.db"), "datastore-2");
    device.restore_k3s(None, &server).unwrap();
    assert_eq!(read(&server.join("db/state.db")), "datastore-1");
    assert_eq!(
        read(&server.join("cred/encryption-config.json")),
        "{\"keys\":1}"
    );

    // The key the owner wrote down opens the backups on a new device.
    let other = scratch("adopt");
    let device2 = test_device(&other, &engine);
    let mut config = melissa_backup::config::Config {
        target: Some(drive.clone()),
        ..Default::default()
    };
    assert!(device2.adopt(&mut config, None).is_err());
    let wrong = melissa_backup::key::RecoveryKey::from_entropy(&[1; 20]);
    assert!(matches!(
        device2.adopt(&mut config, Some(wrong)),
        Err(BackupError::WrongKey)
    ));
    device2.adopt(&mut config, Some(key)).unwrap();
    assert!(device2
        .snapshots()
        .unwrap()
        .iter()
        .any(|s| s.tags.contains(&STATE_TAG.to_string())));
}

#[test]
fn an_unplugged_drive_is_never_written_and_retention_prunes() {
    let engine = need_restic!();
    let root = scratch("unplugged");
    state_fixture(&root);
    let device = test_device(&root, &engine);
    let drive = set_up(&device, &root, false);
    // Unencrypted: no key anywhere.
    assert!(!device.config_dir().join("key").exists());
    assert_eq!(status::pending_key(&device.status_dir()).unwrap(), None);
    for _ in 0..3 {
        device.handle(&Request::Run).unwrap();
        std::thread::sleep(Duration::from_millis(1100));
    }
    // Three runs in one week: the week's latest is kept, and (restic keeps the oldest while
    // there are fewer weeks than asked for) the first; the one in between goes.
    let config = device.config().unwrap();
    let repo = device.open(&config).unwrap();
    let states = engine.snapshots(&repo, &[STATE_TAG.into()]).unwrap();
    assert_eq!(states.len(), 2, "{states:?}");
    let last = Status::load(&device.status_dir())
        .unwrap()
        .last_backup
        .unwrap();
    assert!(last.secrets_left_out, "unencrypted: no secrets");
    let retention = last.items.iter().find(|i| i.item == "retention").unwrap();
    assert!(
        retention
            .message
            .as_deref()
            .is_some_and(|m| m.contains("removed")),
        "{last:#?}"
    );

    // The drive goes away; its mountpoint stays, empty.
    std::fs::rename(&drive, root.join("away")).unwrap();
    std::fs::create_dir(&drive).unwrap();
    assert!(device.handle(&Request::Run).is_err());
    let s = Status::load(&device.status_dir()).unwrap();
    let last = s.last_backup.unwrap();
    assert!(!last.ok);
    assert!(last.error.unwrap().contains("not connected"));
    assert_eq!(
        std::fs::read_dir(&drive).unwrap().count(),
        0,
        "nothing written"
    );
    // Init refuses a folder that isn't empty, and a drive set up already stays as it is.
    write(&drive.join("photo.jpg"), "x");
    let mut c = device.config().unwrap();
    c.repo_id = None;
    assert!(device.init(&mut c).is_err());

    // Back again: backups go on.
    std::fs::remove_dir_all(&drive).unwrap();
    std::fs::rename(root.join("away"), &drive).unwrap();
    device.handle(&Request::Run).unwrap();
}

#[test]
fn the_unit_takes_requests_and_runs_the_schedule() {
    let engine = need_restic!();
    let root = scratch("unit");
    state_fixture(&root);
    let device = test_device(&root, &engine);
    let drive = at(&root, DRIVES_DIR).join("usb1");
    std::fs::create_dir_all(&drive).unwrap();
    let req = device.request_dir();
    std::fs::create_dir_all(&req).unwrap();
    let requester = melissa_backup::request::Requester::new(&req, device.status_dir(), None);
    requester
        .ask(
            &Request::Config(ConfigRequest {
                target: Some(drive.clone()),
                encrypted: Some(false),
                ..Default::default()
            }),
            "owner",
        )
        .unwrap();
    requester.ask(&Request::Init, "owner").unwrap();
    let now = jiff::Zoned::now();
    device.unit(&now).unwrap();
    let s = requester.status().unwrap();
    assert!(s.ready, "{s:#?}");
    // The default schedule (weekly) is due: never run before.
    assert!(s.last_backup.as_ref().is_some_and(|r| r.ok), "{s:#?}");
    assert_eq!(std::fs::read_dir(&req).unwrap().count(), 0);
    // Not due again right away.
    let before = s.last_backup.unwrap().started;
    device.unit(&now).unwrap();
    assert_eq!(
        requester.status().unwrap().last_backup.unwrap().started,
        before
    );
    // A refused setting shows up as the status's problem.
    requester
        .ask(
            &Request::Config(ConfigRequest {
                target: Some("/etc".into()),
                ..Default::default()
            }),
            "owner",
        )
        .unwrap();
    device.unit(&now).unwrap();
    assert!(requester
        .status()
        .unwrap()
        .problem
        .is_some_and(|p| p.contains("below")));

    // The next slot comes while the drive is away: no attempt, it waits for the drive...
    let later = now.checked_add(jiff::Span::new().days(8)).unwrap();
    std::fs::rename(&drive, root.join("away")).unwrap();
    std::fs::create_dir(&drive).unwrap();
    device.unit(&later).unwrap();
    let s = requester.status().unwrap();
    assert_eq!(s.last_backup.as_ref().unwrap().started, before);
    assert!(s.waiting_for_drive.is_some(), "{s:#?}");
    // ...and runs at the first start that finds it back.
    std::fs::remove_dir(&drive).unwrap();
    std::fs::rename(root.join("away"), &drive).unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    device.unit(&later).unwrap();
    let s = requester.status().unwrap();
    assert!(s.last_backup.as_ref().unwrap().started > before, "{s:#?}");
    assert!(s.last_backup.unwrap().ok);
    assert_eq!(s.waiting_for_drive, None);
}

#[test]
fn a_tampered_backup_cannot_move_root_directories_or_secrets() {
    let engine = need_restic!();
    let root = scratch("tampered");
    let storage = storage(&root);
    write(&storage.join("pvc-notes-data/a.txt"), "live");
    // Something of root's, and an app's secret the backup must not touch.
    write(&root.join("var/lib/melissa/auth/users.json"), "people");
    let spec = serde_json::json!({
        "description": "Notes",
        "containers": [{"name": "web", "image": "ghcr.io/example/notes:1.0",
                        "mounts": [{"volume": "data", "path": "/data"}]}],
        "volumes": [{"name": "data", "size": "1Gi"}],
    });
    let mut cluster = FakeCluster::default();
    cluster.apps.insert(
        "notes".into(),
        app(
            "notes",
            spec,
            vec![("data", storage.join("pvc-notes-data"))],
        ),
    );
    let mut device = test_device(&root, &engine);
    device.cluster = Some(&cluster);
    set_up(&device, &root, true);
    device.handle(&Request::Run).unwrap();
    let config = device.config().unwrap();
    let repo = device.open(&config).unwrap();

    // Someone with the drive adds a later "backup" of the app whose app.json moves one of
    // root's directories into the app's volume, and one that names another Secret.
    let meta = device.config_dir().join("staging/apps/notes");
    let evil_exports = [
        serde_json::json!({"format": 1, "app": "notes", "spec": null, "labels": {},
            "annotations": {}, "replicas": 1, "mode": "stop",
            "volumes": [{"volume": "data", "pvc": "notes-data",
                         "path": root.join("var/lib/melissa/auth")}]}),
        serde_json::json!({"format": 1, "app": "notes", "spec": null, "labels": {},
            "annotations": {}, "replicas": 1, "mode": "stop",
            "volumes": [{"volume": "data", "pvc": "notes-data",
                         "path": storage.join("pvc-notes-data/../../../../melissa/auth")}]}),
    ];
    for (i, export) in evil_exports.iter().enumerate() {
        write(&meta.join("app.json"), &export.to_string());
        write(
            &meta.join("secrets.json"),
            r#"[{"name":"k3s-serving","data":{"tls.key":"czNj"}}]"#,
        );
        let tags = ["app:notes".to_string(), "part:files".to_string()];
        let time = melissa_backup::now() + 10 + i as i64;
        engine
            .backup_paths(
                &repo,
                &melissa_backup::engine::Source { tags: &tags, time },
                &[meta.clone(), root.join("var/lib/melissa/auth")],
                &[],
            )
            .unwrap();
        let report = device.restore_app("notes", Some(time), "test");
        assert!(!report.ok, "{report:#?}");
        assert!(
            report
                .error
                .as_deref()
                .is_some_and(|e| e.contains("outside")),
            "{report:#?}"
        );
        // Nothing moved, nothing applied, the app was never stopped.
        assert_eq!(
            read(&root.join("var/lib/melissa/auth/users.json")),
            "people"
        );
        assert_eq!(read(&storage.join("pvc-notes-data/a.txt")), "live");
        assert!(safety_copies(&storage.join("pvc-notes-data")).is_empty());
        assert!(cluster
            .calls
            .borrow()
            .iter()
            .all(|c| !c.starts_with("apply")));
    }
    let _ = std::fs::remove_dir_all(&meta);

    // A volume in the backup that is a symlink (to root's directory) is refused too.
    let link_root = scratch("tampered-link");
    let link = at(&link_root, storage.join("pvc-notes-data"));
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(root.join("var/lib/melissa/auth"), &link).unwrap();
    let good = serde_json::json!({"format": 1, "app": "notes", "spec": null, "labels": {},
        "annotations": {}, "replicas": 1, "mode": "stop",
        "volumes": [{"volume": "data", "pvc": "notes-data",
                     "path": storage.join("pvc-notes-data")}]});
    write(&meta.join("app.json"), &good.to_string());
    let tags = ["app:notes".to_string(), "part:files".to_string()];
    let time = melissa_backup::now() + 20;
    // Back up the symlink under the volume's absolute path: swap the real volume out first.
    let real = storage.join("pvc-notes-data");
    std::fs::rename(&real, root.join("real-volume")).unwrap();
    std::os::unix::fs::symlink(root.join("var/lib/melissa/auth"), &real).unwrap();
    engine
        .backup_paths(
            &repo,
            &melissa_backup::engine::Source { tags: &tags, time },
            &[meta.clone(), real.clone()],
            &[],
        )
        .unwrap();
    std::fs::remove_file(&real).unwrap();
    std::fs::rename(root.join("real-volume"), &real).unwrap();
    let report = device.restore_app("notes", Some(time), "test");
    assert!(!report.ok, "{report:#?}");
    assert_eq!(
        read(&root.join("var/lib/melissa/auth/users.json")),
        "people"
    );
    assert_eq!(read(&storage.join("pvc-notes-data/a.txt")), "live");
    assert!(safety_copies(&storage.join("pvc-notes-data")).is_empty());
}

#[test]
fn a_drive_that_is_not_mounted_is_never_set_up_or_written() {
    // No restic needed: nothing gets as far as the engine.
    let engine = Restic::new("/nonexistent/restic");
    let root = scratch("unmounted");
    let device = Device::new(&root, &engine);
    let drive = at(&root, DRIVES_DIR).join("usb1");
    std::fs::create_dir_all(&drive).unwrap();
    device
        .handle(&Request::Config(ConfigRequest {
            target: Some(drive.clone()),
            ..Default::default()
        }))
        .unwrap();
    let err = device.handle(&Request::Init).unwrap_err();
    assert!(
        matches!(err, BackupError::DriveMissing(ref m) if m.contains("not a mounted drive")),
        "{err}"
    );
    assert_eq!(std::fs::read_dir(&drive).unwrap().count(), 0);
    assert!(!device.drive_present(&device.config().unwrap()));
}

#[test]
fn a_run_killed_with_an_app_stopped_starts_it_again() {
    let engine = Restic::new("/nonexistent/restic");
    let root = scratch("killed");
    let mut cluster = FakeCluster::default();
    let mut notes = app("notes", serde_json::json!({}), vec![]);
    notes.replicas = 0;
    cluster.apps.insert("notes".into(), notes);
    let mut wiki = app("wiki", serde_json::json!({}), vec![]);
    wiki.replicas = 2; // started again by someone else meanwhile
    cluster.apps.insert("wiki".into(), wiki);
    let mut device = test_device(&root, &engine);
    device.cluster = Some(&cluster);
    write(
        &device.config_dir().join("scaled.json"),
        r#"{"notes": 3, "wiki": 1, "gone": 1}"#,
    );
    let _lock = device.begin().unwrap();
    assert_eq!(*cluster.calls.borrow(), ["scale notes 3"]);
    assert!(!device.config_dir().join("scaled.json").exists());
}
