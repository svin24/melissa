//! What sessions report while they run, per provider: the latest value of each plan window
//! and the tokens (and the provider's own cost figure) per model since counting started.
//! Every conversation (Melissa's and each helper's) feeds the same hub. Kept in one small
//! `provider-usage.json` in Melissa's state directory, written when tokens are counted (once
//! per turn) and when a window changes.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};

use super::{ModelTokens, Observation, TokenCount, Window};

/// The file name in Melissa's state directory.
pub const FILE: &str = "provider-usage.json";
/// Running totals remembered per agent process (only to count differences); the oldest go.
const MAX_PROCESSES: usize = 256;

/// One provider's counters.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Counted {
    /// Latest report of each window, by window id.
    #[serde(default)]
    pub windows: BTreeMap<String, Window>,
    /// Tokens and cost per model since `since`.
    #[serde(default)]
    pub models: BTreeMap<String, ModelCount>,
    /// When counting started (Unix seconds).
    #[serde(default)]
    pub since: u64,
}

/// Tokens and cost of one model.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCount {
    #[serde(default)]
    pub tokens: TokenCount,
    /// Cost by currency, in millionths.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cost: BTreeMap<String, u64>,
}

/// Last running totals per (provider, process key), with an age for eviction.
type Processes = HashMap<(String, String), (u64, BTreeMap<String, ModelTokens>)>;

#[derive(Default)]
struct State {
    providers: BTreeMap<String, Counted>,
    processes: Processes,
    tick: u64,
}

/// Shared handle. Cheap to clone.
#[derive(Clone, Default)]
pub struct UsageHub {
    state: Arc<Mutex<State>>,
    /// Where it is saved (`None`: memory only, tests).
    path: Option<PathBuf>,
}

impl UsageHub {
    /// A hub kept in memory only.
    pub fn new() -> Self {
        Self::default()
    }

    /// A hub saved in `<dir>/provider-usage.json`, starting from what is there (a missing or
    /// unreadable file starts empty).
    pub fn open(dir: &Path) -> Self {
        let path = dir.join(FILE);
        let providers = melissa_statefile::read_json(&path, &crate::formats::PROVIDER_USAGE)
            .ok()
            .flatten()
            .map(|l| l.value)
            .unwrap_or_default();
        Self {
            state: Arc::new(Mutex::new(State {
                providers,
                ..State::default()
            })),
            path: Some(path),
        }
    }

    /// Take in one report. `process` identifies the agent process (a new one starts its own
    /// running totals); `now` is Unix seconds.
    pub fn observe(&self, provider: &str, process: &str, obs: Observation, now: u64) {
        let mut state = lock(&self.state);
        let changed = match obs {
            Observation::Windows(windows) => {
                let counted = counted(&mut state, provider, now);
                let mut changed = false;
                for w in windows {
                    if counted.windows.get(&w.id) != Some(&w) {
                        counted.windows.insert(w.id.clone(), w);
                        changed = true;
                    }
                }
                changed
            }
            Observation::ProcessTotals(models) => {
                state.tick += 1;
                let tick = state.tick;
                let key = (provider.to_string(), process.to_string());
                let before = state
                    .processes
                    .insert(
                        key,
                        (
                            tick,
                            models
                                .iter()
                                .map(|m| (m.model.clone(), m.clone()))
                                .collect(),
                        ),
                    )
                    .map(|(_, m)| m)
                    .unwrap_or_default();
                evict(&mut state.processes);
                let counted = counted(&mut state, provider, now);
                let mut changed = false;
                for m in models {
                    let prev = before.get(&m.model);
                    let tokens = prev.map_or(m.tokens, |p| m.tokens.since(&p.tokens));
                    let entry = counted.models.entry(m.model.clone()).or_default();
                    if !tokens.is_zero() {
                        entry.tokens.add(&tokens);
                        changed = true;
                    }
                    if let (Some(cost), Some(currency)) = (m.cost_micros, m.currency.as_ref()) {
                        let then = prev
                            .filter(|p| p.currency.as_ref() == Some(currency))
                            .and_then(|p| p.cost_micros)
                            .unwrap_or(0);
                        let add = if cost >= then { cost - then } else { cost };
                        if add > 0 {
                            let c = entry.cost.entry(currency.clone()).or_default();
                            *c = c.saturating_add(add);
                            changed = true;
                        }
                    }
                }
                changed
            }
        };
        if changed {
            self.save(&state);
        }
    }

    /// What was counted for `provider` so far.
    pub fn counted(&self, provider: &str) -> Option<Counted> {
        lock(&self.state).providers.get(provider).cloned()
    }

    fn save(&self, state: &State) {
        let Some(path) = &self.path else { return };
        if let Err(e) = melissa_statefile::save_json(
            path,
            &crate::formats::PROVIDER_USAGE,
            &state.providers,
            false,
        ) {
            tracing::warn!("cannot save provider usage: {e}");
        }
    }
}

fn counted<'a>(state: &'a mut State, provider: &str, now: u64) -> &'a mut Counted {
    state
        .providers
        .entry(provider.to_string())
        .or_insert_with(|| Counted {
            since: now,
            ..Counted::default()
        })
}

fn evict(processes: &mut Processes) {
    while processes.len() > MAX_PROCESSES {
        let oldest = processes
            .iter()
            .min_by_key(|(_, (tick, _))| *tick)
            .map(|(k, _)| k.clone());
        match oldest {
            Some(k) => {
                processes.remove(&k);
            }
            None => break,
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // A panic while holding the lock leaves plain counters behind; keep using them.
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_usage::WindowKind;

    fn totals(model: &str, input: u64, output: u64, cost: Option<u64>) -> Observation {
        Observation::ProcessTotals(vec![ModelTokens {
            model: model.into(),
            tokens: TokenCount {
                input,
                output,
                ..TokenCount::default()
            },
            cost_micros: cost,
            currency: cost.map(|_| "USD".into()),
        }])
    }

    fn window(id: &str, used: f64) -> Window {
        Window {
            id: id.into(),
            label: id.into(),
            kind: WindowKind::Session,
            used_pct: used,
            resets_at: Some(100),
            window_secs: Some(18_000),
            model_scope: None,
            status: None,
            source: "test".into(),
            observed_at: 1,
            pace: None,
        }
    }

    #[test]
    fn running_totals_count_only_what_is_new_per_process() {
        let hub = UsageHub::new();
        hub.observe("claude", "p1", totals("m", 100, 10, Some(1_000)), 5);
        hub.observe("claude", "p1", totals("m", 150, 30, Some(1_500)), 6);
        // Another process starts from zero.
        hub.observe("claude", "p2", totals("m", 40, 4, Some(200)), 7);
        let c = hub.counted("claude").unwrap();
        assert_eq!(c.since, 5);
        let m = &c.models["m"];
        assert_eq!((m.tokens.input, m.tokens.output), (190, 34));
        assert_eq!(m.cost["USD"], 1_700);
    }

    #[test]
    fn the_latest_report_of_a_window_wins() {
        let hub = UsageHub::new();
        hub.observe(
            "claude",
            "p",
            Observation::Windows(vec![window("five_hour", 10.0)]),
            1,
        );
        hub.observe(
            "claude",
            "p",
            Observation::Windows(vec![window("five_hour", 12.5)]),
            2,
        );
        let c = hub.counted("claude").unwrap();
        assert!((c.windows["five_hour"].used_pct - 12.5).abs() < 1e-9);
        assert!(hub.counted("grok").is_none());
    }

    #[test]
    fn counters_survive_a_restart() {
        let dir = crate::test_support::fresh_path("hub");
        let hub = UsageHub::open(&dir);
        hub.observe("claude", "p", totals("m", 7, 3, None), 1);
        let again = UsageHub::open(&dir);
        assert_eq!(again.counted("claude").unwrap().models["m"].tokens.input, 7);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_processes_are_forgotten() {
        let hub = UsageHub::new();
        for i in 0..(MAX_PROCESSES + 10) {
            hub.observe("claude", &format!("p{i}"), totals("m", 1, 0, None), 1);
        }
        assert_eq!(lock(&hub.state).processes.len(), MAX_PROCESSES);
    }
}
