//! What a conversation (Melissa's or a helper's) has used: turns, time spent in turns,
//! approvals it asked for, and what its provider reports (tokens in context, session cost).
//! Kept small: one `usage.json` in the conversation's state directory, written once per turn,
//! removed with the helper. Nothing here is estimated: a provider that reports no cost has none.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

/// The file name in a conversation's state directory.
pub const FILE: &str = "usage.json";

/// Tokens in the context window, as the provider last reported them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    pub used: u64,
    pub size: u64,
}

/// A conversation's counters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Finished turns (messages, jobs and scheduled runs it answered).
    #[serde(default)]
    pub turns: u64,
    /// Time spent in turns, in milliseconds.
    #[serde(default)]
    pub busy_ms: u64,
    /// Confirmations its tool calls asked the owner for.
    #[serde(default)]
    pub approvals: u64,
    /// The last report of tokens in context (None: the provider never reported it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<Context>,
    /// Cost of finished sessions by unit (`USD`, `credits`), in millionths.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cost_banked: BTreeMap<String, u64>,
    /// The current session's cost so far (providers report a running total per session).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_cost: Option<SessionCost>,
    /// When counting started (Unix seconds).
    #[serde(default)]
    pub since: u64,
}

/// A session's running cost, as its provider reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCost {
    /// Which session (its MCP token), so a new session starts a new running total.
    pub session: String,
    pub unit: String,
    pub micros: u64,
}

impl Usage {
    /// A fresh record starting now.
    pub fn starting(now: u64) -> Self {
        Self {
            since: now,
            ..Self::default()
        }
    }

    /// Read `<dir>/usage.json`; a missing or unreadable file reads as nothing counted yet.
    pub fn load(dir: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path(dir)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Write `<dir>/usage.json` (a failure is logged: counters are not worth failing a turn).
    pub fn save(&self, dir: &Path) {
        let file = path(dir);
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(dir)?;
            let tmp = dir.join(".usage.json.tmp");
            std::fs::write(
                &tmp,
                serde_json::to_vec(self).map_err(std::io::Error::other)?,
            )?;
            std::fs::rename(&tmp, &file)
        };
        if let Err(e) = write() {
            tracing::warn!(path = %file.display(), "cannot save usage counters: {e}");
        }
    }

    /// A turn ended after `ms` and asked for `approvals` confirmations.
    pub fn count_turn(&mut self, ms: u64, approvals: u64) {
        self.turns += 1;
        self.busy_ms = self.busy_ms.saturating_add(ms);
        self.approvals += approvals;
    }

    /// The provider's running cost of `session` is now `micros` of `unit`. A new session
    /// banks the previous one's total first.
    pub fn report_cost(&mut self, session: &str, unit: &str, micros: u64) {
        // A new session, or a unit change within one (kept apart, never mixed): bank it.
        if let Some(prev) = self
            .session_cost
            .take()
            .filter(|p| p.session != session || p.unit != unit)
        {
            let banked = self.cost_banked.entry(prev.unit).or_default();
            *banked = banked.saturating_add(prev.micros);
        }
        self.session_cost = Some(SessionCost {
            session: session.into(),
            unit: unit.into(),
            micros,
        });
    }

    /// Total reported cost per unit (finished sessions plus the current one).
    pub fn cost(&self) -> BTreeMap<String, u64> {
        let mut out = self.cost_banked.clone();
        if let Some(s) = &self.session_cost {
            let total = out.entry(s.unit.clone()).or_default();
            *total = total.saturating_add(s.micros);
        }
        out
    }
}

fn path(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turns_time_and_approvals_add_up() {
        let mut u = Usage::starting(100);
        u.count_turn(1_500, 0);
        u.count_turn(500, 2);
        assert_eq!(
            (u.turns, u.busy_ms, u.approvals, u.since),
            (2, 2_000, 2, 100)
        );
    }

    #[test]
    fn session_cost_is_a_running_total_banked_on_a_new_session() {
        let mut u = Usage::default();
        assert!(u.cost().is_empty(), "no report, no cost");
        u.report_cost("s1", "USD", 10_000);
        u.report_cost("s1", "USD", 25_000);
        assert_eq!(u.cost().get("USD"), Some(&25_000), "same session: replaced");
        u.report_cost("s2", "USD", 5_000);
        assert_eq!(
            u.cost().get("USD"),
            Some(&30_000),
            "new session: banked + new"
        );
        u.report_cost("s2", "credits", 3_000_000);
        assert_eq!(u.cost().get("USD"), Some(&30_000));
        assert_eq!(u.cost().get("credits"), Some(&3_000_000));
    }

    #[test]
    fn saves_and_loads_and_reads_a_missing_file_as_none() {
        let dir = std::env::temp_dir().join(format!("melissa-usage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(Usage::load(&dir), None);
        let mut u = Usage::starting(7);
        u.count_turn(10, 1);
        u.context = Some(Context {
            used: 1_000,
            size: 200_000,
        });
        u.report_cost("s", "USD", 42);
        u.save(&dir);
        assert_eq!(Usage::load(&dir), Some(u));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
