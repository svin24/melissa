//! For the screenshot harness only (`scripts/ui-shots`): the providers' plan usage from a JSON
//! file, so Usage and resources can be shot with realistic numbers and no provider sign-ins.
//! Compiled only with the `fake-usage` feature (`cargo build --features
//! melissa-server/fake-usage`); normal and release builds don't have it.
//!
//! `MELISSA_FAKE_USAGE` names the file, read at every collection:
//!
//! ```json
//! {"providers": [{"id": "claude", "windows": [{"id": "five_hour", "label": "5-hour",
//!   "kind": "session", "used_pct": 38, "resets_in": 7800}], "cost": {"USD": 2460000},
//!   "tokens": [{"model": "fable", "input": 1200, "output": 300}], "unavailable": "reason"}]}
//! ```
//!
//! A window's reset is `resets_at` (Unix seconds) or `resets_in` (seconds from the
//! collection). `unavailable` makes the provider's source fail with that reason. The real
//! collector's cache and refresh limits apply unchanged.

use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

use melissa_agent::{
    provider_usage::{
        collect::{Fetched, Miss, ProviderEntry, Source},
        provider_label, Collector, Cost, CostPeriod, ModelTokens, TokenCount, TokenTotals,
        UsageHub, Window, WindowKind,
    },
    BoxFuture,
};
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
struct File {
    #[serde(default)]
    providers: Vec<FakeProvider>,
}

#[derive(Debug, Clone, Deserialize)]
struct FakeProvider {
    id: String,
    #[serde(default)]
    plan: Option<String>,
    #[serde(default)]
    windows: Vec<FakeWindow>,
    /// Micros per currency.
    #[serde(default)]
    cost: BTreeMap<String, u64>,
    #[serde(default)]
    tokens: Vec<FakeTokens>,
    #[serde(default)]
    unavailable: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct FakeWindow {
    id: String,
    label: String,
    kind: WindowKind,
    used_pct: f64,
    #[serde(default)]
    resets_at: Option<u64>,
    #[serde(default)]
    resets_in: Option<u64>,
    #[serde(default)]
    model_scope: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct FakeTokens {
    model: String,
    #[serde(default)]
    input: u64,
    #[serde(default)]
    output: u64,
    #[serde(default)]
    cache_read: u64,
    #[serde(default)]
    cache_write: u64,
}

/// One provider's fake source.
struct FakeSource {
    provider: FakeProvider,
    source: String,
}

impl Source for FakeSource {
    fn provider(&self) -> &str {
        &self.provider.id
    }

    fn id(&self) -> &str {
        &self.source
    }

    fn fetch(&self, now: u64) -> BoxFuture<'_, Result<Fetched, Miss>> {
        Box::pin(async move { fetched(&self.provider, &self.source, now) })
    }
}

fn fetched(p: &FakeProvider, source: &str, now: u64) -> Result<Fetched, Miss> {
    if let Some(reason) = &p.unavailable {
        return Err(Miss::Unavailable(reason.clone()));
    }
    let windows = p
        .windows
        .iter()
        .map(|w| Window {
            id: w.id.clone(),
            label: w.label.clone(),
            kind: w.kind,
            used_pct: w.used_pct,
            resets_at: w.resets_at.or(w.resets_in.map(|s| now + s)),
            window_secs: None,
            model_scope: w.model_scope.clone(),
            status: w.status.clone(),
            source: source.into(),
            observed_at: now,
            pace: None,
        })
        .collect();
    let cost = p
        .cost
        .iter()
        .map(|(currency, micros)| Cost {
            micros: *micros,
            currency: currency.clone(),
            period: CostPeriod::SinceCounting,
            since: now.saturating_sub(2 * 24 * 3600),
            source: source.into(),
        })
        .collect();
    let tokens = (!p.tokens.is_empty()).then(|| TokenTotals {
        since: now.saturating_sub(2 * 24 * 3600),
        models: p
            .tokens
            .iter()
            .map(|t| ModelTokens {
                model: t.model.clone(),
                tokens: TokenCount {
                    input: t.input,
                    output: t.output,
                    cache_read: t.cache_read,
                    cache_write: t.cache_write,
                    reasoning: 0,
                },
                cost_micros: None,
                currency: None,
            })
            .collect(),
        source: source.into(),
    });
    Ok(Fetched {
        plan: p.plan.clone(),
        windows,
        credits: Vec::new(),
        cost,
        tokens,
    })
}

/// The file's providers and their sources (a missing or broken file: none).
fn plan() -> (Vec<ProviderEntry>, Vec<Arc<dyn Source>>) {
    let file: File = std::env::var_os("MELISSA_FAKE_USAGE")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let mut entries = Vec::new();
    let mut sources: Vec<Arc<dyn Source>> = Vec::new();
    for p in file.providers {
        let source = format!("{}.fake", p.id);
        entries.push(ProviderEntry {
            id: p.id.clone(),
            label: provider_label(&p.id),
            ready: Ok(()),
            sources: vec![source.clone()],
        });
        sources.push(Arc::new(FakeSource {
            provider: p,
            source,
        }));
    }
    (entries, sources)
}

/// The fake collector when the harness asked for one (`MELISSA_FAKE_USAGE` set).
pub(crate) fn collector() -> Option<Collector> {
    static C: OnceLock<Collector> = OnceLock::new();
    std::env::var_os("MELISSA_FAKE_USAGE")?;
    Some(
        C.get_or_init(|| Collector::with_planner(UsageHub::new(), Arc::new(plan)))
            .clone(),
    )
}
