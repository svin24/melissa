//! The Settings list (2026-11-22, owner design C1/C2): on a phone Settings opens as a grouped
//! list of its sections, each row with a short value ("3 ready", "Off"), and a section opens
//! full-screen with a back link. That list is `GET /settings`; every other Settings page is a
//! section (or a page below one). Above 760px both levels look the same: the section list on
//! the left and a section on the right, so `/settings` also carries the first section this
//! person may open (Providers for the owner, Agents for a member) and only CSS decides what
//! shows (`data-list` on `#settings-main`).
//!
//! The values are worked out only for this page and travel to [`super::pages::frame_at`] as a
//! task-local, like `crate::panel`'s flags, so the section handlers stay as they are.

use axum::{
    extract::State,
    response::{IntoResponse, Response},
};
use melissa_settings::tailscale::State as TsState;

use super::{about, agents, files, pages, providers, updates, voice};
use crate::ui::Ui;

/// The list level: each section's short value, by its URL.
#[derive(Debug, Clone, Default)]
pub(crate) struct ListLevel {
    pub values: Vec<(&'static str, String)>,
}

impl ListLevel {
    /// The value shown in `href`'s row, if any.
    pub(crate) fn value(&self, href: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(h, _)| *h == href)
            .map(|(_, v)| v.as_str())
    }
}

tokio::task_local! {
    static LIST: ListLevel;
}

/// The list level when this request is `GET /settings` (None: a section).
pub(crate) fn current() -> Option<ListLevel> {
    LIST.try_with(Clone::clone).ok()
}

/// `GET /settings`: the list (phones) over the first section this person may open.
pub(super) async fn index(State(ui): State<Ui>) -> Response {
    let owner = crate::auth::signed_in().is_none_or(|p| p.is_owner());
    if owner {
        let v = providers::view(&ui, None, None).await;
        let level = values(&ui, Some(&v)).await;
        LIST.scope(level, ui.framed(|f| pages::providers_page(f, &v)))
            .await
            .into_response()
    } else {
        let level = values(&ui, None).await;
        LIST.scope(level, agents::page(State(ui))).await
    }
}

/// Work out the values for the rows this person sees (`providers`: the owner's view).
async fn values(ui: &Ui, providers: Option<&pages::ProvidersView>) -> ListLevel {
    let owner = providers.is_some();
    let mut values: Vec<(&'static str, String)> = Vec::new();
    if let Some(v) = providers {
        values.push(("/settings/providers", ready_words(v)));
    }
    values.push((pages::AGENTS_URL, agents::count(ui).to_string()));
    if owner {
        let rules = ui.registry().permissions().rules().len();
        values.push((pages::PERMISSIONS_URL, count_words(rules, "rule", "rules")));
        if let Some(n) = ui.lead.scheduler().map(|s| s.schedules().list().len()) {
            values.push((pages::SCHEDULES_URL, count_words(n, "job", "jobs")));
        }
    }
    // Every row has a value (owner board P1, 2026-11-23).
    let voice = voice::view(&ui.settings, None);
    let voice = if voice.available {
        on_off(voice.on)
    } else {
        NOT_HERE
    };
    values.push((pages::VOICE_URL, voice.into()));
    values.push((pages::FILES_URL, files::default_words(ui)));
    if owner {
        let apps = match ui.settings.parts().app_access.as_ref() {
            Some(c) => c.view().setting.label().into(),
            None => "Firewall".into(),
        };
        values.push((pages::APPS_URL, apps));
        let tailscale = match ui.settings.parts().tailscale.clone() {
            Some(c) => tailscale_words(&c.cached_state().await),
            None => None,
        };
        values.push((pages::TAILSCALE_URL, tailscale.unwrap_or(NOT_HERE).into()));
        values.push((pages::UPDATES_URL, updates::list_word(ui).into()));
        let perms = ui.registry().permissions();
        let developer = perms.bypass() || perms.full_access_allowed();
        values.push((pages::DEVELOPER_URL, on_off(developer).into()));
        if !super::reset::available(ui) {
            values.push((pages::RESET_URL, NOT_HERE.into()));
        }
        values.push(("/settings/people", ui.auth.people().len().to_string()));
    }
    if let Some(p) = crate::auth::signed_in() {
        values.push(("/settings/account", p.username));
    }
    values.push((pages::ABOUT_URL, about::version(ui)));
    ListLevel { values }
}

/// A row whose setting this install doesn't have.
const NOT_HERE: &str = "Not available";

/// "3 ready": the providers that are on and ready.
fn ready_words(v: &pages::ProvidersView) -> String {
    let n = v.cards.iter().filter(|c| c.enabled && c.ready()).count();
    if n == 0 {
        "None ready".into()
    } else {
        format!("{n} ready")
    }
}

fn count_words(n: usize, one: &str, many: &str) -> String {
    match n {
        0 => "None".into(),
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    }
}

fn on_off(on: bool) -> &'static str {
    if on {
        "On"
    } else {
        "Off"
    }
}

/// Tailscale's state in a word or two (None: Melissa doesn't manage it here).
fn tailscale_words(s: &TsState) -> Option<&'static str> {
    Some(match s {
        TsState::NotManaged => return None,
        TsState::Off => "Off",
        TsState::Starting => "Starting",
        TsState::SignInNeeded => "Sign in needed",
        TsState::NeedsApproval => "Waiting",
        TsState::Connected(_) => "Connected",
        TsState::Error(_) => "Not connected",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_is_found_by_its_row() {
        let level = ListLevel {
            values: vec![("/settings/voice", "Off".into())],
        };
        assert_eq!(level.value("/settings/voice"), Some("Off"));
        assert_eq!(level.value("/settings/files"), None);
        // Outside `GET /settings` there is no list level.
        assert!(current().is_none());
    }
}
