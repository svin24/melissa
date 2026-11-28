use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::Duration,
};

use super::*;
use crate::test_support::scratch;

// --- the lists ----------------------------------------------------------------------------------

#[test]
fn the_lists_never_reach_what_stays() {
    check_lists().unwrap();
    let all: Vec<&str> = WIPE.iter().chain(EMPTY).copied().collect();
    for forbidden in [
        "/",
        "/var",
        "/var/lib",
        "/run",
        "/etc",
        "/usr",
        "/var/lib/melissa",
        "/var/lib/melissa/models",
        "/var/lib/melissa-container",
        "/var/lib/melissa-container/etc-melissa",
        "/var/lib/melissa-container/etc-melissa/server.toml",
        "/var/lib/melissa-container/rancher-node",
        "/var/lib/melissa-container/install.env",
        "/var/lib/melissa-container/status",
        "/var/lib/melissa-container/request",
        "/run/melissa-update",
        "/run/melissa-update-request",
        "/var/lib/rancher",
        MARKER_DIR,
    ] {
        for t in &all {
            assert!(
                !Path::new(forbidden).starts_with(t),
                "{t} would remove {forbidden}"
            );
        }
    }
    // Every target is under a directory Melissa (or k3s, or the journal) owns.
    for t in &all {
        assert!(
            [
                "/var/lib/melissa/",
                "/var/lib/melissa-container/etc-melissa/",
                "/var/lib/melissa-tailscale",
                "/var/lib/melissa-container/cluster",
                "/run/melissa-cluster",
                "/var/lib/rancher/k3s",
                "/var/lib/kubelet",
                "/var/lib/cni",
                "/run/k3s",
                "/var/log/journal",
                "/run/melissa-kube",
                "/var/lib/melissa-container/backup",
            ]
            .iter()
            .any(|p| t.starts_with(p)),
            "{t}"
        );
    }
    // No duplicates.
    let set: BTreeSet<&str> = all.iter().copied().collect();
    assert_eq!(set.len(), all.len());
}

#[test]
fn the_owner_decisions_are_in_the_lists() {
    for t in [
        "/var/lib/melissa/auth",
        "/var/lib/melissa/device.json",
        "/var/lib/melissa/audit.jsonl",
        "/var/lib/melissa/home",
        "/var/lib/melissa-container/etc-melissa/providers.toml",
        "/var/lib/melissa-tailscale",
        "/var/lib/rancher/k3s",
    ] {
        assert!(WIPE.contains(&t), "{t}");
    }
    assert!(EMPTY.contains(&"/var/log/journal"));
    assert!(SWEEP.contains(&"/var/lib/melissa"));
}

#[test]
fn every_erased_path_is_backed_up_or_said_why_not() {
    let under_state = |t: &str| BACKUP_STATE.iter().any(|d| Path::new(t).starts_with(d));
    for t in WIPE {
        assert!(
            under_state(t) != BACKUP_ELSEWHERE.contains(t),
            "{t}: inside the state backup or in BACKUP_ELSEWHERE (exactly one)"
        );
    }
    for t in BACKUP_EXCLUDE {
        assert!(under_state(t), "{t} is not inside the state backup");
    }
    for t in BACKUP_ELSEWHERE {
        assert!(WIPE.contains(t), "{t} is not erased by a reset");
    }
    // Kept entries inside the backup are excluded (the voice model, server.toml).
    for k in KEEP.iter().filter(|k| under_state(k)) {
        assert!(BACKUP_EXCLUDE.contains(k), "{k}");
    }
    // Device-bound identities never travel.
    for t in [
        "/var/lib/melissa-tailscale",
        "/var/lib/melissa-container/cluster",
        "/var/lib/melissa-container/etc-melissa/cluster-join.token",
    ] {
        assert!(!under_state(t) || BACKUP_EXCLUDE.contains(&t), "{t}");
    }
}

#[test]
fn a_file_nobody_listed_still_goes_but_kept_ones_stay() {
    let tree = Tree::new("sweep");
    let host = Fake::default();
    *host.mounts.borrow_mut() = vec![tree.root.clone()];
    let stray = at(&tree.root, "/var/lib/melissa/some-new-state.json");
    let stray_dir = at(&tree.root, "/var/lib/melissa/newthing/x");
    let stray_setting = at(
        &tree.root,
        "/var/lib/melissa-container/etc-melissa/new.toml",
    );
    std::fs::create_dir_all(&stray_dir).unwrap();
    std::fs::write(&stray, "{}").unwrap();
    std::fs::write(&stray_setting, "").unwrap();
    tree.request(false);
    assert_eq!(tree.run(&host).unwrap(), Outcome::Done);
    tree.assert_reset();
    assert!(!stray.exists() && !stray_dir.exists() && !stray_setting.exists());
    assert!(at(&tree.root, "/var/lib/melissa").is_dir());
}

#[test]
fn a_bad_list_is_refused() {
    // The same rules check_lists applies, on paths that must fail them.
    let bad = |t: &str| {
        let p = Path::new(t);
        !(p.is_absolute()
            && p.components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
            && p.components().count() >= 3
            && !KEEP.iter().any(|k| Path::new(k).starts_with(p)))
    };
    for t in [
        "/",
        "/var",
        "var/lib/melissa/x",
        "/var/lib/melissa/../..",
        "/var/lib/melissa-container",
        "/var/lib/melissa/./x/..",
    ] {
        assert!(bad(t), "{t}");
    }
    assert!(!bad("/var/lib/melissa/auth"));
}

#[test]
fn the_confirmation_must_match() {
    assert!(confirm_matches("melissa", CONFIRM_WORD));
    assert!(confirm_matches("  melissa \n", CONFIRM_WORD));
    for wrong in ["", "Melissa", "melissa!", "mel issa", "reset", "MELISSA"] {
        assert!(!confirm_matches(wrong, CONFIRM_WORD), "{wrong}");
    }
    assert!(!confirm_matches("", ""));
}

#[test]
fn mount_points_come_from_mountinfo() {
    let text = "22 1 0:21 / / rw,relatime - overlay overlay rw\n\
                40 22 0:40 / /run/k3s/containerd/io.containerd.grpc.v1.cri/sandboxes/abc/shm rw - tmpfs shm rw\n\
                41 22 0:41 / /var/lib/kubelet/pods/p1/volumes/a\\040b rw - tmpfs tmpfs rw\n\
                bad line\n";
    assert_eq!(
        parse_mountinfo(text),
        vec![
            PathBuf::from("/"),
            PathBuf::from("/run/k3s/containerd/io.containerd.grpc.v1.cri/sandboxes/abc/shm"),
            PathBuf::from("/var/lib/kubelet/pods/p1/volumes/a b"),
        ]
    );
    assert_eq!(unescape_octal("a\\134b\\011"), "a\\b\t");
    assert_eq!(unescape_octal("end\\04"), "end\\04");
}

// --- the marker ---------------------------------------------------------------------------------

#[test]
fn the_marker_round_trips_and_anything_else_still_means_reset() {
    let dir = scratch("marker");
    let path = dir.join(MARKER_NAME);
    assert_eq!(read_marker(&path).unwrap(), None);
    let m = Marker {
        requested_at: 1_700_000_000,
        step: Step::Wiping,
        by: "ui".into(),
        online: true,
    };
    write_marker(&path, &m).unwrap();
    assert_eq!(read_marker(&path).unwrap(), Some(m));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"step\":\"wiping\""), "{text}");
    // No temporary file left behind.
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);

    for junk in ["", "{", "{\"step\":\"later\"}", "not json"] {
        std::fs::write(&path, junk).unwrap();
        let m = read_marker(&path).unwrap().unwrap();
        assert_eq!(m.step, Step::Requested, "{junk}");
        assert!(!m.online);
    }
    // An old marker without the newer fields reads fine.
    std::fs::write(&path, r#"{"requested_at":5,"step":"stopping"}"#).unwrap();
    let m = read_marker(&path).unwrap().unwrap();
    assert_eq!(
        (m.requested_at, m.step, m.online),
        (5, Step::Stopping, false)
    );

    // A symlink is never followed (and still counts as a marker).
    let outside = dir.join("outside.json");
    std::fs::write(&outside, r#"{"requested_at":9,"step":"finishing"}"#).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&outside, &path).unwrap();
    assert_eq!(read_marker(&path).unwrap(), Some(Marker::unreadable()));
    // Writing replaces the link, not the file it points to.
    write_marker(&path, &Marker::new("host", false)).unwrap();
    assert!(!path.symlink_metadata().unwrap().file_type().is_symlink());
    assert!(std::fs::read_to_string(&outside)
        .unwrap()
        .contains("finishing"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn steps_only_go_forward() {
    assert!(Step::Requested < Step::OnlineDone);
    assert!(Step::OnlineDone < Step::Stopping);
    assert!(Step::Stopping < Step::Wiping);
    assert!(Step::Wiping < Step::Finishing);
    let dir = scratch("advance");
    let path = dir.join(MARKER_NAME);
    let mut m = Marker::new("ui", true);
    m.step = Step::Wiping;
    write_marker(&path, &m).unwrap();
    advance(&path, &mut m, Step::Stopping).unwrap();
    assert_eq!(read_marker(&path).unwrap().unwrap().step, Step::Wiping);
    advance(&path, &mut m, Step::Finishing).unwrap();
    assert_eq!(read_marker(&path).unwrap().unwrap().step, Step::Finishing);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_request_writes_the_marker_once_and_audits_it() {
    let dir = scratch("request");
    let audit = AuditLog::new(dir.join("audit.jsonl"));
    let r = Requester::new(dir.join("reset"), Some(audit));
    std::fs::create_dir_all(dir.join("reset")).unwrap();
    assert!(!r.pending());
    r.request("ui").unwrap();
    assert!(r.pending());
    let m = read_marker(&r.marker_path()).unwrap().unwrap();
    assert_eq!(
        (m.step, m.by.as_str(), m.online),
        (Step::Requested, "ui", true)
    );
    assert!(m.requested_at > 0);
    // Again: nothing new.
    r.request("ui").unwrap();
    let log = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap();
    assert_eq!(log.lines().count(), 1);
    assert!(log.contains("factory_reset.requested"));
    r.online_done().unwrap();
    assert_eq!(
        read_marker(&r.marker_path()).unwrap().unwrap().step,
        Step::OnlineDone
    );
    // Never backwards from a later step.
    let mut later = m.clone();
    later.step = Step::Wiping;
    write_marker(&r.marker_path(), &later).unwrap();
    r.online_done().unwrap();
    assert_eq!(
        read_marker(&r.marker_path()).unwrap().unwrap().step,
        Step::Wiping
    );
    std::fs::remove_dir_all(dir).unwrap();
}

// --- a whole reset on a temporary tree ----------------------------------------------------------

/// A fake system: units, a mount table, and switches that make a step fail or "crash".
#[derive(Default)]
struct Fake {
    calls: RefCell<Vec<String>>,
    mounts: RefCell<Vec<PathBuf>>,
    /// These stay mounted whatever unmount does.
    stuck: RefCell<Vec<PathBuf>>,
    server_active: Cell<bool>,
    fail_stop: Cell<bool>,
    crash_in_journal: Cell<bool>,
    /// Write `online_done` into this marker on the third sleep (the server finishing).
    server_marker: RefCell<Option<PathBuf>>,
    sleeps: Cell<u32>,
}

impl Fake {
    fn log(&self, s: &str) {
        self.calls.borrow_mut().push(s.to_string());
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl Host for Fake {
    fn unit_active(&self, unit: &str) -> bool {
        unit == SERVER_UNIT && self.server_active.get()
    }

    fn tailscale_logout(&self) -> Result<(), String> {
        self.log("tailscale logout");
        Ok(())
    }

    fn stop_units(&self, units: &[&str]) -> Result<(), String> {
        self.log(&format!("stop {}", units.join(" ")));
        if units == STOP_JOBS {
            return Err("no such unit".into());
        }
        if self.fail_stop.get() {
            return Err("systemctl failed".into());
        }
        self.server_active.set(false);
        Ok(())
    }

    fn own_cgroup(&self) -> Option<String> {
        Some("/system.slice/melissa-factory-reset.service".into())
    }

    fn kill_cgroup(&self, cgroup: &str) {
        self.log(&format!("kill leftovers {cgroup}"));
    }

    fn kill_shims(&self) {
        self.log("kill shims");
    }

    fn mounts(&self) -> Result<Vec<PathBuf>, String> {
        Ok(self.mounts.borrow().clone())
    }

    fn unmount(&self, path: &Path) -> Result<(), String> {
        self.log(&format!("unmount {}", path.display()));
        if self.stuck.borrow().iter().any(|s| s == path) {
            return Err("busy".into());
        }
        self.mounts.borrow_mut().retain(|m| m != path);
        Ok(())
    }

    fn journal_detach(&self) -> Result<(), String> {
        self.log("journal detach");
        if self.crash_in_journal.replace(false) {
            panic!("killed");
        }
        Ok(())
    }

    fn journal_attach(&self) {
        self.log("journal attach");
    }

    fn restore_layout(&self) -> Result<(), String> {
        self.log("tmpfiles");
        Ok(())
    }

    fn start_units(&self, units: &[&str]) -> Result<(), String> {
        self.log(&format!("start {}", units.join(" ")));
        Ok(())
    }

    fn sleep(&self, _d: Duration) {
        let n = self.sleeps.get() + 1;
        self.sleeps.set(n);
        if n == 3 {
            if let Some(p) = self.server_marker.borrow().as_ref() {
                Requester::new(p.parent().unwrap(), None)
                    .online_done()
                    .unwrap();
            }
        }
    }
}

/// A device's /var with every item in place, plus a few things outside the lists.
struct Tree {
    root: PathBuf,
    device_id: String,
}

impl Tree {
    fn new(name: &str) -> Self {
        let root = scratch(name);
        let w = |rel: &str, text: &str| {
            let p = at(&root, rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        // Wiped: one file in (or as) every entry.
        for t in WIPE {
            if t.ends_with(".json") || t.ends_with(".jsonl") || t.contains(".toml") {
                w(t, "old");
            } else {
                w(&format!("{t}/inside/data"), "old");
            }
        }
        for t in EMPTY {
            w(&format!("{t}/machine/system.journal"), "old");
        }
        // A real login store with an owner, and a device id.
        let state = at(&root, "/var/lib/melissa");
        let auth = melissa_auth::Auth::open(&state.join("auth"))
            .unwrap()
            .with_cost(melissa_auth::Cost::Test);
        let (code, _) = auth.setup_code().unwrap().unwrap();
        auth.claim(&code, "owner", "owner-password", "test", "Test")
            .unwrap();
        assert!(auth.has_owner());
        let _ = std::fs::remove_file(state.join("device.json"));
        let device =
            melissa_agent::device::DeviceIdentity::load_or_create(&state.join("device.json"))
                .unwrap();
        // Kept.
        w("/var/lib/melissa/models/ggml-base.bin", "model");
        w(
            "/var/lib/melissa-container/etc-melissa/server.toml",
            "listen = 1",
        );
        w("/var/lib/melissa-container/rancher-node/password", "node");
        w("/var/lib/melissa-container/install.env", "BIND=0.0.0.0");
        w("/var/lib/rancher/.melissa-relabeled", "");
        w("/etc/hostname", "melissa");
        std::fs::create_dir_all(at(&root, MARKER_DIR)).unwrap();
        // The host helper's request directory (container).
        std::fs::create_dir_all(at(&root, UPDATE_REQUEST_DIR)).unwrap();
        Self {
            root,
            device_id: device.id,
        }
    }

    fn marker(&self) -> PathBuf {
        at(&self.root, MARKER_DIR).join(MARKER_NAME)
    }

    fn request(&self, online: bool) {
        write_marker(&self.marker(), &Marker::new("ui", online)).unwrap();
    }

    fn run(&self, host: &Fake) -> Result<Outcome, ResetError> {
        run_with(&self.root, host, Duration::from_millis(50), &mut |_| {})
    }

    fn exists(&self, rel: &str) -> bool {
        at(&self.root, rel).symlink_metadata().is_ok()
    }

    /// Everything wiped is gone, everything kept is there, the box is new.
    fn assert_reset(&self) {
        // (The audit log is back with one new line, checked below.)
        for t in WIPE.iter().filter(|t| **t != AUDIT_FILE) {
            assert!(!self.exists(t), "{t} still there");
        }
        for t in EMPTY {
            let p = at(&self.root, t);
            assert!(p.is_dir(), "{t} gone");
            assert_eq!(std::fs::read_dir(&p).unwrap().count(), 0, "{t} not empty");
        }
        for k in [
            "/var/lib/melissa/models/ggml-base.bin",
            "/var/lib/melissa-container/etc-melissa/server.toml",
            "/var/lib/melissa-container/rancher-node/password",
            "/var/lib/melissa-container/install.env",
            "/var/lib/rancher/.melissa-relabeled",
            "/etc/hostname",
            MARKER_DIR,
        ] {
            assert!(self.exists(k), "{k} was removed");
        }
        assert!(!self.marker().exists(), "the marker stays");
        // The host is asked to delete its datastore snapshots (old data, old Secrets key).
        let forget = at(&self.root, UPDATE_REQUEST_DIR).join(FORGET_SNAPSHOTS);
        assert_eq!(
            std::fs::read(&forget).unwrap(),
            b"",
            "no forget-snapshots request"
        );
        let state = at(&self.root, "/var/lib/melissa");
        // Exactly one audit line, the reset.
        let audit = std::fs::read_to_string(state.join("audit.jsonl")).unwrap();
        assert_eq!(audit.lines().count(), 1, "{audit}");
        let line: serde_json::Value = serde_json::from_str(audit.trim()).unwrap();
        assert_eq!(line["action"], "factory_reset");
        // Unclaimed, and a new device id at the next start.
        let auth = melissa_auth::Auth::open(&state.join("auth")).unwrap();
        assert!(!auth.has_owner());
        let device =
            melissa_agent::device::DeviceIdentity::load_or_create(&state.join("device.json"))
                .unwrap();
        assert_ne!(device.id, self.device_id);
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn nothing_happens_without_a_marker() {
    let tree = Tree::new("none");
    let host = Fake::default();
    assert_eq!(tree.run(&host).unwrap(), Outcome::NothingToDo);
    assert!(host.calls().is_empty());
    assert!(tree.exists("/var/lib/melissa/auth"));
}

#[test]
fn a_reset_removes_the_list_keeps_the_rest_and_starts_unclaimed() {
    let tree = Tree::new("full");
    // The cluster left mounts inside what goes (pod volumes, container shm).
    let host = Fake::default();
    let pod = at(
        &tree.root,
        "/var/lib/kubelet/pods/p1/volumes/kubernetes.io~empty-dir/data",
    );
    let shm = at(&tree.root, "/run/k3s/containerd/sandboxes/abc/shm");
    for m in [&pod, &shm] {
        std::fs::create_dir_all(m).unwrap();
    }
    *host.mounts.borrow_mut() = vec![tree.root.clone(), pod.clone(), shm.clone()];
    tree.request(false);

    assert_eq!(tree.run(&host).unwrap(), Outcome::Done);
    tree.assert_reset();
    let calls = host.calls();
    let pos = |s: &str| {
        calls
            .iter()
            .position(|c| c.starts_with(s))
            .unwrap_or_else(|| panic!("no {s} in {calls:?}"))
    };
    // Logged out and stopped before anything goes; unmounted before removing; started last.
    assert!(pos("tailscale logout") < pos("stop"));
    assert!(pos("stop") < pos("kill leftovers"));
    assert!(pos("kill leftovers") < pos("unmount"));
    // The reset runs in its own unit: k3s.service's cgroup (the shims) goes too.
    assert!(
        calls.contains(&format!("kill leftovers {K3S_CGROUP}")),
        "{calls:?}"
    );
    assert!(pos("kill leftovers") < pos("kill shims"));
    assert!(pos("journal detach") < pos("journal attach"));
    assert!(pos("tmpfiles") < pos("start"));
    assert_eq!(calls.last().unwrap(), &format!("start {}", START.join(" ")));
    assert!(calls[pos("stop")].contains(SERVER_UNIT) && calls[pos("stop")].contains("k3s"));
    // The tree's own mount is untouched.
    assert_eq!(*host.mounts.borrow(), vec![tree.root.clone()]);

    // Again: nothing more to do.
    assert_eq!(tree.run(&host).unwrap(), Outcome::NothingToDo);
}

#[test]
fn the_server_gets_time_for_its_part_but_not_forever() {
    let tree = Tree::new("online");
    let host = Fake::default();
    host.server_active.set(true);
    *host.server_marker.borrow_mut() = Some(tree.marker());
    tree.request(true);
    assert_eq!(tree.run(&host).unwrap(), Outcome::Done);
    assert_eq!(host.sleeps.get(), 3, "waited until the server was done");
    tree.assert_reset();

    // A server that never says it's done: the wait ends and the reset goes on.
    let tree = Tree::new("online-slow");
    let host = Fake::default();
    host.server_active.set(true);
    tree.request(true);
    assert_eq!(tree.run(&host).unwrap(), Outcome::Done);
    tree.assert_reset();

    // Asked by root directly: no wait at all.
    let tree = Tree::new("offline");
    let host = Fake::default();
    host.server_active.set(true);
    tree.request(false);
    assert_eq!(tree.run(&host).unwrap(), Outcome::Done);
    assert_eq!(host.sleeps.get(), 0);
}

#[test]
fn a_run_killed_at_any_step_finishes_on_the_next() {
    // Stopping fails: nothing removed, the marker records how far it got.
    let tree = Tree::new("kill-stop");
    let host = Fake::default();
    host.fail_stop.set(true);
    tree.request(false);
    assert!(tree.run(&host).is_err());
    assert!(tree.exists("/var/lib/melissa/auth"));
    assert_eq!(
        read_marker(&tree.marker()).unwrap().unwrap().step,
        Step::Stopping
    );
    host.fail_stop.set(false);
    assert_eq!(tree.run(&host).unwrap(), Outcome::Done);
    tree.assert_reset();

    // Killed in the middle of the wipe (a panic stands in for the kill).
    let tree = Tree::new("kill-wipe");
    let host = Fake::default();
    host.crash_in_journal.set(true);
    tree.request(false);
    let killed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tree.run(&host)));
    assert!(killed.is_err());
    assert_eq!(
        read_marker(&tree.marker()).unwrap().unwrap().step,
        Step::Wiping
    );
    assert_eq!(tree.run(&host).unwrap(), Outcome::Done);
    tree.assert_reset();

    // A mount that won't go: that target stays (nothing under it is touched), the rest goes,
    // and the next run finishes once it is free.
    let tree = Tree::new("kill-mount");
    let host = Fake::default();
    let vol = at(&tree.root, "/var/lib/kubelet/pods/p1/volumes/host");
    std::fs::create_dir_all(&vol).unwrap();
    std::fs::write(vol.join("someone-elses-data"), "keep").unwrap();
    *host.mounts.borrow_mut() = vec![vol.clone()];
    *host.stuck.borrow_mut() = vec![vol.clone()];
    tree.request(false);
    let err = tree.run(&host).unwrap_err();
    assert!(err.to_string().contains("still mounted"), "{err}");
    assert!(vol.join("someone-elses-data").exists());
    assert!(!tree.exists("/var/lib/melissa/auth"));
    assert!(tree.marker().exists());
    host.stuck.borrow_mut().clear();
    assert_eq!(tree.run(&host).unwrap(), Outcome::Done);
    tree.assert_reset();

    // Killed after the marker went but before the units started: nothing left to do; the
    // boot starts them anyway.
    let tree = Tree::new("kill-end");
    let host = Fake::default();
    tree.request(false);
    tree.run(&host).unwrap();
    std::fs::write(at(&tree.root, "/var/lib/melissa/audit.jsonl"), "").unwrap();
    assert_eq!(tree.run(&host).unwrap(), Outcome::NothingToDo);
}

#[test]
fn symlinks_are_removed_never_followed() {
    let tree = Tree::new("links");
    let outside = scratch("links-outside");
    std::fs::write(outside.join("precious"), "keep").unwrap();
    // A wiped entry that is a symlink to somewhere else.
    let home = at(&tree.root, "/var/lib/melissa/home");
    std::fs::remove_dir_all(&home).unwrap();
    std::os::unix::fs::symlink(&outside, &home).unwrap();
    // Inside a wiped directory, a link out.
    std::os::unix::fs::symlink(&outside, at(&tree.root, "/var/lib/melissa/work/out")).unwrap();
    let host = Fake::default();
    tree.request(false);
    assert_eq!(tree.run(&host).unwrap(), Outcome::Done);
    tree.assert_reset();
    assert_eq!(
        std::fs::read_to_string(outside.join("precious")).unwrap(),
        "keep"
    );
    std::fs::remove_dir_all(outside).unwrap();
}

#[test]
fn a_symlinked_directory_on_the_way_is_refused() {
    let tree = Tree::new("parent-link");
    let outside = scratch("parent-outside");
    std::fs::write(outside.join("providers.toml"), "someone else's").unwrap();
    let etc = at(&tree.root, "/var/lib/melissa-container/etc-melissa");
    std::fs::remove_dir_all(&etc).unwrap();
    std::os::unix::fs::symlink(&outside, &etc).unwrap();
    let host = Fake::default();
    tree.request(false);
    let err = tree.run(&host).unwrap_err();
    assert!(err.to_string().contains("providers.toml"), "{err}");
    assert!(outside.join("providers.toml").exists());
    assert!(tree.marker().exists(), "retried later");
    std::fs::remove_dir_all(outside).unwrap();
}

#[test]
fn one_run_at_a_time() {
    let tree = Tree::new("lock");
    tree.request(false);
    let held = File::open(at(&tree.root, MARKER_DIR)).unwrap();
    held.lock().unwrap();
    let host = Fake::default();
    assert!(matches!(tree.run(&host), Err(ResetError::Busy)));
    assert!(host.calls().is_empty());
    held.unlock().unwrap();
    assert_eq!(tree.run(&host).unwrap(), Outcome::Done);
}
