//! Settings → Files: how long file boxes keep files by default (see `crate::files` for the
//! per-box setting, the panel and the API). The owner's own change: no confirmation; audited
//! `files.retention`, actor `ui`.

use axum::{
    extract::{Form, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};
use melissa_agent::files::Keep;
use serde::Deserialize;

use super::pages::{self, FilesView, Flash, FILES_URL};
use crate::{components::Tone, is_htmx, ui::Ui};

fn view(ui: &Ui, flash: Option<Flash>) -> FilesView {
    let default = ui.lead.files().retention().default;
    FilesView {
        choices: crate::files::keep_choices(None, Some(default)),
        selected: default.value(),
        overrides: crate::files::overrides(ui),
        flash,
    }
}

pub(super) async fn page(State(ui): State<Ui>) -> Response {
    let v = view(&ui, None);
    ui.framed(|f| pages::files_page(f, &v))
        .await
        .into_response()
}

#[derive(Deserialize)]
pub(super) struct KeepIn {
    #[serde(default)]
    keep: String,
}

pub(super) async fn save(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Form(form): Form<KeepIn>,
) -> Response {
    let result = Keep::parse(&form.keep).and_then(|k| ui.lead.files().set_default(k, "ui"));
    let flash = match result {
        Ok(()) => Flash {
            text: "Saved.".into(),
            tone: Tone::Ok,
        },
        Err(e) => Flash {
            text: format!("Couldn't save that: {e}"),
            tone: Tone::Caution,
        },
    };
    let failed = flash.tone == Tone::Caution;
    let v = view(&ui, Some(flash));
    if is_htmx(&headers) {
        return pages::files_region(&v).into_response();
    }
    if failed {
        return ui
            .framed(|f| pages::files_page(f, &v))
            .await
            .into_response();
    }
    Redirect::to(FILES_URL).into_response()
}
