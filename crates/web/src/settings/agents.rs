//! Settings → Agents: Melissa (the lead agent) and her helpers, what each runs on, and a page
//! per helper (`/settings/agents/{id}`, 2026-11-20: where its menu's Rename agent, Change icon,
//! Agent settings and Delete lead): name, icon, Pin, Fresh start and Delete (in-page
//! confirmation). New helpers: `/settings/agents/new` (the sidebar's "+"), a name and an icon;
//! made by `AgentPool::user_create` and opened in its chat. Deleting is the agent layer's
//! [`melissa_agent::lead::agents::AgentPool::delete`]: refused while the helper looks after
//! apps ("Reassign or remove its apps first"), otherwise its chat, file box and schedules go
//! with it; audited `agent.delete`, actor `ui`. The lead agent can't be deleted; its page
//! (`/settings/agents/lead`, its menu's Rename agent) has its name only (2026-11-21).

use axum::{
    extract::{Form, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Redirect, Response},
};
use melissa_agent::lead::{
    agents::AgentView,
    backends::BackendView,
    icon::{AgentIcon, COLORS, SHAPES},
    registry::LEAD_KEY,
    LeadError,
};
use serde::Deserialize;

use super::pages::{self, AgentPage, AgentRow, Flash, AGENTS_URL};
use crate::{
    agents::picker,
    components::Tone,
    feed::backend_label,
    is_htmx,
    pages::short_date,
    state::{Apps, Keeper},
    ui::Ui,
    workspace::agent_settings_href,
};

/// "Claude · Opus 5.5 · High · Fast": the backend, and the model, level and speed when
/// chosen.
pub(crate) fn runs_on(views: &[BackendView], fallback: &str) -> String {
    let Some(current) = views.iter().find(|v| v.current) else {
        return backend_label(fallback).to_string();
    };
    let backend = backend_label(&current.name);
    let p = picker(views, String::new(), None, None);
    let chip = if p.fast {
        format!("{} · Fast", p.chip)
    } else {
        p.chip
    };
    if chip == backend {
        chip
    } else {
        format!("{backend} · {chip}")
    }
}

pub(crate) fn status_words(status: &str) -> (&'static str, Tone) {
    match status {
        "working" => ("Working", Tone::Info),
        "stopped" => ("Stopped", Tone::Neutral),
        _ => ("Idle", Tone::Ok),
    }
}

/// How many agents the Agents section lists (Melissa and her helpers).
pub(super) fn count(ui: &Ui) -> usize {
    rows(ui).len()
}

fn rows(ui: &Ui) -> Vec<AgentRow> {
    let lead_views = ui.lead.backends();
    let mut rows = vec![AgentRow {
        id: None,
        name: ui.lead.agents().lead_name(),
        href: "/".into(),
        runs_on: runs_on(&lead_views, ""),
        status: status_words(if ui.lead.status().turn.is_some() {
            "working"
        } else {
            "idle"
        }),
    }];
    let pool = ui.lead.agents();
    rows.extend(pool.list().into_iter().map(|a| {
        let views = pool.backends_of(&a.id).unwrap_or_default();
        AgentRow {
            href: agent_settings_href(&a.id),
            runs_on: runs_on(&views, &a.backend),
            status: status_words(a.status),
            name: a.name,
            id: Some(a.id),
        }
    }));
    rows
}

pub(super) async fn page(State(ui): State<Ui>) -> Response {
    let rows = rows(&ui);
    ui.framed(|f| pages::agents_page(f, &rows, None))
        .await
        .into_response()
}

pub(super) async fn delete(
    State(ui): State<Ui>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let flash = match ui.lead.agents().delete(&id, "ui").await {
        Ok(d) => Flash {
            text: format!("Deleted {}.", d.name),
            tone: Tone::Ok,
        },
        Err(LeadError::NoSuchAgent(_)) => Flash {
            text: "That helper is already gone.".into(),
            tone: Tone::Neutral,
        },
        Err(e @ (LeadError::InUse(_) | LeadError::Invalid(_))) => Flash {
            text: e.to_string(),
            tone: Tone::Caution,
        },
        Err(e) => Flash {
            text: format!("Couldn't delete it: {e}"),
            tone: Tone::Caution,
        },
    };
    // From the helper's own page: to Melissa's chat when it is gone, else the page says why.
    let from_page = headers
        .get("hx-target")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t == pages::AGENT_ID);
    if from_page {
        return match flash.tone {
            Tone::Caution => after(&ui, &headers, &id, flash).await,
            _ => hx("hx-redirect", "/"),
        };
    }
    let rows = rows(&ui);
    if is_htmx(&headers) {
        return pages::agents_region(&rows, Some(&flash)).into_response();
    }
    if flash.tone == Tone::Caution {
        return ui
            .framed(|f| pages::agents_page(f, &rows, Some(&flash)))
            .await
            .into_response();
    }
    Redirect::to(AGENTS_URL).into_response()
}

/// `the lead agent (task 3)` → its name (`Melissa`), `the user` → `You`.
fn creator_words(created_by: &str, lead: &str) -> String {
    if created_by.starts_with("the lead agent") {
        lead.into()
    } else if created_by == "the user" || created_by == "ui" {
        "you".into()
    } else {
        created_by.to_string()
    }
}

/// A helper's page, with `flash` after an action.
async fn agent_region(ui: &Ui, a: &AgentView, flash: Option<&Flash>, whole: bool) -> Response {
    agent_region_at(ui, a, flash, false, whole).await
}

/// [`agent_region`], with `flash` in the Icon section when `at_icon`; that answer (an icon
/// change) also updates the sidebar out of band, since the Settings page has no live stream.
async fn agent_region_at(
    ui: &Ui,
    a: &AgentView,
    flash: Option<&Flash>,
    at_icon: bool,
    whole: bool,
) -> Response {
    let snap = ui.cluster.get().await;
    let keeps: Vec<String> = match &snap.apps {
        Apps::Known { installed } => installed
            .iter()
            .filter(|app| matches!(&app.keeper, Some(Keeper::Agent { id, .. }) if *id == a.id))
            .map(crate::state::App::display_name)
            .collect(),
        _ => Vec::new(),
    };
    let kept = (!keeps.is_empty()).then(|| {
        format!(
            "Looks after {}. Reassign or remove its apps before deleting it.",
            keeps.join(", ")
        )
    });
    let views = ui.lead.agents().backends_of(&a.id).unwrap_or_default();
    let runs = runs_on(&views, &a.backend);
    let made = format!(
        "By {}, {}",
        creator_words(&a.created_by, &ui.lead.agents().lead_name()),
        short_date(a.created_at)
    );
    let page = AgentPage {
        id: &a.id,
        lead: false,
        name: &a.name,
        shape: &a.icon.shape,
        color: &a.icon.color,
        shapes: SHAPES,
        colors: COLORS,
        runs_on: &runs,
        status: status_words(a.status),
        made: &made,
        pinned: a.pinned,
        kept: kept.as_deref(),
        flash,
        flash_at_icon: at_icon,
    };
    if whole {
        ui.framed(|f| pages::agent_settings_page(f, &page))
            .await
            .into_response()
    } else if at_icon {
        let side = ui.side_data(&snap);
        maud::html! {
            (pages::agent_settings_region(&page))
            (crate::pages::sidebar_update(&side, None))
        }
        .into_response()
    } else {
        pages::agent_settings_region(&page).into_response()
    }
}

/// The lead agent's page: its name (the rest of its settings are in its chat's menu).
async fn lead_region(ui: &Ui, flash: Option<&Flash>, whole: bool) -> Response {
    let name = ui.lead.agents().lead_name();
    let runs = runs_on(&ui.lead.backends(), "");
    let page = AgentPage {
        id: LEAD_KEY,
        lead: true,
        name: &name,
        shape: "",
        color: "",
        shapes: SHAPES,
        colors: COLORS,
        runs_on: &runs,
        status: status_words(if ui.lead.status().turn.is_some() {
            "working"
        } else {
            "idle"
        }),
        made: "",
        pinned: true,
        kept: None,
        flash,
        flash_at_icon: false,
    };
    if whole {
        ui.framed(|f| pages::agent_settings_page(f, &page))
            .await
            .into_response()
    } else {
        pages::agent_settings_region(&page).into_response()
    }
}

pub(super) async fn agent_page(State(ui): State<Ui>, Path(id): Path<String>) -> Response {
    if id == LEAD_KEY {
        return lead_region(&ui, None, true).await;
    }
    match ui.lead.agents().get(&id) {
        Some(a) => agent_region(&ui, &a, None, true).await,
        None => (
            StatusCode::NOT_FOUND,
            ui.framed(crate::pages::helper_gone).await,
        )
            .into_response(),
    }
}

/// After an action on a helper's page: htmx gets the page's region (with `flash`), a plain
/// form post a redirect back to the page (or the page with the error).
async fn after(ui: &Ui, headers: &HeaderMap, id: &str, flash: Flash) -> Response {
    let Some(a) = ui.lead.agents().get(id) else {
        return Redirect::to(AGENTS_URL).into_response();
    };
    if is_htmx(headers) {
        return agent_region(ui, &a, Some(&flash), false).await;
    }
    if flash.tone == Tone::Caution {
        return agent_region(ui, &a, Some(&flash), true).await;
    }
    Redirect::to(&agent_settings_href(&a.id)).into_response()
}

fn hx(header: &'static str, value: &str) -> Response {
    let mut res = StatusCode::OK.into_response();
    if let Ok(v) = HeaderValue::from_str(value) {
        res.headers_mut().insert(header, v);
    }
    res
}

#[derive(Deserialize)]
pub(super) struct NameForm {
    #[serde(default)]
    name: String,
}

pub(super) async fn rename(
    State(ui): State<Ui>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<NameForm>,
) -> Response {
    if id == LEAD_KEY {
        return rename_lead(&ui, &headers, &form.name).await;
    }
    let flash = match ui.lead.agents().rename(&id, &form.name) {
        // The name shows in the sidebar, the title and the chat: reload the page.
        Ok(_) if is_htmx(&headers) => return hx("hx-refresh", "true"),
        Ok(a) => Flash {
            text: format!("Renamed to {}.", a.name),
            tone: Tone::Ok,
        },
        Err(e) => Flash {
            text: format!("Couldn't rename it: {e}"),
            tone: Tone::Caution,
        },
    };
    after(&ui, &headers, &id, flash).await
}

/// The lead agent's rename (same form and rules as a helper's).
async fn rename_lead(ui: &Ui, headers: &HeaderMap, name: &str) -> Response {
    let flash = match ui.lead.agents().rename_lead(name) {
        Ok(_) if is_htmx(headers) => return hx("hx-refresh", "true"),
        Ok(name) => Flash {
            text: format!("Renamed to {name}."),
            tone: Tone::Ok,
        },
        Err(e) => Flash {
            text: format!("Couldn't rename it: {e}"),
            tone: Tone::Caution,
        },
    };
    if is_htmx(headers) {
        return lead_region(ui, Some(&flash), false).await;
    }
    if flash.tone == Tone::Caution {
        return lead_region(ui, Some(&flash), true).await;
    }
    Redirect::to(&agent_settings_href(LEAD_KEY)).into_response()
}

#[derive(Deserialize, Default)]
pub(super) struct IconForm {
    #[serde(default)]
    shape: String,
    #[serde(default)]
    color: String,
}

pub(super) async fn icon(
    State(ui): State<Ui>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<IconForm>,
) -> Response {
    let flash = match AgentIcon::new(&form.shape, &form.color) {
        None => Flash {
            text: "Pick a shape and a colour.".into(),
            tone: Tone::Caution,
        },
        // htmx: the page's region again, with the notice by the Icon form (the sidebar's live
        // update shows the new icon there).
        Some(icon) => match ui.lead.agents().set_icon(&id, icon) {
            Ok(_) => Flash {
                text: "Icon saved.".into(),
                tone: Tone::Ok,
            },
            Err(e) => Flash {
                text: format!("Couldn't save the icon: {e}"),
                tone: Tone::Caution,
            },
        },
    };
    if is_htmx(&headers) {
        if let Some(a) = ui.lead.agents().get(&id) {
            return agent_region_at(&ui, &a, Some(&flash), true, false).await;
        }
    }
    after(&ui, &headers, &id, flash).await
}

#[derive(Deserialize, Default)]
pub(super) struct PinForm {
    #[serde(default)]
    pinned: String,
    /// The Pinned switch on the helper's page (a checkbox: absent when off).
    #[serde(default)]
    enabled: Option<String>,
}

pub(super) async fn pin(
    State(ui): State<Ui>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(form): Form<PinForm>,
) -> Response {
    let on = form.pinned == "on" || form.enabled.as_deref() == Some("on");
    let flash = match ui.lead.agents().set_pinned(&id, on) {
        Ok(_) => Flash {
            text: if on { "Pinned." } else { "Unpinned." }.into(),
            tone: Tone::Ok,
        },
        Err(e) => Flash {
            text: format!("Couldn't change it: {e}"),
            tone: Tone::Caution,
        },
    };
    after(&ui, &headers, &id, flash).await
}

pub(super) async fn fresh_start(
    State(ui): State<Ui>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let flash = match ui.lead.agents().fresh_start(&id).await {
        Ok(a) => Flash {
            text: format!(
                "{}'s memory is cleared. Its next message starts fresh.",
                a.name
            ),
            tone: Tone::Ok,
        },
        Err(e) => Flash {
            text: format!("Couldn't clear its memory: {e}"),
            tone: Tone::Caution,
        },
    };
    after(&ui, &headers, &id, flash).await
}

pub(super) async fn new_page(State(ui): State<Ui>) -> Response {
    let next = AgentIcon::default_for(&format!("x.a{}", ui.lead.agents().list().len() + 1));
    ui.framed(|f| pages::new_agent_page(f, SHAPES, COLORS, "", (&next.shape, &next.color), None))
        .await
        .into_response()
}

#[derive(Deserialize, Default)]
pub(super) struct NewForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    shape: String,
    #[serde(default)]
    color: String,
}

/// Make a helper; htmx gets `HX-Redirect` to its chat, a plain form post a redirect. A
/// refusal (bad or taken name) shows the form again with what was typed.
pub(super) async fn create(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Form(form): Form<NewForm>,
) -> Response {
    let icon = AgentIcon::new(&form.shape, &form.color);
    let error = match icon.clone() {
        None => "Pick a shape and a colour.".to_string(),
        Some(icon) => match ui.lead.agents().user_create(&form.name, Some(icon)) {
            Ok(a) => {
                let chat = crate::pages::agent_href(&a.id);
                return if is_htmx(&headers) {
                    hx("hx-redirect", &chat)
                } else {
                    Redirect::to(&chat).into_response()
                };
            }
            Err(e) => format!("Couldn't create it: {e}"),
        },
    };
    let flash = Flash {
        text: error,
        tone: Tone::Caution,
    };
    let shape = icon.as_ref().map_or("cube", |i| i.shape.as_str());
    let color = icon.as_ref().map_or("green", |i| i.color.as_str());
    if is_htmx(&headers) {
        return pages::new_agent_region(SHAPES, COLORS, &form.name, (shape, color), Some(&flash))
            .into_response();
    }
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        ui.framed(|f| {
            pages::new_agent_page(f, SHAPES, COLORS, &form.name, (shape, color), Some(&flash))
        })
        .await,
    )
        .into_response()
}
