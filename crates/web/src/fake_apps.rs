//! For the screenshot harness only (`scripts/ui-shots`): answers for the app tools from a JSON
//! file, so the Apps list and an app's details can be shot without k3s. Compiled only with the
//! `fake-apps` feature (`cargo build --features melissa-server/fake-apps`); normal and release
//! builds don't have it.
//!
//! `MELISSA_FAKE_APPS` names the file: `{"list_apps": {...}, "app_status": {"<app>": {...}},
//! "app_logs": {"<app>": {...}}}`, each in the tool's own answer shape. It is read on every
//! call; a missing file or tool falls through to the real tools. `installed_for` may name a
//! person as `user:<username>`: it becomes their account id once they exist (the harness
//! makes the people after the server started).

use std::sync::OnceLock;

use serde_json::Value;

static AUTH: OnceLock<crate::auth::Auth> = OnceLock::new();

/// The login store, to turn `user:<username>` into account ids.
pub(crate) fn set_auth(auth: &crate::auth::Auth) {
    let _ = AUTH.set(auth.clone());
}

/// `user:<username>` → that person's account id (as is when unknown).
fn person_ids(v: &mut Value) {
    let people = AUTH.get().map(|a| a.people()).unwrap_or_default();
    for app in v["apps"].as_array_mut().into_iter().flatten() {
        let Some(name) = app["installed_for"]
            .as_str()
            .and_then(|s| s.strip_prefix("user:"))
        else {
            continue;
        };
        if let Some(p) = people.iter().find(|p| p.username == name) {
            app["installed_for"] = Value::String(p.id.clone());
        }
    }
}

/// The harness asked for fake apps.
pub(crate) fn on() -> bool {
    std::env::var_os("MELISSA_FAKE_APPS").is_some()
}

/// The file's answer for `tool` with `args`, if it has one.
pub(crate) fn answer(tool: &str, args: &Value) -> Option<Value> {
    let path = std::env::var_os("MELISSA_FAKE_APPS")?;
    let text = std::fs::read_to_string(path).ok()?;
    let data: Value = serde_json::from_str(&text).ok()?;
    let v = match tool {
        "list_apps" => {
            let mut v = data["list_apps"].clone();
            person_ids(&mut v);
            v
        }
        "app_status" | "app_logs" => data[tool][args["app"].as_str()?].clone(),
        _ => Value::Null,
    };
    (!v.is_null()).then_some(v)
}
