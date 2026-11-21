//! Settings → Providers: the agent programs, their state and the owner's overrides.
//!
//! One status word per provider and at most one thing to do (Install, Sign in), everything
//! technical behind "Details". Installs and sign-ins run in fixed systemd units
//! (`melissa_settings::install`); Melissa picks up their result without a restart, and saved
//! settings apply by themselves once she is idle (`apply.rs`).

use std::collections::BTreeMap;

use axum::{
    extract::{Form, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Json,
};
use melissa_agent::lead::backends::BackendView;
use melissa_settings::{
    clean_text,
    install::{
        claude_login_url, device_prompt, unit_name, valid_code, InstallState, InstallStatus,
        InstallWay, Job, LoginEnd,
    },
    providers::{default_label, BackendFacts, Change, ProviderOverride},
    updates::{ago, now_secs, HostKind},
    version,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{
    apply::{self, Pending},
    pages::{self, BackendLine, Flash, ProviderCard, ProviderDetails, ProvidersView, Step},
    Settings,
};
use crate::{components::Tone, is_htmx, ui::Ui};

/// Melissa's built-in test partner: never listed as a provider (UI and API). Its backends
/// still count as agents Melissa can run on (tests and `server.toml` echo sessions).
const HIDDEN: &str = "echo";

/// The providers in display order (known ones first), each with its backends.
fn grouped(s: &Settings) -> Vec<(String, Vec<&BackendFacts>)> {
    let mut map: BTreeMap<String, Vec<&BackendFacts>> = BTreeMap::new();
    // Removed providers are hidden (Add provider brings them back).
    for b in s
        .parts()
        .backends
        .iter()
        .filter(|b| b.provider != HIDDEN && !s.parts().store.is_removed(&b.provider))
    {
        map.entry(b.provider.clone()).or_default().push(b);
    }
    let rank = |id: &str| match id {
        "claude" => 0,
        "grok" => 1,
        "opencode" => 2,
        "gemini" => 3,
        "codex-acp" => 4,
        "muse" => 5,
        _ => 9,
    };
    let mut list: Vec<_> = map.into_iter().collect();
    list.sort_by(|a, b| rank(&a.0).cmp(&rank(&b.0)).then(a.0.cmp(&b.0)));
    list
}

fn reason_words(reason: &str) -> &'static str {
    match reason {
        "not_installed" => "not installed",
        "not_logged_in" => "not signed in",
        "sandbox" => "its sandbox can't start",
        _ => "not ready",
    }
}

/// The vendor whose installer `melissa-setup` runs.
fn vendor(id: &str) -> &'static str {
    match id {
        "claude" => "Anthropic",
        "grok" => "xAI",
        "gemini" => "Google",
        "codex-acp" => "OpenAI",
        "muse" => "Meta",
        _ => "OpenCode",
    }
}

/// The variable "Use an API key instead" fills (only providers whose CLI reads one).
pub(super) fn key_var(id: &str) -> Option<&'static str> {
    match id {
        "claude" => Some("ANTHROPIC_API_KEY"),
        "grok" => Some("XAI_API_KEY"),
        "gemini" => Some("GEMINI_API_KEY"),
        "codex-acp" => Some("OPENAI_API_KEY"),
        "muse" => Some("META_API_KEY"),
        _ => None,
    }
}

/// What to do for a provider that isn't installed.
fn install_step(id: &str, label: &str, way: InstallWay, status: Option<InstallStatus>) -> Step {
    let known = unit_name(id).is_some();
    match way {
        InstallWay::Nix => Step::Note(format!(
            "This requires you add {label} via your Nix configuration."
        )),
        InstallWay::Button if known => {
            let st = status.unwrap_or_else(InstallStatus::idle);
            match st.state {
                InstallState::Running => Step::Installing,
                InstallState::Idle => Step::Install {
                    vendor: vendor(id),
                    failed: None,
                    log: Vec::new(),
                },
                InstallState::Failed => Step::Install {
                    vendor: vendor(id),
                    failed: Some("The install failed.".into()),
                    log: st.log,
                },
                InstallState::Done => Step::Install {
                    vendor: vendor(id),
                    failed: Some("The installer finished, but the program wasn't found.".into()),
                    log: st.log,
                },
            }
        }
        _ if !known => Step::Nothing,
        // A run from source, or an image without the install unit: no commands in the UI.
        _ => Step::Note(format!(
            "Install {label} on this computer, then press Check again."
        )),
    }
}

/// The page's words for a sign-in that ended without signing in (None: say nothing).
fn login_failed(end: LoginEnd) -> Option<&'static str> {
    match end {
        LoginEnd::WrongCode => Some("That code didn't work. Sign in again for a new one."),
        LoginEnd::Cancelled => None,
        LoginEnd::TimedOut => Some("The sign-in timed out."),
        LoginEnd::Failed => Some("The sign-in didn't finish."),
    }
}

/// The sign-in step while a sign-in unit runs.
fn login_running(id: &str, st: &InstallStatus, code_sent: bool) -> Step {
    if id == "claude" {
        if code_sent {
            return Step::Checking;
        }
        return match claude_login_url(&st.log) {
            Some(url) => Step::PasteCode { url },
            None => Step::Starting,
        };
    }
    match device_prompt(id, &st.log) {
        Some((url, code)) => Step::DeviceCode { url, code },
        None => Step::Starting,
    }
}

/// What to do for an installed provider that needs a sign-in.
fn sign_in_step(id: &str, host: HostKind, login: Option<&InstallStatus>) -> Step {
    match login {
        Some(st) => match st.state {
            InstallState::Failed => Step::SignIn {
                again: login_failed(LoginEnd::of(st)),
            },
            // It ended fine, but Melissa still finds no login.
            InstallState::Done => Step::SignIn {
                again: Some("The sign-in didn't finish."),
            },
            // Running is handled by the caller.
            _ => Step::SignIn { again: None },
        },
        // No sign-in unit: only a run from source gets a command (its own terminal).
        None if host == HostKind::Other => match id {
            "claude" => Step::Command {
                text: "Or sign in from a terminal:".into(),
                command: "claude auth login".into(),
            },
            "grok" => Step::Command {
                text: "Or sign in from a terminal:".into(),
                command: "grok login --device-auth".into(),
            },
            "codex-acp" => Step::Command {
                text: "Or sign in from a terminal:".into(),
                command: "codex login --device-auth".into(),
            },
            "muse" => Step::Command {
                text: "Or sign in from a terminal:".into(),
                command: "muse login".into(),
            },
            _ => Step::Nothing,
        },
        None => Step::Nothing,
    }
}

/// Everything about one provider, for the page and the API.
struct Info {
    card: ProviderCard,
    running: bool,
    installed: bool,
    ready: bool,
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
        let lead = ui.lead.agents().lead_name();
        return Some(format!(
            "{lead} runs on it now. Switch {lead} to another agent in the chat first."
        ));
    }
    let helpers: Vec<String> = ui
        .lead
        .agents()
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

async fn info(
    ui: &Ui,
    id: &str,
    facts: &[&BackendFacts],
    views: &[BackendView],
    pending: Option<Pending>,
) -> Info {
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
                "off".to_string()
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
                state,
            }
        })
        .collect();
    let secret_set = saved
        .env
        .keys()
        .any(|k| melissa_settings::providers::is_secret_name(k));
    let needs_login = reasons.contains(&"not_logged_in");
    let host = s.parts().updates.kind;
    let login_status = if installed && way == InstallWay::Button && Job::Login.unit(id).is_some() {
        Some(s.parts().installer.job_status(Job::Login, id).await)
    } else {
        None
    };
    let login_state = login_status.as_ref().map(|l| l.state);
    // A saved change for this provider waits for Melissa to apply it.
    let waiting =
        pending.filter(|_| saved.is_enabled() && (!running || (needs_login && secret_set)));
    let (status, tone, ready) = if !saved.is_enabled() {
        ("Disabled", Tone::Muted, false)
    } else if install_state == Some(InstallState::Running) {
        ("Installing…", Tone::Info, false)
    } else if let Some(p) = waiting {
        (p.word(), Tone::Info, false)
    } else if !installed && install_state == Some(InstallState::Failed) {
        ("Failed", Tone::Error, false)
    } else if !installed {
        ("Not installed", Tone::Warn, false)
    } else if reasons.contains(&"not_installed") {
        // Found here, but not on the lead agent's own PATH (e.g. a custom path).
        ("Not installed", Tone::Warn, false)
    } else if login_state == Some(InstallState::Running) {
        ("Signing in…", Tone::Info, false)
    } else if needs_login {
        ("Sign in needed", Tone::Warn, false)
    } else if reasons.contains(&"sandbox") {
        ("Failed", Tone::Error, false)
    } else {
        ("Ready", Tone::Ok, true)
    };
    let label = saved
        .display_name
        .clone()
        .unwrap_or_else(|| default_label(id).to_string());
    let step = if !saved.is_enabled() || waiting.is_some() {
        Step::Nothing
    } else if !installed {
        install_step(id, &label, way, install_status)
    } else if let Some(st) = login_status
        .as_ref()
        .filter(|l| l.state == InstallState::Running)
    {
        login_running(id, st, s.code_sent())
    } else if needs_login {
        sign_in_step(id, host, login_status.as_ref())
    } else if id == "opencode" && ready {
        Step::Note("Free models work without an account.".into())
    } else {
        Step::Nothing
    };
    // The API key option: where the CLI reads one and it is needed (or set already).
    let key =
        (key_var(id).is_some() && installed && saved.is_enabled() && (needs_login || secret_set))
            .then_some(secret_set);
    // Who uses it.
    let mine = |name: &str| facts.iter().any(|f| f.name == name);
    let mut used_by = Vec::new();
    if views.iter().any(|v| v.current && mine(&v.name)) {
        used_by.push(format!("{} (now)", ui.lead.agents().lead_name()));
    }
    if s.parts().agent_default.as_deref().is_some_and(mine) {
        used_by.push("New helpers".to_string());
    }
    for b in ui.lead.agents().list() {
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
    let lock = if saved.is_enabled() {
        lock_reason(ui, id, views)
    } else {
        None
    };
    let default_command = facts
        .iter()
        .find_map(|f| f.command.clone())
        .unwrap_or_default();
    // Melissa uninstalls only what she installed (the container's install units).
    let remove_confirm = if way == InstallWay::Button {
        format!("Uninstall {label} and delete its sign-in and API key?")
    } else {
        format!("Remove {label} from Melissa? Its program stays on this computer.")
    };
    Info {
        card: ProviderCard {
            id: id.to_string(),
            label,
            version,
            enabled: saved.is_enabled(),
            status: status.to_string(),
            tone,
            lock,
            step,
            key,
            key_open: key == Some(false) && way != InstallWay::Button,
            remove_confirm,
            details: ProviderDetails {
                used_by,
                backends,
                models,
                program: program.map(|p| p.display().to_string()),
                display_name: saved.display_name.clone().unwrap_or_default(),
                command: saved.command.clone().unwrap_or_default(),
                default_command,
                args: saved.args.clone(),
                env: saved.env_view(),
            },
        },
        running,
        installed,
        ready,
        install_state,
        login_state,
    }
}

async fn infos(ui: &Ui) -> Vec<Info> {
    let views = ui.lead.backends();
    let pending = apply::pending(ui).await;
    let mut out = Vec::new();
    for (id, facts) in grouped(&ui.settings) {
        out.push(info(ui, &id, &facts, &views, pending).await);
    }
    out
}

/// How to restart by hand, where Melissa can't (a run from source only).
pub(super) fn manual_restart(s: &Settings) -> Option<String> {
    (s.parts().updates.kind == HostKind::Other)
        .then(|| "# stop melissa-server (Ctrl-C) and start it again".to_string())
}

pub(super) async fn view(ui: &Ui, selected: Option<&str>, flash: Option<Flash>) -> ProvidersView {
    let s = &ui.settings;
    let all = infos(ui).await;
    let cards: Vec<ProviderCard> = all.into_iter().map(|i| i.card).collect();
    let selected = selected
        .and_then(|id| cards.iter().position(|c| c.id == id))
        .unwrap_or(0);
    let pending = apply::pending(ui).await;
    // Refresh while something moves; never while a code field waits for the owner.
    let moving = pending.is_some_and(|p| p != Pending::Manual)
        || cards.iter().any(|c| {
            matches!(
                c.step,
                Step::Installing | Step::Starting | Step::Checking | Step::DeviceCode { .. }
            )
        });
    let poll = moving.then(|| match cards.get(selected) {
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
        pending,
        manual_restart: manual_restart(s),
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
    pub(super) fn text(&self) -> String {
        match self {
            Self::Unknown => "No such provider.".into(),
            Self::Refused(m) | Self::Invalid(m) => m.clone(),
        }
    }

    pub(super) fn status(&self) -> StatusCode {
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
        if let Some(why) = lock_reason(ui, id, &ui.lead.backends()) {
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
    // Melissa applies it herself once she is idle.
    apply::schedule(ui);
    Ok(())
}

/// The providers region for htmx: Settings → Providers', or first-run setup's Provider step
/// when the request comes from there (both use these handlers).
pub(super) async fn region(
    ui: &Ui,
    headers: &HeaderMap,
    selected: Option<&str>,
    flash: Option<Flash>,
) -> maud::Markup {
    let v = view(ui, selected, flash).await;
    if super::setup::from_setup(headers) {
        super::setup::provider_region(ui, &v)
    } else {
        pages::providers_region(&v)
    }
}

/// The answer to a UI form: the region (htmx), else a redirect or the page with the error.
async fn reply(
    ui: &Ui,
    headers: &HeaderMap,
    id: &str,
    result: Result<String, ChangeError>,
) -> Response {
    let (flash, status) = match &result {
        // An empty text: the page itself shows what happened.
        Ok(text) if text.is_empty() => (None, StatusCode::OK),
        Ok(text) => (
            Some(Flash {
                text: text.clone(),
                tone: Tone::Ok,
            }),
            StatusCode::OK,
        ),
        Err(e) => (
            Some(Flash {
                text: e.text(),
                tone: Tone::Error,
            }),
            e.status(),
        ),
    };
    let setup = super::setup::from_setup(headers);
    if is_htmx(headers) {
        return region(ui, headers, Some(id), flash).await.into_response();
    }
    if result.is_ok() {
        // First-run setup's Provider step uses these forms too: back to it.
        let to = if setup {
            super::setup::provider_href(id)
        } else {
            format!("/settings/providers?p={id}")
        };
        return Redirect::to(&to).into_response();
    }
    let v = view(ui, Some(id), flash).await;
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
        return region(&ui, &headers, pick.as_deref(), None)
            .await
            .into_response();
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
    let previous = installer.status(id).await;
    audit_previous(ui, Job::Install, id, &previous);
    let before = previous.ended;
    installer
        .start(id)
        .await
        .map_err(|e| ChangeError::Refused(format!("Can't start the install: {e}")))?;
    watch(ui, Job::Install, id, before);
    // `--no-block` returns before the unit starts: wait briefly so the page shows it running.
    for _ in 0..20 {
        if installer.status(id).await.state != InstallState::Idle {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
    Ok(format!("Installing {}…", default_label(id)))
}

/// Did `job` for `id` leave what it should: the program for an install, a login the lead agent
/// accepts for a sign-in (its credential file, checked live)? systemd forgets a oneshot unit
/// that ended well (its state reads as never run), so success is judged by the result.
fn job_worked(ui: &Ui, job: Job, id: &str) -> bool {
    let Some((_, facts)) = grouped(&ui.settings).into_iter().find(|(p, _)| p == id) else {
        return false;
    };
    match job {
        Job::Install => facts
            .iter()
            .find_map(|f| f.command.as_deref())
            .and_then(version::resolve)
            .is_some(),
        Job::Login => {
            let views = ui.lead.backends();
            let mine: Vec<&BackendView> = views
                .iter()
                .filter(|v| facts.iter().any(|f| f.name == v.name))
                .collect();
            !mine.is_empty() && mine.iter().all(|v| v.reason != Some("not_logged_in"))
        }
    }
}

fn job_words(job: Job) -> (&'static str, &'static str) {
    match job {
        Job::Install => ("settings.provider.install", "install"),
        Job::Login => ("settings.provider.login", "sign-in"),
    }
}

fn audit_failure(ui: &Ui, job: Job, id: &str, st: &InstallStatus) {
    let why = match job {
        Job::Login => LoginEnd::of(st).as_str().to_string(),
        Job::Install => st.detail.clone().unwrap_or_else(|| "unknown reason".into()),
    };
    let (action, what) = job_words(job);
    ui.settings.audit(
        action,
        "lead",
        &format!("{id}: {what} failed ({why})"),
        None,
    );
}

/// Before a new run: audit the last one's failure if the watcher didn't get to it (a quick
/// retry starts before its next look).
fn audit_previous(ui: &Ui, job: Job, id: &str, st: &InstallStatus) {
    if st.state == InstallState::Failed
        && st.ended != 0
        && ui.settings.mark_audited(&format!("{job:?}:{id}"), st.ended)
    {
        audit_failure(ui, job, id, st);
    }
}

/// Audit how an install or sign-in ended, once it has (whether or not anyone watches the
/// page). After a success Melissa looks up the new agent's models.
fn watch(ui: &Ui, job: Job, id: &str, before: u64) {
    let key = format!("{job:?}:{id}");
    if !ui.settings.watching().insert(key.clone()) {
        return;
    }
    let ui = ui.clone();
    let id = id.to_string();
    tokio::spawn(async move {
        let s = &ui.settings;
        let mut seen = false;
        // The units give up after 15 minutes; look a little longer.
        for round in 0..(16 * 60 / 2) {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let st = s.parts().installer.job_status(job, &id).await;
            if st.state == InstallState::Running {
                seen = true;
                continue;
            }
            let ended_now = st.ended != 0 && st.ended != before;
            // Not started yet (systemd queues it): give it a moment.
            if !seen && !ended_now && round < 15 {
                continue;
            }
            let worked = job_worked(&ui, job, &id);
            let outcome = if st.state == InstallState::Failed && ended_now {
                if s.mark_audited(&key, st.ended) {
                    audit_failure(&ui, job, &id, &st);
                }
                None
            } else if worked {
                Some("succeeded")
            } else {
                Some(match job {
                    Job::Install => "failed (the program wasn't found)",
                    Job::Login => "failed (still not signed in)",
                })
            };
            if let Some(outcome) = outcome {
                let (action, what) = job_words(job);
                s.audit(action, "lead", &format!("{id}: {what} {outcome}"), None);
            }
            if worked {
                ui.lead.refresh_models_soon();
            }
            break;
        }
        s.watching().remove(&key);
    });
}

/// Check, start and audit a device-code sign-in (Grok). The same path for the UI and the API.
async fn start_login(ui: &Ui, id: &str, actor: &str) -> Result<String, ChangeError> {
    let result = login_checked(ui, id).await;
    let detail = match &result {
        Ok(_) => format!("{}: sign-in requested", clean_text(id, 40)),
        Err(e) => format!("{}: sign-in refused: {}", clean_text(id, 40), e.text()),
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
            "Melissa can't sign this provider in from here.".into(),
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
    let current = installer.job_status(Job::Login, id).await;
    if current.state == InstallState::Running {
        return Err(ChangeError::Refused("A sign-in is waiting already.".into()));
    }
    audit_previous(ui, Job::Login, id, &current);
    s.set_code_sent(false);
    installer
        .start_job(Job::Login, id)
        .await
        .map_err(|e| ChangeError::Refused(format!("Can't start the sign-in: {e}")))?;
    watch(ui, Job::Login, id, current.ended);
    // Wait for the vendor's URL (a few seconds), so the page shows it at once. Only from this
    // run: until the unit runs, the log is the last run's (and Claude's FIFO has no reader).
    for _ in 0..40 {
        let st = installer.job_status(Job::Login, id).await;
        let shown = st.state == InstallState::Running
            && match id {
                "claude" => claude_login_url(&st.log).is_some(),
                _ => device_prompt(id, &st.log).is_some(),
            };
        let ended = st.state == InstallState::Failed && st.ended != current.ended;
        if shown || ended {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    Ok(String::new())
}

/// Hand the pasted code to Claude's waiting sign-in (empty: cancel it). The same path for
/// the UI and the API. The code is never stored, logged or audited.
async fn send_code(ui: &Ui, id: &str, code: &str, actor: &str) -> Result<String, ChangeError> {
    let s = &ui.settings;
    let cancel = code.is_empty();
    let result = send_code_checked(ui, id, code).await;
    let what = if cancel {
        "sign-in cancel"
    } else {
        "sign-in code"
    };
    let detail = match &result {
        Ok(_) => format!("{}: {what} sent", clean_text(id, 40)),
        Err(e) => format!("{}: {what} refused: {}", clean_text(id, 40), e.text()),
    };
    s.audit("settings.provider.login", actor, &detail, None);
    result
}

async fn send_code_checked(ui: &Ui, id: &str, code: &str) -> Result<String, ChangeError> {
    let s = &ui.settings;
    let installer = &s.parts().installer;
    if !grouped(s).iter().any(|(p, _)| p == id) {
        return Err(ChangeError::Unknown);
    }
    if id != "claude" || installer.way != InstallWay::Button {
        return Err(ChangeError::Invalid("No code is needed here.".into()));
    }
    if !code.is_empty() && !valid_code(code) {
        return Err(ChangeError::Invalid(
            "That doesn't look like a sign-in code.".into(),
        ));
    }
    if installer.job_status(Job::Login, id).await.state != InstallState::Running {
        return Err(ChangeError::Refused(
            "No sign-in is waiting. Sign in again.".into(),
        ));
    }
    installer
        .send_code(code)
        .map_err(|e| ChangeError::Refused(e.to_string()))?;
    if code.is_empty() {
        // Give the unit a moment to end, so the page shows the Sign in button again.
        for _ in 0..15 {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            if installer.job_status(Job::Login, id).await.state != InstallState::Running {
                break;
            }
        }
        return Ok("Cancelled.".into());
    }
    s.set_code_sent(true);
    Ok(String::new())
}

#[derive(Deserialize)]
pub(super) struct CodeForm {
    #[serde(default)]
    code: String,
}

pub(super) async fn login_code(
    State(ui): State<Ui>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<CodeForm>,
) -> Response {
    let code = form.code.trim();
    let result = if code.is_empty() {
        Err(ChangeError::Invalid("Paste the code first.".into()))
    } else {
        send_code(&ui, &id, code, "ui").await
    };
    reply(&ui, &headers, &id, result).await
}

pub(super) async fn login_cancel(
    State(ui): State<Ui>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let result = send_code(&ui, &id, "", "ui").await;
    reply(&ui, &headers, &id, result).await
}

#[derive(Deserialize)]
pub(super) struct CodeBody {
    #[serde(default)]
    code: String,
}

pub(super) async fn api_login_code(
    State(ui): State<Ui>,
    Path(id): Path<String>,
    Json(body): Json<CodeBody>,
) -> Response {
    match send_code(&ui, &id, body.code.trim(), "api").await {
        Ok(_) => (StatusCode::ACCEPTED, api_install_json(&ui, &id).await).into_response(),
        Err(e) => (e.status(), Json(json!({"error": e.text()}))).into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct KeyForm {
    #[serde(default)]
    value: String,
}

/// "Use an API key instead": the key goes under the provider's fixed variable name (write-
/// only, like every secret); an empty value removes it.
pub(super) async fn set_key(
    State(ui): State<Ui>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<KeyForm>,
) -> Response {
    let result = match key_var(&id) {
        None => Err(ChangeError::Invalid(
            "This provider takes no API key here.".into(),
        )),
        Some(var) if form.value.trim().is_empty() => {
            apply(&ui, &id, Change::RemoveVar(var.to_string()), "ui").map(|()| "Removed.".into())
        }
        Some(var) => apply(
            &ui,
            &id,
            Change::SetVar {
                name: var.to_string(),
                value: form.value.trim().to_string(),
            },
            "ui",
        )
        .map(|()| "Saved.".into()),
    };
    reply(&ui, &headers, &id, result).await
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
pub(super) async fn poll(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Query(q): Query<PickQuery>,
) -> Response {
    region(&ui, &headers, q.p.as_deref(), None)
        .await
        .into_response()
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
            "install": install_json(&i),
            "sign_in": sign_in_json(&i),
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
    let d = &c.details;
    let backends: Vec<Value> = d
        .backends
        .iter()
        .map(|b| json!({"name": b.name, "state": b.state}))
        .collect();
    json!({
        "id": c.id,
        "label": c.label,
        "enabled": c.enabled,
        "running": i.running,
        "installed": i.installed,
        "ready": i.ready,
        "program": d.program,
        "version": c.version,
        "status": c.status,
        "used_by": d.used_by,
        "can_disable": c.lock.is_none(),
        "lock": c.lock,
        "backends": backends,
        "models": d.models,
        "install": install_json(i),
        "sign_in": sign_in_json(i),
        "api_key": c.key.map(|set| json!({"set": set})),
        "settings": {
            "display_name": (!d.display_name.is_empty()).then_some(&d.display_name),
            "command": (!d.command.is_empty()).then_some(&d.command),
            "args": d.args,
            "env": d.env,
        },
    })
}

/// The sign-in step for the API (null when there is nothing to do).
fn sign_in_json(i: &Info) -> Value {
    let (step, url, code, text) = match &i.card.step {
        Step::SignIn { again } => ("sign_in", None, None, again.map(str::to_string)),
        Step::Starting => ("starting", None, None, None),
        Step::PasteCode { url } => ("paste_code", Some(url.clone()), None, None),
        Step::Checking => ("checking", None, None, None),
        Step::DeviceCode { url, code } => {
            ("device_code", Some(url.clone()), Some(code.clone()), None)
        }
        Step::Command { text, command } if i.installed => {
            return json!({"step": "command", "text": text, "command": command});
        }
        _ => return Value::Null,
    };
    json!({
        "step": step,
        "state": i.login_state.map(InstallState::as_str),
        "url": url,
        "code": code,
        "text": text,
    })
}

/// The install step for the API (null once installed).
fn install_json(i: &Info) -> Value {
    if i.installed {
        return Value::Null;
    }
    match &i.card.step {
        Step::Install { failed, log, .. } => json!({
            "way": "button",
            "state": i.install_state.map_or("idle", InstallState::as_str),
            "text": failed,
            "log": log,
        }),
        Step::Installing => json!({"way": "button", "state": "running"}),
        Step::Note(text) => json!({
            "way": if text.contains("Nix") { "nix" } else { "manual" },
            "text": text,
        }),
        _ => Value::Null,
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
    let views = ui.lead.backends();
    let restart = ui.settings.parts().store.restart_needed();
    let pending = apply::pending(&ui).await;
    for (pid, facts) in grouped(&ui.settings) {
        if pid == id {
            let i = info(&ui, &pid, &facts, &views, pending).await;
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
