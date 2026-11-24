use super::*;

/// A fake generated value (32 characters, the default length).
const VALUE: &str = "TestOnly0fakeValue9NotRealQx7Zk2";
const SALT: &[u8] = b"0123456789abcdef";

fn redactor() -> Redactor {
    Redactor::of_values(SALT, &[("admin_password", VALUE)])
}

fn b64(data: &[u8], url: bool, pad: bool) -> String {
    let abc: &[u8] = if url {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
    } else {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
    };
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = c
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..=c.len() {
            out.push(char::from(abc[(n >> (18 - 6 * i) & 63) as usize]));
        }
        if pad {
            out.push_str(&"=".repeat(3 - c.len()));
        }
    }
    out
}

#[test]
fn copies_and_encodings_are_hidden() {
    let hex = to_hex(format!("pw:{VALUE}").as_bytes());
    let percent: String = VALUE.bytes().map(|b| format!("%{b:02X}")).collect();
    let mut cases = vec![
        ("exact", format!("ADMIN_PASSWORD={VALUE}\nHOME=/root")),
        ("piece of 8+", format!("starts {} ends", &VALUE[5..15])),
        (
            "split across lines",
            format!("{}\n{}", &VALUE[..13], &VALUE[13..]),
        ),
        (
            "wrapped and indented",
            format!("{}\n    {}", &VALUE[..20], &VALUE[20..]),
        ),
        ("hex", hex.clone()),
        ("upper hex", hex.to_uppercase()),
        ("hex, other nibble phase", format!("a{hex}")),
        ("url-encoded", format!("https://example.com/?p={percent}")),
    ];
    // Base64 of the value at all three byte alignments inside a larger blob, standard and
    // URL-safe, padded or not, also with `%2B`-style escapes and wrapped at 20 characters.
    for prefix in ["", "u", "us", "user:"] {
        let plain = format!("{prefix}{VALUE} and more bytes after it");
        for (url, pad) in [(false, true), (true, false)] {
            let enc = b64(plain.as_bytes(), url, pad);
            cases.push(("base64", format!("token: {enc}")));
            cases.push((
                "base64 in a url",
                enc.replace('+', "%2B").replace('/', "%2F"),
            ));
            let wrapped: Vec<String> = enc
                .as_bytes()
                .chunks(20)
                .map(|c| String::from_utf8_lossy(c).into_owned())
                .collect();
            cases.push(("base64 wrapped", wrapped.join("\n")));
        }
    }
    let r = redactor();
    for (name, text) in cases {
        let out = r.redact(&text);
        assert!(
            out.text.contains("[hidden: admin_password]"),
            "{name}: {}",
            out.text
        );
        assert_eq!(
            out.hidden,
            BTreeSet::from(["admin_password".to_string()]),
            "{name}"
        );
        // Nothing matchable is left behind.
        assert!(
            r.redact(&out.text).hidden.is_empty(),
            "{name}: {}",
            out.text
        );
    }
    // Text around a copy stays.
    let out = r.redact(&format!("ADMIN_PASSWORD={VALUE}\nHOME=/root"));
    assert_eq!(
        out.text,
        "ADMIN_PASSWORD=[hidden: admin_password]\nHOME=/root"
    );
}

#[test]
fn ordinary_text_and_short_pieces_stay() {
    let r = redactor();
    let text = format!(
        "Starting server on 0.0.0.0:8080\nsha256:{}\nZm9vYmFyYmF6cXV4 deadbeefcafebabe\n\
         a piece of 7: {}\n%41%42 50%",
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        &VALUE[3..10],
    );
    let out = r.redact(&text);
    assert_eq!(out.text, text);
    assert!(out.hidden.is_empty());
}

#[test]
fn a_cut_end_is_trimmed() {
    // The output limit cut a copy after 5 characters: too short to match, so the tail goes.
    let r = redactor();
    let mut text = r.redact(&format!("line\nPASSWORD={}", &VALUE[..5])).text;
    assert!(drop_cut_tail(&mut text));
    assert_eq!(text, "line\nPASSWORD=[cut]");
    let mut text = "done.\n".to_string();
    assert!(!drop_cut_tail(&mut text));
}

#[test]
fn fingerprints_are_kept_without_the_value() {
    let dir = std::env::temp_dir().join(format!("melissa-prints-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let prints = SecretPrints::in_dir(&dir);
    assert!(prints.redactor("nextcloud").is_none());
    prints.record("nextcloud", "admin_password", VALUE).unwrap();
    prints
        .record(
            "nextcloud",
            "db_password",
            "OtherFake0Value1ForTests2Only345",
        )
        .unwrap();
    let file = dir.join("nextcloud.json");
    let stored = std::fs::read_to_string(&file).unwrap();
    assert!(!stored.contains(&VALUE[..PIECE]), "no value in the record");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let out = prints
        .redactor("nextcloud")
        .unwrap()
        .redact(&format!("{VALUE} OtherFake0Value1ForTests2Only345"));
    assert_eq!(out.text, "[hidden: admin_password] [hidden: db_password]");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn risky_commands_are_flagged() {
    let argv = |a: &[&str]| a.iter().map(ToString::to_string).collect::<Vec<_>>();
    for risky in [
        &["env"][..],
        &["/usr/bin/printenv", "ADMIN_PASSWORD"],
        &["cat", "/proc/1/environ"],
        &["ps", "eww"],
        &["timeout", "5", "env"],
        &["cat", "/var/www/html/config/config.php"],
        &["sh", "-c", "echo $ADMIN_PASSWORD"],
        &["bash", "-lc", "set"],
        &["sh", "-c", "cat .env"],
    ] {
        assert!(exec_may_show_secrets(&argv(risky)), "{risky:?}");
    }
    for fine in [
        &["rcon-cli", "op", "Steve"][..],
        &["ls", "-la", "/data"],
        &["sh", "-c", "ls /data && df -h"],
    ] {
        assert!(!exec_may_show_secrets(&argv(fine)), "{fine:?}");
    }
}

#[test]
fn specs_that_pass_a_secret_to_a_command_are_flagged() {
    let spec = |command: Value, env: Value| {
        AppSpec::from_json(&json!({
            "description": "test app",
            "containers": [{"name": "app", "image": "nginx:1.27", "command": command,
                            "env": env}],
            "secrets": [{"name": "admin_password"}],
        }))
        .unwrap()
    };
    let secret = json!({"PASS": {"secret": "admin_password"}});
    let echo = spec(
        json!(["sh", "-c", "echo ${PASS}; exec nginx"]),
        secret.clone(),
    );
    let note = spec_secret_use(&echo).unwrap();
    assert!(
        note.starts_with(SECRET_USE_NOTE) && note.contains("PASS"),
        "{note}"
    );
    let copied = spec(
        json!([]),
        json!({"PASS": {"secret": "admin_password"}, "OTHER": "$(PASS)"}),
    );
    assert!(spec_secret_use(&copied).is_some());
    // A different variable with the name as prefix, or no use at all.
    let other = spec(json!(["sh", "-c", "echo $PASSWORDLESS"]), secret.clone());
    assert_eq!(spec_secret_use(&other), None);
    assert_eq!(spec_secret_use(&spec(json!([]), secret)), None);
}
