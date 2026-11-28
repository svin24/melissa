#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use serde::Deserialize;
use serde_json::{json, Value};

fn dir(name: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let d = std::env::temp_dir().join(format!(
        "melissa-statefile-{name}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pet {
    name: String,
    legs: u32,
}

/// Format 1: `{"name", "feet"}`; format 2 renamed `feet` to `legs`.
fn feet_to_legs(doc: &mut Value) -> Result<(), String> {
    let o = doc.as_object_mut().ok_or("not an object")?;
    let feet = o.remove("feet").ok_or("no feet")?;
    o.insert("legs".into(), feet);
    Ok(())
}

static PET_V2: JsonFormat = Format {
    name: "pet",
    current: 2,
    migrations: &[feet_to_legs],
    v1_marker: true,
};

static PET_V1: JsonFormat = Format {
    name: "pet",
    current: 1,
    migrations: &[],
    v1_marker: true,
};

#[test]
fn a_file_without_a_format_is_format_1_and_is_migrated() {
    let d = dir("old");
    let path = d.join("pet.json");
    // A file from before formats existed.
    fs::write(&path, r#"{"name": "Rex", "feet": 4}"#).unwrap();
    let loaded: Loaded<Pet> = read_json(&path, &PET_V2).unwrap().unwrap();
    assert_eq!(loaded.found, 1);
    assert!(!loaded.read_only);
    assert_eq!(
        loaded.value,
        Pet {
            name: "Rex".into(),
            legs: 4
        }
    );
    // Reading never writes.
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        r#"{"name": "Rex", "feet": 4}"#
    );
    save_json(&path, &PET_V2, &loaded.value, false).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap()["format"],
        2,
        "{text}"
    );
    let again: Loaded<Pet> = read_json(&path, &PET_V2).unwrap().unwrap();
    assert_eq!((again.found, again.value.legs), (2, 4));
}

#[test]
fn a_newer_file_is_read_only_and_never_replaced() {
    let d = dir("newer");
    let path = d.join("pet.json");
    let newer = r#"{"format": 3, "name": "Rex", "legs": 4, "tail": true}"#;
    fs::write(&path, newer).unwrap();
    // Read as well as this version can (unknown fields would fail `deny_unknown_fields`
    // only in the current format; a newer file is read leniently when possible).
    let res = read_json::<Value>(&path, &PET_V2).unwrap().unwrap();
    assert!(res.read_only);
    assert_eq!(res.found, 3);
    // The strict struct cannot read it: a clear error, not a default.
    let err = read_json::<Pet>(&path, &PET_V2).unwrap_err();
    assert!(err.is_newer(), "{err}");
    assert!(
        err.to_string().contains("newer version of Melissa"),
        "{err}"
    );
    // Saving refuses and leaves the file byte for byte.
    let pet = Pet {
        name: "Rex".into(),
        legs: 3,
    };
    let err = save_json(&path, &PET_V2, &pet, true).unwrap_err();
    assert!(err.is_newer(), "{err}");
    assert_eq!(fs::read_to_string(&path).unwrap(), newer);
    // TOML too.
    let toml_path = d.join("prefs.toml");
    let text = "# header\nformat = 2\ncolour = \"blue\"\n";
    fs::write(&toml_path, text).unwrap();
    let err = save_toml(&toml_path, &PREFS, "", &toml::Table::new()).unwrap_err();
    assert!(err.is_newer());
    assert_eq!(fs::read_to_string(&toml_path).unwrap(), text);
}

#[test]
fn a_current_or_older_or_unreadable_file_may_be_replaced() {
    let d = dir("replace");
    let path = d.join("pet.json");
    let pet = Pet {
        name: "Rex".into(),
        legs: 4,
    };
    save_json(&path, &PET_V1, &pet, true).unwrap();
    save_json(&path, &PET_V2, &pet, true).unwrap();
    // Broken files are the store's business (it decided to write).
    fs::write(&path, "{not json").unwrap();
    save_json(&path, &PET_V2, &pet, true).unwrap();
    assert_eq!(
        read_json::<Pet>(&path, &PET_V2).unwrap().unwrap().value,
        pet
    );
    assert!(read_json::<Pet>(&d.join("missing.json"), &PET_V2)
        .unwrap()
        .is_none());
}

#[test]
fn bad_format_numbers_are_invalid() {
    let d = dir("bad");
    let path = d.join("pet.json");
    for bad in [
        r#"{"format": 0, "name": "x", "legs": 1}"#,
        r#"{"format": "2", "name": "x", "legs": 1}"#,
        r#"{"format": 1.5, "name": "x", "legs": 1}"#,
    ] {
        fs::write(&path, bad).unwrap();
        let err = read_json::<Pet>(&path, &PET_V2).unwrap_err();
        assert!(matches!(err, FormatError::Invalid { .. }), "{bad}: {err}");
    }
}

#[test]
fn a_failed_migration_is_an_error_not_a_default() {
    let d = dir("migfail");
    let path = d.join("pet.json");
    fs::write(&path, r#"{"name": "Rex"}"#).unwrap();
    let err = read_json::<Pet>(&path, &PET_V2).unwrap_err();
    assert!(
        matches!(err, FormatError::Migration { from: 1, to: 2, .. }),
        "{err}"
    );
}

static NO_MARKER: JsonFormat = Format {
    name: "index",
    current: 1,
    migrations: &[],
    v1_marker: false,
};

#[test]
fn format_1_without_a_marker_keeps_the_old_shape() {
    let list = vec![1, 2, 3];
    assert_eq!(to_json(&NO_MARKER, &list, false).unwrap(), b"[1,2,3]");
    let obj = json!({"a": 1});
    assert_eq!(to_json(&NO_MARKER, &obj, false).unwrap(), br#"{"a":1}"#);
    // From format 2 on, the root must be an object.
    static ARRAY_V2: JsonFormat = Format {
        name: "index",
        current: 2,
        migrations: &[|_| Ok(())],
        v1_marker: false,
    };
    assert!(to_json(&ARRAY_V2, &list, false).is_err());
}

static PREFS: TomlFormat = Format {
    name: "preferences",
    current: 1,
    migrations: &[],
    v1_marker: false,
};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Prefs {
    colour: String,
    #[serde(default)]
    sizes: std::collections::BTreeMap<String, u32>,
}

#[test]
fn toml_files_round_trip_with_a_header_and_strip_the_key() {
    let d = dir("toml");
    let path = d.join("prefs.toml");
    let prefs = Prefs {
        colour: "red".into(),
        sizes: [("a".to_string(), 1)].into(),
    };
    save_toml(&path, &PREFS, "# written by tests\n\n", &prefs).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("# written by tests\n\ncolour"), "{text}");
    assert_eq!(
        read_toml::<Prefs>(&path, &PREFS).unwrap().unwrap().value,
        prefs
    );
    // With a marker (format 2): the key comes first and the strict struct still reads it.
    static PREFS_V2: TomlFormat = Format {
        name: "preferences",
        current: 2,
        migrations: &[|_| Ok(())],
        v1_marker: false,
    };
    save_toml(&path, &PREFS_V2, "# h\n", &prefs).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("# h\nformat = 2\n"), "{text}");
    let loaded = read_toml::<Prefs>(&path, &PREFS_V2).unwrap().unwrap();
    assert_eq!((loaded.found, &loaded.value), (2, &prefs));
    // This build's format 1 refuses to replace it.
    assert!(save_toml(&path, &PREFS, "", &prefs).unwrap_err().is_newer());
}

#[test]
fn an_interrupted_write_leaves_the_old_file_intact() {
    let d = dir("atomic");
    let path = d.join("state.json");
    write_private(&path, b"old and complete").unwrap();
    let err = write_atomic_with(&path, 0o600, |f| {
        f.write_all(b"new but ha")?;
        Err(io::Error::other("power cut"))
    })
    .unwrap_err();
    assert_eq!(err.to_string(), "power cut");
    assert_eq!(fs::read(&path).unwrap(), b"old and complete");
    // No temporary file is left behind.
    let names: Vec<_> = fs::read_dir(&d)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("state.json")]);
    // A temporary file from a crash (never renamed) does not change what is read.
    fs::write(d.join(".state.json.1-0.tmp"), "half").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"old and complete");
    write_private(&path, b"new").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"new");
}

#[test]
fn written_files_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let d = dir("mode");
    let path = d.join("sub/secret.json");
    write_private(&path, b"{}").unwrap();
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Event {
    seq: u64,
    text: String,
}

/// Lines: format 1 had `msg`, format 2 has `text`.
static EVENT_V2: JsonFormat = Format {
    name: "test log",
    current: 2,
    migrations: &[|doc| {
        let o = doc.as_object_mut().ok_or("not an object")?;
        let msg = o.remove("msg").ok_or("no msg")?;
        o.insert("text".into(), msg);
        Ok(())
    }],
    v1_marker: true,
};

#[test]
fn log_lines_carry_their_format_and_newer_lines_are_skipped_not_lost() {
    let d = dir("log");
    let path = d.join("log.jsonl");
    // An old line (no format), then this build's, then a newer Melissa's.
    fs::write(&path, "{\"seq\":1,\"msg\":\"old\"}\n").unwrap();
    let line = encode_line(
        &EVENT_V2,
        &Event {
            seq: 2,
            text: "now".into(),
        },
    )
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["format"],
        2,
        "{line}"
    );
    append_line(&path, &line, 0o600).unwrap();
    append_line(&path, r#"{"format":3,"seq":3,"words":["new"]}"#, 0o600).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    let events: Vec<Event> = decode_lines(&EVENT_V2, &text, &path);
    assert_eq!(
        events,
        vec![
            Event {
                seq: 1,
                text: "old".into()
            },
            Event {
                seq: 2,
                text: "now".into()
            }
        ]
    );
    assert_eq!(
        decode_line::<Event>(&EVENT_V2, r#"{"format":3,"seq":3}"#),
        Line::Newer(3)
    );
    // The newer line is still there for the newer Melissa.
    assert!(text.contains("\"words\""));
}

#[test]
fn a_line_cut_short_by_a_crash_does_not_spoil_the_next() {
    let d = dir("torn");
    let path = d.join("log.jsonl");
    fs::write(&path, "{\"seq\":1,\"msg\":\"a\"}\n{\"seq\":2,\"te").unwrap();
    let line = encode_line(
        &EVENT_V2,
        &Event {
            seq: 3,
            text: "c".into(),
        },
    )
    .unwrap();
    append_line(&path, &line, 0o600).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    let seqs: Vec<u64> = decode_lines::<Event>(&EVENT_V2, &text, &path)
        .iter()
        .map(|e| e.seq)
        .collect();
    assert_eq!(seqs, vec![1, 3]);
    assert_eq!(
        decode_line::<Event>(&EVENT_V2, "{\"seq\":2,\"te"),
        Line::Invalid
    );
}
