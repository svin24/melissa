//! The device's name (display only): set in first-run setup's Device step and under
//! Settings → Updates → "This device". Owner only (it is device-wide). Both forms post
//! `name` and go through [`save_name`]: cleaned by `clean_device_name`, saved to `ui.toml`,
//! audited `settings.device_name`.
//!
//! `POST /ui/settings/device-name` (`name`): htmx gets the name's region back with a notice;
//! a plain form post a redirect to the Updates page (or the page with the error).

use axum::{
    extract::{Form, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
};
use melissa_settings::prefs::clean_device_name;
use serde::Deserialize;

use super::pages::{self, DeviceNameView, Flash};
use crate::{components::Tone, is_htmx, ui::Ui};

/// Where the Settings form posts.
pub(crate) const SAVE_URL: &str = "/ui/settings/device-name";

/// The form both places post.
#[derive(Deserialize)]
pub(super) struct NameForm {
    #[serde(default)]
    pub(super) name: String,
}

/// Why a name wasn't saved, in the words both forms show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NameError {
    /// Nothing left after cleaning.
    Empty,
    /// `ui.toml` couldn't be written.
    NotSaved,
}

impl NameError {
    pub(super) fn text(self) -> &'static str {
        match self {
            Self::Empty => "Give the device a name.",
            Self::NotSaved => "Couldn't save the name. Try again.",
        }
    }

    pub(super) fn status(self) -> StatusCode {
        match self {
            Self::Empty => StatusCode::UNPROCESSABLE_ENTITY,
            Self::NotSaved => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// Clean `raw`, save it as the device's name and audit it; the name saved.
pub(super) fn save_name(ui: &Ui, raw: &str) -> Result<String, NameError> {
    let name = clean_device_name(raw).ok_or(NameError::Empty)?;
    if let Err(e) = ui
        .settings
        .update_prefs(|p| p.device_name = Some(name.clone()))
    {
        tracing::error!("saving the device name: {e}");
        return Err(NameError::NotSaved);
    }
    ui.settings.audit(
        "settings.device_name",
        "ui",
        &format!("device name: {name}"),
        None,
    );
    Ok(name)
}

/// What the name field shows: the saved name, else the host's name.
pub(super) fn field_value(ui: &Ui) -> String {
    ui.settings
        .device_name()
        .or_else(host_name)
        .unwrap_or_default()
}

/// The host's name, cleaned like a name typed in.
fn host_name() -> Option<String> {
    ["/proc/sys/kernel/hostname", "/etc/hostname"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .and_then(|n| clean_device_name(n.lines().next().unwrap_or("")))
}

/// The Settings field's view.
pub(super) fn view(ui: &Ui, flash: Option<Flash>) -> DeviceNameView {
    DeviceNameView {
        value: field_value(ui),
        flash,
    }
}

pub(super) async fn save(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Form(form): Form<NameForm>,
) -> Response {
    // The sign-in gate already keeps members out (`auth::OWNER_PREFIXES`); this is the
    // handler's own check.
    if let Some(r) = super::setup::owner_only() {
        return r;
    }
    let result = save_name(&ui, &form.name);
    let flash = match result {
        Ok(_) => Flash {
            text: "Name saved.".into(),
            tone: Tone::Ok,
        },
        Err(e) => Flash {
            text: e.text().into(),
            tone: Tone::Caution,
        },
    };
    if is_htmx(&headers) {
        return pages::device_name_region(&view(&ui, Some(flash))).into_response();
    }
    match result {
        Ok(_) => Redirect::to(pages::UPDATES_URL).into_response(),
        Err(e) => {
            let page = super::updates::page_with_device(&ui, view(&ui, Some(flash))).await;
            (e.status(), page).into_response()
        }
    }
}
