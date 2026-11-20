//! Settings: pages under `/settings/...`, their forms under `/ui/settings/...` and a JSON API
//! under `/api/settings/...`. Sections: Providers, Voice, Updates, Developer; a new section is a
//! file here, an entry in `pages::SECTIONS` and its routes below.
//!
//! UI routes (same-origin checked like every POST; htmx gets the section's region back, a
//! plain form post a redirect):
//! - `GET /settings` → `/settings/providers`; `GET /settings/providers?p=<id>`,
//!   `GET /settings/updates`.
//! - `POST /ui/settings/providers/refresh`: forget cached versions, check again.
//! - `POST /ui/settings/providers/{id}/enabled` (`enabled=on` or nothing),
//!   `POST /ui/settings/providers/{id}` (`display_name`, `command`, `args` one per line),
//!   `POST /ui/settings/providers/{id}/env` (`name`, `value`),
//!   `POST /ui/settings/providers/{id}/env/remove` (`name`),
//!   `POST /ui/settings/providers/{id}/key` (`value`: the provider's API key, stored under
//!   its fixed variable name; write-only).
//! - `POST /ui/settings/providers/{id}/{install|login}`; Claude's sign-in then takes the
//!   pasted code at `POST .../login/code` (`code`) or stops at `POST .../login/cancel`.
//! - `POST /ui/settings/updates/{check|apply|rollback}`: apply and rollback first return a
//!   confirmation card; only a post with `confirm=yes` starts them.
//! - `POST /ui/settings/restart`: restart Melissa now (saved settings also apply by
//!   themselves once she is idle, see `apply.rs`).
//! - `GET /settings/voice`; `POST /ui/settings/voice` (`enabled=on` or nothing): voice input
//!   in the composer (`ui.toml`); turning it on downloads the speech model (progress from
//!   `GET /ui/settings/voice/poll`), `POST /ui/settings/voice/retry` tries again,
//!   `POST /ui/settings/voice/delete` removes the model while voice input is off.
//! - `POST /ui/voice/transcribe` (body: a WAV recording, `Content-Type: audio/wav`, up to
//!   12 MiB and two minutes) → `{"text"}` or `{"error"}` (409 off, 415 not WAV, 413 too big,
//!   422 bad audio, 429 busy, 503 model not ready, 504 too slow). Nothing is stored.
//! - `POST /ui/settings/developer/steps` (`enabled=on` or nothing): show the agents'
//!   thinking and step timings in the chat (`ui.toml`; applies at once).
//!
//! API (JSON; never returns secret values):
//! - `GET /api/settings/providers` → `{"providers": [...], "restart_needed": bool}`.
//! - `POST /api/settings/providers/{id}` `{"enabled"?, "display_name"?, "command"?, "args"?,
//!   "set_env"?: {"NAME": "value"}, "remove_env"?: ["NAME"]}` → the provider. 404 unknown
//!   provider, 409 refused (e.g. disabling the agent the lead agent runs on), 422 invalid.
//! - `POST /api/settings/providers/refresh` → the list again, versions re-read.
//! - `GET /api/settings/updates` → release, host kind, OS image state, available actions.
//! - `POST /api/settings/updates/{check|apply|rollback|restart}` `{"confirm": true}` (needed
//!   for apply/rollback) → 202; 409 without `confirm`; 501 where the root helper is missing.
//!
//! - `POST /api/settings/providers/{id}/login/code` `{"code": "..."}` (empty: cancel).
//!
//! Every change is audited (`settings.provider`, `settings.update`, `settings.voice` with the
//! model download's start and outcome): what changed and the
//! SHA-256 of the new value, never the value. Installs and sign-ins are audited when asked
//! for and again with their outcome (`settings.provider.install`, `settings.provider.login`);
//! sign-in codes never reach the audit log.

mod apply;
mod developer;
pub(crate) mod pages;
mod providers;
mod updates;
mod voice;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard,
    },
};

use axum::{
    extract::DefaultBodyLimit,
    response::Redirect,
    routing::{get, post},
    Router,
};
use melissa_agent::{AuditLog, AuditRecord};
use melissa_settings::{
    install::{InstallState, Installer, Job, INSTALLABLE},
    prefs::{Prefs, PrefsStore},
    providers::{BackendFacts, ProviderOverride, ProviderStore},
    updates::{HostKind, Release, UpdateEnv},
    version::VersionProbe,
};

use crate::ui::Ui;

/// Checks a provider change against the real backend config (the lead agent's merge + its
/// backends' own checks) before it is saved.
pub type CheckFn = Arc<dyn Fn(&str, &ProviderOverride) -> Result<(), String> + Send + Sync>;

/// What the lead agent hands the Settings pages.
pub struct SettingsParts {
    pub store: ProviderStore,
    /// The owner's display preferences (`ui.toml`).
    pub prefs: PrefsStore,
    /// Every configured backend, also those of disabled providers, plus the built-in echo.
    pub backends: Vec<BackendFacts>,
    /// The backend new agents get when the lead agent names none.
    pub agent_default: Option<String>,
    pub check: Option<CheckFn>,
    pub audit: Option<AuditLog>,
    pub updates: UpdateEnv,
    pub release: Release,
    /// The agents' home (`$HOME` of the lead agent's user).
    pub home: Option<PathBuf>,
    /// Installs agent CLIs (Settings → Providers → Install), where the packaging allows it.
    pub installer: Installer,
    /// Voice input: the speech model and the on-device transcriber (Settings → Voice).
    pub voice: melissa_voice::Voice,
}

/// The Settings context. Cheap to clone.
#[derive(Clone)]
pub struct Settings {
    inner: Arc<Inner>,
}

struct Inner {
    parts: SettingsParts,
    versions: VersionProbe,
    apply: Mutex<apply::Apply>,
    /// A code went to the Claude sign-in that runs now (reset when one starts).
    code_sent: AtomicBool,
    /// Install and sign-in runs whose outcome is being watched (for the audit), e.g.
    /// "Install:claude".
    watching: Mutex<BTreeSet<String>>,
    /// The last failed run audited per job ("Install:claude" → systemd's end time), so a
    /// quick retry doesn't lose or repeat an outcome.
    audited: Mutex<BTreeMap<String, u64>>,
}

impl Settings {
    pub fn new(parts: SettingsParts) -> Self {
        Self {
            inner: Arc::new(Inner {
                parts,
                versions: VersionProbe::new(),
                apply: Mutex::default(),
                code_sent: AtomicBool::new(false),
                watching: Mutex::default(),
                audited: Mutex::default(),
            }),
        }
    }

    /// What kind of host this is (container image, Nix, other), e.g. for setup hints.
    pub(crate) fn host(&self) -> HostKind {
        self.inner.parts.updates.kind
    }

    /// No settings file, no backends (routers built without a lead agent config).
    pub fn detached() -> Self {
        let updates = UpdateEnv::system();
        Self::new(SettingsParts {
            store: ProviderStore::in_memory(),
            prefs: PrefsStore::in_memory(),
            backends: Vec::new(),
            agent_default: None,
            check: None,
            audit: None,
            installer: Installer::system(updates.kind),
            updates,
            release: Release::read(&melissa_settings::updates::release_file()),
            home: std::env::var_os("HOME").map(PathBuf::from),
            voice: melissa_voice::Voice::unavailable(),
        })
    }

    /// The owner's display preferences.
    pub(crate) fn prefs(&self) -> Prefs {
        self.inner.parts.prefs.get()
    }

    /// Where the composer sends a recording, when voice input is on and ready.
    pub(crate) fn voice_url(&self) -> Option<&'static str> {
        (self.prefs().voice_input && self.inner.parts.voice.ready())
            .then_some(voice::TRANSCRIBE_URL)
    }

    fn voice(&self) -> &melissa_voice::Voice {
        &self.inner.parts.voice
    }

    fn parts(&self) -> &SettingsParts {
        &self.inner.parts
    }

    fn versions(&self) -> &VersionProbe {
        &self.inner.versions
    }

    fn apply_lock(&self) -> MutexGuard<'_, apply::Apply> {
        // A poisoned lock only means a panic elsewhere; the state is still usable.
        self.inner.apply.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn watching(&self) -> MutexGuard<'_, BTreeSet<String>> {
        self.inner
            .watching
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Record that the failed run that ended at `ended` was audited; false if it was already.
    fn mark_audited(&self, key: &str, ended: u64) -> bool {
        let mut a = self.inner.audited.lock().unwrap_or_else(|e| e.into_inner());
        a.insert(key.to_string(), ended) != Some(ended)
    }

    fn code_sent(&self) -> bool {
        self.inner.code_sent.load(Ordering::Relaxed)
    }

    fn set_code_sent(&self, sent: bool) {
        self.inner.code_sent.store(sent, Ordering::Relaxed);
    }

    /// An install or sign-in unit runs now (a restart would not stop it, but its result
    /// would land while Melissa is away).
    async fn jobs_running(&self) -> bool {
        let installer = &self.parts().installer;
        for id in INSTALLABLE {
            for job in [Job::Install, Job::Login] {
                if job.unit(id).is_some()
                    && installer.job_status(job, id).await.state == InstallState::Running
                {
                    return true;
                }
            }
        }
        false
    }

    /// Append an audit record: `detail` says what changed, `sha256` hashes the new value.
    fn audit(&self, action: &str, actor: &str, detail: &str, sha256: Option<String>) {
        let Some(log) = &self.parts().audit else {
            return;
        };
        let mut record = AuditRecord::now(action, 0, "", detail).by(actor);
        record.args_sha256 = sha256;
        if let Err(e) = log.append(&record) {
            tracing::error!(path = %log.path().display(), "audit log write failed: {e}");
        }
    }
}

/// The UI routes (state: [`Ui`], which carries the [`Settings`]).
pub(crate) fn ui_routes() -> Router<Ui> {
    Router::new()
        .route(
            "/settings",
            get(|| async { Redirect::to("/settings/providers") }),
        )
        .route("/settings/providers", get(providers::page))
        .route("/ui/settings/providers/refresh", post(providers::refresh))
        .route("/ui/settings/providers/poll", get(providers::poll))
        .route("/ui/settings/providers/{id}", post(providers::save))
        .route(
            "/ui/settings/providers/{id}/enabled",
            post(providers::enable),
        )
        .route("/ui/settings/providers/{id}/env", post(providers::set_var))
        .route(
            "/ui/settings/providers/{id}/install",
            post(providers::install),
        )
        .route("/ui/settings/providers/{id}/login", post(providers::login))
        .route(
            "/ui/settings/providers/{id}/login/code",
            post(providers::login_code),
        )
        .route(
            "/ui/settings/providers/{id}/login/cancel",
            post(providers::login_cancel),
        )
        .route("/ui/settings/providers/{id}/key", post(providers::set_key))
        .route(
            "/ui/settings/providers/{id}/env/remove",
            post(providers::remove_var),
        )
        .route(pages::VOICE_URL, get(voice::page))
        .route("/ui/settings/voice", post(voice::toggle))
        .route("/ui/settings/voice/poll", get(voice::poll))
        .route("/ui/settings/voice/retry", post(voice::retry))
        .route("/ui/settings/voice/delete", post(voice::delete))
        .route(
            voice::TRANSCRIBE_URL,
            post(voice::transcribe).layer(DefaultBodyLimit::max(voice::MAX_UPLOAD)),
        )
        .route("/settings/updates", get(updates::page))
        .route("/ui/settings/updates/{action}", post(updates::action))
        .route("/ui/settings/restart", post(updates::restart))
        .route("/ui/settings/developer/steps", post(developer::steps))
}

/// The JSON API routes.
pub(crate) fn api_routes(ui: Ui) -> Router {
    Router::new()
        .route("/api/settings/providers", get(providers::api_list))
        .route(
            "/api/settings/providers/refresh",
            post(providers::api_refresh),
        )
        .route("/api/settings/providers/{id}", post(providers::api_change))
        .route(
            "/api/settings/providers/{id}/install",
            get(providers::api_install_status).post(providers::api_install),
        )
        .route(
            "/api/settings/providers/{id}/login",
            post(providers::api_login),
        )
        .route(
            "/api/settings/providers/{id}/login/code",
            post(providers::api_login_code),
        )
        .route("/api/settings/updates", get(updates::api_status))
        .route("/api/settings/updates/{action}", post(updates::api_action))
        .with_state(ui)
}
