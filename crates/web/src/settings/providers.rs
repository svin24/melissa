//! Settings → Providers: the agent programs, their state and the owner's overrides.

use std::collections::BTreeMap;

use axum::{
    extract::{Form, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Json,
};
use melissa_agent::queen::backends::BackendView;
use melissa_settings::{
    clean_text,
    install::{device_prompt, unit_name, InstallState, InstallStatus, InstallWay, Job},
    providers::{default_label, BackendFacts, Change, ProviderOverride},
    updates::{ago, now_secs, HostKind, UpdateAction},
    version,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{
    pages::{
        self, BackendLine, DeviceLogin, Flash, InstallView, ProviderCard, ProvidersView,
        RestartHint, SignIn,
    },
    Settings,
};
use crate::{components::Tone, is_htmx, ui::Ui};

/// Melissa's built-in test partner: never listed as a provider (UI and API). Its backends
/// still count as agents Melissa can run on (tests and `queen.toml` echo sessions).
const HIDDEN: &str = "echo";

/// The providers in display order (known ones first), each with its backends.
fn grouped(s: &Settings) -> Vec<(String, Vec<&BackendFacts>)> {
    let mut map: BTreeMap<String, Vec<&BackendFacts>> = BTreeMap::new();
    for b in s.parts().backends.iter().filter(|b| b.provider != HIDDEN) {
        map.entry(b.provider.clone()).or_default().push(b);
    }
    let rank = |id: &str| match id {
        "claude" => 0,
        "grok" => 1,
        "opencode" => 2,
        _ => 5,
    };
    let mut list: Vec<_> = map.into_iter().collect();
    list.sort_by(|a, b| rank(&a.0).cmp(&rank(&b.0)).then(a.0.cmp(&b.0)));
    list
}

fn reason_words(reason: &str) -> &'static str {
    match reason {
        "not_installed" => "not installed",
        "not_logged_in" => "not logged in",
        "sandbox" => "its sandbox can't start",
        _ => "not ready",
    }
}

/// The vendor whose installer `melissa-setup` runs.
fn vendor(id: &str) -> &'static str {
    match id {
        "claude" => "Anthropic",
        "grok" => "xAI",
        _ => "OpenCode",
    }
}

/// The Install section for a provider that isn't installed (None: nothing to offer).
fn install_view(
    id: &str,
    label: &str,
    way: InstallWay,
    host: HostKind,
    status: Option<InstallStatus>,
) -> Option<InstallView> {
    let known = unit_name(id).is_some();
    match way {
        InstallWay::Nix => Some(InstallView::Nix),
        InstallWay::Button if known => {
            let st = status.unwrap_or_else(InstallStatus::idle);
            Some(InstallView::Button {
                vendor: vendor(id),
                state: st.state,
                detail: st.detail,
                log: st.log,
            })
        }
        _ if !known => None,
        _ => Some(match host {
            // An image without the install unit: melissa-setup inside, typed on the host.
            HostKind::Container => InstallView::Manual {
                text: "Installing needs an administrator. Run this on the host, then press Check \
                       again:"
                    .into(),
                command: Some(format!(
                    "sudo podman exec -it melissa melissa-setup agent {id}"
                )),
            },
            HostKind::Other => InstallView::Manual {
                text: format!(
                    "Install {label} on this computer so that {id} is on Melissa's PATH, then \
                     restart Melissa."
                ),
                command: None,
            },
            // Unreachable in practice (a Nix host gives InstallWay::Nix), kept total.
            HostKind::Nix => InstallView::Nix,
        }),
    }
}

/// How to sign in, for an installed provider without a key. `needed`: Melissa knows it isn't
/// signed in (or it is new).
fn sign_in(id: &str, host: HostKind, needed: bool) -> Option<SignIn> {
    // A login on the device needs melissa-setup inside the container (not on Nix or from source).
    let device = |id: &str| {
        (host == HostKind::Container)
            .then(|| format!("sudo podman exec -it melissa melissa-setup login {id}"))
    };
    match id {
        "claude" if needed => Some(SignIn {
            text: "Claude Code needs an Anthropic account. Add an API key from the Anthropic \
                   Console below as ANTHROPIC_API_KEY. With a Claude subscription, run \
                   `claude setup-token` on your own computer and add the token it prints as \
                   CLAUDE_CODE_OAUTH_TOKEN. Melissa never asks for your password.",
            command: device(id),
            device: None,
        }),
        "grok" if needed => Some(SignIn {
            text: "Grok needs an xAI account. Sign in with a code you confirm on xAI's site (on \
                   any device), or add an API key from the xAI Console below as XAI_API_KEY. \
                   Melissa never asks for your password.",
            command: device(id),
            device: None,
        }),
        "opencode" => Some(SignIn {
            text: "OpenCode works without an account, using its free models (your prompts go \
                   to OpenCode's hosted service). To use your own provider instead, add its API \
                   key below, e.g. ANTHROPIC_API_KEY or OPENAI_API_KEY.",
            command: None,
            device: None,
        }),
        _ => None,
    }
}

/// A device-code sign-in's state for the page: the vendor's URL and code once printed.
fn device_view(st: InstallStatus) -> DeviceLogin {
    let prompt = device_prompt(&st.log);
    DeviceLogin {
        state: st.state,
        url: prompt.as_ref().map(|p| p.0.clone()),
        code: prompt.map(|p| p.1),
        detail: st.detail,
        log: st.log,
    }
}

/// The example name in the Environment form.
fn var_example(id: &str) -> &'static str {
    match id {
        "claude" => "e.g. ANTHROPIC_API_KEY",
        "grok" => "e.g. XAI_API_KEY",
        _ => "e.g. API_BASE_URL",
    }
}

/// Everything about one provider, for the page and the API.
struct Info {
    card: ProviderCard,
    running: bool,
    installed: bool,
    ready: bool,
    /// Installed after Melissa started (she uses it after a restart).
    fresh: bool,
    install_state: Option<InstallState>,
    login_state: Option<InstallState>,
}

/// Why `id` can't be disabled now (None: it can).
fn lock_reason(ui: &Ui, id: &str, views: &[BackendView]) -> Option<String> {
    let s = &ui.settings;
    let of = |name: &str| {
        s.parts()
            .backends
            .iter()
            .find(|b| b.name == name)
            .map(|b| b.provider.clone())
    };
    if views
        .iter()
        .any(|v| v.current && of(&v.name).as_deref() == Some(id))
    {
        return Some(
            "Melissa runs on it now. Switch her to another agent in the chat first.".into(),
        );
    }
    let helpers: Vec<String> = ui
        .queen
        .bees()
        .list()
        .into_iter()
        .filter(|b| of(&b.backend).as_deref() == Some(id))
        .map(|b| b.name)
        .collect();
    if !helpers.is_empty() {
        return Some(format!(
            "Helpers use it ({}). Switch them to another agent first.",
            helpers.join(", ")
        ));
    }
    let others = views
        .iter()
        .any(|v| of(&v.name).is_some_and(|p| p != id && s.parts().store.get(&p).is_enabled()));
    if !others {
        return Some("Melissa needs at least one other agent to run on.".into());
    }
    None
}

async fn info(ui: &Ui, id: &str, facts: &[&BackendFacts], views: &[BackendView]) -> Info {
    let s = &ui.settings;
    let saved = s.parts().store.get(id);
    let running = facts.iter().any(|f| f.enabled);
    let command = facts.iter().find_map(|f| f.command.clone());
    let way = s.parts().installer.way;
    let install_status = if way == InstallWay::Button && unit_name(id).is_some() {
        Some(s.parts().installer.status(id).await)
    } else {
        None
    };
    let install_state = install_status.as_ref().map(|i| i.state);
    // The program can appear before its installer is done: not installed until the unit ends.
    let program = if install_state == Some(InstallState::Running) {
        None
    } else {
        command.as_deref().and_then(version::resolve)
    };
    let version = match &program {
        Some(p) => s.versions().version(p).await,
        None => None,
    };
    let installed = program.is_some();
    let view = |name: &str| views.iter().find(|v| v.name == name);
    let mut reasons: Vec<&str> = Vec::new();
    let backends: Vec<BackendLine> = facts
        .iter()
        .map(|f| {
            let state = if !f.enabled {
                "left out (disabled)".to_string()
            } else if let Some(v) = view(&f.name) {
                match v.reason {
                    None => "ready".into(),
                    Some(r) => {
                        reasons.push(r);
                        reason_words(r).into()
                    }
                }
            } else if installed {
                "installed".into()
            } else {
                "not installed".into()
            };
            BackendLine {
                name: f.name.clone(),
                kind: if f.session {
                    "chat session"
                } else {
                    "one-shot task"
                },
                state,
            }
        })
        .collect();
    let secret_set = saved
        .env
        .keys()
        .any(|k| melissa_settings::providers::is_secret_name(k));
    let mut extras = Vec::new();
    if saved.command.is_some() {
        extras.push("Custom program".to_string());
    }
    if secret_set {
        extras.push("API key set".to_string());
    } else if !saved.env.is_empty() {
        extras.push(match saved.env.len() {
            1 => "1 variable".to_string(),
            n => format!("{n} variables"),
        });
    }
    // Installed after Melissa started: she finds it after a restart.
    let fresh = installed && running && reasons.contains(&"not_installed");
    let (mut status, tone, ready) = if install_state == Some(InstallState::Running) {
        ("Installing…".to_string(), Tone::Info, false)
    } else if !saved.is_enabled() {
        let s = if running {
            "Disabled · after a restart"
        } else {
            "Disabled"
        };
        (s.to_string(), Tone::Muted, false)
    } else if !running {
        ("Enabled · after a restart".into(), Tone::Info, false)
    } else if !installed && install_state == Some(InstallState::Failed) {
        ("Install failed".into(), Tone::Error, false)
    } else if !installed {
        ("Not installed".into(), Tone::Warn, false)
    } else if fresh {
        ("Installed · after a restart".into(), Tone::Info, false)
    } else if reasons.contains(&"not_logged_in") {
        ("Not logged in".into(), Tone::Warn, false)
    } else if reasons.contains(&"sandbox") {
        ("Sandbox can't start".into(), Tone::Error, false)
    } else {
        ("Ready".into(), Tone::Ok, true)
    };
    if !extras.is_empty() && saved.is_enabled() {
        status = format!("{status} · {}", extras.join(" · "));
    }
    // Who uses it.
    let mine = |name: &str| facts.iter().any(|f| f.name == name);
    let mut used_by = Vec::new();
    if views.iter().any(|v| v.current && mine(&v.name)) {
        used_by.push("Melissa (now)".to_string());
    }
    if s.parts().bee_default.as_deref().is_some_and(mine) {
        used_by.push("New helpers".to_string());
    }
    for b in ui.queen.bees().list() {
        if mine(&b.backend) {
            used_by.push(format!("Helper {}", b.name));
        }
    }
    let mut models: Vec<String> = Vec::new();
    for f in facts {
        for m in view(&f.name)
            .and_then(|v| v.models.clone())
            .unwrap_or_default()
        {
            let label = clean_text(if m.label.is_empty() { &m.id } else { &m.label }, 60);
            if !models.contains(&label) {
                models.push(label);
            }
        }
    }
    let label = saved
        .display_name
        .clone()
        .unwrap_or_else(|| default_label(id).to_string());
    let host = s.parts().updates.kind;
    let install = if installed {
        None
    } else {
        install_view(id, &label, way, host, install_status)
    };
    let mut sign_in = if installed && !secret_set {
        sign_in(id, host, reasons.contains(&"not_logged_in") || fresh)
    } else {
        None
    };
    // A device-code sign-in from the page where the unit is there (Grok only).
    let mut login_state = None;
    if let Some(si) = sign_in.as_mut() {
        if way == InstallWay::Button && Job::Login.unit(id).is_some() {
            let st = s.parts().installer.job_status(Job::Login, id).await;
            login_state = Some(st.state);
            si.command = None;
            si.device = Some(device_view(st));
        }
    }
    let lock = if saved.is_enabled() {
        lock_reason(ui, id, views)
    } else {
        None
    };
    let default_command = facts
        .iter()
        .find_map(|f| f.command.clone())
        .unwrap_or_default();
    Info {
        card: ProviderCard {
            id: id.to_string(),
            label,
            version,
            enabled: saved.is_enabled(),
            status,
            tone,
            program: program.map(|p| p.display().to_string()),
            used_by,
            lock,
            backends,
            models,
            display_name: saved.display_name.clone().unwrap_or_default(),
            command: saved.command.clone().unwrap_or_default(),
            default_command,
            args: saved.args.clone(),
            env: saved.env_view(),
            install,
            sign_in,
            var_example: var_example(id),
        },
        running,
        installed,
        ready,
        fresh,
        install_state,
        login_state,
    }
}

async fn infos(ui: &Ui) -> Vec<Info> {
    let views = ui.queen.backends();
    let mut out = Vec::new();
    for (id, facts) in grouped(&ui.settings) {
        out.push(info(ui, &id, &facts, &views).await);
    }
    out
}

/// Why a restart is needed for saved settings.
pub(super) const SAVED: &str = "Saved. Melissa uses the new provider settings after a restart.";
const INSTALLED: &str = "Installed. Melissa starts using it after a restart.";

pub(super) fn restart_hint(s: &Settings, text: &'static str) -> RestartHint {
    let env = &s.parts().updates;
    RestartHint {
        text,
        button: env.can(UpdateAction::Restart),
        command: match env.kind {
            HostKind::Other => "# stop melissa-queen (Ctrl-C) and start it again".into(),
            HostKind::Container => UpdateAction::Restart.manual_command(),
            HostKind::Nix => "sudo systemctl restart melissa-queen".into(),
        },
    }
}

async fn view(ui: &Ui, selected: Option<&str>, flash: Option<Flash>) -> ProvidersView {
    let s = &ui.settings;
    let all = infos(ui).await;
    let installing = all.iter().any(|i| {
        i.install_state == Some(InstallState::Running)
            || i.login_state == Some(InstallState::Running)
    });
    let fresh = all.iter().any(|i| i.fresh);
    let cards: Vec<ProviderCard> = all.into_iter().map(|i| i.card).collect();
    let selected = selected
        .and_then(|id| cards.iter().position(|c| c.id == id))
        .unwrap_or(0);
    let restart = if s.parts().store.restart_needed() {
        Some(restart_hint(s, SAVED))
    } else if fresh {
        Some(restart_hint(s, INSTALLED))
    } else {
        None
    };
    let poll = installing.then(|| match cards.get(selected) {
        Some(c) => format!("/ui/settings/providers/poll?p={}", c.id),
        None => "/ui/settings/providers/poll".into(),
    });
    let checked = match s.versions().checked_at() {
        Some(t) => {
            let at = t
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            format!("Checked {}", ago(at, now_secs()))
        }
        None => "Not checked yet".into(),
    };
    ProvidersView {
        cards,
        selected,
        checked,
        restart,
        flash,
        home: s.parts().home.as_ref().map(|h| h.display().to_string()),
        file: s.parts().store.path().map(|p| p.display().to_string()),
        poll,
    }
}

// --- Changes --------------------------------------------------------------------------------

pub(super) enum ChangeError {
    Unknown,
    Refused(String),
    Invalid(String),
}

impl ChangeError {
    fn text(&self) -> String {
        match self {
            Self::Unknown => "No such provider.".into(),
            Self::Refused(m) | Self::Invalid(m) => m.clone(),
        }
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::Unknown => StatusCode::NOT_FOUND,
            Self::Refused(_) => StatusCode::CONFLICT,
            Self::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
        }
    }
}

/// Check, save and audit one change. The same path for the UI and the API.
fn apply(ui: &Ui, id: &str, change: Change, actor: &str) -> Result<(), ChangeError> {
    let s = &ui.settings;
    if id == HIDDEN || !s.parts().backends.iter().any(|b| b.provider == id) {
        return Err(ChangeError::Unknown);
    }
    if change == Change::Enable(false) {
        if let Some(why) = lock_reason(ui, id, &ui.queen.backends()) {
            return Err(ChangeError::Refused(format!("Can't disable it: {why}")));
        }
    }
    let mut next: ProviderOverride = s.parts().store.get(id);
    change.apply_to(&mut next);
    next.validate(id)
        .map_err(|e| ChangeError::Invalid(e.to_string()))?;
    if let Some(check) = &s.parts().check {
        check(id, &next).map_err(ChangeError::Invalid)?;
    }
    s.parts()
        .store
        .change(id, &change)
        .map_err(|e| ChangeError::Invalid(e.to_string()))?;
    let (what, hash) = change.describe();
    s.audit("settings.provider", actor, &format!("{id}: {what}"), hash);
    Ok(())
}

/// The answer to a UI form: the region (htmx), else a redirect or the page with the error.
async fn reply(
    ui: &Ui,
    headers: &HeaderMap,
    id: &str,
    result: Result<String, ChangeError>,
) -> Response {
    let (flash, status) = match &result {
        Ok(text) => (
            Flash {
                text: text.clone(),
                tone: Tone::Ok,
            },
            StatusCode::OK,
        ),
        Err(e) => (
            Flash {
                text: e.text(),
                tone: Tone::Error,
            },
            e.status(),
        ),
    };
    if is_htmx(headers) {
        return pages::providers_region(&view(ui, Some(id), Some(flash)).await).into_response();
    }
    if result.is_ok() {
        return Redirect::to(&format!("/settings/providers?p={id}")).into_response();
    }
    let v = view(ui, Some(id), Some(flash)).await;
    (status, ui.framed(|f| pages::providers_page(f, &v)).await).into_response()
}

#[derive(Deserialize)]
pub(super) struct PickQuery {
    #[serde(default)]
    p: Option<String>,
}

pub(super) async fn page(State(ui): State<Ui>, Query(q): Query<PickQuery>) -> Response {
    let v = view(&ui, q.p.as_deref(), None).await;
    ui.framed(|f| pages::providers_page(f, &v))
        .await
        .into_response()
}

pub(super) async fn refresh(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Query(q): Query<PickQuery>,
) -> Response {
    ui.settings.versions().forget();
    if is_htmx(&headers) {
        // Keep the provider that was open: htmx sends the page URL.
        let current = headers
            .get("hx-current-url")
            .and_then(|v| v.to_str().ok())
            .and_then(|u| u.split("p=").nth(1))
            .map(|p| p.split('&').next().unwrap_or(p).to_string());
        let pick = q.p.or(current);
        return pages::providers_region(&view(&ui, pick.as_deref(), None).await).into_response();
    }
    Redirect::to("/settings/providers").into_response()
}

pub(super) async fn enable(
    State(ui): State<Ui>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<Vec<(String, String)>>,
) -> Response {
    let on = form.iter().any(|(k, v)| k == "enabled" && v == "on");
    let result = apply(&ui, &id, Change::Enable(on), "ui").map(|()| {
        if on {
            "Enabled.".to_string()
        } else {
            "Disabled.".to_string()
        }
    });
    reply(&ui, &headers, &id, result).await
}

// --- Install ----------------------------------------------------------------------------------

/// Check, start and audit an install. The same path for the UI and the API.
async fn start_install(ui: &Ui, id: &str, actor: &str) -> Result<String, ChangeError> {
    let s = &ui.settings;
    let result = install_checked(ui, id).await;
    let detail = match &result {
        Ok(_) => format!("{}: install requested", clean_text(id, 40)),
        Err(e) => format!("{}: install refused: {}", clean_text(id, 40), e.text()),
    };
    s.audit("settings.provider.install", actor, &detail, None);
    result
}

async fn install_checked(ui: &Ui, id: &str) -> Result<String, ChangeError> {
    let s = &ui.settings;
    let installer = &s.parts().installer;
    // Only a configured provider from the fixed list.
    let Some((_, facts)) = grouped(s).into_iter().find(|(p, _)| p == id) else {
        return Err(ChangeError::Unknown);
    };
    if unit_name(id).is_none() {
        return Err(ChangeError::Invalid(
            "Melissa can't install this provider; install it on the device.".into(),
        ));
    }
    match installer.way {
        InstallWay::Button => {}
        InstallWay::Nix => {
            return Err(ChangeError::Refused(format!(
                "This requires you add {} via your Nix configuration.",
                default_label(id)
            )))
        }
        InstallWay::Manual => {
            return Err(ChangeError::Refused(
                "Installing from here isn't available on this device.".into(),
            ))
        }
    }
    let installed = facts
        .iter()
        .find_map(|f| f.command.as_deref())
        .and_then(version::resolve)
        .is_some();
    if installed {
        return Err(ChangeError::Refused("It's already installed.".into()));
    }
    if installer.status(id).await.state == InstallState::Running {
        return Err(ChangeError::Refused("It's being installed now.".into()));
    }
    installer
        .start(id)
        .await
        .map_err(|e| ChangeError::Refused(format!("Can't start the install: {e}")))?;
    // `--no-block` returns before the unit starts: wait briefly so the page shows it running.
    for _ in 0..20 {
        if installer.status(id).await.state != InstallState::Idle {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
    Ok(format!("Installing {}.", default_label(id)))
}

/// Check, start and audit a device-code sign-in (Grok). The same path for the UI and the API.
async fn start_login(ui: &Ui, id: &str, actor: &str) -> Result<String, ChangeError> {
    let result = login_checked(ui, id).await;
    let detail = match &result {
        Ok(_) => format!("{}: device sign-in requested", clean_text(id, 40)),
        Err(e) => format!(
            "{}: device sign-in refused: {}",
            clean_text(id, 40),
            e.text()
        ),
    };
    ui.settings
        .audit("settings.provider.login", actor, &detail, None);
    result
}

async fn login_checked(ui: &Ui, id: &str) -> Result<String, ChangeError> {
    let s = &ui.settings;
    let installer = &s.parts().installer;
    let Some((_, facts)) = grouped(s).into_iter().find(|(p, _)| p == id) else {
        return Err(ChangeError::Unknown);
    };
    if Job::Login.unit(id).is_none() {
        return Err(ChangeError::Invalid(
            "Melissa can't sign this provider in from here; add an API key instead.".into(),
        ));
    }
    if installer.way != InstallWay::Button {
        return Err(ChangeError::Refused(
            "Signing in from here isn't available on this device.".into(),
        ));
    }
    let installed = facts
        .iter()
        .find_map(|f| f.command.as_deref())
        .and_then(version::resolve)
        .is_some();
    if !installed {
        return Err(ChangeError::Refused("Install it first.".into()));
    }
    if installer.job_status(Job::Login, id).await.state == InstallState::Running {
        return Err(ChangeError::Refused("A sign-in is waiting already.".into()));
    }
    installer
        .start_job(Job::Login, id)
        .await
        .map_err(|e| ChangeError::Refused(format!("Can't start the sign-in: {e}")))?;
    // Wait for the vendor's URL and code (a few seconds), so the page shows them at once.
    for _ in 0..30 {
        let st = installer.job_status(Job::Login, id).await;
        if st.state != InstallState::Running && st.state != InstallState::Idle
            || device_prompt(&st.log).is_some()
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    Ok("Open the link and confirm the code to sign in.".into())
}

pub(super) async fn login(
    State(ui): State<Ui>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let result = start_login(&ui, &id, "ui").await;
    reply(&ui, &headers, &id, result).await
}

pub(super) async fn api_login(State(ui): State<Ui>, Path(id): Path<String>) -> Response {
    match start_login(&ui, &id, "api").await {
        Ok(_) => (StatusCode::ACCEPTED, api_install_json(&ui, &id).await).into_response(),
        Err(e) => (e.status(), Json(json!({"error": e.text()}))).into_response(),
    }
}

pub(super) async fn install(
    State(ui): State<Ui>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let result = start_install(&ui, &id, "ui").await;
    reply(&ui, &headers, &id, result).await
}

/// The region again while an install runs (htmx polling).
pub(super) async fn poll(State(ui): State<Ui>, Query(q): Query<PickQuery>) -> Response {
    pages::providers_region(&view(&ui, q.p.as_deref(), None).await).into_response()
}

pub(super) async fn api_install(State(ui): State<Ui>, Path(id): Path<String>) -> Response {
    match start_install(&ui, &id, "api").await {
        Ok(_) => (StatusCode::ACCEPTED, api_install_json(&ui, &id).await).into_response(),
        Err(e) => (e.status(), Json(json!({"error": e.text()}))).into_response(),
    }
}

pub(super) async fn api_install_status(State(ui): State<Ui>, Path(id): Path<String>) -> Response {
    if !grouped(&ui.settings).iter().any(|(p, _)| p == &id) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "No such provider."})),
        )
            .into_response();
    }
    api_install_json(&ui, &id).await.into_response()
}

async fn api_install_json(ui: &Ui, id: &str) -> Json<Value> {
    let i = infos(ui).await.into_iter().find(|i| i.card.id == id);
    Json(match i {
        Some(i) => json!({
            "provider": id,
            "installed": i.installed,
            "version": i.card.version,
            "status": i.card.status,
            "install": install_json(&i.card),
            "sign_in": sign_in_json(&i.card),
        }),
        None => json!({"provider": id}),
    })
}

#[derive(Deserialize)]
pub(super) struct SaveForm {
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    command: String,
    #[serde(default)]
    args: String,
}

fn opt(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

pub(super) async fn save(
    State(ui): State<Ui>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<SaveForm>,
) -> Response {
    let saved = ui.settings.parts().store.get(&id);
    let args: Vec<String> = form
        .args
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    let mut changes = Vec::new();
    if opt(&form.display_name) != saved.display_name {
        changes.push(Change::DisplayName(opt(&form.display_name)));
    }
    if opt(&form.command) != saved.command {
        changes.push(Change::Command(opt(&form.command)));
    }
    if args != saved.args {
        changes.push(Change::Args(args));
    }
    let result = if changes.is_empty() {
        Ok("Nothing changed.".to_string())
    } else {
        changes
            .into_iter()
            .try_for_each(|c| apply(&ui, &id, c, "ui"))
            .map(|()| "Saved.".to_string())
    };
    reply(&ui, &headers, &id, result).await
}

#[derive(Deserialize)]
pub(super) struct VarForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    value: String,
}

pub(super) async fn set_var(
    State(ui): State<Ui>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<VarForm>,
) -> Response {
    let name = form.name.trim().to_string();
    let result = if name.is_empty() || form.value.is_empty() {
        Err(ChangeError::Invalid("Enter a name and a value.".into()))
    } else {
        apply(
            &ui,
            &id,
            Change::SetVar {
                name: name.clone(),
                value: form.value,
            },
            "ui",
        )
        .map(|()| format!("{name} saved."))
    };
    reply(&ui, &headers, &id, result).await
}

pub(super) async fn remove_var(
    State(ui): State<Ui>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<VarForm>,
) -> Response {
    let name = form.name.trim().to_string();
    let result =
        apply(&ui, &id, Change::RemoveVar(name.clone()), "ui").map(|()| format!("{name} removed."));
    reply(&ui, &headers, &id, result).await
}

// --- API ------------------------------------------------------------------------------------

fn info_json(i: &Info) -> Value {
    let c = &i.card;
    let backends: Vec<Value> = c
        .backends
        .iter()
        .map(|b| json!({"name": b.name, "kind": b.kind, "state": b.state}))
        .collect();
    json!({
        "id": c.id,
        "label": c.label,
        "enabled": c.enabled,
        "running": i.running,
        "installed": i.installed,
        "ready": i.ready,
        "program": c.program,
        "version": c.version,
        "status": c.status,
        "used_by": c.used_by,
        "can_disable": c.lock.is_none(),
        "lock": c.lock,
        "backends": backends,
        "models": c.models,
        "install": install_json(c),
        "sign_in": sign_in_json(c),
        "settings": {
            "display_name": (!c.display_name.is_empty()).then_some(&c.display_name),
            "command": (!c.command.is_empty()).then_some(&c.command),
            "args": c.args,
            "env": c.env,
        },
    })
}

fn sign_in_json(c: &ProviderCard) -> Value {
    let Some(si) = &c.sign_in else {
        return Value::Null;
    };
    let device = si.device.as_ref().map(
        |d| json!({"state": d.state.as_str(), "url": d.url, "code": d.code, "detail": d.detail}),
    );
    json!({"text": si.text, "command": si.command, "device": device})
}

fn install_json(c: &ProviderCard) -> Value {
    match &c.install {
        None => Value::Null,
        Some(InstallView::Button {
            state, detail, log, ..
        }) => json!({"way": "button", "state": state.as_str(), "detail": detail, "log": log}),
        Some(InstallView::Nix) => json!({
            "way": "nix",
            "text": format!("This requires you add {} via your Nix configuration.", c.label),
        }),
        Some(InstallView::Manual { text, command }) => {
            json!({"way": "manual", "text": text, "command": command})
        }
    }
}

async fn list_json(ui: &Ui) -> Value {
    let restart = ui.settings.parts().store.restart_needed();
    let list: Vec<Value> = infos(ui).await.iter().map(info_json).collect();
    json!({
        "providers": list,
        "restart_needed": restart,
        "file": ui.settings.parts().store.path(),
    })
}

pub(super) async fn api_list(State(ui): State<Ui>) -> Json<Value> {
    Json(list_json(&ui).await)
}

pub(super) async fn api_refresh(State(ui): State<Ui>) -> Json<Value> {
    ui.settings.versions().forget();
    Json(list_json(&ui).await)
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct ChangeBody {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    set_env: BTreeMap<String, String>,
    #[serde(default)]
    remove_env: Vec<String>,
}

pub(super) async fn api_change(
    State(ui): State<Ui>,
    Path(id): Path<String>,
    Json(body): Json<ChangeBody>,
) -> Response {
    let mut changes = Vec::new();
    if let Some(on) = body.enabled {
        changes.push(Change::Enable(on));
    }
    if let Some(n) = body.display_name {
        changes.push(Change::DisplayName(opt(&n)));
    }
    if let Some(c) = body.command {
        changes.push(Change::Command(opt(&c)));
    }
    if let Some(a) = body.args {
        changes.push(Change::Args(a));
    }
    for (name, value) in body.set_env {
        changes.push(Change::SetVar { name, value });
    }
    for name in body.remove_env {
        changes.push(Change::RemoveVar(name));
    }
    for c in changes {
        if let Err(e) = apply(&ui, &id, c, "api") {
            return (e.status(), Json(json!({"error": e.text()}))).into_response();
        }
    }
    let views = ui.queen.backends();
    let restart = ui.settings.parts().store.restart_needed();
    for (pid, facts) in grouped(&ui.settings) {
        if pid == id {
            let i = info(&ui, &pid, &facts, &views).await;
            let mut v = info_json(&i);
            v["restart_needed"] = json!(restart);
            return Json(v).into_response();
        }
    }
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": "No such provider."})),
    )
        .into_response()
}

/// Each agent program and its version ("not installed"), for the Updates page.
pub(super) async fn agent_versions(ui: &Ui) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (id, facts) in grouped(&ui.settings) {
        if id == "echo" {
            continue;
        }
        let program = facts
            .iter()
            .find_map(|f| f.command.as_deref())
            .and_then(version::resolve);
        let text = match program {
            Some(p) => match ui.settings.versions().version(&p).await {
                Some(v) => format!("version {v}"),
                None => "installed (version unknown)".into(),
            },
            None => "not installed".into(),
        };
        let label = ui
            .settings
            .parts()
            .store
            .get(&id)
            .display_name
            .unwrap_or_else(|| default_label(&id).to_string());
        out.push((label, text));
    }
    out
}
