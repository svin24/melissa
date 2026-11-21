//! Settings → Apps: who may open the apps on this device ("Apps open to": This device,
//! Tailnet, Home network; `melissa_settings::app_access`). The owner's own change: no
//! confirmation (agents ask the owner through `set_app_access`); applied at once, audited
//! `settings.app_access`, actor `ui`. Without Melissa's filter (not a container install) the
//! page says the host's firewall decides.

use axum::{
    extract::{Form, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
};
use melissa_settings::app_access::{AppAccess, AppAccessError};
use serde::Deserialize;

use super::pages::{self, AppsState, AppsView, Flash, APPS_URL};
use crate::{components::Tone, is_htmx, ui::Ui};

fn view(ui: &Ui, flash: Option<Flash>) -> AppsView {
    let state = ui.settings.parts().app_access.as_ref().map(|c| {
        let v = c.view();
        AppsState {
            selected: v.setting.as_str().to_string(),
            sentence: v.setting.sentence(),
            problem: v.problem(),
        }
    });
    AppsView {
        choices: AppAccess::ALL
            .iter()
            .map(|a| (a.as_str().to_string(), a.label().to_string()))
            .collect(),
        state,
        flash,
    }
}

pub(super) async fn page(State(ui): State<Ui>) -> Response {
    let v = view(&ui, None);
    ui.framed(|f| pages::apps_page(f, &v)).await.into_response()
}

#[derive(Deserialize)]
pub(super) struct OpenIn {
    #[serde(default)]
    open_to: String,
}

pub(super) async fn save(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Form(form): Form<OpenIn>,
) -> Response {
    let Some(control) = ui.settings.parts().app_access.clone() else {
        return (StatusCode::NOT_FOUND, "Not available on this device.").into_response();
    };
    let flash = match AppAccess::parse(&form.open_to) {
        None => Flash {
            text: "Choose who can open apps.".into(),
            tone: Tone::Error,
        },
        Some(a) => match control.set(a, "ui").await {
            Ok(_) => Flash {
                text: "Saved.".into(),
                tone: Tone::Ok,
            },
            Err(AppAccessError::Apply(problem)) => Flash {
                text: problem,
                tone: Tone::Error,
            },
            Err(AppAccessError::Save(e)) => {
                tracing::warn!("cannot save the app access setting: {e}");
                Flash {
                    text: "Couldn't save that.".into(),
                    tone: Tone::Error,
                }
            }
        },
    };
    let failed = flash.tone == Tone::Error;
    let v = view(&ui, Some(flash));
    if is_htmx(&headers) {
        return pages::apps_region(&v).into_response();
    }
    if failed {
        return ui.framed(|f| pages::apps_page(f, &v)).await.into_response();
    }
    Redirect::to(APPS_URL).into_response()
}
