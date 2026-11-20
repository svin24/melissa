use std::{
    io::Read,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use super::*;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
const ADA: &str = "d-0a1b2c3d.a1";
const BO: &str = "d-0a1b2c3d.a2";

fn ada() -> Principal {
    Principal::Agent(ADA.into())
}

fn bo() -> Principal {
    Principal::Agent(BO.into())
}

struct Fixture {
    root: PathBuf,
    boxes: FileBoxes,
    now: Arc<AtomicU64>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn fixture(tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!(
        "melissa-files-{tag}-{}-{}",
        std::process::id(),
        random_id().unwrap()
    ));
    let now = Arc::new(AtomicU64::new(1_000_000));
    let clock = now.clone();
    let boxes = FileBoxes::open(root.join("files"))
        .with_clock(Arc::new(move || clock.load(Ordering::SeqCst)));
    Fixture { root, boxes, now }
}

fn up(name: &str, bytes: &[u8]) -> Upload {
    Upload {
        name: name.into(),
        bytes: bytes.to_vec(),
    }
}

fn read_all(boxes: &FileBoxes, owner: &Principal, id: &str) -> Vec<u8> {
    let (_, mut f) = boxes.open_file(owner, id).unwrap();
    let mut v = Vec::new();
    f.read_to_end(&mut v).unwrap();
    v
}

#[test]
fn every_agent_has_its_own_box() {
    let fx = fixture("own");
    let b = &fx.boxes;
    let a = b
        .store_uploads(&ada(), vec![up("notes.txt", b"ada's")])
        .unwrap();
    let id = attachment_id(&a[0].file).to_string();
    assert_eq!(
        b.dir(&ada()).unwrap(),
        fx.root.join("files/boxes").join(ADA)
    );
    assert_eq!(
        b.dir(&Principal::Lead).unwrap(),
        fx.root.join("files/boxes/lead")
    );
    assert_ne!(b.dir(&ada()), b.dir(&bo()));
    // Ada's file is in Ada's box only.
    assert_eq!(read_all(b, &ada(), &id), b"ada's");
    assert!(b.open_file(&bo(), &id).is_err());
    assert!(b.open_file(&Principal::Lead, &id).is_err());
    assert!(b.list(&bo()).is_empty());
    // Ids that are not agent ids never make a path.
    for bad in ["../lead", "d-0a1b2c3d.a1/..", "lead", "", "d-0a1b2c3d.a01"] {
        let p = Principal::Agent(bad.into());
        assert!(b.dir(&p).is_none(), "{bad}");
        assert!(b.store_uploads(&p, vec![up("x", b"x")]).is_err(), "{bad}");
    }
    // The index lives outside the box.
    assert!(fx
        .root
        .join("files/index")
        .join(format!("{ADA}.json"))
        .is_file());
    assert_eq!(
        fs::read_dir(b.dir(&ada()).unwrap()).unwrap().count(),
        1,
        "only the file itself is in the box"
    );
}

#[test]
fn owner_uploads_get_clean_unique_names() {
    let fx = fixture("names");
    let b = &fx.boxes;
    let first = b
        .store_uploads(
            &ada(),
            vec![
                up("../../report final.pdf", b"one"),
                up("report_final.pdf", b"two"),
            ],
        )
        .unwrap();
    assert_eq!(first[0].name, "report_final.pdf");
    assert_eq!(first[1].name, "report_final-2.pdf");
    let list = b.list(&ada());
    assert_eq!(list.len(), 2);
    assert!(list
        .iter()
        .all(|f| f.by == AddedBy::Owner && f.added == 1_000_000));
    // Persisted: a new store over the same root sees them.
    let again = FileBoxes::open(fx.root.join("files"));
    assert_eq!(again.list(&ada()).len(), 2);
}

#[test]
fn files_the_agent_writes_show_up_and_symlinks_do_not() {
    let fx = fixture("agent");
    let b = &fx.boxes;
    let dir = b.ensure_dir(&ada()).unwrap();
    fs::write(dir.join("summary.md"), "# done\n").unwrap();
    fs::write(dir.join(".hidden"), "x").unwrap();
    fs::create_dir(dir.join("sub")).unwrap();
    fs::write(dir.join("sub/inner.txt"), "x").unwrap();
    let secret = fx.root.join("secret.txt");
    fs::write(&secret, "do not serve").unwrap();
    std::os::unix::fs::symlink(&secret, dir.join("link.txt")).unwrap();
    let list = b.list(&ada());
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(list[0].name, "summary.md");
    assert_eq!(list[0].by, AddedBy::Agent);
    assert_eq!(list[0].mime, "text/plain");
    assert_eq!(read_all(b, &ada(), &list[0].id), b"# done\n");

    // A file rewritten by the agent gets its new size; a removed one is forgotten.
    fs::write(dir.join("summary.md"), "# done, longer\n").unwrap();
    // Same second: the size tells.
    assert!(b.sync(&ada()));
    assert_eq!(b.indexed(&ada())[0].size, 15);
    fs::remove_file(dir.join("summary.md")).unwrap();
    assert!(b.sync(&ada()));
    assert!(b.list(&ada()).is_empty());
}

#[test]
fn a_file_swapped_for_a_symlink_is_not_served() {
    let fx = fixture("swap");
    let b = &fx.boxes;
    let a = b.store_uploads(&ada(), vec![up("a.txt", b"mine")]).unwrap();
    let id = attachment_id(&a[0].file).to_string();
    let dir = b.dir(&ada()).unwrap();
    let secret = fx.root.join("secret.txt");
    fs::write(&secret, "do not serve").unwrap();
    fs::remove_file(dir.join("a.txt")).unwrap();
    std::os::unix::fs::symlink(&secret, dir.join("a.txt")).unwrap();
    // The index still names it, but opening refuses the symlink.
    assert!(b.get(&ada(), &id).is_some());
    assert!(b.open_file(&ada(), &id).is_err());
    // Deleting removes the link, not its target.
    b.delete(&ada(), &id, "ui").unwrap();
    assert!(secret.exists());
    assert!(fs::symlink_metadata(dir.join("a.txt")).is_err());
}

#[test]
fn the_tool_saves_reads_and_replaces() {
    let fx = fixture("tool");
    let b = &fx.boxes;
    let f = b.save_named(&bo(), "../out/report.txt", b"hello").unwrap();
    assert_eq!(f.name, "report.txt");
    assert_eq!(f.by, AddedBy::Agent);
    let (_, bytes, cut) = b.read_named(&bo(), "report.txt", 3).unwrap();
    assert_eq!((bytes.as_slice(), cut), (&b"hel"[..], true));
    fx.now.store(2_000_000, Ordering::SeqCst);
    b.save_named(&bo(), "report.txt", b"again").unwrap();
    let list = b.list(&bo());
    assert_eq!(list.len(), 1);
    assert_eq!((list[0].size, list[0].added), (5, 2_000_000));
    assert!(matches!(
        b.read_named(&bo(), "missing.txt", 10),
        Err(FileBoxError::NotFound)
    ));
}

#[test]
fn delete_removes_and_audits() {
    let fx = fixture("delete");
    let log = AuditLog::new(fx.root.join("audit.jsonl"));
    let b = fx.boxes.clone().with_audit(Some(log));
    let a = b
        .store_uploads(&Principal::Lead, vec![up("x.png", PNG)])
        .unwrap();
    let id = attachment_id(&a[0].file).to_string();
    let gone = b.delete(&Principal::Lead, &id, "ui").unwrap();
    assert_eq!(gone.name, "x.png");
    assert!(b.list(&Principal::Lead).is_empty());
    assert!(matches!(
        b.delete(&Principal::Lead, &id, "ui"),
        Err(FileBoxError::NotFound)
    ));
    let audit = fs::read_to_string(fx.root.join("audit.jsonl")).unwrap();
    assert!(
        audit.contains("files.deleted") && audit.contains("x.png"),
        "{audit}"
    );
}

#[test]
fn prune_follows_the_default_and_overrides() {
    let fx = fixture("prune");
    let log = AuditLog::new(fx.root.join("audit.jsonl"));
    let b = fx.boxes.clone().with_audit(Some(log));
    let changed = Arc::new(Mutex::new(Vec::new()));
    let seen = changed.clone();
    b.on_change(Arc::new(move |p: &Principal| lock(&seen).push(p.clone())));
    for owner in [Principal::Lead, ada(), bo()] {
        b.store_uploads(&owner, vec![up("old.txt", b"old")])
            .unwrap();
    }
    // Bo keeps files for 1 day, the lead agent forever, Ada the default (5 days).
    b.set_override(&bo(), Some(Keep::Days(1)), "ui").unwrap();
    b.set_override(&Principal::Lead, Some(Keep::Forever), "ui")
        .unwrap();
    assert_eq!(b.keep_for(&ada()), Keep::Days(DEFAULT_KEEP_DAYS));
    assert_eq!(b.keep_for(&bo()), Keep::Days(1));

    // A day minus a second: nothing yet.
    fx.now.store(1_000_000 + DAY - 1, Ordering::SeqCst);
    assert!(b.prune().is_empty());
    // A day: Bo's goes.
    fx.now.store(1_000_000 + DAY, Ordering::SeqCst);
    let removed = b.prune();
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].0, bo());
    assert!(!b.dir(&bo()).unwrap().join("old.txt").exists());
    // Five days: Ada's goes; the lead agent's stays forever.
    fx.now.store(1_000_000 + 5 * DAY, Ordering::SeqCst);
    let removed = b.prune();
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].0, ada());
    fx.now.store(1_000_000 + 5000 * DAY, Ordering::SeqCst);
    assert!(b.prune().is_empty());
    assert_eq!(b.list(&Principal::Lead).len(), 1);
    // A new default applies to boxes without an override.
    b.set_override(&Principal::Lead, None, "ui").unwrap();
    b.set_default(Keep::Days(2), "ui").unwrap();
    assert_eq!(b.prune().len(), 1);

    let audit = fs::read_to_string(fx.root.join("audit.jsonl")).unwrap();
    assert_eq!(audit.matches("files.pruned").count(), 3, "{audit}");
    assert!(audit.contains("files.retention"));
    assert!(lock(&changed).contains(&bo()));
    // Settings survive a restart.
    let again = FileBoxes::open(fx.root.join("files"));
    assert_eq!(again.retention().default, Keep::Days(2));
    assert_eq!(again.override_for(&bo()), Some(Keep::Days(1)));
}

#[test]
fn retention_values_are_checked() {
    assert_eq!(Keep::parse("5").unwrap(), Keep::Days(5));
    assert_eq!(Keep::parse(" Forever ").unwrap(), Keep::Forever);
    for bad in ["0", "-1", "3651", "five", ""] {
        assert!(Keep::parse(bad).is_err(), "{bad}");
    }
    assert_eq!(Keep::Days(1).words(), "1 day");
    assert_eq!(Keep::Days(7).value(), "7");
    assert_eq!(Keep::Forever.value(), "forever");
    assert_eq!(Keep::default(), Keep::Days(5));
    let fx = fixture("bounds");
    assert!(fx.boxes.set_default(Keep::Days(0), "ui").is_err());
    assert!(fx
        .boxes
        .set_override(&ada(), Some(Keep::Days(9999)), "ui")
        .is_err());
}

#[test]
fn old_attachments_move_into_the_boxes_once() {
    let fx = fixture("migrate");
    let old = fx.root.join("lead/attachments");
    fs::create_dir_all(&old).unwrap();
    let ada_file = "0123456789abcdef0123456789abcdef.png";
    let lead_file = "fedcba9876543210fedcba9876543210.txt";
    let stray = "00000000000000000000000000000000.bin";
    fs::write(old.join(ada_file), PNG).unwrap();
    fs::write(old.join(lead_file), b"notes").unwrap();
    fs::write(old.join(stray), b"\0\x01").unwrap();
    fs::write(old.join("not-ours.txt"), b"x").unwrap();
    let whose: HashMap<String, (Principal, String)> = [
        (ada_file.to_string(), (ada(), "shot.png".to_string())),
        (
            lead_file.to_string(),
            (Principal::Lead, "notes.txt".to_string()),
        ),
    ]
    .into();
    let b = &fx.boxes;
    assert_eq!(b.migrate_attachments(&old, &whose).unwrap(), 3);
    // Same ids, so earlier messages find them; names from the chats.
    let a = b.get(&ada(), "0123456789abcdef0123456789abcdef").unwrap();
    assert_eq!((a.name.as_str(), a.by), ("shot.png", AddedBy::Owner));
    assert_eq!(a.added, 1_000_000, "counts as added now");
    let l = b
        .get(&Principal::Lead, "fedcba9876543210fedcba9876543210")
        .unwrap();
    assert_eq!(l.name, "notes.txt");
    assert_eq!(
        b.get(&Principal::Lead, "00000000000000000000000000000000")
            .unwrap()
            .name,
        "file.bin"
    );
    // Originals kept, under a new name; a second run finds nothing to do.
    assert!(!old.exists());
    assert!(fx
        .root
        .join("lead/attachments.migrated")
        .join(ada_file)
        .exists());
    assert_eq!(b.migrate_attachments(&old, &whose).unwrap(), 0);
}

#[test]
fn uploads_stop_at_the_box_cap() {
    let fx = fixture("cap");
    let b = &fx.boxes;
    // Pretend the box is nearly full.
    {
        let mut boxes = lock(&b.inner.boxes);
        b.index(&mut boxes, ADA).push(BoxFile {
            id: "0".repeat(32),
            name: "huge.bin".into(),
            mime: "application/octet-stream".into(),
            size: MAX_BOX_BYTES - 2,
            added: 0,
            by: AddedBy::Agent,
            modified: 0,
        });
    }
    assert_eq!(
        b.store_uploads(&ada(), vec![up("x.txt", b"abc")]),
        Err(AttachmentError::BoxFull)
    );
    assert!(b.save_named(&ada(), "y.txt", b"abc").is_err());
}

#[tokio::test]
async fn the_tool_only_reaches_the_callers_own_box() {
    use crate::tools::{Scope, TaskContext, Tool};
    use serde_json::json;
    let fx = fixture("mytool");
    let tool = MyFilesTool::new(fx.boxes.clone());
    let mut as_ada = TaskContext {
        scope: Scope::Agent { id: ADA.into() },
        ..TaskContext::default()
    };
    let saved = tool
        .call_in(
            1,
            &mut as_ada,
            json!({"action": "save", "name": "plan.md", "text": "# plan"}),
        )
        .await
        .unwrap();
    assert_eq!(saved["saved"]["name"], "plan.md");
    assert!(fx.boxes.dir(&ada()).unwrap().join("plan.md").is_file());
    let read = tool
        .call_in(1, &mut as_ada, json!({"action": "read", "name": "plan.md"}))
        .await
        .unwrap();
    assert_eq!(read["text"], "# plan");
    // Bo sees only his own (empty) box, even when naming Ada's file.
    let mut as_bo = TaskContext {
        scope: Scope::Agent { id: BO.into() },
        ..TaskContext::default()
    };
    let list = tool
        .call_in(2, &mut as_bo, json!({"action": "list"}))
        .await
        .unwrap();
    assert_eq!(list["files"], json!([]));
    assert!(tool
        .call_in(
            2,
            &mut as_bo,
            json!({"action": "read", "name": "../d-0a1b2c3d.a1/plan.md"})
        )
        .await
        .is_err());
    // The user and scope-less tasks have no box.
    let mut user = TaskContext {
        scope: Scope::User,
        ..TaskContext::default()
    };
    assert!(matches!(
        tool.call_in(3, &mut user, json!({"action": "list"})).await,
        Err(crate::tools::ToolError::Refused(_))
    ));
    let deleted = tool
        .call_in(
            1,
            &mut as_ada,
            json!({"action": "delete", "name": "plan.md"}),
        )
        .await
        .unwrap();
    assert_eq!(deleted["deleted"], "plan.md");
    assert!(tool
        .check_args(&json!({"action": "save", "name": "x"}))
        .is_err());
    assert!(tool.check_args(&json!({"action": "nope"})).is_err());
}
