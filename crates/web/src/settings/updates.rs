//! Settings → Updates: Melissa's release, its container image and the fixed update actions.

use axum::{
    extract::{Form, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Json,
};
use melissa_settings::updates::{
    ago, now_secs, HostKind, ImageStatus, ImageView, UpdateAction, UpdateEnv, UpdateState,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::Settings;
use super::{
    apply,
    pages::{self, Flash, ImageLine, UpdatesView},
    providers::{agent_versions, manual_restart},
};
use crate::{
    components::{Tone, UpdateHint},
    is_htmx,
    ui::Ui,
};

/// What a from-source install does.
const SOURCE_UPDATE: &str = "git pull && just demo";
/// The `melissa-host` profile on a non-NixOS host with Nix (packaging/nix/host).
const NIX_PROFILE: &str = "/nix/var/nix/profiles/melissa";

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

fn install_words(env: &UpdateEnv) -> &'static str {
    match env.kind {
        HostKind::Container => "Container",
        HostKind::Nix if env.nixos() => "Nix (part of this computer's NixOS configuration)",
        HostKind::Nix => "Nix (the melissa-host profile on this computer)",
        HostKind::Other => "From source on this computer",
    }
}

/// One short sentence where updating from here isn't the way (None: the buttons are).
fn note(env: &UpdateEnv) -> Option<&'static str> {
    match env.kind {
        HostKind::Container if env.helper_installed() => None,
        HostKind::Container => Some("Updating from here isn't set up on this device."),
        HostKind::Nix => Some("Melissa updates with your Nix configuration."),
        HostKind::Other => Some("Running from source: update it in the source folder."),
    }
}

/// The rail's update button: a dot when the last check found a newer Melissa, or one is
/// downloaded and waits (container with the helper only; nothing is fetched for this).
/// Providers have no "newer version" check (only the installed version), so they add none.
pub(super) fn hint(s: &Settings) -> UpdateHint {
    let env = &s.parts().updates;
    if env.kind != HostKind::Container || !env.helper_installed() {
        return UpdateHint::NONE;
    }
    hint_for(env.state().status.as_ref())
}

fn hint_for(status: Option<&ImageStatus>) -> UpdateHint {
    match status {
        Some(st) if st.staged.is_some() || st.available.is_some() => UpdateHint {
            href: "/settings/updates",
            label: "Melissa update available",
            available: true,
        },
        _ => UpdateHint::NONE,
    }
}

/// The image state in a word or two (container with the helper only).
/// The update status in words ("Up to date"), where Melissa knows it.
pub(super) fn status_word(ui: &Ui) -> Option<&'static str> {
    let env = &ui.settings.parts().updates;
    status_words(env, &env.state()).map(|(w, _)| w)
}

fn status_words(env: &UpdateEnv, state: &UpdateState) -> Option<(&'static str, Tone)> {
    if env.kind != HostKind::Container || !env.helper_installed() {
        return None;
    }
    let s = state.status.as_ref()?;
    Some(if s.staged.is_some() {
        ("Ready to install", Tone::Info)
    } else if s.available.is_some() {
        ("Update available", Tone::Info)
    } else {
        ("Up to date", Tone::Ok)
    })
}

fn auto_words(env: &UpdateEnv, state: &UpdateState) -> &'static str {
    if env.kind == HostKind::Nix {
        return if env.nixos() {
            "Through the NixOS configuration (system.autoUpgrade), if set"
        } else {
            "Off (the Nix profile is updated by hand)"
        };
    }
    match state.status.as_ref().and_then(|s| s.auto_update.as_deref()) {
        Some("registry") => "On (daily)",
        Some("local") => "On for images loaded on the host (daily)",
        Some("off") => "Off",
        _ => "Not set up",
    }
}

/// Commands for the page: Nix and runs from source only.
fn manual_commands(env: &UpdateEnv) -> Vec<(String, String)> {
    match env.kind {
        // No host commands in the container's UI or API: the buttons are the way.
        HostKind::Container => Vec::new(),
        HostKind::Nix if env.nixos() => vec![
            (
                "In the folder of this computer's system flake, update its inputs:".into(),
                "nix flake update".into(),
            ),
            (
                "Rebuild (Melissa restarts if it changed):".into(),
                "sudo nixos-rebuild switch".into(),
            ),
            (
                "Roll back to the previous system:".into(),
                "sudo nixos-rebuild switch --rollback".into(),
            ),
        ],
        HostKind::Nix => vec![
            (
                "Update (Melissa restarts):".into(),
                format!(
                    "sudo nix profile upgrade --profile {NIX_PROFILE} --all && sudo {NIX_PROFILE}/bin/melissa-host install"
                ),
            ),
            (
                "Roll back:".into(),
                format!(
                    "sudo nix profile rollback --profile {NIX_PROFILE} && sudo {NIX_PROFILE}/bin/melissa-host install"
                ),
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
        _ => String::new(),
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
    let mut details = vec![("Installed as", install_words(env).to_string())];
    if let Some(r) = &release.revision {
        details.push(("Revision", r.clone()));
    }
    details.push(("Automatic updates", auto_words(env, &state).to_string()));
    UpdatesView {
        version: release.version.clone(),
        status: status_words(env, &state),
        note: note(env),
        images: images(env, &state),
        details,
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
        commands_open: env.kind == HostKind::Other,
        pending: apply::pending(ui).await,
        manual_restart: manual_restart(s),
        device: super::device::view(ui, None),
    }
}

pub(super) async fn page(State(ui): State<Ui>) -> Response {
    let v = view(&ui, None, None).await;
    ui.framed(|f| pages::updates_page(f, &v))
        .await
        .into_response()
}

/// The page with the device name's field as given (its form's answer without script).
pub(super) async fn page_with_device(ui: &Ui, device: pages::DeviceNameView) -> maud::Markup {
    let mut v = view(ui, None, None).await;
    v.device = device;
    ui.framed(|f| pages::updates_page(f, &v)).await
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

/// How long the lead agent waits after answering before it asks systemd to restart it.
const RESTART_DELAY: std::time::Duration = std::time::Duration::from_millis(750);

fn started_text(action: UpdateAction) -> &'static str {
    match action {
        UpdateAction::Check => "Checking… Look again in a minute.",
        UpdateAction::Apply => "Updating… Melissa restarts when it's ready.",
        UpdateAction::Rollback => "Going back… Melissa restarts in a moment.",
        UpdateAction::Restart => "Restarting…",
    }
}

async fn run(ui: &Ui, action: UpdateAction, confirmed: bool, actor: &str) -> Outcome {
    let s = &ui.settings;
    let env = &s.parts().updates;
    if !env.can(action) {
        let text = match env.kind {
            HostKind::Container => "Melissa can't do that from here.".into(),
            HostKind::Nix => "Melissa updates with your Nix configuration.".into(),
            HostKind::Other => "There is no container image to update here.".into(),
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
                settings.audit("settings.update", "lead", "restart failed", None);
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

#[cfg(test)]
mod hint_tests {
    use super::*;

    #[test]
    fn a_found_or_downloaded_update_lights_the_rail_button() {
        assert_eq!(hint_for(None), UpdateHint::NONE);
        let up_to_date = ImageStatus::default();
        assert_eq!(hint_for(Some(&up_to_date)), UpdateHint::NONE);
        let found = ImageStatus {
            available: Some(ImageView::default()),
            ..ImageStatus::default()
        };
        let hint = hint_for(Some(&found));
        assert!(hint.available);
        assert_eq!(hint.href, "/settings/updates");
        let staged = ImageStatus {
            staged: Some(ImageView::default()),
            ..ImageStatus::default()
        };
        assert!(hint_for(Some(&staged)).available);
    }
}
