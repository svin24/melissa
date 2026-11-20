//! The model picker of a chat: which backend, model and reasoning level the lead agent or a
//! helper runs on, and the choices the user has. Builds the fixed [`ModelPicker`] component
//! from the API's [`BackendView`]s. Only backends that are usable now are offered: one that
//! isn't installed or signed in is set up in Settings → Providers, not here.

use melissa_agent::lead::backends::BackendView;

use crate::{
    components::{ModelPicker, PickerBackend, PickerLevel, PickerModel},
    feed::backend_label,
};

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

/// The letter in a backend's mark.
fn letter(label: &str) -> String {
    label
        .chars()
        .find(|c| c.is_alphanumeric())
        .map_or_else(|| "?".into(), |c| c.to_uppercase().collect())
}

/// The reasoning level to keep when the user picks `model` on `backend` without choosing a
/// level: the current one, if the new model offers it too.
pub(crate) fn kept_effort(
    views: &[BackendView],
    backend: &str,
    model: Option<&str>,
) -> Option<String> {
    let current = views.iter().find(|v| v.current)?;
    let effort = current.effort.as_deref()?;
    let view = views.iter().find(|v| v.name == backend)?;
    let wanted = model.unwrap_or("default");
    view.models
        .iter()
        .flatten()
        .find(|m| m.id == wanted)
        .filter(|m| m.offers_effort(effort))
        .map(|_| effort.to_string())
}

/// The picker for `views`, posting to `action`.
pub(crate) fn picker(views: &[BackendView], action: String, error: Option<String>) -> ModelPicker {
    let usable: Vec<&BackendView> = views.iter().filter(|v| v.available).collect();
    let current = usable.iter().copied().find(|v| v.current);
    let mut backends = Vec::new();
    for v in &usable {
        let label = backend_label(&v.name).to_string();
        let mut models: Vec<PickerModel> = v
            .models
            .iter()
            .flatten()
            .map(|m| PickerModel {
                value: value(&v.name, Some(&m.id)),
                label: m.label.clone(),
                current: v.current && v.model.as_deref() == Some(m.id.as_str()),
            })
            .collect();
        if models.is_empty() || (v.current && !models.iter().any(|m| m.current)) {
            models.insert(
                0,
                PickerModel {
                    value: value(&v.name, None),
                    label: label.clone(),
                    current: v.current,
                },
            );
        }
        backends.push(PickerBackend {
            name: v.name.clone(),
            letter: letter(&label),
            label,
            models,
        });
    }
    let Some(cur) = current else {
        return ModelPicker {
            action,
            chip: "No agent yet".into(),
            letter: None,
            current: String::new(),
            backends,
            levels: Vec::new(),
            error,
        };
    };
    let name = backend_label(&cur.name).to_string();
    let model = cur.model.as_deref();
    let offered = cur
        .models
        .iter()
        .flatten()
        .find(|m| m.id == model.unwrap_or("default"));
    let efforts = offered.map(|m| m.efforts.as_slice()).unwrap_or_default();
    let mut levels = Vec::new();
    if !efforts.is_empty() {
        levels.push(PickerLevel {
            value: String::new(),
            label: "Default".into(),
            current: cur.effort.is_none(),
        });
        levels.extend(efforts.iter().map(|e| PickerLevel {
            value: e.id.clone(),
            label: e.label.clone(),
            current: cur.effort.as_deref() == Some(e.id.as_str()),
        }));
    }
    let effort_label = cur.effort.as_deref().map(|e| {
        efforts
            .iter()
            .find(|l| l.id == e)
            .map_or_else(|| e.to_string(), |l| l.label.clone())
    });
    let base = model
        .and_then(|m| model_label(cur, m))
        .unwrap_or(name.clone());
    let chip = match effort_label {
        Some(e) => format!("{base} · {e}"),
        None => base,
    };
    ModelPicker {
        action,
        chip,
        letter: Some(letter(&name)),
        current: value(&cur.name, model),
        backends,
        levels,
        error,
    }
}

#[cfg(test)]
mod tests {
    use melissa_agent::session::{EffortOption, ModelOption};

    use super::*;

    fn view(name: &str, available: bool, current: bool) -> BackendView {
        BackendView {
            name: name.into(),
            available,
            reason: (!available).then_some("not_installed"),
            current,
            model: None,
            effort: None,
            models: None,
        }
    }

    fn levels(ids: &[&str]) -> Vec<EffortOption> {
        ids.iter()
            .map(|i| EffortOption {
                id: (*i).into(),
                label: i.to_uppercase(),
            })
            .collect()
    }

    fn claude() -> BackendView {
        let mut claude = view("claude-session", true, true);
        claude.model = Some("sonnet".into());
        claude.effort = Some("high".into());
        let mut sonnet = ModelOption::new("sonnet", "Sonnet 5.5");
        sonnet.efforts = levels(&["low", "high"]);
        claude.models = Some(vec![
            ModelOption::new("default", "Default (recommended)"),
            sonnet,
            ModelOption::new("haiku", "Haiku"),
        ]);
        claude
    }

    #[test]
    fn the_picker_offers_only_usable_backends_and_names_the_current_model() {
        let mut grok = view("grok-acp", false, false);
        grok.reason = Some("not_logged_in");
        let views = [
            view("echo-a", true, false),
            claude(),
            grok,
            view("opencode-acp", false, false),
        ];
        let p = picker(&views, "/ui/backend".into(), None);
        assert_eq!(p.chip, "Sonnet 5.5 · HIGH");
        assert_eq!(p.letter.as_deref(), Some("C"));
        assert_eq!(p.current, "claude-session sonnet");
        let names: Vec<&str> = p.backends.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["echo-a", "claude-session"]);
        assert_eq!(p.backends[0].models[0].value, "echo-a");
        let current: Vec<&str> = p.backends[1]
            .models
            .iter()
            .filter(|m| m.current)
            .map(|m| m.value.as_str())
            .collect();
        assert_eq!(current, ["claude-session sonnet"]);
        let lv: Vec<(&str, bool)> = p
            .levels
            .iter()
            .map(|l| (l.value.as_str(), l.current))
            .collect();
        assert_eq!(lv, [("", false), ("low", false), ("high", true)]);
        assert_eq!(
            parse_choice("claude-session sonnet"),
            ("claude-session", Some("sonnet"))
        );
        assert_eq!(parse_choice(" grok-acp "), ("grok-acp", None));
    }

    #[test]
    fn a_model_without_levels_hides_the_column_and_keeps_only_offered_levels() {
        let mut c = claude();
        c.model = Some("haiku".into());
        c.effort = None;
        let p = picker(&[c], "/ui/backend".into(), None);
        assert_eq!(p.chip, "Haiku");
        assert!(p.levels.is_empty());

        let views = [claude()];
        // Sonnet offers `high`: kept; Haiku offers none: dropped.
        assert_eq!(
            kept_effort(&views, "claude-session", Some("sonnet")).as_deref(),
            Some("high")
        );
        assert_eq!(kept_effort(&views, "claude-session", Some("haiku")), None);
    }

    #[test]
    fn nothing_usable_says_so() {
        let p = picker(&[view("grok-acp", false, true)], "/ui/backend".into(), None);
        assert!(p.backends.is_empty());
        assert_eq!(p.letter, None);
        assert_eq!(p.chip, "No agent yet");
    }
}
