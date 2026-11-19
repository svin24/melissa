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
    providers::{default_label, BackendFacts, Change, ProviderOverride},
    updates::{ago, now_secs, HostKind, UpdateAction},
    version,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{
    pages::{self, BackendLine, Flash, ProviderCard, ProvidersView, RestartHint},
    Settings,
};
use crate::{components::Tone, is_htmx, ui::Ui};

/// Agents `melissa-setup agent <name>` can install.
const SETUP_AGENTS: [&str; 3] = ["claude", "grok", "opencode"];

/// The providers in display order (known ones first, echo last), each with its backends.
fn grouped(s: &Settings) -> Vec<(String, Vec<&BackendFacts>)> {
    let mut map: BTreeMap<String, Vec<&BackendFacts>> = BTreeMap::new();
    for b in &s.parts().backends {
        map.entry(b.provider.clone()).or_default().push(b);
    }
    let rank = |id: &str| match id {
        "claude" => 0,
        "grok" => 1,
        "opencode" => 2,
        "echo" => 9,
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

/// Everything about one provider, for the page and the API.
struct Info {
    card: ProviderCard,
    running: bool,
    installed: bool,
    ready: bool,
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
    if id == "echo"
        && !s
            .parts()
            .backends
            .iter()
            .any(|b| b.provider == "echo" && b.session)
    {
        return Some("Built into Melissa; always on.".into());
    }
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
    let builtin = id == "echo";
    let running = facts.iter().any(|f| f.enabled);
    let command = facts.iter().find_map(|f| f.command.clone());
    let program = command.as_deref().and_then(version::resolve);
    let version = match &program {
        Some(p) => s.versions().version(p).await,
        None => None,
    };
    let installed = builtin || program.is_some();
    let view = |name: &str| views.iter().find(|v| v.name == name);
    let mut reasons: Vec<&str> = Vec::new();
    let backends: Vec<BackendLine> = facts
        .iter()
        .map(|f| {
            let state = if !f.enabled {
                "left out (disabled)".to_string()
            } else if builtin {
                "ready".into()
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
    let (mut status, tone, ready) = if builtin {
        ("Built in · always ready".to_string(), Tone::Ok, true)
    } else if !saved.is_enabled() {
        let s = if running {
            "Disabled · after a restart"
        } else {
            "Disabled"
        };
        (s.to_string(), Tone::Muted, false)
    } else if !running {
        ("Enabled · after a restart".into(), Tone::Info, false)
    } else if !installed {
        ("Not installed".into(), Tone::Warn, false)
    } else if reasons.contains(&"not_logged_in") {
        ("Not logged in".into(), Tone::Warn, false)
    } else if reasons.contains(&"sandbox") {
        ("Sandbox can't start".into(), Tone::Error, false)
    } else {
        ("Ready".into(), Tone::Ok, true)
    };
    if !extras.is_empty() && saved.is_enabled() && !builtin {
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
    let host = s.parts().updates.kind;
    let on_image = matches!(host, HostKind::Bootc | HostKind::Container);
    let setup = if !installed && on_image && SETUP_AGENTS.contains(&id) {
        Some(format!("sudo melissa-setup agent {id}"))
    } else if installed
        && reasons.contains(&"not_logged_in")
        && on_image
        && SETUP_AGENTS.contains(&id)
    {
        Some(format!("sudo melissa-setup login {id}"))
    } else {
        None
    };
    let label = saved
        .display_name
        .clone()
        .unwrap_or_else(|| default_label(id).to_string());
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
            builtin,
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
            setup,
        },
        running,
        installed,
        ready,
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

pub(super) fn restart_hint(s: &Settings) -> RestartHint {
    let env = &s.parts().updates;
    RestartHint {
        button: env.can(UpdateAction::Restart),
        command: match env.kind {
            HostKind::Other => "# stop melissa-queen (Ctrl-C) and start it again".into(),
            _ => UpdateAction::Restart.manual_command().into(),
        },
    }
}

async fn view(ui: &Ui, selected: Option<&str>, flash: Option<Flash>) -> ProvidersView {
    let s = &ui.settings;
    let cards: Vec<ProviderCard> = infos(ui).await.into_iter().map(|i| i.card).collect();
    let selected = selected
        .and_then(|id| cards.iter().position(|c| c.id == id))
        .unwrap_or(0);
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
        restart: s.parts().store.restart_needed().then(|| restart_hint(s)),
        flash,
        home: s.parts().home.as_ref().map(|h| h.display().to_string()),
        file: s.parts().store.path().map(|p| p.display().to_string()),
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
    if !s.parts().backends.iter().any(|b| b.provider == id) {
        return Err(ChangeError::Unknown);
    }
    if id == "echo" && !matches!(change, Change::Enable(_)) {
        return Err(ChangeError::Invalid(
            "Echo is built into Melissa; it has no program or variables.".into(),
        ));
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
        "builtin": c.builtin,
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
        "setup": c.setup,
        "settings": {
            "display_name": (!c.display_name.is_empty()).then_some(&c.display_name),
            "command": (!c.command.is_empty()).then_some(&c.command),
            "args": c.args,
            "env": c.env,
        },
    })
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
