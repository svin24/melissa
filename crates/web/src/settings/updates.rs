//! Settings → Updates: Melissa's release, its container image and the fixed update actions.

use axum::{
    extract::{Form, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Json,
};
use melissa_settings::updates::{
    ago, now_secs, HostKind, ImageView, UpdateAction, UpdateEnv, UpdateState,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{
    pages::{self, Flash, ImageLine, UpdatesView},
    providers::{self, agent_versions, restart_hint},
};
use crate::{components::Tone, is_htmx, ui::Ui};

/// What a from-source install does.
const SOURCE_UPDATE: &str = "git pull && just demo";
/// Where a container without the host helper gets a newer image (from the source folder).
const CONTAINER_REBUILD: &str = "just container-build && just container-install";

fn image_detail(i: &ImageView) -> String {
    let mut parts = Vec::new();
    if let Some(v) = &i.version {
        parts.push(format!("version {v}"));
    }
    if let Some(r) = &i.revision {
        parts.push(format!("revision {r}"));
    }
    if let Some(t) = &i.timestamp {
        parts.push(format!("built {}", t.get(..10).unwrap_or(t)));
    }
    if let Some(d) = i.short_digest() {
        parts.push(d);
    }
    if parts.is_empty() {
        parts.push("details after the download".into());
    }
    parts.join(" · ")
}

fn images(env: &UpdateEnv, state: &UpdateState) -> Vec<ImageLine> {
    let mut out = Vec::new();
    let Some(s) = &state.status else {
        // No helper: podman still tells the container which image it runs.
        if let Some(i) = env.container_image() {
            out.push(ImageLine {
                label: "Running",
                image: i.image.clone(),
                detail: image_detail(&i),
            });
        }
        return out;
    };
    let mut push = |label: &'static str, i: &Option<ImageView>| {
        if let Some(i) = i {
            out.push(ImageLine {
                label,
                image: i.image.clone(),
                detail: image_detail(i),
            });
        }
    };
    push("Running", &s.running);
    push("Downloaded, used at the next start", &s.staged);
    push("Update available", &s.available);
    push("Previous (for a rollback)", &s.previous);
    out
}

fn install_words(kind: HostKind) -> &'static str {
    match kind {
        HostKind::Container => "Melissa container (podman, started by systemd)",
        HostKind::Other => "From source on this computer",
    }
}

fn how(env: &UpdateEnv) -> &'static str {
    match env.kind {
        HostKind::Container if env.helper_installed() => {
            "Updates come as a new container image. Check for one, then apply it: Melissa restarts on it, and the image it ran before stays for a rollback. Apps, chats and settings are kept."
        }
        HostKind::Container => {
            "Updates come as a new container image. The host's update helper isn't connected to this container, so run the commands below on the host. Apps, chats and settings are kept."
        }
        HostKind::Other => {
            "This computer isn't running the Melissa container, so there is no image to update. Update Melissa from source instead."
        }
    }
}

fn auto_words(state: &UpdateState) -> String {
    match state.status.as_ref().and_then(|s| s.auto_update.as_deref()) {
        Some("registry") => "Automatic updates: on (podman-auto-update.timer pulls a newer image daily and restarts Melissa on it).".into(),
        Some("local") => "Automatic updates: on for images loaded on the host (podman-auto-update.timer, daily).".into(),
        Some("off") => "Automatic updates: off (install.sh with AUTO_UPDATE=off).".into(),
        _ => "Automatic updates: not set up here.".into(),
    }
}

fn manual_commands(env: &UpdateEnv) -> Vec<(String, String)> {
    let cmd = |label: &str, a: UpdateAction| (label.to_string(), a.manual_command());
    match env.kind {
        HostKind::Container if env.helper_installed() => Vec::new(),
        HostKind::Container => vec![
            cmd("On the host, check for an update:", UpdateAction::Check),
            cmd("Apply it (Melissa restarts):", UpdateAction::Apply),
            cmd("Roll back (Melissa restarts):", UpdateAction::Rollback),
            (
                "Without the helper (a development container), in the Melissa source folder:"
                    .into(),
                CONTAINER_REBUILD.into(),
            ),
        ],
        HostKind::Other => vec![("In the Melissa source folder:".into(), SOURCE_UPDATE.into())],
    }
}

fn last_flash(state: &UpdateState) -> Option<Flash> {
    let l = state.last.as_ref()?;
    let when = ago(l.at, now_secs());
    let what = match l.action.as_str() {
        "check" => "Last check",
        "apply" => "Last update",
        "rollback" => "Last rollback",
        _ => "Last action",
    };
    let msg = l.message.as_deref().filter(|m| !m.is_empty());
    Some(if l.ok {
        Flash {
            text: match msg {
                Some(m) => format!("{what}: done {when} ({m})."),
                None => format!("{what}: done {when}."),
            },
            tone: Tone::Muted,
        }
    } else {
        Flash {
            text: format!(
                "{what} failed {when}: {}",
                msg.unwrap_or("see the host's journal (journalctl -u melissa-update)")
            ),
            tone: Tone::Error,
        }
    })
}

async fn view(ui: &Ui, flash: Option<Flash>, confirm: Option<&'static str>) -> UpdatesView {
    let s = &ui.settings;
    let env = &s.parts().updates;
    let state = env.state();
    let release = &s.parts().release;
    let checked = match (env.helper_installed(), state.status_at) {
        (true, Some(at)) => format!("Checked {}", ago(at, now_secs())),
        (true, None) => "Not checked yet".into(),
        _ => "Nothing to check here".into(),
    };
    let status = state.status.as_ref();
    let mut flash = flash;
    if flash.is_none() {
        if let Some(e) = &state.error {
            flash = Some(Flash {
                text: format!("Couldn't read the update status: {e}"),
                tone: Tone::Error,
            });
        }
    }
    UpdatesView {
        version: release.version.clone(),
        revision: release.revision.clone(),
        install: install_words(env.kind).into(),
        how: how(env).into(),
        images: images(env, &state),
        checked,
        last: last_flash(&state),
        flash,
        can_check: env.can(UpdateAction::Check),
        can_apply: env.can(UpdateAction::Apply)
            && status.is_some_and(|s| s.available.is_some() || s.staged.is_some()),
        can_rollback: env.can(UpdateAction::Rollback)
            && status.is_some_and(|s| s.previous.is_some()),
        confirm,
        commands: manual_commands(env),
        automatic: auto_words(&state),
        agents: agent_versions(ui).await,
        restart: s
            .parts()
            .store
            .restart_needed()
            .then(|| restart_hint(s, providers::SAVED)),
    }
}

pub(super) async fn page(State(ui): State<Ui>) -> Response {
    let v = view(&ui, None, None).await;
    ui.framed(|f| pages::updates_page(f, &v))
        .await
        .into_response()
}

#[derive(Deserialize, Default)]
pub(super) struct ConfirmForm {
    #[serde(default)]
    confirm: String,
}

/// What an action did, or why not.
enum Outcome {
    Started(Flash),
    /// Needs the owner's yes first (apply, rollback).
    Confirm(&'static str),
    NotHere(Flash),
    Failed(Flash),
}

/// How long the queen waits after answering before it asks systemd to restart it.
const RESTART_DELAY: std::time::Duration = std::time::Duration::from_millis(750);

fn started_text(action: UpdateAction) -> &'static str {
    match action {
        UpdateAction::Check => "Checking for updates. Reload this page in a minute for the result.",
        UpdateAction::Apply => {
            "Installing the update. Melissa restarts on the new image when it is downloaded; reload this page in a few minutes."
        }
        UpdateAction::Rollback => {
            "Going back to the previous image. Melissa restarts in a moment; reload this page in a minute."
        }
        UpdateAction::Restart => "Melissa is restarting. Reload this page in a few seconds.",
    }
}

async fn run(ui: &Ui, action: UpdateAction, confirmed: bool, actor: &str) -> Outcome {
    let s = &ui.settings;
    let env = &s.parts().updates;
    if !env.can(action) {
        let text = match env.kind {
            HostKind::Container => format!(
                "Melissa can't do that from here. On the host, run: {}",
                action.manual_command()
            ),
            HostKind::Other => "There is no container image to update on this device.".into(),
        };
        return Outcome::NotHere(Flash {
            text,
            tone: Tone::Warn,
        });
    }
    if action.restarts_melissa() && !confirmed {
        return Outcome::Confirm(action.as_str());
    }
    s.audit(
        "settings.update",
        actor,
        &format!("{} requested", action.as_str()),
        None,
    );
    if action == UpdateAction::Restart {
        // Restarting stops this process at once (no graceful shutdown): asked inline, the
        // reply was lost and the browser/CLI saw a dropped connection. Answer first.
        let settings = s.clone();
        tokio::spawn(async move {
            tokio::time::sleep(RESTART_DELAY).await;
            if let Err(e) = settings.parts().updates.request(action).await {
                tracing::warn!("restart request failed: {e}");
                settings.audit("settings.update", "queen", "restart failed", None);
            }
        });
        return Outcome::Started(Flash {
            text: started_text(action).into(),
            tone: Tone::Info,
        });
    }
    match env.request(action).await {
        Ok(()) => Outcome::Started(Flash {
            text: started_text(action).into(),
            tone: Tone::Info,
        }),
        Err(e) => Outcome::Failed(Flash {
            text: format!("Couldn't start it: {e}"),
            tone: Tone::Error,
        }),
    }
}

async fn ui_reply(ui: &Ui, headers: &HeaderMap, outcome: Outcome) -> Response {
    let (flash, confirm) = match outcome {
        Outcome::Started(f) | Outcome::NotHere(f) | Outcome::Failed(f) => (Some(f), None),
        Outcome::Confirm(a) => (None, Some(a)),
    };
    let v = view(ui, flash, confirm).await;
    if is_htmx(headers) {
        return pages::updates_region(&v).into_response();
    }
    ui.framed(|f| pages::updates_page(f, &v))
        .await
        .into_response()
}

pub(super) async fn action(
    State(ui): State<Ui>,
    Path(action): Path<String>,
    headers: HeaderMap,
    Form(form): Form<ConfirmForm>,
) -> Response {
    let Some(action) = UpdateAction::parse(&action).filter(|a| *a != UpdateAction::Restart) else {
        return Redirect::to("/settings/updates").into_response();
    };
    let outcome = run(&ui, action, form.confirm == "yes", "ui").await;
    ui_reply(&ui, &headers, outcome).await
}

pub(super) async fn restart(State(ui): State<Ui>, headers: HeaderMap) -> Response {
    let outcome = run(&ui, UpdateAction::Restart, true, "ui").await;
    ui_reply(&ui, &headers, outcome).await
}

// --- API ------------------------------------------------------------------------------------

pub(super) async fn api_status(State(ui): State<Ui>) -> Json<Value> {
    let s = &ui.settings;
    let env = &s.parts().updates;
    let actions: Vec<&str> = [
        UpdateAction::Check,
        UpdateAction::Apply,
        UpdateAction::Rollback,
        UpdateAction::Restart,
    ]
    .into_iter()
    .filter(|a| env.can(*a))
    .map(UpdateAction::as_str)
    .collect();
    let agents: Vec<Value> = agent_versions(&ui)
        .await
        .into_iter()
        .map(|(name, version)| json!({"name": name, "version": version}))
        .collect();
    Json(json!({
        "release": s.parts().release,
        "host": env.kind,
        "helper": env.helper_installed(),
        "state": env.state(),
        "actions": actions,
        "commands": manual_commands(env)
            .into_iter()
            .map(|(label, command)| json!({"label": label, "command": command}))
            .collect::<Vec<_>>(),
        "agents": agents,
        "restart_needed": s.parts().store.restart_needed(),
    }))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct ActionBody {
    #[serde(default)]
    confirm: bool,
}

pub(super) async fn api_action(
    State(ui): State<Ui>,
    Path(action): Path<String>,
    body: Option<Json<ActionBody>>,
) -> Response {
    let Some(action) = UpdateAction::parse(&action) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "unknown action (check, apply, rollback, restart)"})),
        )
            .into_response();
    };
    let confirmed = body.map(|Json(b)| b.confirm).unwrap_or(false);
    match run(&ui, action, confirmed, "api").await {
        Outcome::Started(f) => (
            StatusCode::ACCEPTED,
            Json(json!({"started": action.as_str(), "message": f.text})),
        )
            .into_response(),
        Outcome::Confirm(_) => (
            StatusCode::CONFLICT,
            Json(json!({"error": "this restarts Melissa; send {\"confirm\": true}"})),
        )
            .into_response(),
        Outcome::NotHere(f) => {
            (StatusCode::NOT_IMPLEMENTED, Json(json!({"error": f.text}))).into_response()
        }
        Outcome::Failed(f) => {
            (StatusCode::BAD_GATEWAY, Json(json!({"error": f.text}))).into_response()
        }
    }
}
