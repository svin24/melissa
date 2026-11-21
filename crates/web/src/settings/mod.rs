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
//! - Add provider (`add_provider.rs`): `GET /settings/providers/add` (the panel; `?q=`, `?step=manual`,
//!   `?step=identity|config&id=`), `POST /ui/settings/providers/add/{id}` (a removed built-in back),
//!   `GET|POST /ui/settings/providers/registry` (the list from the cache | fetch it now: the only
//!   network call), `POST .../registry/{rid}/add`, `POST /ui/settings/providers/manual` (`name`,
//!   `command`), `POST .../new/{id}/key` (`var`, `value`), `POST .../new/{id}/finish` (`hosts`).
//!   Remove (`remove_provider.rs`): `POST /ui/settings/providers/{id}/remove`.
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
//! - `GET /settings/schedules[?edit=<id>]`: scheduled jobs; `POST /ui/settings/schedules`
//!   (`name`, `agent` = `lead` or an agent id, `instruction`, `repeat` = `hourly`|`daily`|
//!   `weekly`|`custom`, `day` 0–6, `time` HH:MM, `cron`, `max_minutes`) adds one,
//!   `POST /ui/settings/schedules/{id}` (same fields) saves one, and `.../{id}/enabled`
//!   (`enabled=on` or nothing), `.../{id}/run`, `.../{id}/remove` act on one. Their JSON API
//!   is `/api/schedules` (see `crate::api`).
//!   `POST /ui/settings/schedules/wait` (`minutes`, 1–1440): how long a scheduled run's
//!   request waits for the owner before it is withdrawn (default 10). API:
//!   `GET|POST /api/settings/schedules` `{"approval_wait_minutes": n}`.
//! - `GET /settings/files`; `POST /ui/settings/files` (`keep` = days or `forever`): how long
//!   file boxes keep files by default (each box can override it in its chat's file panel).
//!   API: `GET|POST /api/settings/files` (see `crate::files`).
//! - `GET /settings/apps`; `POST /ui/settings/apps` (`open_to` = `this_device`, `tailnet` or
//!   `home_network`): who may open the apps on this device, applied at once (`apps.rs`;
//!   container install only, else the page says the host's firewall decides).
//! - `GET /settings/permissions`: the "Always allow" rules; `POST
//!   /ui/settings/permissions/{id}/remove` removes one.
//! - `POST /ui/settings/developer/bypass` (`enabled=on` or nothing): No permission prompts.
//!   On returns a confirmation card first; only `POST .../bypass/on` with `confirm=yes`
//!   switches it on. Off applies at once.
//! - `POST /ui/settings/developer/full-access` (`enabled=on` or nothing): Full access per
//!   agent, which adds Access (Ask / Full access) to each chat's model picker; off puts every
//!   agent back on Ask. An agent's level is set from its picker (`access=ask|full` to the
//!   picker's action; Full asks in the popover first).
//!
//! API (JSON; never returns secret values):
//! - `GET /api/settings/providers` → `{"providers": [...], "restart_needed": bool}`.
//! - `POST /api/settings/providers/{id}` `{"enabled"?, "display_name"?, "command"?, "args"?,
//!   "set_env"?: {"NAME": "value"}, "remove_env"?: ["NAME"]}` → the provider. 404 unknown
//!   provider, 409 refused (e.g. disabling the agent the lead agent runs on), 422 invalid.
//! - `POST /api/settings/providers/refresh` → the list again, versions re-read.
//! - `DELETE /api/settings/providers/{id}` → `{"removed", "deleted_paths", "restart_needed"}`; 409
//!   while in use. `POST /api/settings/providers/add` (`provider` | `registry` | `name`+`command`).
//!   `GET /api/settings/providers/registry` (cached), `POST .../registry/refresh` (fetch now).
//! - `GET /api/settings/updates` → release, host kind, OS image state, available actions.
//! - `POST /api/settings/updates/{check|apply|rollback|restart}` `{"confirm": true}` (needed
//!   for apply/rollback) → 202; 409 without `confirm`; 501 where the root helper is missing.
//!
//! - `POST /api/settings/providers/{id}/login/code` `{"code": "..."}` (empty: cancel).
//! - `GET /api/settings/permissions` → `{"rules": [...], "bypass": bool,
//!   "full_access_allowed": bool, "full_access": [principal]}`;
//!   `POST /api/settings/permissions/full-access` `{"on": bool}`;
//!   `POST /api/settings/permissions/access` `{"agent": "lead"|name|id, "full": bool,
//!   "confirm": bool}` → 200; 409 Full without `confirm` or with the option off, 404 unknown;
//!   `POST /api/settings/permissions/rules/{id}/remove` → the rule, 404 unknown;
//!   `POST /api/settings/permissions/bypass` `{"on": bool, "confirm": bool}` → 200 with the
//!   confirmations it approved; 409 switching on without `confirm`. Rules are made only by
//!   "Always allow" on a pending request (`POST /api/confirmations/{id}/always`).
//!
//! Every change is audited (`settings.provider`, `settings.update`, `settings.voice` with the
//! model download's start and outcome): what changed and the
//! SHA-256 of the new value, never the value. Installs and sign-ins are audited when asked
//! for and again with their outcome (`settings.provider.install`, `settings.provider.login`);
//! sign-in codes never reach the audit log.

mod add_provider;
pub(crate) mod agents;
mod apply;
mod apps;
mod developer;
mod files;
pub(crate) mod pages;
mod permissions;
mod providers;
mod remove_provider;
mod schedules;
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
    app_access::AppAccessControl,
    install::{InstallState, Installer, Job, INSTALLABLE},
    prefs::{Prefs, PrefsStore},
    providers::{BackendFacts, ProviderOverride, ProviderStore},
    updates::{Release, UpdateEnv},
    version::VersionProbe,
};

use crate::ui::Ui;

pub use add_provider::{Catalog, ProbeFn, ProbeRequest};

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
    /// Add provider: the ACP registry (fetched on request), registry installs, the ACP check.
    pub catalog: Catalog,
    /// Who may open apps (Settings → Apps); `None`: not a container install (the host's
    /// firewall decides).
    pub app_access: Option<AppAccessControl>,
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
            catalog: Catalog::unavailable(),
            app_access: None,
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

    /// The rail's update button (a dot when a Melissa update is available).
    pub(crate) fn update_hint(&self) -> crate::components::UpdateHint {
        updates::hint(self)
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
        .route(add_provider::ADD_URL, get(add_provider::page))
        .route(
            "/ui/settings/providers/add/{id}",
            post(add_provider::add_known),
        )
        .route(
            "/ui/settings/providers/registry",
            get(add_provider::registry_list_region).post(add_provider::registry_refresh),
        )
        .route(
            "/ui/settings/providers/registry/{rid}/add",
            post(add_provider::registry_add),
        )
        .route("/ui/settings/providers/manual", post(add_provider::manual))
        .route(
            "/ui/settings/providers/new/{id}/key",
            post(add_provider::new_key),
        )
        .route(
            "/ui/settings/providers/new/{id}/finish",
            post(add_provider::finish),
        )
        .route(
            "/ui/settings/providers/{id}/remove",
            post(remove_provider::ui_remove),
        )
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
        .route(pages::AGENTS_URL, get(agents::page))
        .route(pages::NEW_AGENT_URL, get(agents::new_page))
        .route("/ui/settings/agents/new", post(agents::create))
        .route("/settings/agents/{id}", get(agents::agent_page))
        .route("/ui/settings/agents/{id}/rename", post(agents::rename))
        .route("/ui/settings/agents/{id}/icon", post(agents::icon))
        .route("/ui/settings/agents/{id}/pin", post(agents::pin))
        .route(
            "/ui/settings/agents/{id}/fresh-start",
            post(agents::fresh_start),
        )
        .route("/ui/settings/agents/{id}/delete", post(agents::delete))
        .route("/settings/permissions", get(permissions::page))
        .route(
            "/ui/settings/permissions/{id}/remove",
            post(permissions::remove),
        )
        .route("/ui/settings/developer/bypass", post(permissions::bypass))
        .route(
            "/ui/settings/developer/full-access",
            post(permissions::full_access),
        )
        .route(
            "/ui/settings/developer/bypass/on",
            post(permissions::bypass_on),
        )
        .route("/settings/schedules", get(schedules::page))
        .route("/ui/settings/schedules", post(schedules::create))
        .route("/ui/settings/schedules/{id}", post(schedules::update))
        .route(
            "/ui/settings/schedules/{id}/enabled",
            post(schedules::enabled),
        )
        .route("/ui/settings/schedules/{id}/run", post(schedules::run))
        .route(
            "/ui/settings/schedules/{id}/remove",
            post(schedules::remove),
        )
        .route("/ui/settings/schedules/wait", post(schedules::wait))
        .route(pages::APPS_URL, get(apps::page))
        .route("/ui/settings/apps", post(apps::save))
        .route(pages::FILES_URL, get(files::page))
        .route("/ui/settings/files", post(files::save))
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
        .route(
            "/api/settings/providers/{id}",
            post(providers::api_change).delete(remove_provider::api_remove),
        )
        .route("/api/settings/providers/add", post(add_provider::api_add))
        .route(
            "/api/settings/providers/registry",
            get(add_provider::api_registry),
        )
        .route(
            "/api/settings/providers/registry/refresh",
            post(add_provider::api_registry_refresh),
        )
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
        .route("/api/settings/permissions", get(permissions::api_list))
        .route(
            "/api/settings/permissions/rules/{id}/remove",
            post(permissions::api_remove),
        )
        .route(
            "/api/settings/permissions/bypass",
            post(permissions::api_bypass),
        )
        .route(
            "/api/settings/permissions/full-access",
            post(permissions::api_full_access),
        )
        .route(
            "/api/settings/permissions/access",
            post(permissions::api_access),
        )
        .route(
            "/api/schedules",
            get(schedules::api_list).post(schedules::api_create),
        )
        .route("/api/schedules/{id}", post(schedules::api_update))
        .route("/api/schedules/{id}/enabled", post(schedules::api_enabled))
        .route("/api/schedules/{id}/run", post(schedules::api_run))
        .route("/api/schedules/{id}/remove", post(schedules::api_remove))
        .route(
            "/api/settings/schedules",
            get(schedules::api_settings).post(schedules::api_set_settings),
        )
        .route("/api/settings/updates", get(updates::api_status))
        .merge(crate::files::api_routes())
        .merge(crate::details::api_routes())
        .route("/api/settings/updates/{action}", post(updates::api_action))
        .with_state(ui)
}
