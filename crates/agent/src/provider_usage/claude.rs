//! Claude Code's usage reports over stream-json (shapes read from Claude Code 2.1.330's own
//! SDK schema on 2026-11-22; `get_usage` is marked experimental there, so everything is read
//! defensively and unknown parts are skipped):
//!
//! - `{"type":"rate_limit_event","rate_limit_info":{status, rateLimitType, utilization (0–1),
//!   resetsAt (Unix s), unifiedWindows?: {five_hour|seven_day|seven_day_overage_included:
//!   {utilization (0–1, may pass 1), resetsAt, status?}}, isUsingOverage?, ...}}` during turns.
//! - The `get_usage` control request (`{"subtype":"get_usage","skip_behaviors":true}`;
//!   skipping the scan of local transcripts) answers `{subscription_type, rate_limits_available,
//!   rate_limits: {five_hour|seven_day|seven_day_oauth_apps|seven_day_opus|seven_day_sonnet:
//!   {utilization (0–100), resets_at (ISO 8601)}|null, model_scoped: [{display_name,
//!   utilization, resets_at}], extra_usage: {is_enabled, monthly_limit, used_credits,
//!   utilization, currency?}}, session: {...}, behaviors: null}`.
//! - `result.modelUsage`: `{<model>: {inputTokens, outputTokens, cacheReadInputTokens,
//!   cacheCreationInputTokens, costUSD, ...}}`, running totals of the process.

use serde_json::Value;

use super::{
    collect::Fetched, finite, parse_time, Credits, ModelTokens, TokenCount, Window, WindowKind,
};

/// The subtype of the control request.
pub const GET_USAGE: &str = "get_usage";
pub const SOURCE_STREAM: &str = "claude.stream";
pub const SOURCE_GET_USAGE: &str = "claude.get_usage";

const HOUR: u64 = 3600;
const WEEK: u64 = 7 * 24 * HOUR;

/// The `request` of the control request.
pub fn get_usage_request() -> Value {
    serde_json::json!({"subtype": GET_USAGE, "skip_behaviors": true})
}

/// What a plan window id means: label, length, model scope.
fn known(id: &str) -> Option<(&'static str, u64, Option<&'static str>)> {
    Some(match id {
        "five_hour" => ("5-hour", 5 * HOUR, None),
        "seven_day" => ("Weekly", WEEK, None),
        "seven_day_opus" => ("Weekly · Opus", WEEK, Some("Opus")),
        "seven_day_sonnet" => ("Weekly · Sonnet", WEEK, Some("Sonnet")),
        "seven_day_oauth_apps" => ("Weekly · apps", WEEK, None),
        "seven_day_overage_included" => ("Weekly · with extra usage", WEEK, None),
        _ => return None,
    })
}

fn window(
    id: &str,
    used_pct: f64,
    resets_at: Option<u64>,
    status: Option<&str>,
    source: &str,
    now: u64,
) -> Option<Window> {
    let (label, secs, scope) = known(id)?;
    Some(Window {
        id: id.into(),
        label: label.into(),
        kind: WindowKind::for_length(secs),
        used_pct: finite(used_pct)?,
        resets_at,
        window_secs: Some(secs),
        model_scope: scope.map(str::to_string),
        status: status.map(str::to_string),
        source: source.into(),
        observed_at: now,
        pace: None,
    })
}

/// The plan windows in a `rate_limit_event` (empty if it has none Melissa can read).
pub fn rate_limit_event(msg: &Value, now: u64) -> Vec<Window> {
    let info = &msg["rate_limit_info"];
    let mut out = Vec::new();
    if let Some(windows) = info["unifiedWindows"].as_object() {
        for (id, w) in windows {
            let Some(util) = w["utilization"].as_f64() else {
                continue;
            };
            if let Some(w) = window(
                id,
                util * 100.0,
                w["resetsAt"].as_u64(),
                w["status"].as_str(),
                SOURCE_STREAM,
                now,
            ) {
                out.push(w);
            }
        }
    }
    // Older events (and accounts without per-window headers) describe only the window that
    // limits now.
    if out.is_empty() {
        if let (Some(id), Some(util)) =
            (info["rateLimitType"].as_str(), info["utilization"].as_f64())
        {
            out.extend(window(
                id,
                util * 100.0,
                info["resetsAt"].as_u64(),
                info["status"].as_str(),
                SOURCE_STREAM,
                now,
            ));
        }
    }
    out
}

/// Read the answer to `get_usage` (the control response's `response`).
pub fn get_usage(answer: &Value, now: u64) -> Result<Fetched, String> {
    if answer["rate_limits_available"].as_bool() == Some(false) {
        return Err("Claude reports no plan limits for this sign-in".into());
    }
    let limits = &answer["rate_limits"];
    let mut fetched = Fetched {
        plan: answer["subscription_type"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(40).collect()),
        ..Fetched::default()
    };
    if let Some(map) = limits.as_object() {
        for (id, w) in map {
            if known(id).is_none() {
                continue;
            }
            let Some(util) = w["utilization"].as_f64() else {
                continue;
            };
            let resets = w["resets_at"].as_str().and_then(parse_time);
            fetched
                .windows
                .extend(window(id, util, resets, None, SOURCE_GET_USAGE, now));
        }
    }
    for m in limits["model_scoped"].as_array().into_iter().flatten() {
        let (Some(name), Some(util)) = (m["display_name"].as_str(), m["utilization"].as_f64())
        else {
            continue;
        };
        let name: String = name.chars().take(40).collect();
        let Some(used_pct) = finite(util) else {
            continue;
        };
        fetched.windows.push(Window {
            id: format!("model:{name}"),
            label: format!("Weekly · {name}"),
            kind: WindowKind::Weekly,
            used_pct,
            resets_at: m["resets_at"].as_str().and_then(parse_time),
            window_secs: Some(WEEK),
            model_scope: Some(name),
            status: None,
            source: SOURCE_GET_USAGE.into(),
            observed_at: now,
            pace: None,
        });
    }
    let extra = &limits["extra_usage"];
    if extra.is_object() {
        fetched.credits.push(Credits {
            id: "extra_usage".into(),
            label: "Extra usage".into(),
            unit: extra["currency"].as_str().map(str::to_string),
            used: extra["used_credits"].as_f64().and_then(finite),
            limit: extra["monthly_limit"].as_f64().and_then(finite),
            balance: None,
            used_pct: extra["utilization"].as_f64().and_then(finite),
            enabled: extra["is_enabled"].as_bool(),
            unlimited: false,
            period_start: None,
            period_end: None,
            source: SOURCE_GET_USAGE.into(),
        });
    }
    if fetched.windows.is_empty() && fetched.credits.is_empty() && limits.is_null() {
        return Err("Claude sent no plan limits".into());
    }
    Ok(fetched)
}

/// Tokens and Claude's cost figure per model in a `result` message (running totals of the
/// agent process).
pub fn model_usage(result: &Value) -> Vec<ModelTokens> {
    let Some(map) = result["modelUsage"].as_object() else {
        return Vec::new();
    };
    map.iter()
        .filter(|(model, _)| crate::session::valid_model_id(model))
        .map(|(model, u)| {
            let n = |k: &str| u[k].as_u64().unwrap_or(0);
            let cost = u["costUSD"].as_f64().and_then(crate::session::micros);
            ModelTokens {
                model: model.clone(),
                tokens: TokenCount {
                    input: n("inputTokens"),
                    output: n("outputTokens"),
                    cache_read: n("cacheReadInputTokens"),
                    cache_write: n("cacheCreationInputTokens"),
                    reasoning: 0,
                },
                cost_micros: cost,
                currency: cost.map(|_| "USD".into()),
            }
        })
        // Entries with nothing counted (only the context size) say nothing.
        .filter(|m| !m.tokens.is_zero() || m.cost_micros.is_some_and(|c| c > 0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let text = match name {
            "rate_limit_event" => include_str!("fixtures/claude_rate_limit_event.json"),
            "rate_limit_event_old" => include_str!("fixtures/claude_rate_limit_event_old.json"),
            "get_usage" => include_str!("fixtures/claude_get_usage.json"),
            "result" => include_str!("fixtures/claude_result.json"),
            _ => unreachable!(),
        };
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn rate_limit_events_give_every_window_they_carry() {
        let w = rate_limit_event(&fixture("rate_limit_event"), 50);
        let five = w.iter().find(|w| w.id == "five_hour").unwrap();
        assert!((five.used_pct - 42.0).abs() < 1e-9);
        assert_eq!(five.resets_at, Some(1_800_000_000));
        assert_eq!(five.window_secs, Some(5 * 3600));
        assert_eq!(five.status.as_deref(), Some("allowed"));
        assert_eq!(five.observed_at, 50);
        let week = w.iter().find(|w| w.id == "seven_day").unwrap();
        // Past the cap is kept as is.
        assert!((week.used_pct - 103.0).abs() < 1e-9);
        assert_eq!(w.len(), 3);
    }

    #[test]
    fn an_event_without_per_window_values_gives_the_limiting_window() {
        let w = rate_limit_event(&fixture("rate_limit_event_old"), 1);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].id, "seven_day");
        assert!((w[0].used_pct - 75.0).abs() < 1e-9);
    }

    #[test]
    fn unknown_or_empty_events_give_nothing() {
        let msg = serde_json::json!({"type": "rate_limit_event", "rate_limit_info": {
            "status": "allowed", "rateLimitType": "something_new", "utilization": 0.5}});
        assert!(rate_limit_event(&msg, 1).is_empty());
        assert!(rate_limit_event(&serde_json::json!({}), 1).is_empty());
    }

    #[test]
    fn get_usage_gives_windows_model_windows_extra_usage_and_the_plan() {
        let f = get_usage(&fixture("get_usage"), 9).unwrap();
        assert_eq!(f.plan.as_deref(), Some("max"));
        let ids: Vec<_> = f.windows.iter().map(|w| w.id.as_str()).collect();
        assert!(ids.contains(&"five_hour"));
        assert!(ids.contains(&"seven_day"));
        assert!(ids.contains(&"seven_day_sonnet"));
        // `seven_day_opus: null` is skipped.
        assert!(!ids.contains(&"seven_day_opus"));
        let fable = f.windows.iter().find(|w| w.id == "model:Fable").unwrap();
        assert_eq!(fable.model_scope.as_deref(), Some("Fable"));
        assert!((fable.used_pct - 61.0).abs() < 1e-9);
        assert_eq!(fable.resets_at, Some(1_800_003_600));
        let five = f.windows.iter().find(|w| w.id == "five_hour").unwrap();
        assert!((five.used_pct - 12.5).abs() < 1e-9);
        assert_eq!(five.resets_at, Some(1_800_000_000));
        let extra = &f.credits[0];
        assert_eq!(extra.enabled, Some(true));
        assert_eq!(extra.used, Some(1250.0));
        assert_eq!(extra.limit, Some(5000.0));
        assert_eq!(extra.unit.as_deref(), Some("USD"));
    }

    #[test]
    fn get_usage_without_plan_limits_is_unavailable() {
        let answer = serde_json::json!({"rate_limits_available": false, "rate_limits": null});
        assert!(get_usage(&answer, 1).is_err());
        assert!(get_usage(&serde_json::json!({}), 1).is_err());
    }

    #[test]
    fn result_model_usage_gives_tokens_and_cost_per_model() {
        let mut m = model_usage(&fixture("result"));
        m.sort_by(|a, b| a.model.cmp(&b.model));
        assert_eq!(m.len(), 2);
        assert!(
            model_usage(&serde_json::json!({"modelUsage": {"m": {"contextWindow": 1}}})).is_empty()
        );
        assert_eq!(m[0].model, "claude-example-large");
        assert_eq!(m[0].tokens.input, 1200);
        assert_eq!(m[0].tokens.cache_read, 30000);
        assert_eq!(m[0].tokens.cache_write, 800);
        assert_eq!(m[0].tokens.output, 450);
        assert_eq!(m[0].cost_micros, Some(123_400));
        assert_eq!(m[1].cost_micros, None);
    }
}
