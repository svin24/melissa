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
        conversation::{load_session, save_session, saved_taint, SavedSession},
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

    let session = load_session(&state.join("lead/session.json")).unwrap();
    assert_eq!(
        (session.backend.as_str(), session.session_id.as_str()),
        ("claude-session", "00000000-0000-4000-8000-000000000001")
    );
    assert!(session.tainted);
    assert!(saved_taint(&state.join("lead")));

    let ids = crate::sandbox::users::Range {
        start: 1000,
        count: 6,
    };
    let table =
        crate::sandbox::users::load_table(&state.join("agent-users.json"), ids, ids).unwrap();
    assert_eq!(table.slots.get("d-0a1b2c3d.a1"), Some(&1));
    assert_eq!(table.slots.get("lead"), Some(&0));

    let hub = crate::provider_usage::UsageHub::open(&state.join("lead"));
    let claude = hub.counted("claude").unwrap();
    assert_eq!(claude.since, 1_764_200_000);
    assert_eq!(claude.models["opus"].tokens.input, 1200);
    assert!((claude.windows["five_hour"].used_pct - 42.0).abs() < 1e-9);

    let threads = crate::lead::threads::Threads::new(state.join("threads"));
    let thread = threads.read("lead", "d-0a1b2c3d.a1").unwrap();
    assert_eq!(thread.len(), 2);
    assert_eq!(thread[1].text, "All backups ran.");
    assert!(thread[1].verified);
    // New messages go on after the old ones, each on its own line, in the current format.
    threads.record(&crate::lead::threads::ThreadEntry::new(
        "lead",
        "d-0a1b2c3d.a1",
        "message",
        "data",
        "Thanks.",
    ));
    let thread = threads.read("lead", "d-0a1b2c3d.a1").unwrap();
    assert_eq!(thread.len(), 3);
    assert_eq!(thread[2].text, "Thanks.");

    // The audit log is only appended to: the old lines stay exactly as they were.
    let audit_path = state.join("audit.jsonl");
    let old_audit = std::fs::read_to_string(&audit_path).unwrap();
    crate::audit::AuditLog::new(&audit_path)
        .append(&crate::audit::AuditRecord::now(
            "task.finish",
            3,
            "claude-session",
            "done",
        ))
        .unwrap();
    let audit = std::fs::read_to_string(&audit_path).unwrap();
    let new_line = audit.strip_prefix(&old_audit).unwrap();
    let record: serde_json::Value = serde_json::from_str(new_line.trim_end()).unwrap();
    assert_eq!(record["format"], 1);
    assert_eq!(record["action"], "task.finish");
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
    let models_path = state.join("lead/models.json");
    let session_path = state.join("lead/session.json");
    let provider_usage_path = state.join("lead/provider-usage.json");
    let device_path = state.join("device.json");
    // A model list to save, from the old file (before it becomes a newer one).
    let options = ModelCache::open(models_path.clone())
        .get("claude-session")
        .unwrap();
    let newer: Vec<(PathBuf, String)> = [
        &registry_path,
        &schedules_path,
        &permissions_path,
        &retention_path,
        &backend_path,
        &usage_path,
        &models_path,
        &session_path,
        &provider_usage_path,
        &device_path,
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

    // The model cache reads as empty and a fresh list is not saved over the file.
    let models = ModelCache::open(models_path.clone());
    assert!(models.get("claude-session").is_none());
    models.put(
        "claude-session",
        &crate::session::ModelList {
            current: None,
            options,
        },
    );

    // The saved session is read (resume works), counts as tainted, and is not saved over.
    let session = load_session(&session_path).unwrap();
    assert!(saved_taint(&state.join("lead")));
    save_session(
        &session_path,
        &SavedSession {
            tainted: false,
            ..session
        },
    );

    // Provider usage starts empty here and the newer counters are not overwritten.
    let hub = crate::provider_usage::UsageHub::open(&state.join("lead"));
    hub.observe(
        "claude",
        "p",
        crate::provider_usage::Observation::ProcessTotals(vec![
            crate::provider_usage::ModelTokens {
                model: "opus".into(),
                tokens: crate::provider_usage::TokenCount {
                    input: 5,
                    ..Default::default()
                },
                cost_micros: None,
                currency: None,
            },
        ]),
        1,
    );

    // The device keeps its id; its file is not rewritten.
    let device = DeviceIdentity::load_or_create(&device_path).unwrap();
    assert_eq!(device.id, DEVICE);

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
