//! Putting a [`UsageReport`] together: what the sessions reported (the [`UsageHub`]) plus
//! each provider's sources, asked only when the report is asked for and reused for
//! [`CACHE_FOR`]. Sources run in parallel, each in its own task with its own time limit, so
//! one that fails, hangs or panics shows up as unavailable and never holds up the others.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

use super::{
    claude, codex, gemini, grok,
    https::HttpClient,
    hub::{Counted, UsageHub},
    now_secs, opencode, pace, provider_label, sort_windows, Cost, CostPeriod, Credits, ModelTokens,
    ProviderUsage, SourceState, SourceStatus, TokenTotals, UsageReport, Window,
};
use crate::{lead::backends, session::SessionBackend, BoxFuture};

/// A report younger than this is reused as is.
pub const CACHE_FOR: Duration = Duration::from_secs(60);
/// Even an explicit refresh reuses a report younger than this (a refresh button pressed
/// twice makes one round of calls).
pub const MIN_REFRESH: Duration = Duration::from_secs(15);
/// The longest one source may take (a short agent session included).
pub const SOURCE_TIMEOUT: Duration = Duration::from_secs(45);

/// What a source found (every part optional).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Fetched {
    pub plan: Option<String>,
    pub windows: Vec<Window>,
    pub credits: Vec<Credits>,
    pub cost: Vec<Cost>,
    pub tokens: Option<TokenTotals>,
}

/// Why a source has nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Miss {
    /// Not asked: nothing to ask with (no sign-in, no key).
    Off(String),
    /// Asked and failed, or has nothing yet.
    Unavailable(String),
}

impl From<String> for Miss {
    fn from(reason: String) -> Self {
        Self::Unavailable(reason)
    }
}

/// One place numbers come from.
pub trait Source: Send + Sync {
    /// The provider it belongs to.
    fn provider(&self) -> &str;
    /// `claude.get_usage`, `gemini.quota`, ...
    fn id(&self) -> &str;
    /// The host Melissa itself calls, if any.
    fn host(&self) -> Option<&str> {
        None
    }
    fn fetch(&self, now: u64) -> BoxFuture<'_, Result<Fetched, Miss>>;
}

/// A provider as the collection sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderEntry {
    pub id: String,
    pub label: String,
    /// `Err(reason)`: not installed or not signed in; its sources are not asked.
    pub ready: Result<(), String>,
    /// Source ids it would have (listed as off when not ready).
    pub sources: Vec<String>,
}

/// The providers and their sources, worked out at each collection (sign-ins change).
pub type Plan = (Vec<ProviderEntry>, Vec<Arc<dyn Source>>);
type Planner = Arc<dyn Fn() -> Plan + Send + Sync>;

/// What [`Collector::new`] needs.
pub struct CollectorParts {
    pub hub: UsageHub,
    /// Every session backend (one per provider is used).
    pub backends: Vec<Arc<dyn SessionBackend>>,
    /// The agents' home (sign-in files). `None`: sources that need it are unavailable.
    pub home: Option<PathBuf>,
    /// The HTTPS client for Gemini's and OpenCode's endpoints. `None`: those unavailable.
    pub http: Option<Arc<dyn HttpClient>>,
    /// Where Codex keeps its sessions for each conversation (read when collecting).
    pub codex_roots: Arc<dyn Fn() -> Vec<PathBuf> + Send + Sync>,
    /// Short usage sessions run in `<probe_dir>/<backend>-usage` (empty directories).
    pub probe_dir: PathBuf,
}

/// One source's answer in a round.
struct Answer {
    provider: String,
    id: String,
    host: Option<String>,
    at: u64,
    result: Result<Fetched, Miss>,
}

/// The backend chosen for a provider and whether it can run.
type Chosen = (Arc<dyn SessionBackend>, Result<(), String>);

/// The last round of source answers.
struct Round {
    at: tokio::time::Instant,
    results: Vec<Answer>,
}

/// Puts reports together. Cheap to clone.
#[derive(Clone)]
pub struct Collector {
    hub: UsageHub,
    planner: Planner,
    last: Arc<tokio::sync::Mutex<Option<Round>>>,
}

impl Collector {
    pub fn new(parts: CollectorParts) -> Self {
        let hub = parts.hub.clone();
        let parts = Arc::new(parts);
        Self::with_planner(hub, Arc::new(move || plan(&parts)))
    }

    /// A collector with its own plan (tests).
    pub fn with_planner(hub: UsageHub, planner: Planner) -> Self {
        Self {
            hub,
            planner,
            last: Arc::default(),
        }
    }

    /// The live counters (sessions report into it).
    pub fn hub(&self) -> &UsageHub {
        &self.hub
    }

    /// The report: sources are asked unless the last round is younger than [`CACHE_FOR`]
    /// (with `refresh`, [`MIN_REFRESH`]). One round runs at a time; callers meanwhile wait
    /// and get its answers.
    pub async fn report(&self, refresh: bool) -> UsageReport {
        let (providers, sources) = (self.planner)();
        let mut last = self.last.lock().await;
        let keep = if refresh { MIN_REFRESH } else { CACHE_FOR };
        let fresh = last.as_ref().is_some_and(|r| r.at.elapsed() < keep);
        if !fresh {
            *last = Some(Round {
                at: tokio::time::Instant::now(),
                results: run_sources(sources).await,
            });
        }
        let results = last.as_ref().map(|r| r.results.as_slice()).unwrap_or(&[]);
        build(&providers, results, &self.hub, now_secs())
    }
}

/// Ask every source at once, each within [`SOURCE_TIMEOUT`].
async fn run_sources(sources: Vec<Arc<dyn Source>>) -> Vec<Answer> {
    let now = now_secs();
    let tasks: Vec<_> = sources
        .into_iter()
        .map(|s| {
            let meta = (
                s.provider().to_string(),
                s.id().to_string(),
                s.host().map(str::to_string),
            );
            let task = tokio::spawn(async move {
                tokio::time::timeout(SOURCE_TIMEOUT, s.fetch(now))
                    .await
                    .unwrap_or_else(|_| {
                        Err(Miss::Unavailable(format!(
                            "no answer within {} s",
                            SOURCE_TIMEOUT.as_secs()
                        )))
                    })
            });
            (meta, task)
        })
        .collect();
    let mut out = Vec::new();
    for ((provider, id, host), task) in tasks {
        let result = task.await.unwrap_or_else(|e| {
            tracing::warn!(source = %id, "provider usage source failed: {e}");
            Err(Miss::Unavailable("the source failed".into()))
        });
        if let Err(Miss::Unavailable(reason)) = &result {
            tracing::debug!(source = %id, "provider usage unavailable: {reason}");
        }
        out.push(Answer {
            provider,
            id,
            host,
            at: now,
            result,
        });
    }
    out
}

/// The report from the providers, the round's answers and the hub, at `now`.
fn build(providers: &[ProviderEntry], results: &[Answer], hub: &UsageHub, now: u64) -> UsageReport {
    let mut out = Vec::new();
    for p in providers {
        let mut usage = ProviderUsage {
            provider: p.id.clone(),
            label: p.label.clone(),
            ready: p.ready.is_ok(),
            not_ready: p.ready.clone().err(),
            plan: None,
            windows: Vec::new(),
            credits: Vec::new(),
            cost: Vec::new(),
            tokens: None,
            sources: Vec::new(),
        };
        let mut windows: BTreeMap<String, Window> = BTreeMap::new();
        if let Some(counted) = hub.counted(&p.id) {
            add_counted(&mut usage, &mut windows, &counted, &p.id);
        } else if p.id == "claude" && p.ready.is_ok() {
            usage.sources.push(SourceStatus {
                id: claude::SOURCE_STREAM.into(),
                state: SourceState::Unavailable,
                reason: Some("nothing reported yet: numbers come with the next chat".into()),
                host: None,
                fetched_at: None,
            });
        }
        if let Err(reason) = &p.ready {
            for id in &p.sources {
                usage.sources.push(SourceStatus {
                    id: id.clone(),
                    state: SourceState::Off,
                    reason: Some(reason.clone()),
                    host: None,
                    fetched_at: None,
                });
            }
        } else {
            for a in results.iter().filter(|a| a.provider == p.id) {
                let (state, reason, fetched_at) = match &a.result {
                    Ok(f) => {
                        merge(&mut usage, &mut windows, f);
                        (SourceState::Ok, None, Some(a.at))
                    }
                    Err(Miss::Off(r)) => (SourceState::Off, Some(r.clone()), None),
                    Err(Miss::Unavailable(r)) => (SourceState::Unavailable, Some(r.clone()), None),
                };
                usage.sources.push(SourceStatus {
                    id: a.id.clone(),
                    state,
                    reason,
                    host: a.host.clone(),
                    fetched_at,
                });
            }
        }
        let mut windows: Vec<Window> = windows.into_values().collect();
        for w in &mut windows {
            w.pace = match (w.resets_at, w.window_secs) {
                (Some(r), Some(len)) => pace::pace(w.used_pct, r, len, now),
                _ => None,
            };
        }
        sort_windows(&mut windows);
        usage.windows = windows;
        out.push(usage);
    }
    UsageReport {
        generated_at: now,
        providers: out,
    }
}

/// The source name of what sessions reported for `provider`.
fn hub_source(provider: &str) -> String {
    if provider == "claude" {
        claude::SOURCE_STREAM.into()
    } else {
        format!("{provider}.sessions")
    }
}

fn add_counted(
    usage: &mut ProviderUsage,
    windows: &mut BTreeMap<String, Window>,
    counted: &Counted,
    provider: &str,
) {
    let source = hub_source(provider);
    let mut latest = None::<u64>;
    for w in counted.windows.values() {
        latest = latest.max(Some(w.observed_at));
        keep_newer(windows, w.clone());
    }
    let mut cost: BTreeMap<String, u64> = BTreeMap::new();
    let models: Vec<ModelTokens> = counted
        .models
        .iter()
        .map(|(model, c)| {
            for (currency, micros) in &c.cost {
                let total = cost.entry(currency.clone()).or_default();
                *total = total.saturating_add(*micros);
            }
            // One currency per model in practice; with several, the tokens carry none.
            let single = (c.cost.len() == 1).then(|| c.cost.iter().next()).flatten();
            ModelTokens {
                model: model.clone(),
                tokens: c.tokens,
                cost_micros: single.map(|(_, m)| *m),
                currency: single.map(|(cur, _)| cur.clone()),
            }
        })
        .collect();
    if !models.is_empty() {
        usage.tokens = Some(TokenTotals {
            since: counted.since,
            models,
            source: source.clone(),
        });
    }
    usage.cost = cost
        .into_iter()
        .map(|(currency, micros)| Cost {
            micros,
            currency,
            period: CostPeriod::SinceCounting,
            since: counted.since,
            source: source.clone(),
        })
        .collect();
    usage.sources.push(SourceStatus {
        id: source,
        state: SourceState::Ok,
        reason: None,
        host: None,
        fetched_at: latest,
    });
}

fn merge(usage: &mut ProviderUsage, windows: &mut BTreeMap<String, Window>, f: &Fetched) {
    if usage.plan.is_none() {
        usage.plan.clone_from(&f.plan);
    }
    for w in &f.windows {
        keep_newer(windows, w.clone());
    }
    for c in &f.credits {
        if !usage.credits.iter().any(|have| have.id == c.id) {
            usage.credits.push(c.clone());
        }
    }
    usage.cost.extend(f.cost.iter().cloned());
    if let Some(t) = &f.tokens {
        usage.tokens = Some(t.clone());
    }
}

/// Keep the newer report of a window (same id).
fn keep_newer(windows: &mut BTreeMap<String, Window>, w: Window) {
    match windows.get(&w.id) {
        Some(have) if have.observed_at > w.observed_at => {}
        _ => {
            windows.insert(w.id.clone(), w);
        }
    }
}

/// The source ids a provider has.
fn source_ids(provider: &str) -> &'static [&'static str] {
    match provider {
        "claude" => &[claude::SOURCE_GET_USAGE],
        "grok" => &[grok::SOURCE],
        "codex-acp" => &[codex::SOURCE],
        "gemini" => &[gemini::SOURCE],
        "opencode" => &[opencode::SOURCE],
        "muse" => &[MUSE_SOURCE],
        _ => &[],
    }
}

const MUSE_SOURCE: &str = "muse.usage";

/// The real plan: providers from the session backends, their sources.
fn plan(parts: &Arc<CollectorParts>) -> Plan {
    // One backend per provider: the first one that can run (else the first one seen).
    let mut chosen: BTreeMap<String, Chosen> = BTreeMap::new();
    for b in &parts.backends {
        let Some(id) = b.provider_id() else { continue };
        let info = b.info();
        let ready = if !info.available {
            Err("not installed".to_string())
        } else {
            match backends::reason(b.not_ready()) {
                None => Ok(()),
                Some("not_logged_in") => Err("not signed in".into()),
                Some("not_installed") => Err("not installed".into()),
                Some(other) => Err(other.replace('_', " ")),
            }
        };
        let better = chosen
            .get(&id)
            .is_none_or(|(_, have)| have.is_err() && ready.is_ok());
        if better {
            chosen.insert(id, (b.clone(), ready));
        }
    }
    let mut providers = Vec::new();
    let mut sources: Vec<Arc<dyn Source>> = Vec::new();
    for (id, (backend, ready)) in chosen {
        if ready.is_ok() {
            sources.extend(source_for(&id, backend, parts));
        }
        providers.push(ProviderEntry {
            label: provider_label(&id),
            sources: source_ids(&id).iter().map(|s| (*s).to_string()).collect(),
            id,
            ready,
        });
    }
    (providers, sources)
}

fn source_for(
    provider: &str,
    backend: Arc<dyn SessionBackend>,
    parts: &Arc<CollectorParts>,
) -> Option<Arc<dyn Source>> {
    let kind = match provider {
        "claude" => Kind::ClaudeGetUsage,
        "grok" => Kind::GrokBilling,
        "codex-acp" => Kind::CodexFiles,
        "gemini" => Kind::GeminiQuota,
        "opencode" => Kind::OpencodeGo,
        "muse" => Kind::Muse,
        _ => return None,
    };
    Some(Arc::new(Real {
        provider: provider.to_string(),
        kind,
        backend,
        parts: parts.clone(),
    }))
}

#[derive(Clone, Copy)]
enum Kind {
    ClaudeGetUsage,
    GrokBilling,
    CodexFiles,
    GeminiQuota,
    OpencodeGo,
    Muse,
}

struct Real {
    provider: String,
    kind: Kind,
    backend: Arc<dyn SessionBackend>,
    parts: Arc<CollectorParts>,
}

impl Real {
    /// Ask a short session of the backend (no prompt) for its usage answer.
    async fn ask_session(&self) -> Result<serde_json::Value, Miss> {
        let name = self.backend.info().name;
        let cwd = self.parts.probe_dir.join(format!("{name}-usage"));
        match backends::probe_usage(self.backend.as_ref(), cwd).await {
            Ok(Some(answer)) => Ok(answer),
            Ok(None) => Err(Miss::Unavailable(
                "this version of the agent gives no usage numbers".into(),
            )),
            Err(e) => Err(Miss::Unavailable(e)),
        }
    }

    async fn run(&self, now: u64) -> Result<Fetched, Miss> {
        match self.kind {
            Kind::ClaudeGetUsage => {
                let answer = self.ask_session().await?;
                Ok(claude::get_usage(&answer, now)?)
            }
            Kind::GrokBilling => {
                let answer = self.ask_session().await?;
                Ok(grok::billing(&answer, now)?)
            }
            Kind::CodexFiles => {
                let roots = self.parts.codex_roots.clone();
                tokio::task::spawn_blocking(move || codex::collect(&roots(), now))
                    .await
                    .map_err(|_| Miss::Unavailable("reading Codex's sessions failed".into()))?
                    .map_err(Miss::Unavailable)
            }
            Kind::GeminiQuota | Kind::OpencodeGo => {
                let (Some(home), Some(http)) = (&self.parts.home, &self.parts.http) else {
                    return Err(Miss::Unavailable("no HTTPS client or home here".into()));
                };
                match self.kind {
                    Kind::GeminiQuota => gemini::fetch(home, http.as_ref(), now).await,
                    _ => opencode::fetch(home, http.as_ref(), now).await,
                }
            }
            Kind::Muse => Err(Miss::Unavailable(
                "Muse reports usage only over its own protocol, not ACP".into(),
            )),
        }
    }
}

impl Source for Real {
    fn provider(&self) -> &str {
        &self.provider
    }

    fn id(&self) -> &str {
        match self.kind {
            Kind::ClaudeGetUsage => claude::SOURCE_GET_USAGE,
            Kind::GrokBilling => grok::SOURCE,
            Kind::CodexFiles => codex::SOURCE,
            Kind::GeminiQuota => gemini::SOURCE,
            Kind::OpencodeGo => opencode::SOURCE,
            Kind::Muse => MUSE_SOURCE,
        }
    }

    fn host(&self) -> Option<&str> {
        match self.kind {
            Kind::GeminiQuota => Some(gemini::HOST),
            Kind::OpencodeGo => Some(opencode::HOST),
            _ => None,
        }
    }

    fn fetch(&self, now: u64) -> BoxFuture<'_, Result<Fetched, Miss>> {
        Box::pin(self.run(now))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::provider_usage::{Observation, WindowKind};

    struct Fake {
        provider: &'static str,
        id: &'static str,
        calls: Arc<AtomicUsize>,
        behave: fn() -> Result<Fetched, Miss>,
        hang: bool,
        panic: bool,
    }

    impl Source for Fake {
        fn provider(&self) -> &str {
            self.provider
        }
        fn id(&self) -> &str {
            self.id
        }
        fn fetch(&self, _now: u64) -> BoxFuture<'_, Result<Fetched, Miss>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let (hang, panic, behave) = (self.hang, self.panic, self.behave);
            Box::pin(async move {
                if hang {
                    std::future::pending::<()>().await;
                }
                assert!(!panic, "a broken source");
                behave()
            })
        }
    }

    fn window(id: &str, used: f64, observed_at: u64) -> Window {
        Window {
            id: id.into(),
            label: id.into(),
            kind: WindowKind::Session,
            used_pct: used,
            resets_at: Some(now_secs() + 3600),
            window_secs: Some(5 * 3600),
            model_scope: None,
            status: None,
            source: "test".into(),
            observed_at,
            pace: None,
        }
    }

    fn entry(id: &str, ready: bool) -> ProviderEntry {
        ProviderEntry {
            id: id.into(),
            label: provider_label(id),
            ready: if ready {
                Ok(())
            } else {
                Err("not signed in".into())
            },
            sources: vec![format!("{id}.x")],
        }
    }

    fn fake(
        provider: &'static str,
        id: &'static str,
        calls: &Arc<AtomicUsize>,
        behave: fn() -> Result<Fetched, Miss>,
    ) -> Fake {
        Fake {
            provider,
            id,
            calls: calls.clone(),
            behave,
            hang: false,
            panic: false,
        }
    }

    #[tokio::test]
    async fn one_failing_hanging_or_panicking_source_does_not_spoil_the_others() {
        tokio::time::pause();
        let calls = Arc::new(AtomicUsize::new(0));
        let ok = fake("grok", "grok.billing", &calls, || {
            Ok(Fetched {
                plan: Some("tier".into()),
                windows: vec![window("credits", 10.0, 1)],
                ..Fetched::default()
            })
        });
        let failing = fake("gemini", "gemini.quota", &calls, || {
            Err(Miss::Unavailable("Google answered 500".into()))
        });
        let hanging = Fake {
            hang: true,
            ..fake("opencode", "opencode.go", &calls, || Ok(Fetched::default()))
        };
        let panicking = Fake {
            panic: true,
            ..fake("codex-acp", "codex.sessions", &calls, || {
                Ok(Fetched::default())
            })
        };
        let sources: Vec<Arc<dyn Source>> = vec![
            Arc::new(ok),
            Arc::new(failing),
            Arc::new(hanging),
            Arc::new(panicking),
        ];
        let providers = vec![
            entry("grok", true),
            entry("gemini", true),
            entry("opencode", true),
            entry("codex-acp", true),
        ];
        let c = Collector::with_planner(
            UsageHub::new(),
            Arc::new(move || (providers.clone(), sources.clone())),
        );
        let report = c.report(false).await;
        let by = |id: &str| report.providers.iter().find(|p| p.provider == id).unwrap();
        let grok = by("grok");
        assert_eq!(grok.plan.as_deref(), Some("tier"));
        assert_eq!(grok.windows.len(), 1);
        assert!(grok.windows[0].pace.is_some());
        assert_eq!(grok.sources[0].state, SourceState::Ok);
        for (p, words) in [
            ("gemini", "500"),
            ("opencode", "no answer within"),
            ("codex-acp", "failed"),
        ] {
            let s = &by(p).sources[0];
            assert_eq!(s.state, SourceState::Unavailable, "{p}");
            assert!(s.reason.as_deref().unwrap().contains(words), "{p}: {s:?}");
        }
    }

    #[tokio::test]
    async fn answers_are_reused_until_the_cache_runs_out() {
        tokio::time::pause();
        let calls = Arc::new(AtomicUsize::new(0));
        let source: Arc<dyn Source> = Arc::new(fake("grok", "grok.billing", &calls, || {
            Ok(Fetched::default())
        }));
        let providers = vec![entry("grok", true)];
        let c = Collector::with_planner(
            UsageHub::new(),
            Arc::new(move || (providers.clone(), vec![source.clone()])),
        );
        c.report(false).await;
        c.report(false).await;
        // A refresh within MIN_REFRESH still reuses.
        c.report(true).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        tokio::time::advance(MIN_REFRESH + Duration::from_secs(1)).await;
        c.report(true).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        c.report(false).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        tokio::time::advance(CACHE_FOR).await;
        c.report(false).await;
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn providers_not_signed_in_are_off_and_not_asked() {
        let calls = Arc::new(AtomicUsize::new(0));
        let providers = vec![entry("gemini", false)];
        let c = Collector::with_planner(
            UsageHub::new(),
            Arc::new(move || (providers.clone(), vec![])),
        );
        let report = c.report(false).await;
        let p = &report.providers[0];
        assert!(!p.ready);
        assert_eq!(p.not_ready.as_deref(), Some("not signed in"));
        assert_eq!(p.sources[0].state, SourceState::Off);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn session_reports_and_source_answers_merge_newest_first() {
        let hub = UsageHub::new();
        hub.observe(
            "claude",
            "p",
            Observation::Windows(vec![
                window("five_hour", 30.0, 200),
                window("seven_day", 5.0, 50),
            ]),
            1,
        );
        hub.observe(
            "claude",
            "p",
            Observation::ProcessTotals(vec![ModelTokens {
                model: "m".into(),
                tokens: crate::provider_usage::TokenCount {
                    input: 5,
                    ..Default::default()
                },
                cost_micros: Some(2_000),
                currency: Some("USD".into()),
            }]),
            1,
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let source: Arc<dyn Source> = Arc::new(fake("claude", "claude.get_usage", &calls, || {
            Ok(Fetched {
                plan: Some("max".into()),
                windows: vec![
                    window("five_hour", 20.0, 100),
                    window("seven_day", 9.0, 100),
                ],
                ..Fetched::default()
            })
        }));
        let providers = vec![entry("claude", true)];
        let c = Collector::with_planner(
            hub,
            Arc::new(move || (providers.clone(), vec![source.clone()])),
        );
        let report = c.report(false).await;
        let p = &report.providers[0];
        let used = |id: &str| p.windows.iter().find(|w| w.id == id).unwrap().used_pct;
        // The stream's report is newer for the 5-hour window, get_usage's for the week.
        assert!((used("five_hour") - 30.0).abs() < 1e-9);
        assert!((used("seven_day") - 9.0).abs() < 1e-9);
        assert_eq!(p.plan.as_deref(), Some("max"));
        assert_eq!(p.cost[0].micros, 2_000);
        assert_eq!(p.tokens.as_ref().unwrap().models[0].tokens.input, 5);
        let ids: Vec<_> = p.sources.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["claude.stream", "claude.get_usage"]);
    }

    #[test]
    fn the_report_serializes_without_secrets_or_raw_answers() {
        let report = build(&[entry("grok", true)], &[], &UsageHub::new(), 5);
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["providers"][0]["provider"], "grok");
        assert_eq!(json["providers"][0]["ready"], true);
        assert!(json["providers"][0]["windows"]
            .as_array()
            .unwrap()
            .is_empty());
    }
}
