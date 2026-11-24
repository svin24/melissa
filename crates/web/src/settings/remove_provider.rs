//! Settings → Providers → Remove provider (2026-11-20, owner-requested twice).
//!
//! Removing a provider: refused, naming them, while Melissa or a helper is set to use it,
//! while its install or sign-in runs, or when Melissa would have no other provider left.
//! Otherwise:
//! - where Melissa installed it (the container image: the agents' home `/var/lib/melissa/home`
//!   and its install units), its program and its sign-in files are deleted from that home: a
//!   fixed list of paths per built-in provider, or the folder Melissa unpacked a registry
//!   agent into. Elsewhere (Nix, a run from source) the program and the person's own files
//!   are left alone;
//! - its settings go (API keys included): a built-in provider is marked `removed`, an added
//!   one is deleted from `providers.toml`;
//! - Melissa applies the change when idle (`apply::schedule`): its backends are left out, so it
//!   is gone from the model picker. Adding it again goes through Add provider.
//!
//! Audited `settings.provider.remove` (refusals too).

use std::path::{Path as FsPath, PathBuf};

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Json,
};
use melissa_settings::{
    install::{InstallState, InstallWay, Job},
    providers::{default_label, Change},
    registry::AGENTS_DIR,
};
use serde_json::json;

use super::{
    apply,
    pages::{self, Flash},
    providers::ChangeError,
};
use crate::{components::Tone, is_htmx, ui::Ui};

/// Files a built-in provider's installer and sign-in leave in the agents' home (see
/// `packaging/common/usr/bin/melissa-setup` and the sandbox presets).
fn install_paths(id: &str) -> &'static [&'static str] {
    match id {
        "claude" => &[
            ".local/bin/claude",
            ".local/share/claude",
            ".local/state/claude",
            ".cache/claude",
            ".claude",
            ".claude.json",
            ".claude.json.backup",
        ],
        "grok" => &[".grok"],
        "opencode" => &[
            ".opencode",
            ".local/share/opencode",
            ".local/state/opencode",
            ".cache/opencode",
            ".config/opencode",
        ],
        // npm installs (melissa-setup): the package folder and the link on PATH.
        "gemini" => &[
            ".local/share/melissa-npm/gemini",
            ".local/bin/gemini",
            ".gemini",
        ],
        "codex-acp" => &[
            ".local/share/melissa-npm/codex-acp",
            ".local/bin/codex-acp",
            ".codex",
        ],
        "muse" => &[
            ".local/bin/muse",
            ".config/muse",
            ".local/share/muse",
            ".local/state/muse",
            ".cache/muse",
        ],
        _ => &[],
    }
}

/// Delete `rel` under `home` without following a symlink out of it. True if something went.
fn delete_under(home: &FsPath, path: &FsPath) -> std::io::Result<bool> {
    if !path.starts_with(home) || path == home {
        return Ok(false);
    }
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
        // `remove_dir_all` does not follow symlinks inside the tree.
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path).map(|()| true),
        Ok(_) => std::fs::remove_file(path).map(|()| true),
    }
}

/// The user-facing name of a provider.
fn label_of(ui: &Ui, id: &str) -> String {
    let store = &ui.settings.parts().store;
    if let Some(a) = store.added().get(id) {
        return a.label.clone();
    }
    store
        .get(id)
        .display_name
        .unwrap_or_else(|| default_label(id).to_string())
}

/// Who is set to use `id`: Melissa (her current backend) and helpers, by name.
fn users(ui: &Ui, id: &str) -> Vec<String> {
    let s = &ui.settings;
    let of = |name: &str| {
        s.parts()
            .backends
            .iter()
            .find(|b| b.name == name)
            .map(|b| b.provider.clone())
    };
    let mut names: Vec<String> = ui
        .lead
        .agents()
        .list()
        .into_iter()
        .filter(|a| of(&a.backend).as_deref() == Some(id))
        .map(|a| a.name)
        .collect();
    if ui
        .lead
        .backends()
        .iter()
        .any(|v| v.current && of(&v.name).as_deref() == Some(id))
    {
        names.push(ui.lead.agents().lead_name());
    }
    names
}

fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Remove `id`. Ok: what to tell the owner.
pub(super) async fn remove(ui: &Ui, id: &str, actor: &str) -> Result<(String, usize), ChangeError> {
    let s = &ui.settings;
    let store = &s.parts().store;
    let added = store.added().get(id).cloned();
    let configured = s.parts().backends.iter().any(|b| b.provider == id);
    if id == "echo" || (!configured && added.is_none()) || store.is_removed(id) {
        return Err(ChangeError::Unknown);
    }
    let label = label_of(ui, id);
    let refuse = |why: String| {
        s.audit(
            "settings.provider.remove",
            actor,
            &format!("{id}: refused ({why})"),
            None,
        );
        Err(ChangeError::Refused(why))
    };
    let names = users(ui, id);
    if !names.is_empty() {
        let verb = if names.len() == 1 { "uses" } else { "use" };
        let who = join_names(&names);
        let whom = if names.len() == 1 {
            who.as_str()
        } else {
            "them"
        };
        return refuse(format!(
            "{who} {verb} {label}. Switch {whom} to another provider first."
        ));
    }
    for job in [Job::Install, Job::Login] {
        if job.unit(id).is_some()
            && s.parts().installer.job_status(job, id).await.state == InstallState::Running
        {
            return refuse("Wait until its install or sign-in has finished.".into());
        }
    }
    // Melissa needs a provider to run on: another session backend that stays.
    let others = s.parts().backends.iter().any(|b| {
        b.session
            && b.provider != id
            && b.provider != "echo"
            && b.enabled
            && store.get(&b.provider).is_enabled()
            && !store.is_removed(&b.provider)
    }) || store.added().keys().any(|k| k != id);
    if !others {
        return refuse("Melissa needs another provider to run on. Add one first.".into());
    }
    // Uninstall where Melissa installed it, inside the agents' home only.
    let mut deleted = 0;
    let mut failed = Vec::new();
    let home = s.parts().home.clone();
    let melissa_installs = s.parts().installer.way == InstallWay::Button;
    if let Some(home) = &home {
        let mut paths: Vec<PathBuf> = Vec::new();
        if melissa_installs && added.is_none() {
            paths.extend(install_paths(id).iter().map(|p| home.join(p)));
        }
        if let Some(dir) = added.as_ref().and_then(|a| a.installed_in.as_deref()) {
            let dir = PathBuf::from(dir);
            // Only folders Melissa made for registry agents.
            if dir.starts_with(home.join(AGENTS_DIR)) {
                paths.push(dir);
            }
        }
        for p in paths {
            match delete_under(home, &p) {
                Ok(true) => deleted += 1,
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(path = %p.display(), "remove provider: {e}");
                    failed.push(p.display().to_string());
                }
            }
        }
    }
    let saved = match added {
        Some(_) => store.forget(id).map(|_| ()),
        None => store.change(id, &Change::Remove),
    };
    if let Err(e) = saved {
        return Err(ChangeError::Invalid(e.to_string()));
    }
    s.audit(
        "settings.provider.remove",
        actor,
        &format!(
            "{id}: removed ({deleted} paths deleted{})",
            if failed.is_empty() {
                String::new()
            } else {
                format!(", {} could not be deleted", failed.len())
            }
        ),
        None,
    );
    apply::schedule(ui);
    let text = if failed.is_empty() {
        format!("Removed {label}.")
    } else {
        format!("Removed {label}. Some of its files couldn't be deleted.")
    };
    Ok((text, deleted))
}

/// `POST /ui/settings/providers/{id}/remove` (after the in-page confirmation).
pub(super) async fn ui_remove(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = remove(&ui, &id, "user").await;
    let (flash, status, selected) = match &result {
        Ok((text, _)) => (
            Flash {
                text: text.clone(),
                tone: Tone::Ok,
            },
            StatusCode::OK,
            None,
        ),
        Err(e) => (
            Flash {
                text: e.text(),
                tone: Tone::Caution,
            },
            e.status(),
            Some(id.as_str()),
        ),
    };
    if !is_htmx(&headers) && result.is_ok() {
        return Redirect::to("/settings/providers").into_response();
    }
    let v = super::providers::view(&ui, selected, Some(flash)).await;
    if is_htmx(&headers) {
        return (status, pages::providers_region(&v)).into_response();
    }
    (status, ui.framed(|f| pages::providers_page(f, &v)).await).into_response()
}

/// `DELETE /api/settings/providers/{id}`.
pub(super) async fn api_remove(State(ui): State<Ui>, Path(id): Path<String>) -> Response {
    match remove(&ui, &id, "api").await {
        Ok((_, deleted)) => Json(json!({
            "removed": id,
            "deleted_paths": deleted,
            "restart_needed": ui.settings.parts().store.restart_needed(),
        }))
        .into_response(),
        Err(e) => (e.status(), Json(json!({"error": e.text()}))).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_installable_provider_lists_what_its_install_leaves() {
        for id in melissa_settings::install::INSTALLABLE {
            let paths = install_paths(id);
            assert!(!paths.is_empty(), "{id}");
            for p in paths {
                let p = FsPath::new(p);
                assert!(
                    p.is_relative()
                        && p.components()
                            .all(|c| matches!(c, std::path::Component::Normal(_))),
                    "{id}: {}",
                    p.display()
                );
            }
        }
        // npm installs: the package folder and the link on PATH go too.
        assert!(install_paths("gemini").contains(&".local/share/melissa-npm/gemini"));
        assert!(install_paths("codex-acp").contains(&".local/bin/codex-acp"));
        assert!(install_paths("muse").contains(&".config/muse"));
    }

    #[test]
    fn deletes_only_inside_home_and_not_through_symlinks() {
        let dir = std::env::temp_dir().join(format!("melissa-remove-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("home");
        let outside = dir.join("outside");
        std::fs::create_dir_all(home.join(".grok/bin")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep"), "x").unwrap();
        std::os::unix::fs::symlink(&outside, home.join(".grok/link")).unwrap();
        assert!(delete_under(&home, &home.join(".grok")).unwrap());
        assert!(!home.join(".grok").exists());
        assert!(outside.join("keep").exists());
        // Outside home, or home itself: never.
        assert!(!delete_under(&home, &outside).unwrap());
        assert!(!delete_under(&home, &home).unwrap());
        assert!(outside.exists() && home.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
