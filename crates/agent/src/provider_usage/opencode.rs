//! OpenCode's Go plan: `GET https://opencode.ai/zen/go/v1/usage` with the Go key from the
//! agents' `~/.local/share/opencode/auth.json` (`{"opencode-go": {"type": "api", "key"}}`),
//! asked by Melissa (the host OpenCode itself uses, already on its allowlist). The answer:
//! `{usage: {rolling, weekly, monthly: {percent (0–100), resetsAt (ISO 8601 or Unix ms)}}}`;
//! the rolling window is 5 hours and the weekly one 7 days (as T3 Code reads them; OpenCode
//! does not send the lengths). 403: the key has no Go subscription.
//!
//! Not used: OpenCode 2's local server (`/api/experimental/usage/limits`): Melissa runs
//! `opencode acp` inside its sandbox, where no server Melissa could reach is listening; and
//! OpenCode 2's credential database (would need SQLite). Session cost comes over ACP and is
//! counted per conversation already.

use std::path::Path;

use serde_json::Value;

use super::{
    collect::{Fetched, Miss},
    https::{HttpClient, HttpRequest, Secret},
    json_num, json_time, Window, WindowKind,
};

pub const SOURCE: &str = "opencode.go";
pub const HOST: &str = "opencode.ai";
const USAGE_URL: &str = "https://opencode.ai/zen/go/v1/usage";
/// The sign-in file, relative to the agents' home.
pub const AUTH: &str = ".local/share/opencode/auth.json";

/// The Go key in OpenCode's `auth.json`.
pub fn go_key(auth: &Value) -> Option<Secret> {
    let entry = &auth["opencode-go"];
    if entry["type"].as_str().is_some_and(|t| t != "api") {
        return None;
    }
    entry["key"]
        .as_str()
        .filter(|k| !k.is_empty())
        .map(Secret::new)
}

/// The windows in a usage answer.
pub fn usage_windows(answer: &Value, now: u64) -> Vec<Window> {
    let usage = if answer["usage"].is_object() {
        &answer["usage"]
    } else {
        answer
    };
    let mut out = Vec::new();
    for (id, label, secs, kind) in [
        ("rolling", "5-hour", Some(5 * 3600), WindowKind::Session),
        ("weekly", "Weekly", Some(7 * 24 * 3600), WindowKind::Weekly),
        ("monthly", "Monthly", None, WindowKind::Monthly),
    ] {
        let w = &usage[id];
        let Some(used) = json_num(&w["percent"]) else {
            continue;
        };
        out.push(Window {
            id: id.into(),
            label: label.into(),
            kind,
            used_pct: used,
            resets_at: json_time(&w["resetsAt"]),
            window_secs: secs,
            model_scope: None,
            status: None,
            source: SOURCE.into(),
            observed_at: now,
            pace: None,
        });
    }
    out
}

/// Ask OpenCode for the Go plan's windows of the key in `home`.
pub async fn fetch(home: &Path, http: &dyn HttpClient, now: u64) -> Result<Fetched, Miss> {
    let Ok(text) = std::fs::read_to_string(home.join(AUTH)) else {
        return Err(Miss::Off("no OpenCode Go key".into()));
    };
    let auth: Value = serde_json::from_str(&text)
        .map_err(|_| Miss::Unavailable("OpenCode's sign-in file is unreadable".into()))?;
    let key = go_key(&auth).ok_or_else(|| Miss::Off("no OpenCode Go key".into()))?;
    let res = http
        .send(HttpRequest::get(USAGE_URL, key))
        .await
        .map_err(|e| Miss::Unavailable(e.to_string()))?;
    match res.status {
        200..=299 => {}
        401 => return Err(Miss::Unavailable("OpenCode refused the Go key".into())),
        403 => return Err(Miss::Unavailable("this key has no Go subscription".into())),
        s => return Err(Miss::Unavailable(format!("OpenCode answered {s}"))),
    }
    let answer: Value = serde_json::from_slice(&res.body)
        .map_err(|_| Miss::Unavailable("OpenCode's answer was not JSON".into()))?;
    let windows = usage_windows(&answer, now);
    if windows.is_empty() {
        return Err(Miss::Unavailable("OpenCode reported no Go usage".into()));
    }
    Ok(Fetched {
        plan: Some("go".into()),
        windows,
        ..Fetched::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_usage::https::fake::FakeHttp;

    fn home_with(name: &str, auth: &str) -> std::path::PathBuf {
        let home =
            std::env::temp_dir().join(format!("melissa-opencode-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".local/share/opencode")).unwrap();
        std::fs::write(home.join(AUTH), auth).unwrap();
        home
    }

    #[tokio::test]
    async fn the_go_windows_are_read_with_the_go_key() {
        let home = home_with("ok", include_str!("fixtures/opencode_auth.json"));
        let http = FakeHttp::default().answer(
            USAGE_URL,
            200,
            include_str!("fixtures/opencode_go_usage.json"),
        );
        let f = fetch(&home, &http, 5).await.unwrap();
        assert_eq!(f.plan.as_deref(), Some("go"));
        let ids: Vec<_> = f.windows.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, ["rolling", "weekly", "monthly"]);
        assert!((f.windows[0].used_pct - 18.0).abs() < 1e-9);
        assert_eq!(f.windows[0].resets_at, Some(1_800_000_000));
        // Unix milliseconds work too.
        assert_eq!(f.windows[1].resets_at, Some(1_800_400_000));
        assert_eq!(f.windows[2].window_secs, None);
        let asked = http.asked();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].host().as_deref(), Some(HOST));
        assert_eq!(
            asked[0].bearer.as_ref().unwrap().expose(),
            "placeholder-go-key"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn without_a_go_key_nothing_is_asked() {
        let home = home_with(
            "nokey",
            r#"{"anthropic": {"type": "api", "key": "placeholder"}}"#,
        );
        let http = FakeHttp::default();
        assert!(matches!(fetch(&home, &http, 1).await, Err(Miss::Off(_))));
        assert!(http.asked().is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn a_key_without_a_subscription_is_unavailable() {
        let home = home_with("nosub", include_str!("fixtures/opencode_auth.json"));
        let http = FakeHttp::default().answer(USAGE_URL, 403, "{}");
        let err = fetch(&home, &http, 1).await.unwrap_err();
        assert!(
            matches!(err, Miss::Unavailable(ref r) if r.contains("no Go")),
            "{err:?}"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn a_network_failure_is_unavailable() {
        let home = home_with("down", include_str!("fixtures/opencode_auth.json"));
        let http =
            FakeHttp::default().fail(USAGE_URL, crate::provider_usage::https::HttpError::Timeout);
        let err = fetch(&home, &http, 1).await.unwrap_err();
        assert!(
            matches!(err, Miss::Unavailable(ref r) if r.contains("no answer")),
            "{err:?}"
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
