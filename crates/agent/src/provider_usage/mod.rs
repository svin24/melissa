//! What each provider's plan allows and what was used: plan windows (a 5-hour window, a
//! week, a month), credits, cost and tokens per model. The data layer behind Usage and
//! resources (its Limits, Cost and Tokens views, `melissa_web`'s `usage`); the owner also
//! reads it as JSON at `GET /api/usage/providers`.
//!
//! Rules (docs/DECISIONS.md, 2026-11-20 and 2026-11-22):
//! - **Counted, never estimated.** Every number is one a provider reported. There is no price
//!   table and no cost worked out from tokens; a provider that reports no cost has none.
//! - **Each source fails on its own.** A source that cannot answer shows up as
//!   [`SourceState::Unavailable`] with a reason; the others still report.
//! - **Sign-in tokens stay put.** Where Melissa reads an agent's sign-in token from the
//!   agents' home (Gemini, OpenCode), it is sent only to that provider's own host, never
//!   logged, never stored, never shown to an agent or the UI ([`https::Secret`]).
//! - **Only when asked.** Remote calls happen when the panel opens or is refreshed, at most
//!   once per [`collect::CACHE_FOR`]; nothing polls in the background. Providers that are not
//!   installed or not signed in are skipped without any call.
//!
//! Where numbers come from, per provider:
//! - **Claude** (stream-json): `rate_limit_event`s during turns ([`claude::rate_limit_event`],
//!   kept per window in the [`hub::UsageHub`]); `result.modelUsage` per turn (tokens and
//!   Claude's own cost per model, [`claude::model_usage`]); and the `get_usage` control request
//!   in a short session with no prompt ([`claude::get_usage`]). Claude makes its own call to
//!   its own host inside its sandbox.
//! - **Codex**: the `token_count` events with `rate_limits` in the session files under each
//!   conversation's state directory ([`codex`]); no network.
//! - **Grok**: the ACP extension method `_x.ai/billing` in a short session with no prompt
//!   ([`grok`]); Grok calls its own host inside its sandbox.
//! - **Gemini CLI**: Code Assist's `loadCodeAssist` (tier, project) and `retrieveUserQuota`
//!   (per-model buckets) on `cloudcode-pa.googleapis.com`, with the Google sign-in in the
//!   agents' home ([`gemini`]). Melissa makes this call.
//! - **OpenCode**: the Go plan's windows from `opencode.ai/zen/go/v1/usage` with the Go key in
//!   the agents' home ([`opencode`]). Melissa makes this call. Session cost comes over ACP
//!   (already counted per conversation in [`crate::lead::usage`]).
//! - **Muse Code**: nothing yet. Muse reports usage only over its own protocol, not `muse acp`.

pub mod claude;
pub mod codex;
pub mod collect;
pub mod gemini;
pub mod grok;
pub mod https;
pub mod hub;
pub mod opencode;
pub mod pace;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use collect::{Collector, CollectorParts};
pub use hub::UsageHub;
pub use pace::Pace;

/// Everything the panel shows, for every provider Melissa has.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsageReport {
    /// When this report was put together (Unix seconds).
    pub generated_at: u64,
    /// When the providers' sources were last asked (Unix seconds): the start of the round
    /// this report reuses. `None` before the first round.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<u64>,
    pub providers: Vec<ProviderUsage>,
}

/// One provider's plan, windows, credits, cost and tokens.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProviderUsage {
    /// The provider id as in Settings → Providers (`claude`, `grok`, `opencode`, `gemini`,
    /// `codex-acp`, `muse`, or an added provider's id).
    pub provider: String,
    /// Display name.
    pub label: String,
    /// Installed, and signed in where Melissa can tell. Nothing is fetched otherwise.
    pub ready: bool,
    /// Why not ready (`not installed`, `not signed in`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_ready: Option<String>,
    /// The plan or tier as the provider names it (`max`, `plus`, `standard-tier`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// Usage windows, most important first (session-length, then weekly, then monthly).
    pub windows: Vec<Window>,
    /// Credit pools (Claude's extra usage, Grok's credits, Codex credits).
    pub credits: Vec<Credits>,
    /// Cost as the provider reports it.
    pub cost: Vec<Cost>,
    /// Tokens per model, where the provider reports them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenTotals>,
    /// What each source of this provider did on the last collection.
    pub sources: Vec<SourceStatus>,
}

/// How long a window is, roughly (for grouping; `window_secs` has the length when known).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowKind {
    /// A few hours (Claude's and Codex's 5-hour window).
    Session,
    Daily,
    Weekly,
    Monthly,
    Other,
}

impl WindowKind {
    /// The kind a window of `secs` seconds is.
    pub fn for_length(secs: u64) -> Self {
        const DAY: u64 = 24 * 3600;
        match secs {
            0 => Self::Other,
            s if s < DAY => Self::Session,
            s if s < 7 * DAY => Self::Daily,
            s if s < 28 * DAY => Self::Weekly,
            _ => Self::Monthly,
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Session => 0,
            Self::Daily => 1,
            Self::Weekly => 2,
            Self::Monthly => 3,
            Self::Other => 4,
        }
    }
}

/// A usage window of a plan: how much of it is used and when it starts over.
///
/// Percentages are never NaN or infinite: parsers drop such values (see [`finite`]), which is
/// why the type can be `Eq`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Window {
    /// Stable id within the provider (`five_hour`, `seven_day`, `model:Fable`, `primary`).
    pub id: String,
    /// Short words (`5-hour`, `Weekly`, `Weekly · Fable`).
    pub label: String,
    pub kind: WindowKind,
    /// Share used, in percent. May exceed 100 (Codex and Claude report overruns).
    pub used_pct: f64,
    /// When it starts over (Unix seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<u64>,
    /// Its length in seconds, when known (needed for [`Pace`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_secs: Option<u64>,
    /// The model (or model family) this window is limited to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_scope: Option<String>,
    /// The provider's status for it, when it sends one (`allowed`, `allowed_warning`,
    /// `rejected`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Which source reported it (`claude.stream`, `claude.get_usage`, `codex.sessions`, ...).
    pub source: String,
    /// When the provider reported it (Unix seconds).
    pub observed_at: u64,
    /// Where usage "should" be at this point of the window: filled in when the report is put
    /// together (needs the length and the reset time).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pace: Option<Pace>,
}

impl Eq for Window {}

/// A pool of credits or money on top of (or instead of) the windows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Credits {
    /// Stable id within the provider (`extra_usage`, `credits`).
    pub id: String,
    pub label: String,
    /// What the amounts count (`USD`, `credits`, ...). None: the provider gave no unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
    /// What is left (a prepaid balance).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance: Option<f64>,
    /// Share used, in percent, as the provider reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_pct: Option<f64>,
    /// Whether the pool is switched on, when the provider says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Unlimited (Codex: an unlimited credit balance).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unlimited: bool,
    /// The period it counts over (Unix seconds), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_start: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_end: Option<u64>,
    pub source: String,
}

impl Eq for Credits {}

/// What a cost covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostPeriod {
    /// Since Melissa started counting for this provider (`TokenTotals::since`).
    SinceCounting,
}

/// A cost the provider reported (its own figure; Claude Code calls its figure an estimate).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cost {
    /// Millionths of `currency`.
    pub micros: u64,
    /// `USD`, `credits`, ...
    pub currency: String,
    pub period: CostPeriod,
    /// Start of the period (Unix seconds).
    pub since: u64,
    pub source: String,
}

/// Token counts as a provider reports them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenCount {
    /// Input tokens not read from or written to a cache.
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_write: u64,
    /// Reasoning tokens, where reported separately (already part of `output` for Codex).
    #[serde(default)]
    pub reasoning: u64,
}

impl TokenCount {
    pub fn add(&mut self, other: &TokenCount) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
        self.reasoning = self.reasoning.saturating_add(other.reasoning);
    }

    /// `self - before`, field by field; a field that went down counts from zero (a new
    /// process started its own running total).
    pub fn since(&self, before: &TokenCount) -> TokenCount {
        let d = |now: u64, then: u64| if now >= then { now - then } else { now };
        TokenCount {
            input: d(self.input, before.input),
            output: d(self.output, before.output),
            cache_read: d(self.cache_read, before.cache_read),
            cache_write: d(self.cache_write, before.cache_write),
            reasoning: d(self.reasoning, before.reasoning),
        }
    }

    pub fn is_zero(&self) -> bool {
        *self == TokenCount::default()
    }
}

/// Tokens of one model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelTokens {
    pub model: String,
    pub tokens: TokenCount,
    /// The provider's cost figure for these tokens, in millionths of `currency`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micros: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
}

/// Tokens per model over a period.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenTotals {
    /// Start of the period (Unix seconds).
    pub since: u64,
    pub models: Vec<ModelTokens>,
    pub source: String,
}

/// What one source did on the last collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceStatus {
    /// `claude.get_usage`, `gemini.quota`, ...
    pub id: String,
    pub state: SourceState,
    /// Why it is unavailable or off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The host Melissa itself called for it, if any (agents' own calls are not listed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// When it last answered (Unix seconds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
    /// Answered (possibly from the short cache).
    Ok,
    /// Tried and failed, or has nothing to say yet (`reason` says which).
    Unavailable,
    /// Not tried: the provider is not installed or not signed in.
    Off,
}

/// A usage report from inside a session ([`crate::session::SessionEvent::ProviderUsage`]).
#[derive(Debug, Clone, PartialEq)]
pub enum Observation {
    /// Plan windows reported during a turn (Claude's `rate_limit_event`); each replaces the
    /// one with its id.
    Windows(Vec<Window>),
    /// Running totals of the agent process per model (Claude's `result.modelUsage`; they grow
    /// over the process's life, so the hub counts the difference to the last report).
    ProcessTotals(Vec<ModelTokens>),
}

impl Eq for Observation {}

/// `v` if it is a usable number.
pub(crate) fn finite(v: f64) -> Option<f64> {
    v.is_finite().then_some(v)
}

/// The provider id of a session backend's command (its file name): Settings' provider ids.
pub fn provider_for_command(command: &str) -> String {
    let name = std::path::Path::new(command)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(command);
    name.to_string()
}

/// Display name of a provider id.
pub fn provider_label(provider: &str) -> String {
    match provider {
        "claude" => "Claude".into(),
        "grok" => "Grok".into(),
        "opencode" => "OpenCode".into(),
        "gemini" => "Gemini".into(),
        "codex-acp" => "Codex".into(),
        "muse" => "Muse".into(),
        other => other.to_string(),
    }
}

/// Order windows: shortest kind first, then model-wide before model-scoped, then by id.
pub(crate) fn sort_windows(windows: &mut [Window]) {
    windows.sort_by(|a, b| {
        (a.kind.rank(), a.model_scope.is_some(), &a.id).cmp(&(
            b.kind.rank(),
            b.model_scope.is_some(),
            &b.id,
        ))
    });
}

/// Unix seconds now.
pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// An RFC 3339 time (`2026-11-22T10:00:00Z`, with fractions or an offset) as Unix seconds.
pub(crate) fn parse_time(s: &str) -> Option<u64> {
    let t: jiff::Timestamp = s.parse().ok()?;
    u64::try_from(t.as_second()).ok()
}

/// A number given as a number, a decimal string or `{"val": ...}`.
pub(crate) fn json_num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64().and_then(finite),
        Value::String(s) => s.trim().parse::<f64>().ok().and_then(finite),
        Value::Object(o) => o.get("val").and_then(json_num),
        _ => None,
    }
}

/// A time given as ISO 8601 or Unix seconds or milliseconds.
pub(crate) fn json_time(v: &Value) -> Option<u64> {
    match v {
        Value::String(s) => parse_time(s).or_else(|| s.parse::<u64>().ok().map(secs_or_millis)),
        Value::Number(n) => n.as_u64().map(secs_or_millis),
        _ => None,
    }
}

fn secs_or_millis(t: u64) -> u64 {
    if t > 100_000_000_000 {
        t / 1000
    } else {
        t
    }
}

/// Words for a window length (`5-hour`, `Weekly`, `30-day`).
pub(crate) fn length_words(secs: u64) -> String {
    const H: u64 = 3600;
    const D: u64 = 24 * H;
    match secs {
        s if s == 7 * D => "Weekly".into(),
        s if s == D => "Daily".into(),
        s if s % D == 0 && s >= D => format!("{}-day", s / D),
        s if s % H == 0 && s >= H => format!("{}-hour", s / H),
        s => format!("{}-minute", s / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_kinds_follow_their_length() {
        assert_eq!(WindowKind::for_length(5 * 3600), WindowKind::Session);
        assert_eq!(WindowKind::for_length(24 * 3600), WindowKind::Daily);
        assert_eq!(WindowKind::for_length(7 * 24 * 3600), WindowKind::Weekly);
        assert_eq!(WindowKind::for_length(30 * 24 * 3600), WindowKind::Monthly);
        assert_eq!(WindowKind::for_length(0), WindowKind::Other);
    }

    #[test]
    fn times_parse_with_offsets_and_fractions() {
        assert_eq!(parse_time("1970-01-01T00:01:00Z"), Some(60));
        assert_eq!(parse_time("1970-01-01T01:01:00.250+01:00"), Some(60));
        assert_eq!(parse_time("soon"), None);
    }

    #[test]
    fn token_differences_restart_from_zero_when_a_count_went_down() {
        let before = TokenCount {
            input: 10,
            output: 5,
            ..TokenCount::default()
        };
        let now = TokenCount {
            input: 15,
            output: 2,
            ..TokenCount::default()
        };
        let d = now.since(&before);
        assert_eq!((d.input, d.output), (5, 2));
    }
}
