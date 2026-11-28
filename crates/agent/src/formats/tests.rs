//! A state directory from before formats existed (`tests/fixtures/state-v1`, the shapes the
//! images up to 2026-11-28 wrote) loads with this build, and files a newer Melissa wrote are
//! never changed by this one (a rollback).

use std::path::{Path, PathBuf};

use jiff::tz::TimeZone;

use crate::{
    device::DeviceIdentity,
    files::{FileBoxes, Keep},
    lead::{
        backends::{load_choice, ModelCache},
        registry::AgentRegistry,
        schedules::Schedules,
        usage::Usage,
    },
    tools::{ActionKey, Grant, Permissions, Principal},
};

const DEVICE: &str = "d-0a1b2c3d";

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// A copy of the old state directory.
fn old_state(name: &str) -> PathBuf {
    let dir = crate::test_support::fresh_path(name);
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/state-v1"),
        &dir,
    );
    dir
}

fn restart_key() -> ActionKey {
    ActionKey {
        principal: Principal::Lead,
        tool: "restart_app".into(),
        target: Some("nextcloud".into()),
    }
}

#[test]
fn a_state_directory_from_before_formats_loads() {
    let state = old_state("formats-old");
    let device = DeviceIdentity::load_or_create(&state.join("device.json")).unwrap();
    assert_eq!(device.id, DEVICE);

    let registry = AgentRegistry::open(&state.join("agents"), DEVICE).unwrap();
    let names: Vec<String> = registry.list().into_iter().map(|a| a.name).collect();
    assert_eq!(names, ["Ada", "Bo"]);
    assert_eq!(registry.lead_name(), "Mel");
    let bo = registry.get("d-0a1b2c3d.a2").unwrap();
    assert!(bo.stopped);
    assert_eq!(bo.former_names, ["Bee"]);
    // Opening gave the agents their icons and saved the registry in the current format.
    let text = std::fs::read_to_string(state.join("agents/registry.json")).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["format"], 1, "{text}");

    let schedules = Schedules::open(state.join("schedules.json"), TimeZone::UTC).unwrap();
    assert!(!schedules.read_only());
    assert_eq!(schedules.approval_wait_minutes(), 20);
    let nightly = &schedules.list()[0];
    assert_eq!((nightly.id, nightly.name.as_str()), (1, "Nightly check"));
    assert_eq!(nightly.last_run.as_ref().map(|r| r.turn), Some(Some(4)));

    let permissions = Permissions::open(state.join("permissions.json")).unwrap();
    assert_eq!(
        permissions.grant(Some(&restart_key())),
        Some(Grant::Rule(1))
    );

    let files = FileBoxes::open(state.join("files"));
    assert_eq!(files.retention().default, Keep::Days(7));
    assert_eq!(files.override_for(&Principal::Lead), Some(Keep::Forever));

    let choice = load_choice(&state.join("lead/backend.json")).unwrap();
    assert_eq!(
        (choice.backend.as_str(), choice.model.as_deref()),
        ("claude-session", Some("opus"))
    );
    let models = ModelCache::open(state.join("lead/models.json"));
    assert_eq!(models.get("claude-session").map(|m| m.len()), Some(2));
    assert_eq!(models.get("grok-acp").map(|m| m.len()), Some(1));
    let usage = Usage::load(&state.join("lead")).unwrap();
    assert_eq!((usage.turns, usage.approvals), (12, 3));

    let transcript =
        crate::lead::transcript::Transcript::new(state.join("lead/transcript.jsonl"), 1 << 20);
    let chat = transcript.load_chat(10);
    assert_eq!(chat.len(), 7, "{chat:?}");
    std::fs::remove_dir_all(state).unwrap();
}

/// Every store's file, rewritten as a newer Melissa might: format 99 and a field this version
/// does not know.
fn make_newer(path: &Path) -> String {
    let mut doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let obj = doc.as_object_mut().unwrap();
    obj.insert("format".into(), 99.into());
    obj.insert("from_the_future".into(), true.into());
    let text = serde_json::to_string_pretty(&doc).unwrap();
    std::fs::write(path, &text).unwrap();
    text
}

#[test]
fn files_from_a_newer_melissa_are_never_changed() {
    let state = old_state("formats-newer");
    let registry_path = state.join("agents/registry.json");
    let schedules_path = state.join("schedules.json");
    let permissions_path = state.join("permissions.json");
    let retention_path = state.join("files/retention.json");
    let backend_path = state.join("lead/backend.json");
    let usage_path = state.join("lead/usage.json");
    let newer: Vec<(PathBuf, String)> = [
        &registry_path,
        &schedules_path,
        &permissions_path,
        &retention_path,
        &backend_path,
        &usage_path,
    ]
    .into_iter()
    .map(|p| (p.clone(), make_newer(p)))
    .collect();

    // The registry is read (agents shown) but nothing is saved: not the icons it would add,
    // not a rename.
    let registry = AgentRegistry::open(&state.join("agents"), DEVICE).unwrap();
    assert_eq!(registry.list().len(), 2);
    let err = registry.rename("Ada", "Ann").unwrap_err();
    assert!(
        err.to_string().contains("newer version of Melissa"),
        "{err}"
    );

    // Schedules are shown, do not run, and cannot be changed.
    let schedules = Schedules::open(&schedules_path, TimeZone::UTC).unwrap();
    assert!(schedules.read_only());
    assert_eq!(schedules.list().len(), 1);
    assert!(schedules
        .set_approval_wait(30, &crate::lead::schedules::Creator::Owner)
        .is_err());

    // Standing permissions from a newer Melissa are not guessed at: the caller starts with
    // none (and saves nothing over them).
    assert!(Permissions::open(&permissions_path).is_err());

    // Retention: nothing is removed meanwhile, and the setting is not saved over.
    let files = FileBoxes::open(state.join("files"));
    assert_eq!(files.keep_for(&Principal::Lead), Keep::Forever);
    assert!(files.set_default(Keep::Days(1), "ui").is_err());

    // The backend choice and the counters are read; saving them is refused.
    assert!(load_choice(&backend_path).is_some());
    assert!(crate::lead::backends::save_choice(
        &backend_path,
        &load_choice(&backend_path).unwrap()
    )
    .is_err());
    let usage = Usage::load(&state.join("lead")).unwrap();
    usage.save(&state.join("lead"));

    for (path, text) in newer {
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            text,
            "{} was changed",
            path.display()
        );
    }
    std::fs::remove_dir_all(state).unwrap();
}
