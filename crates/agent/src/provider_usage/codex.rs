//! Codex: its session files. The ACP adapter does not pass Codex's rate limits on, but Codex
//! writes them into every session file (`rollout-*.jsonl` under `~/.codex/sessions/YYYY/MM/DD`,
//! which for Melissa's conversations is `<state>/agent-sessions/codex-acp`). No network.
//!
//! The format is Codex's own and not documented, so it is read defensively: a line that does
//! not parse is skipped. Lines are `{"timestamp": ISO, "type": ..., "payload": {...}}`:
//! - `type: "turn_context"`, `payload.model`: the model of the turns that follow.
//! - `type: "event_msg"`, `payload.type: "token_count"`: `info` (`total_token_usage`:
//!   `input_tokens` (cached ones included), `cached_input_tokens`, `output_tokens`
//!   (reasoning included), `reasoning_output_tokens`; running totals of the session) and
//!   `rate_limits` (`limit_id?`, `primary`/`secondary`: `used_percent` (may pass 100),
//!   `window_minutes`, `resets_at` (Unix s; older versions: `resets_in_seconds`),
//!   `plan_type`, `credits`: `has_credits`, `unlimited`, `balance` (a decimal string)).

use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use serde_json::Value;

use super::{
    collect::Fetched, finite, length_words, parse_time, Credits, ModelTokens, TokenCount,
    TokenTotals, Window, WindowKind,
};

pub const SOURCE: &str = "codex.sessions";
/// Session files changed this recently are read; tokens count from this long ago.
pub const LOOK_BACK: Duration = Duration::from_secs(30 * 24 * 3600);
/// At most this many files (the newest).
const MAX_FILES: usize = 400;
/// At most this much of one file.
const MAX_FILE_BYTES: u64 = 64 << 20;
/// Lines longer than this (whole tool outputs) are skipped unread.
const MAX_LINE: usize = 1 << 20;

/// What one session file says.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct FileUsage {
    /// The newest rate-limit report: (event time, windows, plan, credits).
    pub limits: Option<Limits>,
    /// Tokens per model in events at or after the cut-off.
    pub tokens: BTreeMap<String, TokenCount>,
}

/// Read one session file. Events before `cutoff` (Unix s) add no tokens.
pub fn read_session(reader: impl BufRead, cutoff: u64) -> FileUsage {
    let mut out = FileUsage::default();
    let mut model = String::from("unknown");
    let mut last_total = TokenCount::default();
    let mut lines = reader.split(b'\n');
    while let Some(Ok(line)) = lines.next() {
        if line.len() > MAX_LINE || !line.starts_with(b"{") {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let at = v["timestamp"].as_str().and_then(parse_time).unwrap_or(0);
        let payload = &v["payload"];
        match v["type"].as_str() {
            Some("turn_context") => {
                if let Some(m) = payload["model"]
                    .as_str()
                    .filter(|m| crate::session::valid_model_id(m))
                {
                    model = m.to_string();
                }
            }
            Some("event_msg") if payload["type"] == "token_count" => {
                if let Some(total) = token_usage(&payload["info"]["total_token_usage"]) {
                    let added = total.since(&last_total);
                    last_total = total;
                    if at >= cutoff && !added.is_zero() {
                        out.tokens.entry(model.clone()).or_default().add(&added);
                    }
                }
                if let Some(limits) = rate_limits(&payload["rate_limits"], at) {
                    if out.limits.as_ref().is_none_or(|(t, ..)| at >= *t) {
                        out.limits = Some(limits);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Codex's token counts in Melissa's terms (input without the cached part).
fn token_usage(u: &Value) -> Option<TokenCount> {
    let n = |k: &str| u[k].as_u64();
    let input = n("input_tokens")?;
    let cached = n("cached_input_tokens").unwrap_or(0);
    Some(TokenCount {
        input: input.saturating_sub(cached),
        output: n("output_tokens").unwrap_or(0),
        cache_read: cached,
        cache_write: 0,
        reasoning: n("reasoning_output_tokens").unwrap_or(0),
    })
}

/// A rate-limit report: when, the windows, the plan, the credits.
pub type Limits = (u64, Vec<Window>, Option<String>, Option<Credits>);

fn rate_limits(r: &Value, at: u64) -> Option<Limits> {
    if !r.is_object() {
        return None;
    }
    // Codex can report other meters too; the Codex plan's is `codex` (or unnamed).
    if r["limit_id"].as_str().is_some_and(|id| id != "codex") {
        return None;
    }
    let mut windows = Vec::new();
    for id in ["primary", "secondary"] {
        let w = &r[id];
        let Some(used) = w["used_percent"].as_f64().and_then(finite) else {
            continue;
        };
        let secs = w["window_minutes"].as_u64().map(|m| m * 60);
        let resets_at = w["resets_at"]
            .as_u64()
            .or_else(|| w["resets_in_seconds"].as_u64().map(|s| at + s));
        windows.push(Window {
            id: id.into(),
            label: secs.map_or_else(|| id.to_string(), length_words),
            kind: secs.map_or(WindowKind::Other, WindowKind::for_length),
            used_pct: used,
            resets_at,
            window_secs: secs,
            model_scope: None,
            status: None,
            source: SOURCE.into(),
            observed_at: at,
            pace: None,
        });
    }
    let plan = r["plan_type"]
        .as_str()
        .filter(|p| !p.is_empty())
        .map(|p| p.chars().take(40).collect());
    let c = &r["credits"];
    let credits = c.is_object().then(|| Credits {
        id: "credits".into(),
        label: "Credits".into(),
        unit: Some("credits".into()),
        used: None,
        limit: None,
        balance: c["balance"]
            .as_str()
            .and_then(|b| b.parse::<f64>().ok())
            .or_else(|| c["balance"].as_f64())
            .and_then(finite),
        used_pct: None,
        enabled: c["has_credits"].as_bool(),
        unlimited: c["unlimited"].as_bool() == Some(true),
        period_start: None,
        period_end: None,
        source: SOURCE.into(),
    });
    if windows.is_empty() && plan.is_none() && credits.is_none() {
        return None;
    }
    Some((at, windows, plan, credits))
}

/// Session files under `roots` changed since `cutoff`, newest first.
pub fn session_files(roots: &[PathBuf], cutoff: SystemTime) -> Vec<PathBuf> {
    let mut found: Vec<(SystemTime, PathBuf)> = Vec::new();
    for root in roots {
        walk(root, 0, cutoff, &mut found);
    }
    found.sort_by_key(|f| std::cmp::Reverse(f.0));
    found.truncate(MAX_FILES);
    found.into_iter().map(|(_, p)| p).collect()
}

fn walk(dir: &Path, depth: usize, cutoff: SystemTime, found: &mut Vec<(SystemTime, PathBuf)>) {
    // `YYYY/MM/DD/rollout-*.jsonl`: three levels, a little slack.
    if depth > 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        // No symlinks: the directory is the agent's to write.
        let Ok(ft) = e.file_type() else { continue };
        let path = e.path();
        if ft.is_dir() {
            walk(&path, depth + 1, cutoff, found);
        } else if ft.is_file() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if !(name.starts_with("rollout-") && name.ends_with(".jsonl")) {
                continue;
            }
            if let Ok(modified) = e.metadata().and_then(|m| m.modified()) {
                if modified >= cutoff {
                    found.push((modified, path));
                }
            }
        }
    }
}

/// Read every recent session file under `roots` (each `<state>/agent-sessions/codex-acp`).
/// Blocking: run it off the async threads.
pub fn collect(roots: &[PathBuf], now: u64) -> Result<Fetched, String> {
    let cutoff_secs = now.saturating_sub(LOOK_BACK.as_secs());
    let cutoff = SystemTime::UNIX_EPOCH + Duration::from_secs(cutoff_secs);
    let files = session_files(roots, cutoff);
    if files.is_empty() {
        return Err("no Codex sessions yet: numbers appear after the first chat".into());
    }
    let mut newest: Option<Limits> = None;
    let mut tokens: BTreeMap<String, TokenCount> = BTreeMap::new();
    for path in files {
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        let usage = read_session(BufReader::new(file.take(MAX_FILE_BYTES)), cutoff_secs);
        for (model, t) in usage.tokens {
            tokens.entry(model).or_default().add(&t);
        }
        if let Some(l) = usage.limits {
            if newest.as_ref().is_none_or(|n| l.0 > n.0) {
                newest = Some(l);
            }
        }
    }
    let mut fetched = Fetched::default();
    if let Some((_, windows, plan, credits)) = newest {
        fetched.windows = windows;
        fetched.plan = plan;
        fetched.credits.extend(credits);
    }
    if !tokens.is_empty() {
        fetched.tokens = Some(TokenTotals {
            since: cutoff_secs,
            models: tokens
                .into_iter()
                .map(|(model, tokens)| ModelTokens {
                    model,
                    tokens,
                    cost_micros: None,
                    currency: None,
                })
                .collect(),
            source: SOURCE.into(),
        });
    }
    if fetched.windows.is_empty() && fetched.tokens.is_none() && fetched.plan.is_none() {
        return Err("Codex's session files hold no usage numbers yet".into());
    }
    Ok(fetched)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("fixtures/codex_rollout.jsonl");

    #[test]
    fn the_newest_rate_limits_and_tokens_per_model_are_read() {
        let u = read_session(FIXTURE.as_bytes(), 0);
        let (at, windows, plan, credits) = u.limits.unwrap();
        assert_eq!(at, 1_800_000_600);
        assert_eq!(plan.as_deref(), Some("plus"));
        let primary = &windows[0];
        assert_eq!(primary.id, "primary");
        assert_eq!(primary.label, "5-hour");
        assert_eq!(primary.kind, WindowKind::Session);
        assert!((primary.used_pct - 105.0).abs() < 1e-9);
        assert_eq!(primary.resets_at, Some(1_800_010_000));
        let secondary = &windows[1];
        assert_eq!(secondary.label, "Weekly");
        // Older files give the time left instead of the reset time.
        assert_eq!(secondary.resets_at, Some(1_800_000_600 + 86_400));
        let credits = credits.unwrap();
        assert_eq!(credits.balance, Some(12.5));
        assert_eq!(credits.enabled, Some(true));
        // Two models: the totals' differences go to the model of the turn they belong to.
        let a = u.tokens["example-model-a"];
        assert_eq!((a.input, a.cache_read, a.output), (700, 300, 100));
        let b = u.tokens["example-model-b"];
        assert_eq!(
            (b.input, b.cache_read, b.output, b.reasoning),
            (400, 100, 50, 20)
        );
    }

    #[test]
    fn events_before_the_cutoff_add_no_tokens() {
        let u = read_session(FIXTURE.as_bytes(), 1_800_000_300);
        assert!(!u.tokens.contains_key("example-model-a"));
        assert!(u.tokens.contains_key("example-model-b"));
    }

    #[test]
    fn broken_lines_and_other_meters_are_skipped() {
        let text = "not json\n{\"type\":\"event_msg\",\"timestamp\":\"2027-01-15T08:00:00Z\",\
                    \"payload\":{\"type\":\"token_count\",\"info\":null,\"rate_limits\":\
                    {\"limit_id\":\"other\",\"primary\":{\"used_percent\":5}}}}\n{\"type\":";
        let u = read_session(text.as_bytes(), 0);
        assert!(u.limits.is_none());
        assert!(u.tokens.is_empty());
    }

    #[test]
    fn files_are_found_under_dated_folders() {
        let root = crate::test_support::fresh_path("codex");
        let day = root.join("2027/01/15");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(day.join("rollout-2027-01-15T08-00-00-x.jsonl"), FIXTURE).unwrap();
        std::fs::write(day.join("notes.txt"), "x").unwrap();
        let files = session_files(std::slice::from_ref(&root), SystemTime::UNIX_EPOCH);
        assert_eq!(files.len(), 1);
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let f = collect(std::slice::from_ref(&root), now).unwrap();
        assert_eq!(f.windows.len(), 2);
        assert_eq!(f.plan.as_deref(), Some("plus"));
        // The fixture's events are dated in 2027: outside the last 30 days of `now` unless
        // the clock says otherwise, so tokens may or may not count; windows always do.
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_files_is_unavailable_not_an_error_of_the_panel() {
        let err = collect(&[PathBuf::from("/nonexistent/melissa")], 1).unwrap_err();
        assert!(err.contains("no Codex sessions"), "{err}");
    }
}
