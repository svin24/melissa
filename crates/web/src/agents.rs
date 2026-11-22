//! The model picker of a chat: which backend, model, reasoning level and speed the lead agent
//! or a helper runs on, its access level, and the choices the user has. Builds the fixed
//! [`ModelPicker`] component from the API's [`BackendView`]s. Only backends that are usable
//! now are offered: one that isn't installed or signed in is set up in Settings → Providers,
//! not here. Option groups show only when they apply (2026-11-20, owner request): reasoning
//! levels and Speed only for a model the agent reported them for, Access only with the
//! developer option "Full access per agent".

use melissa_agent::lead::backends::BackendView;

use crate::{
    components::{ModelPicker, PickerAccess, PickerBackend, PickerLevel, PickerModel},
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
pub(crate) fn letter(label: &str) -> String {
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

/// Fast mode to keep when the user picks `model` on `backend` without choosing a speed: on,
/// if it is on now and the new model offers it too.
pub(crate) fn kept_fast(views: &[BackendView], backend: &str, model: Option<&str>) -> bool {
    if !views.iter().any(|v| v.current && v.fast) {
        return false;
    }
    let wanted = model.unwrap_or("default");
    views
        .iter()
        .find(|v| v.name == backend)
        .and_then(|v| v.models.as_ref())
        .is_some_and(|models| models.iter().any(|m| m.id == wanted && m.fast))
}

/// The access level of the chat's agent, for the picker: None while the developer option
/// "Full access per agent" is off (the group is left out), else whether it is on Full access.
pub(crate) type Access = Option<bool>;

/// The picker for `views`, posting to `action`. `access`: see [`Access`].
pub(crate) fn picker(
    views: &[BackendView],
    action: String,
    error: Option<String>,
    access: Access,
) -> ModelPicker {
    let access = access.map(|full| PickerAccess { full });
    let full_access = access.as_ref().is_some_and(|a| a.full);
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
            speed: None,
            fast: false,
            access,
            full_access,
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
    // Speed only for a model the agent reported a fast mode for (a model that is itself a
    // fast variant reports none).
    let speed = offered.filter(|m| m.fast).map(|_| cur.fast);
    ModelPicker {
        action,
        chip,
        letter: Some(letter(&name)),
        current: value(&cur.name, model),
        backends,
        levels,
        speed,
        fast: speed == Some(true),
        access,
        full_access,
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
            fast: false,
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
        let p = picker(&views, "/ui/backend".into(), None, None);
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
        let p = picker(&[c], "/ui/backend".into(), None, None);
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

    /// Claude as the installed CLI reports it: Opus has a fast mode and levels, Haiku (a
    /// fast model already) has neither.
    fn claude_fast() -> BackendView {
        let mut c = view("claude-session", true, true);
        c.model = Some("opus".into());
        let mut opus = ModelOption::new("opus", "Opus 5.5");
        opus.efforts = levels(&["high", "max"]);
        opus.fast = true;
        c.models = Some(vec![opus, ModelOption::new("haiku", "Haiku 4.5")]);
        c
    }

    #[test]
    fn speed_shows_only_for_a_model_with_a_fast_mode() {
        let mut c = claude_fast();
        let p = picker(std::slice::from_ref(&c), "/ui/backend".into(), None, None);
        assert_eq!(p.speed, Some(false));
        assert!(!p.fast);
        assert!(!p.levels.is_empty());
        c.fast = true;
        let p = picker(std::slice::from_ref(&c), "/ui/backend".into(), None, None);
        assert_eq!(p.speed, Some(true));
        assert!(p.fast);
        // A fast model: no Speed group, no levels column.
        c.model = Some("haiku".into());
        c.fast = false;
        let p = picker(&[c], "/ui/backend".into(), None, None);
        assert_eq!(p.speed, None);
        assert!(p.levels.is_empty());
        // An ACP agent that reports only levels (like grok): no Speed either.
        let mut grok = view("grok-acp", true, true);
        grok.model = Some("grok-4.7".into());
        let mut m = ModelOption::new("grok-4.7", "Grok 4.7");
        m.efforts = levels(&["low", "high"]);
        grok.models = Some(vec![m, ModelOption::new("grok-4.7-build-fast", "Fast")]);
        let p = picker(&[grok], "/ui/backend".into(), None, None);
        assert_eq!(p.speed, None);
        assert_eq!(p.levels.len(), 3);
    }

    #[test]
    fn fast_is_kept_only_when_the_new_model_offers_it() {
        let mut c = claude_fast();
        c.fast = true;
        let views = [c];
        assert!(kept_fast(&views, "claude-session", Some("opus")));
        assert!(!kept_fast(&views, "claude-session", Some("haiku")));
        assert!(!kept_fast(&views, "claude-session", Some("nope")));
        let mut off = claude_fast();
        off.fast = false;
        assert!(!kept_fast(&[off], "claude-session", Some("opus")));
    }

    #[test]
    fn access_shows_only_with_the_developer_option_and_marks_the_chip() {
        let c = claude_fast();
        let p = picker(std::slice::from_ref(&c), "/ui/backend".into(), None, None);
        assert!(p.access.is_none());
        assert!(!p.full_access);
        let p = picker(
            std::slice::from_ref(&c),
            "/ui/backend".into(),
            None,
            Some(false),
        );
        assert!(p.access.as_ref().is_some_and(|a| !a.full));
        assert!(!p.full_access);
        let p = picker(&[c], "/ui/backend".into(), None, Some(true));
        assert!(p.full_access);
        let html = crate::components::model_chip(&p).into_string();
        assert!(html.contains("chip-warn"));
        let html = crate::components::model_picker(&p).into_string();
        assert!(html.contains(r#"name="access" value="ask""#));
        assert!(html.contains("Speed"));
    }

    #[test]
    fn the_footer_is_left_out_when_nothing_applies() {
        let mut c = claude_fast();
        c.model = Some("haiku".into());
        let p = picker(&[c], "/ui/backend".into(), None, None);
        let html = crate::components::model_picker(&p).into_string();
        assert!(!html.contains("picker-footer"));
        assert!(!html.contains("picker-levels"));
        let chip = crate::components::model_chip(&p).into_string();
        assert!(!chip.contains("chip-tag") && !chip.contains("chip-warn"));
    }

    #[test]
    fn nothing_usable_says_so() {
        let p = picker(
            &[view("grok-acp", false, true)],
            "/ui/backend".into(),
            None,
            None,
        );
        assert!(p.backends.is_empty());
        assert_eq!(p.letter, None);
        assert_eq!(p.chip, "No agent yet");
    }
}
