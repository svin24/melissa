//! The agent menu of a chat: which backend (and model) the Queen or a helper runs on, and
//! the choices the user has. Builds the fixed [`AgentMenu`] component from the API's
//! [`BackendView`]s and holds the UI words for a backend that cannot be used yet (what fixes it
//! depends on the host: Settings → Providers and `melissa-setup` in the container, the Nix
//! configuration on Nix, `melissa-setup` on the host otherwise).

use melissa_agent::queen::backends::BackendView;
use melissa_settings::updates::HostKind;

use crate::{
    components::{AgentGroup, AgentHint, AgentMenu, AgentOption},
    feed::backend_label,
};

/// The agent name `melissa-setup agent|login <name>` takes for a backend.
fn setup_name(backend: &str) -> Option<&'static str> {
    match backend {
        "claude-session" | "claude" => Some("claude"),
        "grok-acp" | "grok" => Some("grok"),
        "opencode-acp" | "opencode" => Some("opencode"),
        _ => None,
    }
}

/// Form value for a backend and model (`<backend>` or `<backend> <model>`; neither has
/// spaces: model ids are checked, backend names come from the config).
fn value(backend: &str, model: Option<&str>) -> String {
    match model {
        Some(m) => format!("{backend} {m}"),
        None => backend.to_string(),
    }
}

/// The backend and model of a submitted `choice`.
pub(crate) fn parse_choice(choice: &str) -> (&str, Option<&str>) {
    match choice.trim().split_once(' ') {
        Some((b, m)) => (b, Some(m.trim()).filter(|m| !m.is_empty())),
        None => (choice.trim(), None),
    }
}

/// A model's display name ("Sonnet 5.5"); None for the backend's default.
fn model_label(view: &BackendView, model: &str) -> Option<String> {
    if model == "default" {
        return None;
    }
    let label = view
        .models
        .iter()
        .flatten()
        .find(|m| m.id == model)
        .map_or(model, |m| m.label.as_str());
    Some(label.to_string())
}

/// What to tell the user about an agent that isn't installed (`login`: it is, but isn't logged
/// in), and the command that fixes it, for this kind of host.
fn setup_hint(label: &str, backend: &str, login: bool, host: HostKind) -> (String, Option<String>) {
    let name = setup_name(backend);
    let verb = if login { "login" } else { "agent" };
    match (host, login) {
        (HostKind::Container, false) => (
            format!("{label} isn't set up on this device. Install it in Settings → Providers."),
            name.map(|a| format!("sudo podman exec -it melissa melissa-setup agent {a}")),
        ),
        (HostKind::Container, true) => (
            format!("{label} isn't logged in. Settings → Providers shows how to sign in."),
            name.map(|a| format!("sudo podman exec -it melissa melissa-setup login {a}")),
        ),
        (HostKind::Nix, false) => (
            format!("This requires you add {label} via your Nix configuration."),
            None,
        ),
        (HostKind::Nix, true) => (
            format!("{label} isn't logged in. Settings → Providers shows how to sign in."),
            None,
        ),
        (HostKind::Other, false) => (
            format!("{label} isn't set up on this device."),
            name.map(|a| format!("sudo melissa-setup {verb} {a}")),
        ),
        (HostKind::Other, true) => (
            format!("{label} isn't logged in."),
            name.map(|a| format!("sudo melissa-setup {verb} {a}")),
        ),
    }
}

/// The menu for `views`, posting to `action`; setup hints fit `host`.
pub(crate) fn menu(
    views: &[BackendView],
    action: String,
    error: Option<String>,
    host: HostKind,
) -> AgentMenu {
    let current = views
        .iter()
        .find(|v| v.current)
        .map(|v| {
            let name = backend_label(&v.name).to_string();
            match v.model.as_deref().and_then(|m| model_label(v, m)) {
                Some(m) => format!("{name} · {m}"),
                None => name,
            }
        })
        .unwrap_or_else(|| "no agent yet".into());
    let mut groups = Vec::new();
    let mut unavailable = Vec::new();
    for v in views {
        let label = backend_label(&v.name).to_string();
        if !v.available {
            let (text, command) = match v.reason {
                Some("not_logged_in") => setup_hint(&label, &v.name, true, host),
                Some("sandbox") => (
                    format!("{label} can't start its sandbox here; the log says why."),
                    None,
                ),
                _ => setup_hint(&label, &v.name, false, host),
            };
            unavailable.push(AgentHint {
                label,
                text,
                command,
            });
            continue;
        }
        let mut options: Vec<AgentOption> = v
            .models
            .iter()
            .flatten()
            .map(|m| AgentOption {
                value: value(&v.name, Some(&m.id)),
                label: m.label.clone(),
                selected: v.current && v.model.as_deref() == Some(m.id.as_str()),
            })
            .collect();
        if options.is_empty() || (v.current && !options.iter().any(|o| o.selected)) {
            options.insert(
                0,
                AgentOption {
                    value: value(&v.name, None),
                    label: label.clone(),
                    selected: v.current,
                },
            );
        }
        groups.push(AgentGroup { label, options });
    }
    AgentMenu {
        action,
        current,
        groups,
        unavailable,
        error,
    }
}

#[cfg(test)]
mod tests {
    use melissa_agent::session::ModelOption;

    use super::*;

    fn view(name: &str, available: bool, current: bool) -> BackendView {
        BackendView {
            name: name.into(),
            available,
            reason: (!available).then_some("not_installed"),
            current,
            model: None,
            models: None,
        }
    }

    #[test]
    fn the_menu_names_the_current_agent_and_explains_the_missing_ones() {
        let mut claude = view("claude-session", true, true);
        claude.model = Some("sonnet".into());
        claude.models = Some(vec![
            ModelOption {
                id: "default".into(),
                label: "Default (recommended)".into(),
            },
            ModelOption {
                id: "sonnet".into(),
                label: "Sonnet 5.5".into(),
            },
        ]);
        let mut grok = view("grok-acp", false, false);
        grok.reason = Some("not_logged_in");
        let views = [
            view("echo-a", true, false),
            claude,
            grok,
            view("opencode-acp", false, false),
        ];
        let m = menu(&views, "/ui/backend".into(), None, HostKind::Other);
        assert_eq!(m.current, "Claude · Sonnet 5.5");
        assert_eq!(m.groups.len(), 2);
        assert_eq!(m.groups[0].options[0].value, "echo-a");
        let claude = &m.groups[1];
        assert_eq!(claude.label, "Claude");
        let selected: Vec<&str> = claude
            .options
            .iter()
            .filter(|o| o.selected)
            .map(|o| o.value.as_str())
            .collect();
        assert_eq!(selected, ["claude-session sonnet"]);
        assert_eq!(m.unavailable.len(), 2);
        assert_eq!(
            m.unavailable[0].command.as_deref(),
            Some("sudo melissa-setup login grok")
        );
        assert_eq!(
            m.unavailable[1].command.as_deref(),
            Some("sudo melissa-setup agent opencode")
        );
        assert_eq!(
            parse_choice("claude-session sonnet"),
            ("claude-session", Some("sonnet"))
        );
        assert_eq!(parse_choice(" grok-acp "), ("grok-acp", None));

        // In the container image: Settings → Providers, or melissa-setup through podman.
        let m = menu(&views, "/ui/backend".into(), None, HostKind::Container);
        assert_eq!(
            m.unavailable[0].command.as_deref(),
            Some("sudo podman exec -it melissa melissa-setup login grok")
        );
        assert!(m.unavailable[1].text.contains("Settings → Providers"));
        assert_eq!(
            m.unavailable[1].command.as_deref(),
            Some("sudo podman exec -it melissa melissa-setup agent opencode")
        );
        // On Nix: the Nix configuration, no melissa-setup.
        let m = menu(&views, "/ui/backend".into(), None, HostKind::Nix);
        assert_eq!(
            m.unavailable[1].text,
            "This requires you add OpenCode via your Nix configuration."
        );
        assert!(m.unavailable.iter().all(|h| h.command.is_none()));
    }
}
