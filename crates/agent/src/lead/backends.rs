//! Choosing the backend (and model) of a conversation: the user switches the lead agent or an agent
//! to another configured session backend at runtime.
//!
//! - The choice is the user's, so it lives in Melissa's state, not in the config file: the
//!   Lead agent's in `$XDG_STATE_HOME/melissa/lead/backend.json`, an agent's in the agent registry.
//! - It takes effect at the next turn, never mid-turn: the running session is closed before
//!   the next message and a session on the chosen backend opens instead. Context carries
//!   over the way it does after a lost session: the same backend (a model change) resumes
//!   its own session; another backend starts a new session primed with the last messages
//!   of Melissa's transcript.
//! - Models: only what the agents themselves report (ACP `configOptions` of category
//!   `model`, Claude's handshake answer), remembered per backend with the time they were
//!   seen in `$XDG_STATE_HOME/melissa/lead/models.json`, whenever a session on it opens.
//!   Melissa never makes up model ids; a backend whose models are unknown offers none. The
//!   agent's whole list is offered, uncurated (decided 2026-11-19).
//! - Probes: to learn the models before any chat ran on a backend, Melissa opens a session
//!   the way real sessions open (same sandbox and network rules), reads the models from the
//!   handshake (Claude) or `session/new` (ACP), and closes it without sending a prompt
//!   ([`probe`]). [`ModelCache::refresh`] probes usable backends: automatically when the
//!   list is missing or older than [`STALE_AFTER`], at most once per backend per
//!   [`PROBE_INTERVAL`] (a forced refresh skips both limits), each within [`PROBE_TIMEOUT`].

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    device::write_private,
    session::{
        ModelList, ModelOption, NotReady, OpenSession, PermissionDecision, PermissionHandler,
        PermissionRequest, SessionBackend,
    },
    BoxFuture,
};

/// How long one model probe may take (start the agent, handshake, open a session).
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// At most one probe per backend in this time, unless forced.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// An automatic refresh probes a backend whose list is older than this (or missing).
pub const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
/// How long closing a probe session may take before it is dropped (which kills the agent).
const PROBE_CLOSE: Duration = Duration::from_secs(3);

/// The backend (and model) the user chose for a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendChoice {
    pub backend: String,
    /// A model id the backend reported; None: the backend's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// A reasoning level the backend reported for the model; None: the agent's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Fast mode, for a model the backend reported one for; false: standard speed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fast: bool,
}

/// One backend the user can choose, as clients see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackendView {
    pub name: String,
    /// Usable now (installed, logged in where Melissa can tell, sandbox ready).
    pub available: bool,
    /// Why not (`not_installed`, `not_logged_in`, `sandbox`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    /// The conversation runs (or will run on its next message) on this backend.
    pub current: bool,
    /// The model in use or chosen on the current backend, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The reasoning level chosen on the current backend (None: the agent's default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Fast mode is chosen on the current backend.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fast: bool,
    /// Models the backend offers, once Melissa has seen them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<ModelOption>>,
}

/// What a switch did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Switched {
    pub backend: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fast: bool,
    /// False: that was already the choice (nothing recorded).
    pub changed: bool,
    /// A turn is running: the switch applies when it has ended.
    pub after_turn: bool,
}

/// Models each backend offered when a session last opened on it (or a probe asked it).
/// Shared by the lead agent and every agent; saved so the list is there after a restart. Cheap to
/// clone.
#[derive(Clone, Default)]
pub struct ModelCache {
    inner: Arc<ModelCacheInner>,
}

#[derive(Default)]
struct ModelCacheInner {
    path: Option<PathBuf>,
    lists: Mutex<BTreeMap<String, Stored>>,
    probes: Mutex<Probes>,
}

/// One backend's list as saved: the models and when they were seen (Unix seconds; 0 when
/// unknown, from a file written before timestamps were kept).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Stored {
    models: Vec<ModelOption>,
    #[serde(default)]
    updated: u64,
}

/// The saved form, or the older one (a bare list per backend).
#[derive(Deserialize)]
#[serde(untagged)]
enum OnDisk {
    Stored(Stored),
    Bare(Vec<ModelOption>),
}

/// Probe bookkeeping (in memory): when each backend was last probed, which run now.
#[derive(Default)]
struct Probes {
    last: BTreeMap<String, Instant>,
    running: BTreeSet<String>,
}

/// What a refresh did for one backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbeOutcome {
    pub backend: String,
    /// `refreshed` (models saved), `no_models` (the agent listed none), `skipped` (fresh
    /// enough, probed recently, or a probe is running), `failed`.
    pub result: &'static str,
    /// Models now known for the backend.
    pub models: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ModelCache {
    /// In memory only (tests).
    pub fn new() -> Self {
        Self::default()
    }

    /// Kept in `path` (a JSON object: backend name → `{models: [{id, label}], updated}`,
    /// mode 0600). Also reads the older form (backend name → `[{id, label}]`).
    pub fn open(path: PathBuf) -> Self {
        let lists = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<BTreeMap<String, OnDisk>>(&t).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|(k, v)| {
                let stored = match v {
                    OnDisk::Stored(s) => s,
                    OnDisk::Bare(models) => Stored { models, updated: 0 },
                };
                (k, stored)
            })
            .collect();
        Self {
            inner: Arc::new(ModelCacheInner {
                path: Some(path),
                lists: Mutex::new(lists),
                probes: Mutex::default(),
            }),
        }
    }

    pub fn get(&self, backend: &str) -> Option<Vec<ModelOption>> {
        lock(&self.inner.lists)
            .get(backend)
            .map(|s| s.models.clone())
    }

    /// When the backend's list was last seen, if known.
    pub fn updated(&self, backend: &str) -> Option<SystemTime> {
        lock(&self.inner.lists)
            .get(backend)
            .filter(|s| s.updated > 0)
            .map(|s| UNIX_EPOCH + Duration::from_secs(s.updated))
    }

    /// Remember what a backend offered, now (saved).
    pub fn put(&self, backend: &str, models: &ModelList) {
        if models.options.is_empty() {
            return;
        }
        let mut lists = lock(&self.inner.lists);
        // An agent may report per-model options (reasoning levels, speed) only for the model
        // its session used (ACP): what was seen before for a model reported without any is
        // kept.
        let mut next = models.options.clone();
        if let Some(before) = lists.get(backend) {
            let current = models.current.as_deref();
            for m in next.iter_mut().filter(|m| !m.has_options()) {
                if let Some(old) = before.models.iter().find(|o| o.id == m.id) {
                    m.efforts = old.efforts.clone();
                    if current != Some(m.id.as_str()) {
                        m.fast = old.fast;
                    }
                }
            }
        }
        lists.insert(
            backend.to_string(),
            Stored {
                models: next,
                updated: unix_now(),
            },
        );
        if let Some(path) = &self.inner.path {
            match serde_json::to_vec_pretty(&*lists) {
                Ok(json) => {
                    if let Err(e) = write_private(path, &json) {
                        tracing::warn!(path = %path.display(), "cannot save the model list: {e}");
                    }
                }
                Err(e) => tracing::warn!("cannot save the model list: {e}"),
            }
        }
    }

    /// Whether to probe `backend` now; if so it is marked running (and probed now).
    fn claim(&self, backend: &str, force: bool) -> bool {
        let fresh = self
            .updated(backend)
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age < STALE_AFTER);
        let mut p = lock(&self.inner.probes);
        if p.running.contains(backend) {
            return false;
        }
        let recent = p
            .last
            .get(backend)
            .is_some_and(|t| t.elapsed() < PROBE_INTERVAL);
        if !force && (fresh || recent) {
            return false;
        }
        p.running.insert(backend.to_string());
        p.last.insert(backend.to_string(), Instant::now());
        true
    }

    fn release(&self, backend: &str) {
        lock(&self.inner.probes).running.remove(backend);
    }

    /// Probe the usable backends among `backends` for their models (in parallel), each in
    /// its own empty directory under `workroot`. Without `force`, only backends whose list
    /// is missing or stale and that were not probed in the last [`PROBE_INTERVAL`].
    /// Backends that are not usable now (not installed, not logged in) are left out.
    pub async fn refresh(
        &self,
        backends: &[Arc<dyn SessionBackend>],
        workroot: &Path,
        force: bool,
    ) -> Vec<ProbeOutcome> {
        let mut tasks = Vec::new();
        for b in backends {
            let info = b.info();
            if !info.available || b.not_ready().is_some() {
                continue;
            }
            let name = info.name;
            if !self.claim(&name, force) {
                tasks.push(tokio::spawn({
                    let models = self.get(&name).map_or(0, |m| m.len());
                    async move {
                        ProbeOutcome {
                            backend: name,
                            result: "skipped",
                            models,
                            error: None,
                        }
                    }
                }));
                continue;
            }
            let (cache, backend) = (self.clone(), b.clone());
            let cwd = workroot.join(&name);
            tasks.push(tokio::spawn(async move {
                let result = probe(backend.as_ref(), cwd).await;
                cache.release(&name);
                let (result, error) = match result {
                    Ok(Some(list)) if !list.options.is_empty() => {
                        cache.put(&name, &list);
                        ("refreshed", None)
                    }
                    Ok(_) => ("no_models", None),
                    Err(e) => {
                        tracing::info!(backend = %name, "model probe failed: {e}");
                        ("failed", Some(e))
                    }
                };
                ProbeOutcome {
                    models: cache.get(&name).map_or(0, |m| m.len()),
                    backend: name,
                    result,
                    error,
                }
            }));
        }
        let mut out = Vec::new();
        for t in tasks {
            match t.await {
                Ok(o) => out.push(o),
                Err(e) => tracing::warn!("model probe task failed: {e}"),
            }
        }
        out
    }
}

/// Ask one backend which models it offers: open a session (sandboxed like every session,
/// no tools, every permission refused) in `cwd`, read the list, close it. Sends no prompt.
/// `Ok(None)`: the agent lists no models.
pub async fn probe(
    backend: &dyn SessionBackend,
    cwd: PathBuf,
) -> Result<Option<ModelList>, String> {
    probe_within(backend, cwd, PROBE_TIMEOUT).await
}

async fn probe_within(
    backend: &dyn SessionBackend,
    cwd: PathBuf,
    limit: Duration,
) -> Result<Option<ModelList>, String> {
    std::fs::create_dir_all(&cwd).map_err(|e| format!("cannot create {}: {e}", cwd.display()))?;
    let open = OpenSession::new(cwd, Vec::new(), Arc::new(RefuseAll));
    let opened = tokio::time::timeout(limit, backend.open(open))
        .await
        .map_err(|_| format!("no answer within {} s", limit.as_secs_f32()))?
        .map_err(|e| e.to_string())?;
    // Dropping the session kills the agent if closing takes too long.
    let _ = tokio::time::timeout(PROBE_CLOSE, opened.session.close()).await;
    Ok(opened.models)
}

/// Ask one backend for its plan usage ([`crate::session::AgentSession::plan_usage`]): open a session like
/// [`probe`] does (sandboxed, no tools, no prompt), ask, close. `Ok(None)`: the agent has no
/// such request. The whole exchange takes at most [`PROBE_TIMEOUT`] to open plus the
/// request's own limit.
pub async fn probe_usage(
    backend: &dyn SessionBackend,
    cwd: PathBuf,
) -> Result<Option<serde_json::Value>, String> {
    std::fs::create_dir_all(&cwd).map_err(|e| format!("cannot create {}: {e}", cwd.display()))?;
    let open = OpenSession::new(cwd, Vec::new(), Arc::new(RefuseAll));
    let opened = tokio::time::timeout(PROBE_TIMEOUT, backend.open(open))
        .await
        .map_err(|_| format!("no answer within {} s", PROBE_TIMEOUT.as_secs()))?
        .map_err(|e| e.to_string())?;
    let answer = opened.session.plan_usage().await.map_err(|e| e.to_string());
    // Dropping the session kills the agent if closing takes too long.
    let _ = tokio::time::timeout(PROBE_CLOSE, opened.session.close()).await;
    answer
}

/// A probe runs no prompt, so the agent has nothing to ask; refuse anything anyway.
struct RefuseAll;

impl PermissionHandler for RefuseAll {
    fn decide<'a>(&'a self, _req: &'a PermissionRequest) -> BoxFuture<'a, PermissionDecision> {
        Box::pin(async { PermissionDecision::RejectOnce })
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The lead agent's saved choice, if any.
pub(crate) fn load_choice(path: &Path) -> Option<BackendChoice> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&text) {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!(path = %path.display(), "ignoring the saved backend choice: {e}");
            None
        }
    }
}

pub(crate) fn save_choice(path: &Path, choice: &BackendChoice) -> std::io::Result<()> {
    let json = serde_json::to_vec_pretty(choice).map_err(std::io::Error::other)?;
    write_private(path, &json)
}

pub(crate) fn reason(r: Option<NotReady>) -> Option<&'static str> {
    r.map(NotReady::as_str)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // Poisoning only means another thread panicked mid-update; the data is still usable.
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(ids: &[&str]) -> ModelList {
        ModelList {
            current: None,
            options: ids
                .iter()
                .map(|i| ModelOption::new(*i, i.to_uppercase()))
                .collect(),
        }
    }

    #[test]
    fn model_cache_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("melissa-models-{}", std::process::id()));
        let path = dir.join("models.json");
        let cache = ModelCache::open(path.clone());
        assert_eq!(cache.get("claude-session"), None);
        cache.put("claude-session", &list(&["sonnet", "opus"]));
        // An empty list never replaces a known one.
        cache.put("claude-session", &list(&[]));
        let again = ModelCache::open(path.clone());
        let ids: Vec<String> = again
            .get("claude-session")
            .unwrap_or_default()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, ["sonnet", "opus"]);
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).map(|m| m.permissions().mode() & 0o777);
        assert_eq!(mode.ok(), Some(0o600));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// An ACP agent reports reasoning levels and speeds only for the model its session uses.
    #[test]
    fn what_one_model_reported_is_kept_when_another_is_used() {
        let cache = ModelCache::new();
        let level = |id: &str| crate::session::EffortOption {
            id: id.into(),
            label: id.into(),
        };
        // An ACP agent reports levels only for the model its session uses.
        let mut first = list(&["a", "b"]);
        first.options[0].efforts = vec![level("low"), level("high")];
        cache.put("grok-acp", &first);
        let mut second = list(&["a", "b"]);
        second.options[1].efforts = vec![level("max")];
        cache.put("grok-acp", &second);
        let got = cache.get("grok-acp").unwrap_or_default();
        assert!(got[0].offers_effort("high"));
        assert!(got[1].offers_effort("max"));
        assert!(!got[1].offers_effort("low"));

        // Speeds the same way, but the current model's own report wins.
        // Session on `a`: it reports a fast mode.
        let mut first = list(&["a", "b"]);
        first.current = Some("a".into());
        first.options[0].fast = true;
        cache.put("x-acp", &first);
        // Session on `b`: nothing said about `a`, which keeps its fast mode.
        let mut second = list(&["a", "b"]);
        second.current = Some("b".into());
        cache.put("x-acp", &second);
        let got = cache.get("x-acp").unwrap_or_default();
        assert!(got[0].fast);
        assert!(!got[1].fast);
        // Session on `a` again, which now has no speed option: dropped.
        let mut third = list(&["a", "b"]);
        third.current = Some("a".into());
        cache.put("x-acp", &third);
        assert!(!cache.get("x-acp").unwrap_or_default()[0].fast);
    }

    /// A session backend that reports `models` (or hangs), counting opens and prompts.
    struct Probed {
        name: &'static str,
        models: Option<ModelList>,
        hang: bool,
        ready: bool,
        opens: std::sync::atomic::AtomicUsize,
        echo: crate::EchoSessionBackend,
    }

    impl Probed {
        fn new(name: &'static str, models: Option<ModelList>) -> Self {
            Self {
                name,
                models,
                hang: false,
                ready: true,
                opens: Default::default(),
                echo: crate::EchoSessionBackend::new(&crate::EchoSpec { name: name.into() }),
            }
        }

        fn opens(&self) -> usize {
            self.opens.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl SessionBackend for Probed {
        fn info(&self) -> crate::BackendInfo {
            crate::BackendInfo {
                name: self.name.into(),
                kind: "test",
                detail: String::new(),
                available: true,
                tools: true,
            }
        }

        fn not_ready(&self) -> Option<NotReady> {
            (!self.ready).then_some(NotReady::NotLoggedIn)
        }

        fn open(
            &self,
            req: OpenSession,
        ) -> BoxFuture<'_, Result<crate::session::OpenedSession, crate::AgentError>> {
            Box::pin(async move {
                self.opens.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                // A probe gives the agent no tools and a directory of its own.
                assert!(req.mcp_servers.is_empty());
                assert!(req.cwd.ends_with(self.name) && req.cwd.is_dir());
                if self.hang {
                    std::future::pending::<()>().await;
                }
                let mut opened = self.echo.open(req).await?;
                opened.models = self.models.clone();
                Ok(opened)
            })
        }
    }

    fn results(out: &[ProbeOutcome]) -> Vec<(&str, &str)> {
        out.iter().map(|o| (o.backend.as_str(), o.result)).collect()
    }

    #[tokio::test]
    async fn probes_fill_the_cache_without_a_prompt_and_are_rate_limited() {
        let dir = std::env::temp_dir().join(format!("melissa-probe-{}", std::process::id()));
        let cache = ModelCache::open(dir.join("models.json"));
        let claude = Arc::new(Probed::new(
            "claude-session",
            Some(list(&["default", "opus"])),
        ));
        let grok = Arc::new(Probed::new("grok-acp", None));
        let mut logged_out = Probed::new("opencode-acp", Some(list(&["x"])));
        logged_out.ready = false;
        let logged_out = Arc::new(logged_out);
        let all: Vec<Arc<dyn SessionBackend>> =
            vec![claude.clone(), grok.clone(), logged_out.clone()];
        let work = dir.join("probe");

        let out = cache.refresh(&all, &work, false).await;
        assert_eq!(
            results(&out),
            [("claude-session", "refreshed"), ("grok-acp", "no_models")]
        );
        assert_eq!(out[0].models, 2);
        // Not usable: never started.
        assert_eq!(logged_out.opens(), 0);
        let ids: Vec<String> = cache
            .get("claude-session")
            .unwrap_or_default()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, ["default", "opus"], "exactly what the agent listed");
        assert!(cache.updated("claude-session").is_some());
        assert_eq!(cache.get("grok-acp"), None, "no list is made up");

        // Fresh (Claude) or probed a moment ago (Grok): skipped, unless forced.
        let out = cache.refresh(&all, &work, false).await;
        assert_eq!(
            results(&out),
            [("claude-session", "skipped"), ("grok-acp", "skipped")]
        );
        assert_eq!((claude.opens(), grok.opens()), (1, 1));
        let out = cache.refresh(&all, &work, true).await;
        assert_eq!(
            results(&out),
            [("claude-session", "refreshed"), ("grok-acp", "no_models")]
        );
        assert_eq!((claude.opens(), grok.opens()), (2, 2));

        // Saved with its time; a restart keeps both.
        let again = ModelCache::open(dir.join("models.json"));
        assert_eq!(again.get("claude-session").map(|m| m.len()), Some(2));
        assert!(again.updated("claude-session").is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_probe_that_hangs_times_out() {
        let dir = std::env::temp_dir().join(format!("melissa-probe-t-{}", std::process::id()));
        let mut stuck = Probed::new("grok-acp", Some(list(&["a"])));
        stuck.hang = true;
        let limit = Duration::from_millis(50);
        let err = probe_within(&stuck, dir.join("grok-acp"), limit)
            .await
            .unwrap_err();
        assert!(err.contains("no answer within 0.05 s"), "{err}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_older_model_file_still_reads() {
        let dir = std::env::temp_dir().join(format!("melissa-models-old-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("models.json");
        std::fs::write(&path, r#"{"grok-acp": [{"id": "g", "label": "G"}]}"#).ok();
        let cache = ModelCache::open(path);
        assert_eq!(cache.get("grok-acp").map(|m| m.len()), Some(1));
        // Age unknown: an automatic refresh probes it.
        assert_eq!(cache.updated("grok-acp"), None);
        assert!(cache.claim("grok-acp", false));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn choice_round_trips_and_bad_files_are_ignored() {
        let dir = std::env::temp_dir().join(format!("melissa-choice-{}", std::process::id()));
        let path = dir.join("backend.json");
        assert_eq!(load_choice(&path), None);
        let c = BackendChoice {
            backend: "claude-session".into(),
            model: Some("sonnet".into()),
            effort: Some("high".into()),
            fast: true,
        };
        save_choice(&path, &c).ok();
        assert_eq!(load_choice(&path), Some(c));
        std::fs::write(&path, "not json").ok();
        assert_eq!(load_choice(&path), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
