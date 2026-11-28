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
    let storage = root.join("storage");
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

    let mut device = Device::new(&root, &engine);
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
    assert_eq!(
        read(&storage.join("pvc-notes-data.pre-restore/notes/a.txt")),
        "v2"
    );
    // Excluded: not copied, so not restored; the whole excluded volume was left alone.
    assert!(!storage.join("pvc-notes-data/cache").exists());
    assert_eq!(read(&storage.join("pvc-notes-db/pg/PG_VERSION")), "17");
    assert!(!storage.join("pvc-notes-db.pre-restore").exists());
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
    assert_eq!(read(&m.join("home/.claude/.credentials.json")), "signed in");
    assert!(!m.join("tmp").exists());
    assert!(!m.join("home/.cache").exists());
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
    let device2 = Device::new(&other, &engine);
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
    let device = Device::new(&root, &engine);
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
    let device = Device::new(&root, &engine);
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
}
