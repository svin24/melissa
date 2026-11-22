//! First-run setup (owner decision 2026-11-21: a wizard plus a checklist). After the claim
//! the owner goes through four steps: Device (the name the UI shows), Provider (an agent
//! program Melissa can use), Model (Melissa's model) and Extras (Tailscale, voice input).
//!
//! Progress comes from the real state: a device name saved in `ui.toml`, a usable backend,
//! a saved choice in `lead/backend.json`, voice input or Melissa's Tailscale on. The only
//! thing setup keeps itself is `[setup]` in `ui.toml`: the steps put off with "Later" and
//! whether the checklist was hidden. So it resumes by itself after a restart (adding a
//! provider restarts Melissa, `apply.rs`).
//!
//! - `GET /setup` → the first step to do; `GET /setup/{device|provider|model|extras}`
//!   (`/setup/provider?p=<id>` picks a provider). Owner only: members go to the chat.
//! - `POST /ui/setup/device` (`name`), `POST /ui/setup/model` (`choice`: `<backend>` or
//!   `<backend> <model>`, written through `Lead::use_backend`), `POST /ui/setup/{step}/later`,
//!   `POST /ui/setup/dismiss` (hide the checklist). Plain forms; each answers with a redirect.
//! - The Provider step shows the providers as tiles; the chosen one's Install, Sign in and
//!   API key are the Settings → Providers forms and handlers, which answer with this step's
//!   region when the request comes from `/setup` ([`from_setup`]). Extras reuses the Voice
//!   and Tailscale regions and handlers the same way.
//! - While a required step (Device, Provider, Model) is neither done nor put off, the owner's
//!   `/` and `/chat` go to it ([`redirect_for`]); Model waits while no provider is usable.
//!   Steps put off (or not done) show as a checklist in Melissa's empty chat until done or
//!   hidden.

use axum::{
    extract::{Form, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Router,
};
use maud::{html, Markup};
use melissa_agent::lead::backends::BackendView;
use serde::Deserialize;

use super::{
    device,
    pages::{self, ProvidersView, Step as ProviderStep},
    providers, tailscale, voice,
};
use crate::{
    components::{
        link_button, live_region, notice, notice_link, provider_tiles, select_field_at,
        setup_actions, setup_page, setup_submit, text_field, Btn, Choice, ProviderTile, TileLink,
        Tone,
    },
    feed::backend_label,
    ui::Ui,
};

pub(crate) const SETUP_URL: &str = "/setup";
pub(crate) const EXTRAS_URL: &str = "/setup/extras";
pub(crate) const PROVIDER_URL: &str = "/setup/provider";
pub(crate) const DISMISS_URL: &str = "/ui/setup/dismiss";
const MODEL_REGION_ID: &str = "setup-model";
/// The model list is asked for again this many times (every 3 s) while a provider hasn't
/// reported its models yet.
const MODEL_POLLS: u32 = 10;

/// The steps, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetupStep {
    Device,
    Provider,
    Model,
    Extras,
}

pub(crate) const STEPS: [SetupStep; 4] = [
    SetupStep::Device,
    SetupStep::Provider,
    SetupStep::Model,
    SetupStep::Extras,
];

impl SetupStep {
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Device => "device",
            Self::Provider => "provider",
            Self::Model => "model",
            Self::Extras => "extras",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        STEPS.into_iter().find(|s| s.key() == key)
    }

    fn index(self) -> usize {
        STEPS.iter().position(|s| *s == self).unwrap_or(0)
    }

    /// Device, Provider and Model must be done or put off before the chat opens.
    pub(crate) fn required(self) -> bool {
        self != Self::Extras
    }

    pub(crate) fn href(self) -> String {
        format!("{SETUP_URL}/{}", self.key())
    }

    /// The stepper's word.
    fn short(self) -> &'static str {
        match self {
            Self::Device => "Device",
            Self::Provider => "Provider",
            Self::Model => "Model",
            Self::Extras => "Extras",
        }
    }

    /// The step's heading, also its line in the checklist.
    fn heading(self) -> &'static str {
        match self {
            Self::Device => "Name this device",
            Self::Provider => "Connect a provider",
            Self::Model => "Choose Melissa's model",
            Self::Extras => "Tailscale and voice",
        }
    }
}

/// What setup knows now: the real state plus the owner's "Later"s.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Progress {
    pub device_named: bool,
    pub provider_ready: bool,
    pub model_chosen: bool,
    pub extras_on: bool,
    pub skipped: Vec<String>,
    pub dismissed: bool,
}

impl Progress {
    pub(crate) fn done(&self, step: SetupStep) -> bool {
        match step {
            SetupStep::Device => self.device_named,
            SetupStep::Provider => self.provider_ready,
            SetupStep::Model => self.model_chosen,
            SetupStep::Extras => self.extras_on,
        }
    }

    pub(crate) fn skipped(&self, step: SetupStep) -> bool {
        self.skipped.iter().any(|s| s == step.key())
    }

    /// A step that still asks to be done: not done, not put off, and (Model) possible now.
    /// Provider stops asking once a model was chosen: a provider that breaks later is a
    /// notice in the chat, never a trip back to setup.
    fn open(&self, step: SetupStep) -> bool {
        !self.done(step)
            && !self.skipped(step)
            && (step != SetupStep::Model || self.provider_ready)
            && (step != SetupStep::Provider || !self.model_chosen)
    }

    /// The required step the owner's chat sends them to, if any.
    pub(crate) fn blocking(&self) -> Option<SetupStep> {
        STEPS.into_iter().find(|s| s.required() && self.open(*s))
    }

    /// Where `GET /setup` starts: the first open step, else Extras.
    fn start(&self) -> SetupStep {
        STEPS
            .into_iter()
            .find(|s| self.open(*s))
            .unwrap_or(SetupStep::Extras)
    }

    /// The page after `step`: the next step in order, and the chat after Extras. Steps
    /// already done are still shown (the owner sees and confirms them: Provider is "done" as
    /// soon as any provider works, but the owner picks which one there); only Model is passed
    /// over while no provider works, since it can't be done then.
    fn after(&self, step: SetupStep) -> String {
        if step == SetupStep::Extras {
            return crate::pages::MELISSA_CHAT_URL.into();
        }
        STEPS
            .into_iter()
            .skip(step.index() + 1)
            .find(|s| *s != SetupStep::Model || self.provider_ready)
            .unwrap_or(SetupStep::Extras)
            .href()
    }

    /// The checklist's lines: required steps not done, and Extras once put off and not done.
    pub(crate) fn checklist(&self) -> Vec<SetupStep> {
        if self.dismissed {
            return Vec::new();
        }
        STEPS
            .into_iter()
            .filter(|s| !self.done(*s) && (s.required() || self.skipped(*s)))
            .collect()
    }
}

/// Where the owner's `/` or `/chat` goes instead, if setup isn't through its required steps.
/// Members never see setup; neither does a router built without it (tests).
pub(crate) fn redirect_for(onboarding: bool, owner: bool, p: &Progress) -> Option<String> {
    (onboarding && owner)
        .then(|| p.blocking())
        .flatten()
        .map(SetupStep::href)
}

/// The request came from a setup page (htmx sends the page's URL; a plain form its
/// Referer): the Settings handlers it shares answer for setup then.
pub(crate) fn from_setup(headers: &HeaderMap) -> bool {
    let url = headers
        .get("hx-current-url")
        .or_else(|| headers.get(header::REFERER))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let path = match url.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("/", |i| &rest[i..]),
        None => url,
    };
    let path = path.split(['?', '#']).next().unwrap_or("");
    path == SETUP_URL || path.starts_with("/setup/")
}

pub(crate) fn provider_href(id: &str) -> String {
    format!("{PROVIDER_URL}?p={id}")
}

// --- The state ------------------------------------------------------------------------------

/// The backend's provider (`echo` is Melissa's built-in test partner, never a provider).
fn provider_of(ui: &Ui, backend: &str) -> String {
    ui.settings
        .parts()
        .backends
        .iter()
        .find(|b| b.name == backend)
        .map_or_else(|| backend.to_string(), |b| b.provider.clone())
}

/// Melissa's backends that work now (installed, signed in), without the test partner.
pub(crate) fn usable(ui: &Ui) -> Vec<BackendView> {
    ui.lead
        .backends()
        .into_iter()
        .filter(|b| b.available && provider_of(ui, &b.name) != "echo")
        .collect()
}

/// Whether at least one provider works now (the chat says so when none does).
pub(crate) fn provider_ready(ui: &Ui) -> bool {
    !usable(ui).is_empty()
}

pub(crate) async fn progress(ui: &Ui) -> Progress {
    let prefs = ui.settings.prefs();
    let extras_on = prefs.voice_input || tailscale::is_on(ui).await;
    Progress {
        device_named: prefs.device_name.is_some(),
        provider_ready: provider_ready(ui),
        model_chosen: ui.lead.backend_choice().is_some(),
        extras_on,
        skipped: prefs.setup.skipped,
        dismissed: prefs.setup.dismissed,
    }
}

/// The owner's redirect for `/` and `/chat` now (None: show the chat).
pub(crate) async fn redirect(ui: &Ui) -> Option<String> {
    let owner = crate::auth::signed_in().is_some_and(|p| p.is_owner());
    if !ui.settings.onboarding() || !owner {
        return None;
    }
    redirect_for(true, owner, &progress(ui).await)
}

/// The checklist lines for Melissa's empty chat (the owner's only; empty when nothing is
/// left or it was hidden).
pub(crate) async fn checklist(ui: &Ui) -> Vec<(SetupStep, bool)> {
    let owner = crate::auth::signed_in().is_some_and(|p| p.is_owner());
    if !ui.settings.onboarding() || !owner {
        return Vec::new();
    }
    let p = progress(ui).await;
    p.checklist()
        .into_iter()
        .map(|s| (s, p.skipped(s)))
        .collect()
}

/// A checklist line's words and link.
pub(crate) fn checklist_line(step: SetupStep, later: bool) -> (&'static str, String, &'static str) {
    (
        step.heading(),
        step.href(),
        if later { "Later" } else { "Not done" },
    )
}

// --- Pages ----------------------------------------------------------------------------------

pub(super) fn owner_only() -> Option<Response> {
    match crate::auth::signed_in() {
        Some(p) if p.is_owner() => None,
        _ => Some((StatusCode::FORBIDDEN, "Only the owner can do this.").into_response()),
    }
}

/// Members don't see setup: their chat instead.
fn members_away() -> Option<Response> {
    owner_only().map(|_| Redirect::to(crate::pages::MELISSA_CHAT_URL).into_response())
}

fn step_page(step: SetupStep, content: Markup) -> Markup {
    let names: Vec<&str> = STEPS.iter().map(|s| s.short()).collect();
    setup_page(&names, step.index(), step.heading(), content)
}

fn later_url(step: SetupStep) -> String {
    format!("/ui/setup/{}/later", step.key())
}

async fn start(State(ui): State<Ui>) -> Response {
    if let Some(r) = members_away() {
        return r;
    }
    Redirect::to(&progress(&ui).await.start().href()).into_response()
}

#[derive(Deserialize, Default)]
pub(super) struct StepQuery {
    #[serde(default)]
    p: Option<String>,
    /// Model step: how often the list was asked for again.
    #[serde(default)]
    n: Option<u32>,
}

async fn step(
    State(ui): State<Ui>,
    Path(key): Path<String>,
    Query(q): Query<StepQuery>,
) -> Response {
    if let Some(r) = members_away() {
        return r;
    }
    let Some(step) = SetupStep::from_key(&key) else {
        return (StatusCode::NOT_FOUND, crate::pages::not_found()).into_response();
    };
    let content = match step {
        SetupStep::Device => device_form(&ui, None),
        SetupStep::Provider => {
            let pick = q.p.clone().or_else(|| first_ready(&ui));
            let v = providers::view(&ui, pick.as_deref(), None).await;
            provider_region(&ui, &v)
        }
        SetupStep::Model => model_region(&ui, None, 0, q.p.as_deref()),
        SetupStep::Extras => extras(&ui).await,
    };
    step_page(step, content).into_response()
}

fn device_form(ui: &Ui, error: Option<&str>) -> Markup {
    let value = device::field_value(ui);
    html! {
        @if let Some(e) = error { (notice(e, Tone::Error)) }
        form.stack #setup-device-form method="post" action="/ui/setup/device" {
            (crate::components::csrf_field())
            (text_field("name", "Device name", &value, "", "text",
                Some("Shown in Melissa only.")))
        }
        (setup_actions(setup_submit("setup-device-form", "Save name"),
            Some(&later_url(SetupStep::Device))))
    }
}

/// The provider to show first: one that works, else the first.
fn first_ready(ui: &Ui) -> Option<String> {
    usable(ui).first().map(|b| provider_of(ui, &b.name))
}

/// The Model step's link from the Provider step: with the chosen provider when it works, so
/// the Model step lists its models and starts on its default.
fn model_href(selected: Option<&str>, working: &[String]) -> String {
    match selected {
        Some(p) if working.iter().any(|w| w == p) => format!("{}?p={p}", SetupStep::Model.href()),
        _ => SetupStep::Model.href(),
    }
}

/// The Provider step's region: the providers as tiles with their status, the chosen one's
/// next action (the Settings → Providers forms) and, once one works, "Choose a model".
pub(crate) fn provider_region(ui: &Ui, v: &ProvidersView) -> Markup {
    let hrefs: Vec<String> = v.cards.iter().map(|c| provider_href(&c.id)).collect();
    let letters: Vec<String> = v
        .cards
        .iter()
        .map(|c| {
            c.label
                .chars()
                .next()
                .unwrap_or('?')
                .to_uppercase()
                .to_string()
        })
        .collect();
    let tiles: Vec<ProviderTile<'_>> = v
        .cards
        .iter()
        .enumerate()
        .filter(|(_, c)| c.enabled)
        .map(|(i, c)| ProviderTile {
            id: &c.id,
            label: &c.label,
            letter: &letters[i],
            vendor: "",
            action: None,
            link: Some(TileLink {
                href: &hrefs[i],
                status: &c.status,
                tone: c.tone,
                current: i == v.selected,
            }),
        })
        .collect();
    let ready = provider_ready(ui);
    let selected = v.cards.get(v.selected).filter(|c| c.enabled);
    // One filled button: the chosen provider's own step, else "Choose a model".
    let busy =
        selected.is_some_and(|c| !matches!(c.step, ProviderStep::Nothing | ProviderStep::Note(_)));
    let working: Vec<String> = usable(ui)
        .iter()
        .map(|b| provider_of(ui, &b.name))
        .collect();
    let model_href = model_href(selected.map(|c| c.id.as_str()), &working);
    let footer = setup_actions(
        html! {
            @if ready { (link_button(if busy { Btn::secondary() } else { Btn::primary() }, &model_href, "Choose a model")) }
        },
        Some(&later_url(SetupStep::Provider)),
    );
    pages::setup_provider_region(
        v,
        provider_tiles("Providers", &tiles, pages::PROVIDERS_ID),
        selected,
        footer,
    )
}

/// A model's name without a repeat of its provider ("opencode/big-pickle" on OpenCode reads
/// "big-pickle"); the id sent stays whole.
fn model_name<'a>(backend: &str, label: &'a str) -> &'a str {
    let Some((head, rest)) = label.split_once('/') else {
        return label;
    };
    let base = backend.split('-').next().unwrap_or(backend);
    let same = head.eq_ignore_ascii_case(base) || head.eq_ignore_ascii_case(backend_label(backend));
    if same && !rest.is_empty() {
        rest
    } else {
        label
    }
}

/// The Model step's options (form value, label) for `views`, and the value chosen now
/// (Melissa's current model, else the first backend's default). One default entry per
/// backend: the agent's own `default` model (Claude's "Default (recommended)") is that entry,
/// not a second one. The provider's name leads the label only when several are listed.
fn model_options(views: &[BackendView]) -> (Vec<(String, String)>, Option<String>) {
    let several = views.len() > 1;
    let mut values: Vec<(String, String)> = Vec::new();
    for b in views {
        let name = backend_label(&b.name);
        let labelled = |label: &str| {
            if several {
                format!("{name} · {label}")
            } else {
                label.to_string()
            }
        };
        let models = b.models.iter().flatten();
        let default = models
            .clone()
            .find(|m| m.id == "default" && !m.label.is_empty())
            .map_or("Default model", |m| m.label.as_str());
        values.push((b.name.clone(), labelled(default)));
        for m in models.filter(|m| m.id != "default") {
            let label = if m.label.is_empty() { &m.id } else { &m.label };
            values.push((
                format!("{} {}", b.name, m.id),
                labelled(model_name(&b.name, label)),
            ));
        }
    }
    let current = views
        .iter()
        .find(|b| b.current)
        .or_else(|| views.first())
        .map(|b| match &b.model {
            Some(m) if m != "default" && b.models.iter().flatten().any(|o| &o.id == m) => {
                format!("{} {m}", b.name)
            }
            _ => b.name.clone(),
        });
    (values, current)
}

/// The Model step's region: the models of the provider chosen in the Provider step
/// (`provider`), else of every usable one, Melissa's current one chosen.
fn model_region(ui: &Ui, error: Option<&str>, polls: u32, provider: Option<&str>) -> Markup {
    let mut views = usable(ui);
    // Only a provider that works now narrows the list (and goes into the poll's URL).
    let provider = provider.filter(|p| views.iter().any(|b| provider_of(ui, &b.name) == *p));
    if let Some(p) = provider {
        views.retain(|b| provider_of(ui, &b.name) == p);
    }
    if views.is_empty() {
        return html! {
            (notice_link("No provider is ready yet.",
                Some((PROVIDER_URL, "Connect a provider")), Tone::Warn))
            (setup_actions(html! {}, Some(&later_url(SetupStep::Model))))
        };
    }
    // Learn the models of providers that never had a chat (no prompt; rate-limited).
    ui.lead.refresh_models_soon();
    let (values, current) = model_options(&views);
    let choices: Vec<Choice<'_>> = values
        .iter()
        .map(|(value, label)| Choice {
            value,
            label,
            enabled: true,
        })
        .collect();
    let waiting = views.iter().any(|b| b.models.is_none()) && polls < MODEL_POLLS;
    let poll = match provider {
        Some(p) => format!("/ui/setup/model?n={}&p={p}", polls + 1),
        None => format!("/ui/setup/model?n={}", polls + 1),
    };
    live_region(
        MODEL_REGION_ID,
        &poll,
        3,
        waiting,
        html! {
            @if let Some(e) = error { (notice(e, Tone::Error)) }
            form.stack #setup-model-form method="post" action="/ui/setup/model" {
                (crate::components::csrf_field())
                (select_field_at("choice", "Model", &choices, current.as_deref()))
            }
            @if waiting { p.quiet.small { "Asking the provider for its models…" } }
            (setup_actions(setup_submit("setup-model-form", "Use this model"),
                Some(&later_url(SetupStep::Model))))
        },
    )
}

async fn model_poll(State(ui): State<Ui>, Query(q): Query<StepQuery>) -> Response {
    if let Some(r) = owner_only() {
        return r;
    }
    model_region(&ui, None, q.n.unwrap_or(MODEL_POLLS), q.p.as_deref()).into_response()
}

/// The Extras step. With neither extra possible here it says so in one line, and "Finish
/// setup" is the only button; "Later" (which lists the step in the chat's checklist) shows
/// only while there is something left to turn on.
async fn extras(ui: &Ui) -> Markup {
    let voice = voice::view(&ui.settings, None);
    let finish = link_button(
        Btn::primary(),
        crate::pages::MELISSA_CHAT_URL,
        "Finish setup",
    );
    if !voice.available && !voice.on && !tailscale::managed(ui).await {
        return html! {
            (notice("Voice input and Tailscale aren't available on this install.", Tone::Info))
            (setup_actions(finish, None))
        };
    }
    let ts = tailscale::setup_view(ui).await;
    let later = (!voice.on && !tailscale::is_on(ui).await).then(|| later_url(SetupStep::Extras));
    html! {
        (pages::voice_region(&voice))
        (pages::tailscale_region(&ts))
        (setup_actions(finish, later.as_deref()))
    }
}

// --- Forms ----------------------------------------------------------------------------------

async fn save_device(State(ui): State<Ui>, Form(form): Form<device::NameForm>) -> Response {
    if let Some(r) = owner_only() {
        return r;
    }
    if let Err(e) = device::save_name(&ui, &form.name) {
        let page = step_page(SetupStep::Device, device_form(&ui, Some(e.text())));
        return (e.status(), page).into_response();
    }
    let next = progress(&ui).await.after(SetupStep::Device);
    Redirect::to(&next).into_response()
}

#[derive(Deserialize)]
pub(super) struct ModelForm {
    #[serde(default)]
    choice: String,
}

async fn save_model(State(ui): State<Ui>, Form(form): Form<ModelForm>) -> Response {
    if let Some(r) = owner_only() {
        return r;
    }
    let (backend, model) = match form.choice.trim().split_once(' ') {
        Some((b, m)) => (b.to_string(), Some(m.trim().to_string())),
        None => (form.choice.trim().to_string(), None),
    };
    match ui.lead.use_backend(&backend, model.as_deref(), None) {
        Ok(_) => {
            let next = progress(&ui).await.after(SetupStep::Model);
            Redirect::to(&next).into_response()
        }
        Err(e) => {
            tracing::warn!("setup: choosing {backend}: {e}");
            let page = step_page(
                SetupStep::Model,
                model_region(
                    &ui,
                    Some("That model can't be used now. Choose another one."),
                    MODEL_POLLS,
                    Some(&provider_of(&ui, &backend)),
                ),
            );
            (StatusCode::CONFLICT, page).into_response()
        }
    }
}

async fn later(State(ui): State<Ui>, Path(key): Path<String>) -> Response {
    if let Some(r) = owner_only() {
        return r;
    }
    let Some(step) = SetupStep::from_key(&key) else {
        return (StatusCode::NOT_FOUND, crate::pages::not_found()).into_response();
    };
    let saved = ui.settings.parts().prefs.update(|p| {
        if !p.setup.skipped.iter().any(|s| s == step.key()) {
            p.setup.skipped.push(step.key().to_string());
        }
    });
    if let Err(e) = saved {
        // Not kept: the owner may be asked again after a restart. Go on anyway.
        tracing::error!("saving setup progress: {e}");
    }
    let next = progress(&ui).await.after(step);
    Redirect::to(&next).into_response()
}

async fn dismiss(State(ui): State<Ui>, headers: HeaderMap) -> Response {
    if let Some(r) = owner_only() {
        return r;
    }
    if let Err(e) = ui
        .settings
        .parts()
        .prefs
        .update(|p| p.setup.dismissed = true)
    {
        tracing::error!("hiding the setup checklist: {e}");
    }
    if crate::is_htmx(&headers) {
        // The card goes away.
        return html! {}.into_response();
    }
    Redirect::to(crate::pages::MELISSA_CHAT_URL).into_response()
}

pub(super) fn routes() -> Router<Ui> {
    Router::new()
        .route(SETUP_URL, get(start))
        .route("/setup/{step}", get(step))
        .route("/ui/setup/device", post(save_device))
        .route("/ui/setup/model", get(model_poll).post(save_model))
        .route("/ui/setup/{step}/later", post(later))
        .route(DISMISS_URL, post(dismiss))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> Progress {
        Progress::default()
    }

    #[test]
    fn steps_are_done_from_the_real_state() {
        let mut p = p();
        assert_eq!(p.blocking(), Some(SetupStep::Device));
        assert_eq!(p.start(), SetupStep::Device);
        p.device_named = true;
        assert_eq!(p.blocking(), Some(SetupStep::Provider));
        p.provider_ready = true;
        assert_eq!(p.blocking(), Some(SetupStep::Model));
        p.model_chosen = true;
        assert_eq!(p.blocking(), None);
        // Extras never blocks the chat; it is where setup ends.
        assert_eq!(p.start(), SetupStep::Extras);
        assert!(p.checklist().is_empty());
        // Every provider breaks later: a notice in the chat, not a trip back to setup.
        p.provider_ready = false;
        assert_eq!(p.blocking(), None);
    }

    #[test]
    fn later_lets_the_chat_open_and_lists_the_step() {
        let mut p = p();
        p.skipped = vec!["device".into(), "provider".into()];
        // Model can't be done without a provider: it waits instead of blocking.
        assert_eq!(p.blocking(), None);
        assert_eq!(p.start(), SetupStep::Extras);
        assert_eq!(
            p.checklist(),
            vec![SetupStep::Device, SetupStep::Provider, SetupStep::Model]
        );
        // A provider appears: Model asks to be done again.
        p.provider_ready = true;
        assert_eq!(p.blocking(), Some(SetupStep::Model));
        // Done wins over Later.
        p.device_named = true;
        p.model_chosen = true;
        assert_eq!(p.blocking(), None);
        assert!(p.checklist().is_empty());
        // Extras shows only once put off, until it is done.
        p.skipped.push("extras".into());
        assert_eq!(p.checklist(), vec![SetupStep::Extras]);
        p.extras_on = true;
        assert!(p.checklist().is_empty());
    }

    #[test]
    fn a_hidden_checklist_stays_hidden() {
        let mut p = p();
        p.skipped = vec!["device".into()];
        p.dismissed = true;
        assert!(p.checklist().is_empty());
    }

    #[test]
    fn the_steps_go_in_order_and_model_waits_for_a_provider() {
        let mut p = p();
        // A provider already works (signed in on this machine): Provider still comes next,
        // where the owner picks which one.
        p.provider_ready = true;
        assert_eq!(p.after(SetupStep::Device), "/setup/provider");
        assert_eq!(p.after(SetupStep::Provider), "/setup/model");
        assert_eq!(p.after(SetupStep::Model), "/setup/extras");
        p.model_chosen = true;
        assert_eq!(p.after(SetupStep::Device), "/setup/provider");
        assert_eq!(p.after(SetupStep::Extras), "/chat");
        // No provider works: Model can't be done, so "Later" on Provider goes to Extras.
        let p = Progress {
            device_named: true,
            ..Progress::default()
        };
        assert_eq!(p.after(SetupStep::Device), "/setup/provider");
        assert_eq!(p.after(SetupStep::Provider), "/setup/extras");
    }

    #[test]
    fn choose_a_model_carries_the_chosen_provider() {
        let working = ["claude".to_string(), "grok".to_string()];
        assert_eq!(
            model_href(Some("claude"), &working),
            "/setup/model?p=claude"
        );
        // Chosen but not working yet, or nothing chosen: every working provider's models.
        assert_eq!(model_href(Some("gemini"), &working), "/setup/model");
        assert_eq!(model_href(None, &working), "/setup/model");
    }

    fn backend(name: &str, current: bool, models: &[(&str, &str)]) -> BackendView {
        BackendView {
            name: name.into(),
            available: true,
            reason: None,
            current,
            model: None,
            effort: None,
            fast: false,
            models: Some(
                models
                    .iter()
                    .map(|(id, label)| melissa_agent::session::ModelOption::new(*id, *label))
                    .collect(),
            ),
        }
    }

    #[test]
    fn one_default_per_provider_and_no_repeated_prefix() {
        let claude = backend(
            "claude-session",
            false,
            &[("default", "Default (recommended)"), ("sonnet", "Sonnet")],
        );
        let (values, current) = model_options(std::slice::from_ref(&claude));
        assert_eq!(
            values,
            [
                (
                    "claude-session".to_string(),
                    "Default (recommended)".to_string()
                ),
                ("claude-session sonnet".into(), "Sonnet".into()),
            ]
        );
        assert_eq!(current.as_deref(), Some("claude-session"));

        let opencode = backend(
            "opencode-acp",
            true,
            &[("opencode/big-pickle", ""), ("anthropic/x", "Anthropic/X")],
        );
        let (values, current) = model_options(&[claude, opencode]);
        assert_eq!(
            values,
            [
                (
                    "claude-session".to_string(),
                    "Claude · Default (recommended)".to_string()
                ),
                ("claude-session sonnet".into(), "Claude · Sonnet".into()),
                ("opencode-acp".into(), "OpenCode · Default model".into()),
                // The id stays whole; only the label loses the provider's own name.
                (
                    "opencode-acp opencode/big-pickle".into(),
                    "OpenCode · big-pickle".into()
                ),
                (
                    "opencode-acp anthropic/x".into(),
                    "OpenCode · Anthropic/X".into()
                ),
            ]
        );
        // Melissa's current backend is chosen.
        assert_eq!(current.as_deref(), Some("opencode-acp"));
    }

    #[test]
    fn the_current_model_is_preselected_and_default_means_the_default_entry() {
        let mut claude = backend(
            "claude-session",
            true,
            &[("default", "Default (recommended)"), ("sonnet", "Sonnet")],
        );
        claude.model = Some("sonnet".into());
        let (_, current) = model_options(std::slice::from_ref(&claude));
        assert_eq!(current.as_deref(), Some("claude-session sonnet"));
        claude.model = Some("default".into());
        let (_, current) = model_options(&[claude]);
        assert_eq!(current.as_deref(), Some("claude-session"));
    }

    #[test]
    fn only_the_owner_is_sent_to_setup() {
        let p = p();
        assert_eq!(
            redirect_for(true, true, &p).as_deref(),
            Some("/setup/device")
        );
        assert_eq!(redirect_for(true, false, &p), None);
        assert_eq!(redirect_for(false, true, &p), None);
        let skipped = Progress {
            skipped: vec!["device".into(), "provider".into(), "model".into()],
            ..Progress::default()
        };
        assert_eq!(redirect_for(true, true, &skipped), None);
    }

    #[test]
    fn requests_from_setup_pages_are_recognised() {
        let h = |name: &'static str, v: &'static str| {
            let mut m = HeaderMap::new();
            m.insert(name, v.parse().unwrap());
            m
        };
        assert!(from_setup(&h(
            "hx-current-url",
            "http://box.example.com/setup/provider?p=claude"
        )));
        assert!(from_setup(&h("referer", "http://box.example.com/setup")));
        assert!(!from_setup(&h(
            "hx-current-url",
            "http://box.example.com/settings/providers"
        )));
        assert!(!from_setup(&h("referer", "http://box.example.com/setupx")));
        assert!(!from_setup(&HeaderMap::new()));
    }
}
