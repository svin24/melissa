//! Grok: the ACP extension method `_x.ai/billing`, asked in a short session with no prompt
//! (Grok CLI 1.0.92 answers it without a session; without a sign-in it answers "Authentication
//! required", checked 2026-11-22). Grok fetches the numbers from its own host inside its
//! sandbox (`cli-chat-proxy.grok.com`, already on its allowlist); Melissa makes no call and
//! never reads Grok's sign-in.
//!
//! The answer is Grok's billing summary, the one `GET /v1/billing?format=credits` gives:
//! `{config: {creditUsagePercent (0–100), currentPeriod: {type, start, end} (ISO 8601),
//! monthlyLimit?, onDemandCap?, onDemandUsed?, prepaidBalance?, ...}}`, possibly without the
//! `config` wrapper. Its exact shape over ACP is not documented, so it is searched for the
//! object that carries `creditUsagePercent` and every field is optional.

use serde_json::Value;

use super::{
    collect::Fetched, json_num as num, json_time as time, length_words, Credits, Window, WindowKind,
};

/// The extension method (ACP extension methods start with `_`).
pub const BILLING_METHOD: &str = "_x.ai/billing";
pub const SOURCE: &str = "grok.billing";

/// The object holding the billing fields.
fn summary(answer: &Value) -> Option<&Value> {
    let candidates = [
        answer,
        &answer["config"],
        &answer["billing"],
        &answer["billing"]["config"],
    ];
    candidates
        .into_iter()
        .find(|v| v.get("creditUsagePercent").is_some() || v.get("currentPeriod").is_some())
}

/// Read the answer to `_x.ai/billing`.
pub fn billing(answer: &Value, now: u64) -> Result<Fetched, String> {
    let s = summary(answer).ok_or("Grok's billing answer has no usage figures")?;
    let field = |camel: &str, snake: &str| -> Option<&Value> {
        s.get(camel)
            .or_else(|| s.get(snake))
            .filter(|v| !v.is_null())
    };
    let period = field("currentPeriod", "current_period");
    let start = period.and_then(|p| time(&p["start"]));
    let end = period.and_then(|p| time(&p["end"]));
    let length = match (start, end) {
        (Some(a), Some(b)) if b > a => Some(b - a),
        _ => None,
    };
    let mut fetched = Fetched {
        plan: [
            answer.get("subscription_tier"),
            answer.get("subscriptionTier"),
            s.get("subscription_tier"),
            s.get("subscriptionTier"),
        ]
        .into_iter()
        .flatten()
        .find_map(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(|t| t.chars().take(40).collect()),
        ..Fetched::default()
    };
    // A period without a percentage counts as nothing used yet.
    let used_pct = field("creditUsagePercent", "credit_usage_percent")
        .and_then(num)
        .or(length.map(|_| 0.0));
    if let Some(used_pct) = used_pct {
        let kind = match period.and_then(|p| p["type"].as_str()) {
            Some(t) if t.contains("WEEK") => WindowKind::Weekly,
            Some(t) if t.contains("MONTH") => WindowKind::Monthly,
            _ => length.map_or(WindowKind::Other, WindowKind::for_length),
        };
        let label = match kind {
            WindowKind::Weekly => "Weekly credits".to_string(),
            WindowKind::Monthly => "Monthly credits".to_string(),
            _ => length.map_or_else(
                || "Credits".into(),
                |l| format!("{} credits", length_words(l)),
            ),
        };
        fetched.windows.push(Window {
            id: "credits".into(),
            label,
            kind,
            used_pct,
            resets_at: end,
            window_secs: length,
            model_scope: None,
            status: None,
            source: SOURCE.into(),
            observed_at: now,
            pace: None,
        });
    }
    let monthly_limit = field("monthlyLimit", "monthly_limit").and_then(num);
    let on_demand_cap = field("onDemandCap", "on_demand_cap").and_then(num);
    let on_demand_used = field("onDemandUsed", "on_demand_used").and_then(num);
    let prepaid = field("prepaidBalance", "prepaid_balance").and_then(num);
    let credit = |id: &str, label: &str| Credits {
        id: id.into(),
        label: label.into(),
        unit: None,
        used: None,
        limit: None,
        balance: None,
        used_pct: None,
        enabled: None,
        unlimited: false,
        period_start: start,
        period_end: end,
        source: SOURCE.into(),
    };
    if monthly_limit.is_some() {
        fetched.credits.push(Credits {
            limit: monthly_limit,
            used_pct,
            ..credit("included", "Included credits")
        });
    }
    if on_demand_cap.is_some() || on_demand_used.is_some() {
        fetched.credits.push(Credits {
            used: on_demand_used,
            limit: on_demand_cap,
            ..credit("on_demand", "On-demand")
        });
    }
    if prepaid.is_some() {
        fetched.credits.push(Credits {
            balance: prepaid,
            period_start: None,
            period_end: None,
            ..credit("prepaid", "Prepaid balance")
        });
    }
    if fetched.windows.is_empty() && fetched.credits.is_empty() {
        return Err("Grok's billing answer has no usage figures".into());
    }
    Ok(fetched)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn billing_answers_become_a_credit_window_and_pools() {
        let answer: Value =
            serde_json::from_str(include_str!("fixtures/grok_billing.json")).unwrap();
        let f = billing(&answer, 7).unwrap();
        assert_eq!(f.plan.as_deref(), Some("example-tier"));
        let w = &f.windows[0];
        assert_eq!(w.kind, WindowKind::Monthly);
        assert!((w.used_pct - 37.5).abs() < 1e-9);
        assert_eq!(w.resets_at, Some(1_800_000_000));
        assert_eq!(w.window_secs, Some(31 * 24 * 3600));
        let ids: Vec<_> = f.credits.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["included", "on_demand", "prepaid"]);
        assert_eq!(f.credits[1].used, Some(2.5));
        assert_eq!(f.credits[2].balance, Some(10.0));

        // A flat answer with a weekly period and no percentage reads as unused.
        let answer = serde_json::json!({"currentPeriod": {"type": "USAGE_PERIOD_TYPE_WEEKLY",
            "start": "2027-01-08T08:00:00Z", "end": "2027-01-15T08:00:00Z"}});
        let f = billing(&answer, 1).unwrap();
        assert_eq!(f.windows[0].kind, WindowKind::Weekly);
        assert!(f.windows[0].used_pct.abs() < 1e-9);
        // Without figures: unavailable.
        assert!(billing(&serde_json::json!({"ok": true}), 1).is_err());
    }
}
