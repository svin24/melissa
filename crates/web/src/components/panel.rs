//! Panels (2026-11-20, owner-requested): Settings, Apps and Usage and resources open as a large
//! modal over the current page, from icon buttons at the bottom of the rail. The same markup is
//! the full page without script (and the deep link): `melissa.js` moves a page's panel into the
//! `#overlay` dialog, and loads other panels into it with htmx (`HX-Target: overlay-body`, see
//! `crate::panel`). Also the Settings row pattern: cards made of rows, each a title and a short
//! description on the left and its control on the right.
//!
//! Part of the fixed component library (`components.rs`); a separate file to keep it together.
//! The layout idea (a settings dialog with a searchable, grouped section list and a breadcrumb
//! header) follows the owner's reference screenshot; the code is our own.

use maud::{html, Markup};

use super::{badge, crumbs, Nav, Tone};

/// DOM id of the modal that holds a panel.
pub const OVERLAY_ID: &str = "overlay";
/// DOM id of the element a panel is loaded into (also the `HX-Target` that asks for a panel).
pub const OVERLAY_BODY_ID: &str = "overlay-body";

// --- Icons (16-unit viewBox, 1.5 stroke, `svg.glyph`) -------------------------------------------

/// A line icon of the panels and the rail's bottom bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    Gear,
    Hexagon,
    Chart,
    Refresh,
    Search,
    Close,
    Person,
    Plug,
    Key,
    Clock,
    File,
    Mic,
    Code,
    // The agent workspace (2026-11-20).
    More,
    Pencil,
    Shapes,
    Pin,
    Restart,
    History,
    Trash,
    Plus,
    Sidebar,
    Chevron,
    Warn,
    Check,
    Server,
    Expand,
    Image,
}

pub fn glyph(g: Glyph) -> Markup {
    html! {
        svg.glyph viewBox="0 0 16 16" aria-hidden="true" focusable="false" {
            @match g {
                Glyph::Gear => {
                    path d="M6.88 3.13 7.06 1.32h1.88l.18 1.81a5 5 0 0 1 1.53.63l1.41-1.15 1.33 1.33-1.15 1.41a5 5 0 0 1 .63 1.53l1.81.18v1.88l-1.81.18a5 5 0 0 1-.63 1.53l1.15 1.41-1.33 1.33-1.41-1.15a5 5 0 0 1-1.53.63l-.18 1.81H7.06l-.18-1.81a5 5 0 0 1-1.53-.63l-1.41 1.15-1.33-1.33 1.15-1.41a5 5 0 0 1-.63-1.53l-1.81-.18V7.06l1.81-.18a5 5 0 0 1 .63-1.53L2.61 3.94l1.33-1.33 1.41 1.15a5 5 0 0 1 1.53-.63Z" {}
                    circle cx="8" cy="8" r="2" {}
                }
                Glyph::Hexagon => {
                    path d="M8 1.5 13.63 4.75v6.5L8 14.5l-5.63-3.25v-6.5Z" {}
                }
                Glyph::Chart => { path d="M3.5 13.5h9M4.5 11V8.5M8 11V3.5M11.5 11V6.5" {} }
                Glyph::Refresh => {
                    path d="M13.5 8a5.5 5.5 0 0 1-9.6 3.7M2.5 8a5.5 5.5 0 0 1 9.6-3.7M12.5 1.8v2.6H9.9M3.5 14.2v-2.6h2.6" {}
                }
                Glyph::Search => {
                    circle cx="7" cy="7" r="4.25" {}
                    path d="m10.2 10.2 3.3 3.3" {}
                }
                Glyph::Close => { path d="m4 4 8 8M12 4l-8 8" {} }
                Glyph::Person => {
                    circle cx="8" cy="5.5" r="2.75" {}
                    path d="M2.75 14a5.25 5.25 0 0 1 10.5 0" {}
                }
                Glyph::Plug => { path d="M6 1.75v3M10 1.75v3M4 4.75h8v2.5a4 4 0 0 1-8 0ZM8 11.25v3" {} }
                Glyph::Key => {
                    circle cx="5.25" cy="10.75" r="3" {}
                    path d="m7.4 8.6 6.1-6.1M11.5 4.5l1.75 1.75M9.75 6.25l1.5 1.5" {}
                }
                Glyph::Clock => {
                    circle cx="8" cy="8" r="6.25" {}
                    path d="M8 4.5V8l2.5 1.5" {}
                }
                Glyph::File => { path d="M4 1.5h5l3.5 3.5v9.5h-8.5ZM9 1.5V5h3.5" {} }
                Glyph::Mic => {
                    rect x="5.75" y="1.75" width="4.5" height="8" rx="2.25" {}
                    path d="M3.5 7.5a4.5 4.5 0 0 0 9 0M8 12v2.25" {}
                }
                Glyph::Code => { path d="M5.5 4.5 2 8l3.5 3.5M10.5 4.5 14 8l-3.5 3.5" {} }
                Glyph::More => {
                    path.dots d="M3.5 8h.01M8 8h.01M12.5 8h.01" {}
                }
                Glyph::Pencil => { path d="M10.5 2.75 13.25 5.5 5.5 13.25H2.75V10.5ZM9 4.25 11.75 7" {} }
                Glyph::Shapes => {
                    circle cx="5" cy="5" r="2.75" {}
                    rect x="8.75" y="8.75" width="5" height="5" rx="1" {}
                    path d="M11.25 1.75 14 6.25H8.5Z" {}
                }
                Glyph::Pin => { path d="M6 1.75h4M7 1.75v4L4.5 9h7L9 5.75v-4M8 9v5.25" {} }
                Glyph::Restart => { path d="M2.75 8a5.25 5.25 0 1 0 1.6-3.8M2.5 1.75v2.75h2.75" {} }
                Glyph::History => {
                    path d="M2.75 8a5.25 5.25 0 1 0 1.6-3.8M2.5 1.75v2.75h2.75M8 5v3l2 1.25" {}
                }
                Glyph::Trash => { path d="M2.75 4.25h10.5M6.25 4.25V2.5h3.5v1.75M4 4.25l.75 9.25h6.5l.75-9.25" {} }
                Glyph::Plus => { path d="M8 3v10M3 8h10" {} }
                Glyph::Sidebar => {
                    rect x="1.75" y="2.75" width="12.5" height="10.5" rx="1.5" {}
                    path d="M6 2.75v10.5" {}
                }
                Glyph::Chevron => { path.chev d="m4.5 6.25 3.5 3.5 3.5-3.5" {} }
                Glyph::Warn => {
                    path d="M8 1.75 13.25 3.75v4c0 3.25-2.25 5.5-5.25 6.5-3-1-5.25-3.25-5.25-6.5v-4Z" {}
                    path d="M8 5.25v3.25M8 10.75v.01" {}
                }
                Glyph::Check => { path d="M3.25 8.5 6.5 11.75l6.25-7.5" {} }
                Glyph::Server => {
                    rect x="2.5" y="2.5" width="11" height="4.5" rx="1.25" {}
                    rect x="2.5" y="9" width="11" height="4.5" rx="1.25" {}
                    path d="M5 4.75h.01M5 11.25h.01" {}
                }
                Glyph::Expand => { path d="M9.5 2.5h4v4M6.5 13.5h-4v-4M13.5 2.5 9.25 6.75M2.5 13.5l4.25-4.25" {} }
                Glyph::Image => {
                    rect x="2" y="2.5" width="12" height="11" rx="1.5" {}
                    circle cx="5.75" cy="6.25" r="1.25" {}
                    path d="m2.5 12 3.75-3.75 2.5 2.5 1.75-1.75L13.5 12" {}
                }
            }
        }
    }
}

// --- The modal and a panel ----------------------------------------------------------------------

/// The empty modal on every page with the rail; melissa.js fills and opens it.
pub fn overlay_dialog() -> Markup {
    html! {
        dialog.overlay id=(OVERLAY_ID) aria-label="Panel" {
            div id=(OVERLAY_BODY_ID) {}
        }
    }
}

/// The close control of a panel: a link back to the chat (works without script); melissa.js
/// closes the modal instead.
fn panel_close(what: &str) -> Markup {
    html! {
        a.panel-close href="/" data-panel-close title="Close" {
            (glyph(Glyph::Close))
            span.visually-hidden { "Close " (what) }
        }
    }
}

/// A panel with a title, a close control and its content (Apps, Usage and resources).
pub fn panel(title: &str, content: Markup) -> Markup {
    html! {
        div.panel.panel-plain data-panel data-title=(title) {
            header.panel-head {
                h1 { (title) }
                (panel_close(title))
            }
            div.panel-body { (content) }
        }
    }
}

// --- Settings -----------------------------------------------------------------------------------

/// One section in the Settings list.
pub struct SettingsSection<'a> {
    pub href: &'a str,
    pub label: &'a str,
    pub current: bool,
    /// The group it is listed under ("Agents").
    pub group: &'a str,
    pub icon: Glyph,
    /// Titles of the settings in it, for the search field (not shown).
    pub keywords: &'a str,
}

/// The Settings panel: a search field and the grouped section list on the left; a breadcrumb
/// ("Settings / Providers", or "Settings / Developer / Task #3" for a page below a section),
/// the close control and the section's content on the right.
pub fn settings_frame_at(
    sections: &[SettingsSection<'_>],
    current: &str,
    page: Option<&str>,
    content: Markup,
) -> Markup {
    let section_href = sections.iter().find(|s| s.label == current).map(|s| s.href);
    let mut groups: Vec<&str> = Vec::new();
    for s in sections {
        if !groups.contains(&s.group) {
            groups.push(s.group);
        }
    }
    html! {
        div.panel.settings data-panel data-title="Settings" {
            nav.settings-nav aria-label="Settings sections" {
                p.settings-title { "Settings" }
                // Filled in by melissa.js; without script every section is listed anyway.
                div.settings-search data-settings-search hidden {
                    label.visually-hidden for="settings-q" { "Search settings" }
                    (glyph(Glyph::Search))
                    input #settings-q type="search" placeholder="Search" autocomplete="off"
                        aria-keyshortcuts="/" aria-controls="settings-sections";
                    kbd aria-hidden="true" { "/" }
                }
                div #settings-sections {
                    @for (i, g) in groups.iter().enumerate() {
                        p.settings-group id={ "settings-group-" (i) } { (g) }
                        ul aria-labelledby={ "settings-group-" (i) } {
                            @for s in sections.iter().filter(|s| s.group == *g) {
                                li data-keywords=(s.keywords) {
                                    a href=(s.href) aria-current=[s.current.then_some("page")] {
                                        (glyph(s.icon)) span { (s.label) }
                                    }
                                }
                            }
                        }
                    }
                    p.settings-none.quiet role="status" hidden { "No settings match." }
                }
            }
            div.settings-main {
                header.panel-head {
                    (crumbs(&match (page, section_href) {
                        (Some(_), Some(href)) => vec![("/settings", "Settings"), (href, current)],
                        _ => vec![("/settings", "Settings")],
                    }, page.unwrap_or(current)))
                    (panel_close("settings"))
                }
                div.panel-body { (content) }
            }
        }
    }
}

// --- Setting rows -------------------------------------------------------------------------------

/// One setting: its title and a one-line description on the left, its control on the right;
/// `extra` (progress, a confirmation, details) spans the row below them.
pub fn setting_row_with(
    title: Markup,
    desc: Option<Markup>,
    control: Markup,
    extra: Option<Markup>,
) -> Markup {
    html! {
        div.setting-row data-setting {
            div.setting-text {
                p.setting-title { (title) }
                @if let Some(d) = desc { p.setting-desc { (d) } }
            }
            div.setting-control { (control) }
            @if let Some(x) = extra { div.setting-extra { (x) } }
        }
    }
}

/// [`setting_row_with`] for plain text and no extra part.
pub fn setting_row(title: &str, desc: Option<&str>, control: Markup) -> Markup {
    setting_row_with(
        html! { (title) },
        desc.map(|d| html! { (d) }),
        control,
        None,
    )
}

/// A card of setting rows with an optional heading above it.
pub fn setting_card(heading: Option<&str>, rows: Markup) -> Markup {
    html! {
        section.setting-group {
            @if let Some(h) = heading { h2.setting-heading { (h) } }
            div.setting-card { (rows) }
        }
    }
}

/// A one-field form for a row's control: the field (its label read by screen readers only)
/// and a Save button. With htmx it saves on change and the button shows only for keyboard
/// focus and without script, like [`super::toggle`].
pub fn inline_form(action: &str, target: &str, field: Markup) -> Markup {
    html! {
        form.inline-form method="post" action=(action) hx-post=(action) hx-trigger="change, submit"
            hx-target={ "#" (target) } hx-swap="outerHTML" {
            (crate::components::csrf_field())
            (field)
            button.toggle-save.secondary type="submit" { "Save" }
        }
    }
}

/// A select for [`inline_form`].
pub fn inline_select(
    name: &str,
    label: &str,
    choices: &[(String, String)],
    selected: &str,
) -> Markup {
    let id = format!("f-{name}");
    html! {
        label.visually-hidden for=(id) { (label) }
        span.select-wrap {
            select id=(id) name=(name) {
                @for (value, text) in choices {
                    option value=(value) selected[value == selected] { (text) }
                }
            }
        }
    }
}

/// A number input for [`inline_form`].
pub fn inline_number(name: &str, label: &str, value: &str, min: u32, max: u32) -> Markup {
    let id = format!("f-{name}");
    html! {
        label.visually-hidden for=(id) { (label) }
        input.inline-number id=(id) name=(name) type="number" inputmode="numeric"
            min=(min) max=(max) value=(value) required;
    }
}

// --- The sections bar ---------------------------------------------------------------------------

/// What the sections bar's Updates link says and where it leads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateHint {
    pub href: &'static str,
    /// Its tooltip and label.
    pub label: &'static str,
    /// Show the dot: an update is available.
    pub available: bool,
}

impl UpdateHint {
    /// No update known: a quiet link to Settings → Updates.
    pub const NONE: Self = Self {
        href: "/settings/updates",
        label: "Updates",
        available: false,
    };
}

/// The sections bar at the bottom of the sidebar: Settings, Apps, Usage and resources, and
/// Updates (with a dot when one is available). One bar for every width: above 760px small
/// muted icons with Updates alone on the right (the words are for screen readers); on the
/// phone's agent list a tab bar with the words under the icons (CSS). The links open the
/// panels (melissa.js); without script they are pages.
pub fn section_bar(current: Nav, update: &UpdateHint) -> Markup {
    let tab = |href: &str, label: &str, title: &str, icon: Glyph, here: bool| {
        html! {
            a.tab href=(href) title=(title) aria-current=[here.then_some("page")] {
                (glyph(icon))
                span.tab-label { (label) }
            }
        }
    };
    html! {
        nav.tab-bar aria-label="Sections" {
            (tab("/settings", "Settings", "Settings", Glyph::Gear, current == Nav::Settings))
            (tab("/cluster", "Apps", "Apps", Glyph::Hexagon, current == Nav::Cluster))
            (tab("/usage", "Usage", "Usage and resources", Glyph::Chart, current == Nav::Usage))
            a class={ "tab tab-update" @if update.available { " has-update" } } href=(update.href)
                title=(update.label) data-update=(if update.available { "available" } else { "none" }) {
                span.tab-icon {
                    (glyph(Glyph::Refresh))
                    @if update.available { span.update-dot aria-hidden="true" {} }
                }
                span.tab-label { "Updates" }
                @if update.available { span.visually-hidden { ": " (update.label) } }
            }
        }
    }
}

// --- Usage and resources ------------------------------------------------------------------------

/// One agent's line in Usage and resources (all text already in words).
pub struct UsageRow<'a> {
    pub name: &'a str,
    pub href: &'a str,
    pub role: &'a str,
    pub turns: &'a str,
    pub time: &'a str,
    pub approvals: &'a str,
    /// Tokens in context ("45k of 200k"), `None` when the provider did not report them.
    pub context: Option<&'a str>,
    /// Reported cost ("US$0.42"), `None` when the provider did not report it.
    pub cost: Option<&'a str>,
}

/// The agents table of Usage and resources. The Context and Cost columns appear only when at
/// least one row has a value; a row without one shows a muted dash ("Not reported" to screen
/// readers).
pub fn usage_table(rows: &[UsageRow<'_>], note: &str) -> Markup {
    let show_context = rows.iter().any(|r| r.context.is_some());
    let show_cost = rows.iter().any(|r| r.cost.is_some());
    html! {
        div.table-wrap {
            table.usage {
                caption.visually-hidden { "Usage per agent" }
                thead { tr {
                    th scope="col" { "Agent" }
                    th.num scope="col" { "Turns" }
                    th.num scope="col" { "Time" }
                    th.num scope="col" { "Approvals asked" }
                    @if show_context { th.num scope="col" { "Context" } }
                    @if show_cost { th.num scope="col" { "Cost" } }
                } }
                tbody {
                    @for r in rows {
                        tr {
                            th scope="row" { a href=(r.href) { (r.name) } " " (badge(r.role, Tone::Muted)) }
                            td.num { (r.turns) }
                            td.num { (r.time) }
                            td.num { (r.approvals) }
                            @if show_context { (reported(r.context)) }
                            @if show_cost { (reported(r.cost)) }
                        }
                    }
                }
            }
        }
        p.quiet { (note) }
    }
}

/// A number cell that may be empty: the value, or a muted dash read as "Not reported".
fn reported(value: Option<&str>) -> Markup {
    html! {
        @match value {
            Some(v) => td.num { (v) },
            None => td.num title="Not reported" {
                span.quiet aria-hidden="true" { "\u{2014}" }
                span.visually-hidden { "Not reported" }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row<'a>(context: Option<&'a str>, cost: Option<&'a str>) -> UsageRow<'a> {
        UsageRow {
            name: "Ada",
            href: "/agents/a1",
            role: "Helper",
            turns: "1",
            time: "1 min",
            approvals: "0",
            context,
            cost,
        }
    }

    #[test]
    fn usage_table_hides_context_and_cost_when_nothing_is_reported() {
        let html =
            usage_table(&[row(None, None), row(None, None)], "Counted since 21 Nov.").into_string();
        assert!(html.contains(">Approvals asked</th>"));
        for absent in [">Context</th>", ">Cost</th>", "Not reported", "\u{2014}"] {
            assert!(!html.contains(absent), "unexpected {absent}");
        }
        assert_eq!(html.matches("<td").count(), 6);
        assert!(html.contains("<p class=\"quiet\">Counted since 21 Nov.</p>"));
    }

    #[test]
    fn usage_table_shows_a_column_with_a_dash_where_one_row_is_missing() {
        let html =
            usage_table(&[row(Some("45k of 200k"), None), row(None, None)], "").into_string();
        assert!(html.contains(">Context</th>"));
        assert!(!html.contains(">Cost</th>"));
        assert!(html.contains("<td class=\"num\">45k of 200k</td>"));
        assert!(html.contains(
            "<td class=\"num\" title=\"Not reported\"><span class=\"quiet\" aria-hidden=\"true\">\u{2014}</span><span class=\"visually-hidden\">Not reported</span></td>"
        ));
        assert_eq!(html.matches("<td").count(), 8);
    }

    #[test]
    fn the_sections_bar_has_four_labelled_tabs() {
        let html = section_bar(Nav::Chat, &UpdateHint::NONE).into_string();
        for (href, label) in [
            ("/settings", "Settings"),
            ("/cluster", "Apps"),
            ("/usage", "Usage"),
            ("/settings/updates", "Updates"),
        ] {
            assert!(html.contains(&format!("href=\"{href}\"")), "{href}");
            assert!(
                html.contains(&format!("<span class=\"tab-label\">{label}</span>")),
                "{label}"
            );
        }
        assert!(html.contains("aria-label=\"Sections\""));
        assert!(html.contains("href=\"/usage\" title=\"Usage and resources\""));
        assert!(!html.contains("aria-current"));
    }

    #[test]
    fn the_updates_tab_shows_a_dot_only_when_an_update_is_available() {
        let quiet = section_bar(Nav::Chat, &UpdateHint::NONE).into_string();
        assert!(quiet.contains("data-update=\"none\""));
        assert!(quiet.contains("title=\"Updates\""));
        assert!(!quiet.contains("update-dot") && !quiet.contains("has-update"));
        let hint = UpdateHint {
            href: "/settings/updates",
            label: "Melissa update available",
            available: true,
        };
        let dot = section_bar(Nav::Chat, &hint).into_string();
        assert!(dot.contains("class=\"tab tab-update has-update\""));
        assert!(dot.contains("data-update=\"available\""));
        assert!(dot.contains("<span class=\"update-dot\" aria-hidden=\"true\"></span>"));
        assert!(dot.contains(": Melissa update available"));
    }

    #[test]
    fn the_current_panel_is_marked_in_the_sections_bar() {
        let html = section_bar(Nav::Usage, &UpdateHint::NONE).into_string();
        assert!(
            html.contains("href=\"/usage\" title=\"Usage and resources\" aria-current=\"page\"")
        );
        assert!(!html.contains("href=\"/settings\" title=\"Settings\" aria-current"));
        let html = section_bar(Nav::Settings, &UpdateHint::NONE).into_string();
        assert!(html.contains("href=\"/settings\" title=\"Settings\" aria-current=\"page\""));
    }
}
