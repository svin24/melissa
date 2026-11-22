//! Apps (2026-11-22, owner design "Apps" A1–A6): the list of apps grouped by kind, with a
//! filter by person for the owner, and an app's details in the same panel.
//!
//! Part of the fixed component library (`components.rs`); a separate file to keep it together.
//! The list is one live region (`#cluster-apps`) that refreshes itself; the filter's links
//! swap that region in place with htmx and are plain links without script. Details are a
//! panel page of their own (`/cluster/apps/{app}`), so melissa.js opens them in the same modal
//! and Back returns to the list.

use maud::{html, Markup};

use super::{
    agent::{avatar, icon_picker, Avatar},
    glyph, live_region_on, log_view, status_badge, Btn, Glyph, Health, LogLine, Tone,
};

/// The list's region id (its refresh and the filter swap it).
pub const APPS_LIST_ID: &str = "cluster-apps";
/// An app's details region (actions swap it).
pub const APP_DETAILS_ID: &str = "app-details";

// --- Small parts ------------------------------------------------------------------------------

/// An app's icon on its tile. `large`: the details' head.
pub fn app_icon(shape: &str, color: &str, large: bool) -> Markup {
    html! {
        span class={ "app-icon" @if large { " app-icon-lg" } } {
            (avatar(&Avatar::Agent { shape, color }, if large { "av" } else { "av-sm" }))
        }
    }
}

/// An app's status: a badge with a dot (a spinner while busy) and the word.
pub fn app_status(health: Health, words: &str) -> Markup {
    let (tone, mark) = health.tone_mark();
    status_badge(words, tone, mark)
}

/// Whose an app is, as a chip ("Installed by" for screen readers).
fn person_chip(name: &str) -> Markup {
    html! {
        span.person-chip {
            (glyph(Glyph::Person))
            span.visually-hidden { "Installed by " }
            (name)
        }
    }
}

// --- The list -----------------------------------------------------------------------------------

/// One app in the list.
pub struct AppRow<'a> {
    pub name: &'a str,
    pub shape: &'a str,
    pub color: &'a str,
    /// Where to reach it (`box:8096 (HTTP)`), if known.
    pub address: Option<&'a str>,
    /// The address as a link (web apps; http/https only, the caller checks).
    pub link: Option<&'a str>,
    /// Whose it is (shown to the owner only).
    pub person: Option<&'a str>,
    pub health: Health,
    pub status: &'a str,
    /// The details page.
    pub details: &'a str,
}

/// One group of the list ("Games").
pub struct AppGroup<'a> {
    pub title: &'a str,
    pub rows: Vec<AppRow<'a>>,
}

/// One choice of the owner's filter.
pub struct ShowChoice<'a> {
    pub label: &'a str,
    /// The `?by=` value.
    pub key: &'a str,
    pub current: bool,
}

/// What the list shows when it has no rows.
pub struct AppsEmpty<'a> {
    pub title: Option<&'a str>,
    pub text: &'a str,
}

/// The Apps list.
pub struct AppList<'a> {
    /// The owner's filter (None: a member, who sees only their own apps).
    pub filter: Option<&'a [ShowChoice<'a>]>,
    /// Number of apps shown.
    pub count: usize,
    /// "Add an app": into the chat.
    pub add: &'a str,
    pub groups: &'a [AppGroup<'a>],
    /// No rows: why (None: there are rows).
    pub empty: Option<AppsEmpty<'a>>,
    /// The region's own URL (with the filter), for its refresh.
    pub url: &'a str,
}

fn app_row(r: &AppRow<'_>) -> Markup {
    html! {
        li.app-row {
            (app_icon(r.shape, r.color, false))
            div.app-row-main {
                span.app-row-name { (r.name) }
                @if let Some(address) = r.address {
                    @if let Some(link) = r.link {
                        a.app-row-address href=(link) rel="noreferrer" target="_blank" { (address) }
                    } @else {
                        span.app-row-address { (address) }
                    }
                }
            }
            @if let Some(p) = r.person {
                span.app-row-person { (person_chip(p)) }
            }
            span.app-row-status { (app_status(r.health, r.status)) }
            a.app-row-details.(Btn::secondary()) href=(r.details) {
                "Details" span.visually-hidden { ": " (r.name) }
            }
        }
    }
}

/// The owner's filter: links that swap the list in place (plain links without script).
fn show_filter(choices: &[ShowChoice<'_>]) -> Markup {
    html! {
        nav.show-filter aria-labelledby="apps-show-label" {
            span #apps-show-label { "Show" }
            ul.segmented {
                @for c in choices {
                    li {
                        a.seg id={ "apps-by-" (c.key) } href={ "/cluster?by=" (c.key) }
                            hx-get={ "/ui/cluster/apps?by=" (c.key) }
                            hx-target={ "#" (APPS_LIST_ID) } hx-swap="outerHTML"
                            aria-current=[c.current.then_some("true")] {
                            (c.label)
                        }
                    }
                }
            }
        }
    }
}

fn add_button(href: &str) -> Markup {
    html! {
        a.(Btn::primary()) href=(href) { (glyph(Glyph::Plus)) "Add an app" }
    }
}

/// The list's content (inside its live region).
pub fn app_list(l: &AppList<'_>) -> Markup {
    let content = html! {
        @if let Some(empty) = &l.empty {
            @if let Some(filter) = l.filter.filter(|f| f.len() > 1) {
                div.apps-bar { (show_filter(filter)) }
            }
            div.apps-empty {
                span.app-icon.app-icon-lg { (glyph(Glyph::Hexagon)) }
                @if let Some(title) = empty.title { p.apps-empty-title { (title) } }
                p { (empty.text) }
                (add_button(l.add))
            }
        } @else {
            div.apps-bar {
                @match l.filter {
                    Some(filter) => (show_filter(filter)),
                    None => p.apps-mine { "Your apps" },
                }
                div.apps-bar-end {
                    span.apps-count role="status" {
                        (l.count) @if l.count == 1 { " app" } @else { " apps" }
                    }
                    (add_button(l.add))
                }
            }
            @for (n, g) in l.groups.iter().enumerate() {
                section.app-group aria-labelledby={ "apps-g" (n) } {
                    h2.app-group-head {
                        span id={ "apps-g" (n) } { (g.title) }
                        span.count { (g.rows.len()) span.visually-hidden { " apps" } }
                    }
                    ul.app-rows {
                        @for r in &g.rows { (app_row(r)) }
                    }
                }
            }
        }
    };
    // Also refreshed at once after a change in an app's details (`cluster-changed`).
    live_region_on(APPS_LIST_ID, l.url, 15, "cluster-changed", content)
}

// --- Details ------------------------------------------------------------------------------------

/// One fact of an app.
pub struct AppFact<'a> {
    pub label: &'a str,
    /// One or more lines (e.g. every address).
    pub lines: Vec<&'a str>,
}

/// The tool asked for the user's OK before stopping (the in-page confirmation).
pub struct AppAsk<'a> {
    pub question: &'a str,
    pub detail: &'a str,
    pub label: &'a str,
    pub approve: &'a str,
    pub deny: &'a str,
}

/// An app's details (owner design A5).
pub struct AppDetailsView<'a> {
    /// The app's name (`minecraft`): URLs and the delete confirmation.
    pub app: &'a str,
    /// Its display name.
    pub name: &'a str,
    pub shape: &'a str,
    pub color: &'a str,
    pub health: Health,
    pub status: &'a str,
    /// Whose it is.
    pub person: Option<&'a str>,
    /// Open: its web link.
    pub open: Option<&'a str>,
    /// Stop or Start: (label, POST url).
    pub toggle: Option<(&'a str, &'a str)>,
    pub ask: Option<AppAsk<'a>>,
    pub message: Option<(&'a str, Tone)>,
    pub facts: &'a [AppFact<'a>],
    /// The icon picker: shapes, colours and where to post (None: may not change it).
    pub icon: Option<(&'a [&'a str], &'a [&'a str], &'a str)>,
    pub logs_url: &'a str,
    pub delete_url: Option<&'a str>,
    /// Deleted: the details are gone, only the message is left.
    pub gone: bool,
}

/// The details' region (actions swap it whole).
pub fn app_details_region(d: &AppDetailsView<'_>) -> Markup {
    html! {
        div.app-details id=(APP_DETAILS_ID) {
            div.app-details-main {
                header.app-head {
                    span #app-head-icon { (app_icon(d.shape, d.color, true)) }
                    div.app-head-text {
                        h2 { (d.name) }
                        @if !d.gone {
                            div.app-head-tags {
                                (app_status(d.health, d.status))
                                @if let Some(p) = d.person { (person_chip(p)) }
                            }
                        }
                    }
                    @if !d.gone {
                        div.app-head-actions {
                            @if let Some(open) = d.open {
                                a.(Btn::secondary()) href=(open) rel="noreferrer" target="_blank" {
                                    (glyph(Glyph::Expand)) "Open"
                                }
                            }
                            @if let Some((label, url)) = d.toggle {
                                @if d.ask.is_none() {
                                    button.(Btn::secondary()) type="button" hx-post=(url)
                                        hx-target={ "#" (APP_DETAILS_ID) } hx-swap="outerHTML"
                                        hx-disabled-elt="this" { (label) }
                                }
                            }
                        }
                    }
                }
                @if let Some((text, tone)) = d.message {
                    p class={ "notice " (tone.class()) } role="status" { (text) }
                }
                @if let Some(a) = &d.ask {
                    section.app-ask aria-labelledby="app-ask-q" {
                        div {
                            p #app-ask-q .app-ask-q { (a.question) }
                            p.app-ask-detail { (a.detail) }
                        }
                        div.app-ask-actions {
                            button.(Btn::secondary()) type="button" hx-post=(a.deny)
                                hx-target={ "#" (APP_DETAILS_ID) } hx-swap="outerHTML" autofocus {
                                "Cancel"
                            }
                            button.(Btn::primary()) type="button" hx-post=(a.approve)
                                hx-target={ "#" (APP_DETAILS_ID) } hx-swap="outerHTML"
                                hx-disabled-elt="this" { (a.label) }
                        }
                    }
                }
                @if !d.gone {
                    section.app-card aria-labelledby="app-facts-h" {
                        h3 #app-facts-h { "Facts" }
                        dl.app-facts {
                            @for f in d.facts {
                                div {
                                    dt { (f.label) }
                                    dd { @for line in &f.lines { span { (line) } } }
                                }
                            }
                        }
                    }
                    @if let Some((shapes, colors, url)) = d.icon {
                        (app_icon_card(shapes, colors, d.shape, d.color, url, None))
                    }
                }
            }
            @if !d.gone {
                div.app-details-side {
                    section.app-card aria-labelledby="app-logs-h" {
                        h3 #app-logs-h { "Logs" }
                        div #app-logs hx-get=(d.logs_url) hx-trigger="load" hx-swap="outerHTML" {
                            p.quiet { "Loading…" }
                        }
                    }
                    @if let Some(url) = d.delete_url {
                        section.app-card.app-danger aria-labelledby="app-delete-h" {
                            h3 #app-delete-h { "Force delete" }
                            form hx-post=(url) hx-target={ "#" (APP_DETAILS_ID) } hx-swap="outerHTML"
                                hx-disabled-elt="find button" {
                                p { "Removes the app and its data now. This can't be undone." }
                                label.app-field for="app-delete-confirm" {
                                    "Type " strong { (d.app) } " to confirm"
                                }
                                div.app-inline {
                                    input.input #app-delete-confirm type="text" name="confirm" required
                                        autocomplete="off" autocapitalize="off" spellcheck="false"
                                        pattern=(d.app) placeholder=(d.app);
                                    button.(Btn::danger()) type="submit" { "Force delete" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The Icon card: the helpers' shapes and colours; saves on change (a Save button without
/// script). `saved`: a short note after saving.
pub fn app_icon_card(
    shapes: &[&str],
    colors: &[&str],
    shape: &str,
    color: &str,
    url: &str,
    saved: Option<&str>,
) -> Markup {
    html! {
        section.app-card #app-icon-card aria-labelledby="app-icon-h" {
            h3 #app-icon-h { "Icon" }
            form method="post" action=(url) hx-post=(url) hx-trigger="change"
                hx-target="#app-icon-card" hx-swap="outerHTML" {
                (super::csrf_field())
                (icon_picker(shapes, colors, shape, color))
                button.(Btn::secondary()) type="submit" data-no-script-only { "Save icon" }
                @if let Some(s) = saved { p.quiet role="status" { (s) } }
            }
        }
    }
}

/// The head's icon, for an out-of-band swap after the icon changed.
pub fn app_head_icon_oob(shape: &str, color: &str) -> Markup {
    html! {
        span #app-head-icon hx-swap-oob="outerHTML" { (app_icon(shape, color, true)) }
    }
}

/// The details as a panel: a breadcrumb back to Apps, the close control and the region.
pub fn app_details_panel(name: &str, region: Markup) -> Markup {
    html! {
        div.panel.panel-plain data-panel data-title=(name) {
            header.panel-head {
                nav.crumbs.app-crumbs aria-label="Breadcrumb" {
                    a.crumb-back href="/cluster" title="Back to Apps" {
                        (glyph(Glyph::Chevron))
                        span.visually-hidden { "Back to Apps" }
                    }
                    ol {
                        li { a href="/cluster" { "Apps" } }
                        li { span aria-current="page" { (name) } }
                    }
                }
                (super::panel::panel_close_link(name))
            }
            div.panel-body { (region) }
        }
    }
}

/// The logs region of an app's details. Log text is untrusted: it is only ever escaped text.
pub struct AppLogsView<'a> {
    pub url: &'a str,
    /// The containers to choose from (shown when there are several) and the current one.
    pub containers: &'a [String],
    pub container: Option<&'a str>,
    pub lines: &'a [&'a str],
    /// Shown instead of lines (no pods, an error).
    pub empty: Option<&'a str>,
}

pub fn app_logs_region(l: &AppLogsView<'_>) -> Markup {
    html! {
        div #app-logs {
            form.app-logs-bar method="get" action=(l.url) hx-get=(l.url) hx-target="#app-logs"
                hx-swap="outerHTML" {
                @if l.containers.len() > 1 {
                    label.app-field for="app-logs-container" { "Container" }
                    div.app-inline {
                        span.select-wrap {
                            select.input #app-logs-container name="container" hx-get=(l.url)
                                hx-trigger="change" hx-target="#app-logs" hx-swap="outerHTML"
                                hx-include="this" {
                                @for c in l.containers {
                                    option value=(c) selected[l.container == Some(c.as_str())] { (c) }
                                }
                            }
                        }
                        button.(Btn::secondary()) type="submit" { (glyph(Glyph::Refresh)) "Refresh" }
                    }
                } @else {
                    @if let Some(c) = l.container {
                        input type="hidden" name="container" value=(c);
                    }
                    button.(Btn::secondary()) type="submit" { (glyph(Glyph::Refresh)) "Refresh" }
                }
            }
            @if let Some(text) = l.empty {
                p.quiet { (text) }
            } @else {
                div.app-log tabindex="0" aria-label="Log lines" {
                    (log_view(l.lines.iter().map(|t| LogLine { text: t, is_error: false })))
                }
            }
        }
    }
}
