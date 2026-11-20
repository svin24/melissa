//! Components of Settings → Providers → Add provider (2026-11-20, owner-requested): an
//! in-page panel with three steps (Provider, Identity, Config), a grid of the providers
//! Melissa runs itself, the ACP registry list with its search, and a link styled as a button.
//! Part of the fixed component library (`components.rs`); a separate file only to keep the
//! diff apart from other work there.

use maud::{html, Markup};

use super::{badge, provider_logo, Tone};

/// A link that looks like a button (`primary`: the main one).
pub fn link_button(href: &str, label: &str, primary: bool) -> Markup {
    html! {
        a class={ "button-like " @if primary { "primary" } @else { "secondary" } } href=(href) {
            (label)
        }
    }
}

/// The step list at the top of the panel; `current` counts from 0.
pub fn stepper(steps: &[&str], current: usize) -> Markup {
    html! {
        ol.stepper aria-label="Steps" {
            @for (i, s) in steps.iter().enumerate() {
                li class=[(i < current).then_some("done")]
                    aria-current=[(i == current).then_some("step")] {
                    span.step-n aria-hidden="true" { (i + 1) }
                    span.step-name { (s) }
                }
            }
        }
    }
}

/// The dialog-like panel: a heading with a Close link (back to `close`), the steps, the
/// content. A page of its own (no script needed); `id` lets forms inside swap it.
pub fn add_panel(id: &str, title: &str, close: &str, steps: Markup, content: Markup) -> Markup {
    html! {
        section.add-panel id=(id) aria-labelledby={ (id) "-h" } {
            header.add-panel-head {
                h1 id={ (id) "-h" } { (title) }
                a.panel-close href=(close) aria-label="Close" title="Close" {
                    svg viewBox="0 0 16 16" aria-hidden="true" focusable="false" {
                        path d="M4 4l8 8M12 4l-8 8" {}
                    }
                }
            }
            (steps)
            div.add-panel-body { (content) }
        }
    }
}

/// A provider Melissa runs itself, in the first step's grid.
pub struct ProviderTile<'a> {
    /// Provider id (picks the logo).
    pub id: &'a str,
    pub label: &'a str,
    pub letter: &'a str,
    /// Who makes it ("Anthropic").
    pub vendor: &'a str,
    /// Where choosing it posts; None: it is added already.
    pub action: Option<&'a str>,
}

/// The grid of [`ProviderTile`]s. Choosing one posts its form (htmx swaps `#target`).
pub fn provider_tiles(label: &str, tiles: &[ProviderTile<'_>], target: &str) -> Markup {
    html! {
        ul.ptiles aria-label=(label) {
            @for t in tiles {
                li {
                    @if let Some(action) = t.action {
                        form method="post" action=(action) hx-post=(action)
                            hx-target={ "#" (target) } hx-swap="outerHTML" {
                            button.ptile type="submit" {
                                (provider_logo(t.id, t.letter, false))
                                span.ptile-name { (t.label) }
                                span.ptile-note { (t.vendor) }
                            }
                        }
                    } @else {
                        div.ptile.added {
                            (provider_logo(t.id, t.letter, true))
                            span.ptile-name { (t.label) }
                            span.ptile-note { (badge("Added", Tone::Muted)) }
                        }
                    }
                }
            }
        }
    }
}

/// What a registry row offers.
pub enum RegistryAction<'a> {
    /// Melissa can install it: a POST.
    Add { action: &'a str },
    /// Melissa can't install it: why (short), and the manual form prefilled.
    Manual { why: &'a str, href: &'a str },
    /// Installing; `what` is the step ("Downloading…").
    Working { what: &'a str },
    /// Installed and saved: continue to its sign-in step.
    Added { href: &'a str },
    /// The install failed: why, and a POST to try again.
    Failed { why: &'a str, action: &'a str },
}

/// One agent in the ACP registry list. Text comes from the registry (cleaned, escaped here).
pub struct RegistryRow<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub letter: &'a str,
    pub version: &'a str,
    pub description: &'a str,
    /// Its website (https only; opens in a new tab, no referrer).
    pub link: Option<&'a str>,
    pub action: RegistryAction<'a>,
}

/// The registry list. Remote icons are never loaded: each row has Melissa's own mark.
pub fn registry_list(rows: &[RegistryRow<'_>], target: &str, empty: &str) -> Markup {
    html! {
        @if rows.is_empty() {
            p.quiet { (empty) }
        } @else {
            ul.reg-list {
                @for r in rows {
                    li.reg-row {
                        (provider_logo(r.id, r.letter, false))
                        div.reg-text {
                            p.reg-name {
                                span { (r.name) }
                                @if !r.version.is_empty() { " " span.quiet.small { (r.version) } }
                            }
                            @if !r.description.is_empty() { p.reg-desc { (r.description) } }
                            @if let Some(href) = r.link.filter(|h| h.starts_with("https://")) {
                                a.small href=(href) target="_blank" rel="noopener noreferrer" {
                                    "Website" span.visually-hidden { " of " (r.name) " (new tab)" }
                                }
                            }
                        }
                        div.reg-act {
                            @match &r.action {
                                RegistryAction::Add { action } => {
                                    form method="post" action=(action) hx-post=(action)
                                        hx-target={ "#" (target) } hx-swap="outerHTML" {
                                        button.secondary type="submit" {
                                            "Add" span.visually-hidden { " " (r.name) }
                                        }
                                    }
                                }
                                RegistryAction::Manual { why, href } => {
                                    span.quiet.small { (why) }
                                    a.small href=(href) { "Enter manually" }
                                }
                                RegistryAction::Working { what } => {
                                    span.quiet role="status" { (what) }
                                }
                                RegistryAction::Added { href } => {
                                    (badge("Added", Tone::Ok))
                                    a.small href=(href) { "Continue" }
                                }
                                RegistryAction::Failed { why, action } => {
                                    span.reg-error role="alert" { (why) }
                                    form method="post" action=(action) hx-post=(action)
                                        hx-target={ "#" (target) } hx-swap="outerHTML" {
                                        button.secondary type="submit" { "Try again" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A search field that filters a list as you type (htmx GET of `live_url` into `#target`)
/// and, without script, submits to `page_url` like any form.
pub fn search_box(
    page_url: &str,
    live_url: &str,
    target: &str,
    label: &str,
    value: &str,
) -> Markup {
    html! {
        form.search-box method="get" action=(page_url) role="search" {
            label.visually-hidden for="registry-search" { (label) }
            input #registry-search type="search" name="q" value=(value) placeholder=(label)
                autocomplete="off" spellcheck="false" maxlength="100"
                hx-get=(live_url) hx-trigger="input changed delay:300ms, search"
                hx-target={ "#" (target) } hx-swap="outerHTML";
            button.secondary type="submit" { "Search" }
        }
    }
}
