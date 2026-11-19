//! Settings: pages under `/settings/...`, their forms under `/ui/settings/...` and a JSON API
//! under `/api/settings/...`. Two sections today (Providers, Updates); a new section is a
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
//!   `POST /ui/settings/providers/{id}/env/remove` (`name`).
//! - `POST /ui/settings/updates/{check|apply|rollback}`: apply and rollback first return a
//!   confirmation card; only a post with `confirm=yes` starts them.
//! - `POST /ui/settings/restart`: restart Melissa to use saved settings.
//!
//! API (JSON; never returns secret values):
//! - `GET /api/settings/providers` → `{"providers": [...], "restart_needed": bool}`.
//! - `POST /api/settings/providers/{id}` `{"enabled"?, "display_name"?, "command"?, "args"?,
//!   "set_env"?: {"NAME": "value"}, "remove_env"?: ["NAME"]}` → the provider. 404 unknown
//!   provider, 409 refused (e.g. disabling the agent the Queen runs on), 422 invalid.
//! - `POST /api/settings/providers/refresh` → the list again, versions re-read.
//! - `GET /api/settings/updates` → release, host kind, OS image state, available actions.
//! - `POST /api/settings/updates/{check|apply|rollback|restart}` `{"confirm": true}` (needed
//!   for apply/rollback) → 202; 409 without `confirm`; 501 where the root helper is missing.
//!
//! Every change is audited (`settings.provider`, `settings.update`): what changed and the
//! SHA-256 of the new value, never the value.

pub(crate) mod pages;
mod providers;
mod updates;

use std::{path::PathBuf, sync::Arc};

use axum::{
    response::Redirect,
    routing::{get, post},
    Router,
};
use melissa_agent::{AuditLog, AuditRecord};
use melissa_settings::{
    install::Installer,
    providers::{BackendFacts, ProviderOverride, ProviderStore},
    updates::{HostKind, Release, UpdateEnv},
    version::VersionProbe,
};

use crate::ui::Ui;

/// Checks a provider change against the real backend config (the queen's merge + its
/// backends' own checks) before it is saved.
pub type CheckFn = Arc<dyn Fn(&str, &ProviderOverride) -> Result<(), String> + Send + Sync>;

/// What the queen hands the Settings pages.
pub struct SettingsParts {
    pub store: ProviderStore,
    /// Every configured backend, also those of disabled providers, plus the built-in echo.
    pub backends: Vec<BackendFacts>,
    /// The backend new bees get when the Queen names none.
    pub bee_default: Option<String>,
    pub check: Option<CheckFn>,
    pub audit: Option<AuditLog>,
    pub updates: UpdateEnv,
    pub release: Release,
    /// The agents' home (`$HOME` of the queen's user).
    pub home: Option<PathBuf>,
    /// Installs agent CLIs (Settings → Providers → Install), where the packaging allows it.
    pub installer: Installer,
}

/// The Settings context. Cheap to clone.
#[derive(Clone)]
pub struct Settings {
    inner: Arc<Inner>,
}

struct Inner {
    parts: SettingsParts,
    versions: VersionProbe,
}

impl Settings {
    pub fn new(parts: SettingsParts) -> Self {
        Self {
            inner: Arc::new(Inner {
                parts,
                versions: VersionProbe::new(),
            }),
        }
    }

    /// What kind of host this is (container image, Nix, other), e.g. for setup hints.
    pub(crate) fn host(&self) -> HostKind {
        self.inner.parts.updates.kind
    }

    /// No settings file, no backends (routers built without a queen config).
    pub fn detached() -> Self {
        let updates = UpdateEnv::system();
        Self::new(SettingsParts {
            store: ProviderStore::in_memory(),
            backends: Vec::new(),
            bee_default: None,
            check: None,
            audit: None,
            installer: Installer::system(updates.kind),
            updates,
            release: Release::read(&melissa_settings::updates::release_file()),
            home: std::env::var_os("HOME").map(PathBuf::from),
        })
    }

    fn parts(&self) -> &SettingsParts {
        &self.inner.parts
    }

    fn versions(&self) -> &VersionProbe {
        &self.inner.versions
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
            "/ui/settings/providers/{id}/env/remove",
            post(providers::remove_var),
        )
        .route("/settings/updates", get(updates::page))
        .route("/ui/settings/updates/{action}", post(updates::action))
        .route("/ui/settings/restart", post(updates::restart))
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
        .route("/api/settings/updates", get(updates::api_status))
        .route("/api/settings/updates/{action}", post(updates::api_action))
        .with_state(ui)
}
