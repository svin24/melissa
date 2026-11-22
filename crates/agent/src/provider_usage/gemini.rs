//! Gemini CLI with a Google sign-in: Code Assist's quota, asked by Melissa on
//! `cloudcode-pa.googleapis.com` (the host Gemini itself uses, already on its allowlist) with
//! the access token in the agents' `~/.gemini/oauth_creds.json` (`access_token`,
//! `expiry_date` in Unix ms). Two calls, both `POST` with a JSON body:
//!
//! 1. `v1internal:loadCodeAssist` `{"metadata": {ideType, platform, pluginType}}` →
//!    `{currentTier: {id, name}, cloudaicompanionProject: "<id>" | {id}}`: the tier (the plan)
//!    and the project the quota belongs to.
//! 2. `v1internal:retrieveUserQuota` `{"project": "<id>"}` → `{buckets: [{modelId,
//!    tokenType, remainingFraction (0–1), remainingAmount?, resetTime (ISO 8601)}]}`.
//!
//! Melissa never renews the token (that would need Gemini CLI's OAuth client, which is not
//! Melissa's to ship): an expired token reads as unavailable until Gemini's next run renews
//! it. Sign-ins with an API key have no quota to ask for.

use std::path::Path;

use serde_json::{json, Value};

use super::{
    collect::{Fetched, Miss},
    https::{HttpClient, HttpRequest, Secret},
    json_num, json_time, Window, WindowKind,
};

pub const SOURCE: &str = "gemini.quota";
pub const HOST: &str = "cloudcode-pa.googleapis.com";
const LOAD_URL: &str = "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
const QUOTA_URL: &str = "https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota";
/// The sign-in file, relative to the agents' home.
pub const CREDS: &str = ".gemini/oauth_creds.json";
/// A token this close to expiring is not used.
const EXPIRY_MARGIN_SECS: u64 = 60;

/// The access token in the sign-in file, if it is still good at `now`.
pub fn access_token(creds: &Value, now: u64) -> Result<Secret, Miss> {
    let token = creds["access_token"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| Miss::Off("no Google sign-in".into()))?;
    if let Some(expiry) = json_time(&creds["expiry_date"]) {
        if expiry <= now + EXPIRY_MARGIN_SECS {
            return Err(Miss::Unavailable(
                "the Google sign-in expired; Gemini renews it on its next run".into(),
            ));
        }
    }
    Ok(Secret::new(token))
}

/// The tier and project in a `loadCodeAssist` answer.
pub fn tier_and_project(answer: &Value) -> (Option<String>, Option<String>) {
    let tier = &answer["currentTier"];
    let plan = tier["name"]
        .as_str()
        .or_else(|| tier["id"].as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(60).collect());
    let p = &answer["cloudaicompanionProject"];
    let project = p
        .as_str()
        .or_else(|| p["id"].as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    (plan, project)
}

/// The windows in a `retrieveUserQuota` answer: one per model and kind of quota.
pub fn quota_windows(answer: &Value, now: u64) -> Vec<Window> {
    let mut out: Vec<Window> = Vec::new();
    for b in answer["buckets"].as_array().into_iter().flatten() {
        let Some(model) = b["modelId"]
            .as_str()
            .filter(|m| crate::session::valid_model_id(m))
        else {
            continue;
        };
        let Some(left) = json_num(&b["remainingFraction"]) else {
            continue;
        };
        let kind = b["tokenType"].as_str().unwrap_or("");
        let id = if kind.is_empty() || kind == "REQUESTS" {
            format!("model:{model}")
        } else {
            format!("model:{model}:{}", kind.to_ascii_lowercase())
        };
        if out.iter().any(|w| w.id == id) {
            continue;
        }
        out.push(Window {
            id,
            label: model.to_string(),
            // Google gives the reset time but not the window's length.
            kind: WindowKind::Other,
            used_pct: ((1.0 - left) * 100.0).max(0.0),
            resets_at: json_time(&b["resetTime"]),
            window_secs: None,
            model_scope: Some(model.to_string()),
            status: None,
            source: SOURCE.into(),
            observed_at: now,
            pace: None,
        });
    }
    out
}

fn status_miss(status: u16) -> Miss {
    Miss::Unavailable(match status {
        401 => "Google refused the sign-in; Gemini renews it on its next run".into(),
        403 => "Google refused the quota request (403)".into(),
        429 => "Google is rate limiting quota requests; try again later".into(),
        s => format!("Google answered {s}"),
    })
}

async fn post(
    http: &dyn HttpClient,
    url: &str,
    token: &Secret,
    body: Value,
) -> Result<Value, Miss> {
    let res = http
        .send(HttpRequest::post_json(url, token.clone(), body))
        .await
        .map_err(|e| Miss::Unavailable(e.to_string()))?;
    if !(200..300).contains(&res.status) {
        return Err(status_miss(res.status));
    }
    serde_json::from_slice(&res.body)
        .map_err(|_| Miss::Unavailable("Google's answer was not JSON".into()))
}

/// Ask Google for the plan and quota of the sign-in in `home`.
pub async fn fetch(home: &Path, http: &dyn HttpClient, now: u64) -> Result<Fetched, Miss> {
    let text = match std::fs::read_to_string(home.join(CREDS)) {
        Ok(t) => t,
        Err(_) => return Err(Miss::Off("no Google sign-in".into())),
    };
    let creds: Value = serde_json::from_str(&text)
        .map_err(|_| Miss::Unavailable("the Google sign-in file is unreadable".into()))?;
    let token = access_token(&creds, now)?;
    let load = post(
        http,
        LOAD_URL,
        &token,
        json!({"metadata": {
            "ideType": "IDE_UNSPECIFIED",
            "platform": "PLATFORM_UNSPECIFIED",
            "pluginType": "GEMINI",
        }}),
    )
    .await?;
    let (plan, project) = tier_and_project(&load);
    let body = match &project {
        Some(p) => json!({ "project": p }),
        None => json!({}),
    };
    let quota = post(http, QUOTA_URL, &token, body).await?;
    let windows = quota_windows(&quota, now);
    if windows.is_empty() && plan.is_none() {
        return Err(Miss::Unavailable("Google reported no quota".into()));
    }
    Ok(Fetched {
        plan,
        windows,
        ..Fetched::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_usage::https::fake::FakeHttp;

    fn home_with(name: &str, creds: &str) -> std::path::PathBuf {
        let home =
            std::env::temp_dir().join(format!("melissa-gemini-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".gemini")).unwrap();
        std::fs::write(home.join(CREDS), creds).unwrap();
        home
    }

    const CREDS_OK: &str = include_str!("fixtures/gemini_oauth_creds.json");

    #[tokio::test]
    async fn plan_and_per_model_quota_are_read_with_the_sign_in_token() {
        let home = home_with("ok", CREDS_OK);
        let http = FakeHttp::default()
            .answer(
                LOAD_URL,
                200,
                include_str!("fixtures/gemini_load_code_assist.json"),
            )
            .answer(QUOTA_URL, 200, include_str!("fixtures/gemini_quota.json"));
        let f = fetch(&home, &http, 1_700_000_000).await.unwrap();
        assert_eq!(f.plan.as_deref(), Some("Example tier"));
        assert_eq!(f.windows.len(), 2);
        let pro = f
            .windows
            .iter()
            .find(|w| w.id == "model:example-pro")
            .unwrap();
        assert!((pro.used_pct - 75.0).abs() < 1e-9);
        assert_eq!(pro.resets_at, Some(1_800_000_000));
        let asked = http.asked();
        assert_eq!(asked.len(), 2);
        assert_eq!(
            asked[1].json,
            Some(json!({"project": "example-project-0000"}))
        );
        // The token goes only in the request's bearer field, to Google's host.
        for req in &asked {
            assert_eq!(req.host().as_deref(), Some(HOST));
            assert_eq!(
                req.bearer.as_ref().unwrap().expose(),
                "placeholder-access-token"
            );
            assert!(!req.url.contains("placeholder-token"));
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn an_expired_token_is_not_sent() {
        let home = home_with("expired", CREDS_OK);
        let http = FakeHttp::default();
        let err = fetch(&home, &http, 1_900_000_000).await.unwrap_err();
        assert!(
            matches!(err, Miss::Unavailable(ref r) if r.contains("expired")),
            "{err:?}"
        );
        assert!(http.asked().is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn no_sign_in_file_is_off_and_a_refusal_is_unavailable() {
        let empty = std::env::temp_dir().join("melissa-gemini-nohome");
        let http = FakeHttp::default();
        assert!(matches!(fetch(&empty, &http, 1).await, Err(Miss::Off(_))));
        let home = home_with("refused", CREDS_OK);
        let http = FakeHttp::default().answer(LOAD_URL, 401, "{}");
        let err = fetch(&home, &http, 1_700_000_000).await.unwrap_err();
        assert!(
            matches!(err, Miss::Unavailable(ref r) if r.contains("refused")),
            "{err:?}"
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
