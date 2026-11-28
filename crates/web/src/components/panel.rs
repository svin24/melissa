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

use super::{badge, Btn, Choice, Nav, Tone};

/// DOM id of the modal that holds a panel.
pub const OVERLAY_ID: &str = "overlay";
/// DOM id of the element a panel is loaded into (also the `HX-Target` that asks for a panel).
pub const OVERLAY_BODY_ID: &str = "overlay-body";
/// DOM id of the open Settings section (breadcrumb and content). melissa.js swaps only it when
/// another section opens, so the section list stays put; as `HX-Target` it also asks for the
/// panel alone.
pub const SETTINGS_MAIN_ID: &str = "settings-main";

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
    // Settings → About (2026-11-22).
    Info,
    Heart,
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
                Glyph::Info => {
                    circle cx="8" cy="8" r="6.25" {}
                    path d="M8 7.25v3.75M8 4.75v.01" {}
                }
                // Stroked in the rose identity colour wherever it shows (`.glyph .heart`).
                Glyph::Heart => {
                    path.heart d="M8 13.25C4.25 10.75 2 8.5 2 6.1a3 3 0 0 1 6-1.35 3 3 0 0 1 6 1.35c0 2.4-2.25 4.65-6 7.15Z" {}
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
        a.panel-close.(Btn::ghost().icon()) href="/" data-panel-close title="Close" {
            (glyph(Glyph::Close))
            span.visually-hidden { "Close " (what) }
        }
    }
}

/// [`panel_close`] for panels with their own head (an app's details).
pub(crate) fn panel_close_link(what: &str) -> Markup {
    panel_close(what)
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
    /// A short value for its row in the phone's list ("3 ready", "Off"); not shown above 760px.
    pub value: Option<&'a str>,
    /// Its row in the phone's list is in the danger colour (Reset).
    pub danger: bool,
}

/// What a Settings page puts in its head bar and above its content (2026-11-23, owner
/// decisions 1 and 2): one optional short line that says what the section is for (hidden on
/// phones), and the section's tools on the right of the head bar ("Checked just now · Check
/// again", "Add provider"; [`settings_tools_oob`] replaces them after a swap).
#[derive(Default)]
pub struct SettingsHead<'a> {
    pub lead: Option<&'a str>,
    pub tools: Option<Markup>,
}

/// DOM id of the head bar's tools.
pub const SETTINGS_TOOLS_ID: &str = "settings-tools";

/// The head bar's tools as an out-of-band swap: added to a section's htmx answer, so a region
/// swap also updates them ("Checked just now").
pub fn settings_tools_oob(tools: Markup) -> Markup {
    html! { div.settings-tools id=(SETTINGS_TOOLS_ID) hx-swap-oob="true" { (tools) } }
}

/// The Settings panel: a search field and the grouped section list on the left; on the right
/// the head bar (the section's title, or "Developer / Task #3" for a page below a section,
/// its tools and the close control) and the section's content.
///
/// Phones (2026-11-22, owner design C1/C2) show one level at a time: `list` (`GET /settings`)
/// shows the list alone, as rows with a value and a chevron; any other page shows the section
/// alone under a back link ("Settings", or the section for a page below one) and its title.
/// The phone-only parts are in the markup at every width and hidden by CSS above 760px.
pub fn settings_frame_at(
    sections: &[SettingsSection<'_>],
    current: &str,
    page: Option<&str>,
    list: bool,
    head: SettingsHead<'_>,
    content: Markup,
) -> Markup {
    let section_href = sections.iter().find(|s| s.label == current).map(|s| s.href);
    // Where the phone's back link goes: up one level.
    let (up, up_label) = match (page, section_href) {
        (Some(_), Some(href)) => (href, current),
        _ => ("/settings", "Settings"),
    };
    let mut groups: Vec<&str> = Vec::new();
    for s in sections {
        if !groups.contains(&s.group) {
            groups.push(s.group);
        }
    }
    html! {
        div.panel.settings data-panel data-title="Settings" {
            nav.settings-nav aria-label="Settings sections" {
                p.settings-title tabindex="-1" { "Settings" }
                // The phone's list has no section beside it: its own close control.
                a.panel-close.settings-list-close href="/" data-panel-close title="Close" {
                    (glyph(Glyph::Close))
                    span.visually-hidden { "Close settings" }
                }
                // Filled in by melissa.js; without script every section is listed anyway.
                div.settings-search.search-field data-settings-search hidden {
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
                                    a href=(s.href) aria-current=[s.current.then_some("page")]
                                        class=[s.danger.then_some("settings-danger")] {
                                        (glyph(s.icon)) span.settings-label { (s.label) }
                                        @if let Some(v) = s.value { span.settings-value { (v) } }
                                        span.settings-chevron { (glyph(Glyph::Chevron)) }
                                    }
                                }
                            }
                        }
                    }
                    p.settings-none.quiet role="status" hidden { "No settings match." }
                }
            }
            // `data-section`: the list entry to mark current when only this part is swapped in.
            // `data-list`: this is the list level (phones show the list, not this part).
            div.settings-main id=(SETTINGS_MAIN_ID) data-section=[section_href] data-list[list]
                tabindex="-1" {
                header.panel-head {
                    a.settings-back href=(up) data-settings-back title="Back" {
                        (glyph(Glyph::Chevron))
                        span.visually-hidden { "Back to " (up_label) }
                    }
                    div.settings-heading {
                        // A page below a section: the section first ("Agents / Backup"), above
                        // 760px only (on a phone the back link leads there).
                        @if let (Some(_), Some(href)) = (page, section_href) {
                            a.settings-parent href=(href) { (current) }
                            span.settings-sep aria-hidden="true" { "/" }
                        }
                        h1.settings-here tabindex="-1" { (page.unwrap_or(current)) }
                    }
                    div.settings-tools id=(SETTINGS_TOOLS_ID) {
                        @if let Some(t) = head.tools { (t) }
                    }
                    (panel_close("settings"))
                }
                div.panel-body {
                    @if let Some(l) = head.lead { p.settings-lead { (l) } }
                    (content)
                }
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

/// A row that spans the card: a form opened in place of a row's button ("Change password",
/// "New schedule"; owner decision 3, 2026-11-23), under an optional title. The form caps its
/// fields at 24rem (CSS).
pub fn setting_row_full(title: Option<&str>, body: Markup) -> Markup {
    html! {
        div.setting-row.setting-row-full data-setting {
            @if let Some(t) = title { h3.setting-title { (t) } }
            (body)
        }
    }
}

/// A row that opens below itself (Settings → About's licenses, a helper's Change icon): the
/// whole row is the `<summary>`, with an optional value (the current icon) and `show`
/// ("Show", "Change"; "Hide" while open) on the right; `body` spans the card below it.
/// Native `<details>`: keyboard and screen readers work, no script.
pub fn setting_row_details(
    title: &str,
    desc: Option<&str>,
    value: Option<Markup>,
    show: &str,
    open: bool,
    body: Markup,
) -> Markup {
    html! {
        details.setting-row.setting-details data-setting open[open] {
            summary {
                span.setting-text {
                    span.setting-title { (title) }
                    @if let Some(d) = desc { span.setting-desc { (d) } }
                }
                span.setting-show {
                    @if let Some(v) = value { (v) }
                    span.show-closed { (show) }
                    span.show-open { "Hide" }
                }
            }
            div.setting-extra { (body) }
        }
    }
}

/// A card of setting rows with an optional heading above it.
pub fn setting_card(heading: Option<&str>, rows: Markup) -> Markup {
    setting_card_with(heading, None, rows)
}

/// [`setting_card`] with one quiet line between the heading and the card (why its settings
/// can't be changed right now: "You can change these when the update is done").
pub fn setting_card_with(heading: Option<&str>, lead: Option<&str>, rows: Markup) -> Markup {
    html! {
        section.setting-group {
            @if let Some(h) = heading { h2.setting-heading { (h) } }
            @if let Some(l) = lead { p.setting-lead { (l) } }
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
            button.toggle-save.(Btn::secondary().sm()) type="submit" { "Save" }
        }
    }
}

/// A text field and a visible Save button for a row's control (a name): unlike
/// [`inline_form`] it saves only on Save or Enter, not while typing.
pub fn inline_text_form(
    action: &str,
    target: &str,
    name: &str,
    label: &str,
    value: &str,
    max: u32,
) -> Markup {
    let id = format!("f-{name}");
    html! {
        form.inline-form method="post" action=(action) hx-post=(action)
            hx-target={ "#" (target) } hx-swap="outerHTML" {
            (crate::components::csrf_field())
            label.visually-hidden for=(id) { (label) }
            input.inline-text.input id=(id) type="text" name=(name) value=(value)
                maxlength=(max) autocomplete="off" spellcheck="false";
            button.(Btn::secondary().sm()) type="submit" { "Save" }
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
            select.input id=(id) name=(name) {
                @for (value, text) in choices {
                    option value=(value) selected[value == selected] { (text) }
                }
            }
        }
    }
}

/// [`inline_select`] with choices that may be shown but not picked ("Beta (not available yet)"),
/// and the whole select `disabled` while it can't be changed (an update runs).
pub fn inline_select_with(
    name: &str,
    label: &str,
    choices: &[Choice<'_>],
    selected: &str,
    disabled: bool,
) -> Markup {
    let id = format!("f-{name}");
    html! {
        label.visually-hidden for=(id) { (label) }
        span.select-wrap {
            select.input id=(id) name=(name) disabled[disabled] {
                @for c in choices {
                    option value=(c.value) selected[c.value == selected] disabled[!c.enabled] { (c.label) }
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
        input.inline-number.input id=(id) name=(name) type="number" inputmode="numeric"
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
    /// A check, update or rollback runs: the spinner in place of the glyph (no dot), this as
    /// its label, and the link asks [`UPDATE_HINT_URL`] again every few seconds until it ends.
    pub busy: Option<&'static str>,
}

impl UpdateHint {
    /// No update known: a quiet link to Settings → Updates.
    pub const NONE: Self = Self {
        href: "/settings/updates",
        label: "Updates",
        available: false,
        busy: None,
    };
}

impl Default for UpdateHint {
    fn default() -> Self {
        Self::NONE
    }
}

/// Where the sections bar's Updates link asks for itself again while something runs.
pub const UPDATE_HINT_URL: &str = "/ui/update-hint";
/// DOM id of the sections bar's Updates link.
pub const UPDATE_TAB_ID: &str = "tab-update";

/// The sections bar's Updates link alone: idle, with the dot (an update is available) or with
/// the spinner (busy; it polls [`UPDATE_HINT_URL`] then). `oob`: as an out-of-band update (the
/// Updates page's region carries it while it polls).
pub fn update_tab(update: &UpdateHint, oob: bool) -> Markup {
    let busy = update.busy;
    let title = busy.unwrap_or(update.label);
    let state = match (busy, update.available) {
        (Some(_), _) => "busy",
        (None, true) => "available",
        (None, false) => "none",
    };
    html! {
        a.tab.tab-update.has-update[update.available && busy.is_none()] id=(UPDATE_TAB_ID)
            href=(update.href) title=(title) data-update=(state)
            hx-get=[busy.map(|_| UPDATE_HINT_URL)] hx-trigger=[busy.map(|_| "every 5s")]
            hx-swap=[busy.map(|_| "outerHTML")] hx-swap-oob=[oob.then_some("true")] {
            span.tab-icon {
                @if busy.is_some() {
                    span.icon.spin aria-hidden="true" {}
                } @else {
                    (glyph(Glyph::Refresh))
                }
                @if update.available && busy.is_none() { span.update-dot aria-hidden="true" {} }
            }
            span.tab-label { "Updates" }
            @if let Some(b) = busy {
                span.visually-hidden { ": " (b) }
            } @else if update.available {
                span.visually-hidden { ": " (update.label) }
            }
        }
    }
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
            (update_tab(update, false))
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
                            th scope="row" { a href=(r.href) { (r.name) } " " (badge(r.role, Tone::Neutral)) }
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
    fn usage_columns_show_only_what_some_row_reports() {
        // (rows, Context column, Cost column, cells, a row marked "Not reported").
        for (rows, context, cost, cells, missing) in [
            (
                vec![row(None, None), row(None, None)],
                false,
                false,
                6,
                false,
            ),
            (
                vec![row(Some("45k"), None), row(None, None)],
                true,
                false,
                8,
                true,
            ),
            (
                vec![row(None, Some("$1")), row(None, Some("$2"))],
                false,
                true,
                8,
                false,
            ),
        ] {
            let html = usage_table(&rows, "").into_string();
            assert_eq!(html.contains(">Context</th>"), context, "{html}");
            assert_eq!(html.contains(">Cost</th>"), cost, "{html}");
            assert_eq!(html.matches("<td").count(), cells, "{html}");
            assert_eq!(html.contains("Not reported"), missing, "{html}");
        }
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

    #[test]
    fn the_updates_link_shows_the_dot_or_the_spinner() {
        let idle = update_tab(&UpdateHint::NONE, false).into_string();
        assert!(idle.contains("data-update=\"none\"") && !idle.contains("hx-get"));
        let available = UpdateHint {
            label: "Melissa update available",
            available: true,
            ..UpdateHint::NONE
        };
        let html = update_tab(&available, false).into_string();
        assert!(
            html.contains("update-dot") && html.contains("has-update"),
            "{html}"
        );
        // Busy: the spinner, no dot, a poll until it ends; out of band for the region's answer.
        let busy = UpdateHint {
            busy: Some("Updating Melissa"),
            ..available
        };
        let html = update_tab(&busy, true).into_string();
        assert!(
            html.contains("spin") && !html.contains("update-dot"),
            "{html}"
        );
        assert!(
            html.contains("hx-get=\"/ui/update-hint\"") && html.contains("hx-swap-oob=\"true\"")
        );
        assert!(html.contains("title=\"Updating Melissa\""));
    }
}
