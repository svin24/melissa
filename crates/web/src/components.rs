//! The fixed component library.
//!
//! This module is the ONLY place that defines markup structure and CSS classes.
//! Pages (see `pages.rs`) are assembled from these functions; they never write their own
//! elements. AI agents never generate markup: they pick components and fill in data.
//! Adding or changing a component is a human-reviewed code change.
//!
//! All text passed in is escaped by maud. Nothing here emits raw, unescaped HTML. Agent
//! and model prose goes through [`markdown`]: CommonMark + GFM read into a safe tree by
//! `crate::markdown` (raw HTML as text, only http/https/mailto links, no images loaded) and
//! rendered here element by element. What the user typed goes through [`plain_text`], which
//! only splits it into paragraphs and list items and links bare URLs.

use crate::markdown::{self, Align, El, Node, Segment};
use maud::{html, Markup, DOCTYPE};
use melissa_agent::attachments::{MAX_FILES, MAX_FILE_BYTES, MAX_MESSAGE_BYTES};

/// Visual tone used by badges and notices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Ok,
    Info,
    Warn,
    Error,
    Muted,
}

impl Tone {
    fn class(self) -> &'static str {
        match self {
            Self::Ok => "tone-ok",
            Self::Info => "tone-info",
            Self::Warn => "tone-warn",
            Self::Error => "tone-error",
            Self::Muted => "tone-muted",
        }
    }
}

/// State of an activity row (a tool call or a helper's job).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityState {
    Working,
    Done,
    Failed,
    Stopped,
}

/// State of an approval card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalState {
    Pending,
    Approved,
    Denied,
    /// Timed out or the asking task ended.
    Withdrawn,
}

/// Tone of a system note in the chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteTone {
    Quiet,
    Caution,
    /// The conversation read text from outside; its changes need the user's OK.
    Guard,
}

/// Status colour of an app workspace or a device dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// Running fine.
    Ok,
    /// Installing, starting, removing.
    Busy,
    /// Failing; needs attention.
    Problem,
    /// Not running (e.g. removed, data kept) or unknown.
    Idle,
}

impl Health {
    fn class(self) -> &'static str {
        match self {
            Self::Ok => "h-ok",
            Self::Busy => "h-busy",
            Self::Problem => "h-problem",
            Self::Idle => "h-idle",
        }
    }
}

/// The section of the app the page belongs to (highlighted in the rail).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Chat,
    /// A conversation with a helper (agent).
    Helper,
    Cluster,
    Waiting,
    /// An archived chat with Melissa.
    PastChat,
    /// The Settings pages.
    Settings,
}

/// DOM id of the pending-confirmations count in the rail (updated live).
pub const WAITING_COUNT_ID: &str = "waiting-count";
/// DOM id of the composer's status line (updated live).
pub const COMPOSER_STATUS_ID: &str = "composer-status";
/// DOM id of the composer form.
pub const COMPOSER_ID: &str = "composer";
/// DOM id of the chat feed list.
pub const FEED_ID: &str = "feed";
/// DOM id of the list on the approvals page.
pub const APPROVALS_ID: &str = "approvals";

fn head(title: &str) -> Markup {
    html! {
        meta charset="utf-8";
        meta name="viewport" content="width=device-width, initial-scale=1";
        meta name="color-scheme" content="light dark";
        // Keep htmx from injecting inline styles, so the CSP can forbid them.
        meta name="htmx-config" content=r#"{"includeIndicatorStyles":false,"allowEval":false}"#;
        title { (title) " · Melissa" }
        link rel="preload" href="/assets/fonts/AtkinsonHyperlegibleNext-Variable.woff2" as="font" type="font/woff2" crossorigin;
        link rel="stylesheet" href="/assets/melissa.css";
        link rel="icon" href="/assets/mark.svg" type="image/svg+xml";
        script src="/assets/htmx.min.js" defer {}
        script src="/assets/htmx-ext-sse.min.js" defer {}
        script src="/assets/melissa.js" defer {}
    }
}

/// A full page with the side rail: banner, rail, main column.
pub fn app_page(title: &str, rail: Markup, main: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head { (head(title)) }
            body {
                a.skip href="#main" { "Skip to content" }
                (banner())
                div.shell {
                    (rail)
                    main #main tabindex="-1" { (main) }
                }
            }
        }
    }
}

/// A full page without the rail (task pages, errors).
pub fn page(title: &str, content: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head { (head(title)) }
            body {
                (banner())
                header.topbar {
                    a.brand href="/" { (mark("mark")) span { "Melissa" } }
                }
                main #main .plain { (content) }
            }
        }
    }
}

/// The slim pre-alpha notice at the top of every page.
pub fn banner() -> Markup {
    html! {
        p.banner role="note" {
            strong { "Pre-alpha." }
            " Things will break. Don't trust Melissa with data you can't lose yet."
        }
    }
}

/// Melissa's mark: a honey hexagon. `class` sizes it.
pub fn mark(class: &str) -> Markup {
    html! {
        svg class=(class) viewBox="0 0 24 24" aria-hidden="true" focusable="false" {
            polygon points="12,1.5 21.1,6.75 21.1,17.25 12,22.5 2.9,17.25 2.9,6.75" {}
        }
    }
}

/// One device in the rail.
pub struct RailDevice<'a> {
    pub name: &'a str,
    pub health: Health,
    pub status: &'a str,
}

/// A conversation with a helper in the rail.
pub struct RailHelper<'a> {
    pub href: &'a str,
    pub name: &'a str,
    pub health: Health,
    pub status: &'a str,
    pub current: bool,
}

/// An archived chat with Melissa in the rail (read-only).
pub struct RailPastChat<'a> {
    pub href: &'a str,
    /// The first message, shortened (escaped text).
    pub title: &'a str,
    /// When, e.g. "18 Nov".
    pub when: &'a str,
    pub current: bool,
}

/// URL the "New chat" button posts to.
pub const NEW_CHAT_URL: &str = "/ui/new-chat";

/// The side rail: brand, navigation, conversations (Melissa, each helper, past chats with
/// Melissa), devices. On phones it collapses to a top bar with the navigation only.
pub fn rail(
    current: Nav,
    waiting: usize,
    devices: &[RailDevice<'_>],
    helpers: &[RailHelper<'_>],
    past: &[RailPastChat<'_>],
) -> Markup {
    let item = |nav: Nav, href: &str, label: &str, extra: Markup| {
        html! {
            li {
                a href=(href) aria-current=[(nav == current).then_some("page")] {
                    span { (label) } (extra)
                }
            }
        }
    };
    html! {
        nav.rail aria-label="Main" {
            a.brand href="/" { (mark("mark")) span { "Melissa" } }
            ul.nav {
                (item(Nav::Chat, "/", "Chat", html! {}))
                (item(Nav::Cluster, "/cluster", "Apps and devices", html! {}))
                (item(Nav::Waiting, "/approvals", "Waiting for you", waiting_count(waiting, false)))
                (item(Nav::Settings, "/settings", "Settings", html! {}))
            }
            section.rail-section aria-labelledby="conv-h" {
                h2 #conv-h { "Conversations" }
                (rail_conversations(current == Nav::Chat, helpers, false))
                // A plain form: works without JS; the server redirects to the empty chat.
                form method="post" action=(NEW_CHAT_URL) {
                    button.ghost type="submit" title="Start over with Melissa. This chat is kept under Past chats." { "New chat" }
                }
                @if !past.is_empty() {
                    h3.rail-sub #past-h { "Past chats" }
                    ul.conversations.past aria-labelledby="past-h" {
                        @for p in past {
                            li {
                                a href=(p.href) aria-current=[p.current.then_some("page")] {
                                    span.name { (p.title) }
                                    span.when { (p.when) }
                                }
                            }
                        }
                    }
                }
            }
            section.rail-section aria-labelledby="dev-h" {
                h2 #dev-h { "Devices" }
                @if devices.is_empty() {
                    p.quiet { "Checking…" }
                } @else {
                    ul.devices {
                        @for d in devices {
                            li {
                                span class={ "dot " (d.health.class()) } aria-hidden="true" {}
                                span.name { (d.name) }
                                span.visually-hidden { ": " (d.status) }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// DOM id of the rail's list of conversations (Melissa and each helper).
pub const RAIL_CONVERSATIONS_ID: &str = "rail-conversations";

/// The rail's conversations: Melissa (`melissa_current`: her chat is open), then each helper.
/// Has a stable id so the live stream can replace it when helpers come, go or change
/// (`oob`: as an out-of-band replacement).
pub fn rail_conversations(melissa_current: bool, helpers: &[RailHelper<'_>], oob: bool) -> Markup {
    html! {
        ul.conversations id=(RAIL_CONVERSATIONS_ID) hx-swap-oob=[oob.then_some("true")] {
            li { a href="/" aria-current=[melissa_current.then_some("page")] { "Melissa" } }
            @for h in helpers {
                li {
                    a href=(h.href) aria-current=[h.current.then_some("page")] {
                        span.with-dot {
                            span class={ "dot " (h.health.class()) } aria-hidden="true" {}
                            span.name { (h.name) }
                        }
                        span.visually-hidden { ", helper: " (h.status) }
                    }
                }
            }
        }
    }
}

/// Shown in place of the composer when the conversation cannot go on (its helper was
/// deleted): a short notice and a way back. `oob`: replaces the composer out of band.
pub fn composer_closed(text: &str, back: &str, back_label: &str, oob: bool) -> Markup {
    let swap = oob.then(|| format!("outerHTML:#{COMPOSER_ID}"));
    html! {
        div.composer.composer-closed hx-swap-oob=[swap] {
            (notice(text, Tone::Warn))
            (link(back, back_label))
        }
    }
}

/// The count of pending confirmations (a badge; empty when zero). Has a stable id so the
/// live stream can replace it (`oob`: as an out-of-band replacement).
pub fn waiting_count(n: usize, oob: bool) -> Markup {
    html! {
        span id=(WAITING_COUNT_ID) hx-swap-oob=[oob.then_some("true")] {
            @if n > 0 { span.count { (n) span.visually-hidden { " waiting" } } }
        }
    }
}

/// A heading with an optional lead line.
pub fn page_header(title: &str, lead: Option<&str>) -> Markup {
    html! {
        header.page-header {
            h1 { (title) }
            @if let Some(lead) = lead { p.lead { (lead) } }
        }
    }
}

/// A titled section of a page.
pub fn section(title: &str, content: Markup) -> Markup {
    html! {
        section.block {
            h2 { (title) }
            (content)
        }
    }
}

/// An empty state: a short title and one line of help.
pub fn empty_state(title: &str, text: &str) -> Markup {
    html! {
        div.empty-state {
            (mark("mark-lg"))
            p.title { (title) }
            p { (text) }
        }
    }
}

// --- Apps as app_grid workspaces ---------------------------------------------------------

/// One app as a hexagon workspace.
pub struct AppWorkspace<'a> {
    pub name: &'a str,
    pub health: Health,
    /// Short status, e.g. "Running".
    pub status: &'a str,
    /// Second line, e.g. "Looked after by you" or a problem.
    pub detail: Option<&'a str>,
    /// Where the app is served, if known.
    pub url: Option<&'a str>,
    /// The app's details ([`app_details`]), loaded into the page's [`app_dialog`].
    pub details: Option<&'a str>,
}

/// A hexagon workspace for an app.
pub fn app_tile(workspace: &AppWorkspace<'_>) -> Markup {
    html! {
        li class={ "hex " (workspace.health.class()) } {
            div.hex-inner {
                @if let Some(url) = workspace.url {
                    a.hex-name href=(url) rel="noreferrer" { (workspace.name) }
                } @else {
                    span.hex-name { (workspace.name) }
                }
                span.hex-status { (workspace.status) }
                @if let Some(detail) = workspace.detail { span.hex-detail { (detail) } }
                @if let Some(details) = workspace.details {
                    button.hex-more type="button" hx-get=(details) hx-target="#app-dialog"
                        hx-swap="innerHTML" aria-haspopup="dialog" aria-label={ "Details: " (workspace.name) } {
                        "Details"
                    }
                }
            }
        }
    }
}

// --- App details (a modal dialog) -----------------------------------------------------------

/// The page's one app dialog, empty until a Details button loads [`app_details`] into it
/// (melissa.js then opens it as a modal; Escape and Close shut it, focus goes back).
pub fn app_dialog() -> Markup {
    html! { dialog #app-dialog .sheet aria-labelledby="app-dialog-h" {} }
}

/// One fact in a details list.
pub struct Fact<'a> {
    pub label: &'a str,
    pub value: &'a str,
    /// Shown as a link (http/https only; the caller checks).
    pub href: Option<&'a str>,
}

/// What the app dialog shows.
pub struct AppDetails<'a> {
    pub name: &'a str,
    pub health: Health,
    pub status: &'a str,
    pub facts: &'a [Fact<'a>],
    /// The logs region's URL ([`app_logs`]); loads when the dialog opens.
    pub logs_url: &'a str,
    /// Stop or Start: (label, POST url). None while neither applies.
    pub toggle: Option<(&'a str, &'a str)>,
    /// The force delete form's POST url.
    pub delete_url: &'a str,
    /// The word to type to confirm the force delete (the app's name).
    pub delete_word: &'a str,
    /// A short message after an action (e.g. why it failed).
    pub message: Option<(&'a str, Tone)>,
}

fn dialog_head(name: &str, status: Option<(Health, &str)>) -> Markup {
    html! {
        header.sheet-head {
            h2 #app-dialog-h { (name) }
            @if let Some((health, status)) = status {
                span class={ "pill " (health.class()) } { (status) }
            }
            form method="dialog" { button.secondary type="submit" autofocus { "Close" } }
        }
    }
}

/// The inside of the app dialog: status, logs and actions.
pub fn app_details(d: &AppDetails<'_>) -> Markup {
    html! {
        (dialog_head(d.name, Some((d.health, d.status))))
        @if let Some((text, tone)) = d.message {
            p class={ "sheet-msg " (tone.class()) } role="status" { (text) }
        }
        dl.facts {
            @for f in d.facts {
                div {
                    dt { (f.label) }
                    dd {
                        @if let Some(href) = f.href {
                            a href=(href) rel="noreferrer" target="_blank" { (f.value) }
                        } @else { (f.value) }
                    }
                }
            }
        }
        div.action-row {
            @if let Some((label, url)) = d.toggle {
                button.secondary type="button" hx-post=(url) hx-target="#app-dialog"
                    hx-swap="innerHTML" hx-disabled-elt="this" { (label) }
            }
        }
        details.danger-zone {
            summary { "Force delete" }
            form hx-post=(d.delete_url) hx-target="#app-dialog" hx-swap="innerHTML"
                hx-disabled-elt="find button" {
                p { "Deletes " (d.name) " and all its data." }
                label.field {
                    span { "Type " strong { (d.delete_word) } " to confirm" }
                    input type="text" name="confirm" required autocomplete="off"
                        autocapitalize="off" spellcheck="false" pattern=(d.delete_word);
                }
                button.primary.danger type="submit" { "Force delete" }
            }
        }
        section.sheet-logs aria-labelledby="app-logs-h" {
            h3 #app-logs-h { "Logs" }
            div #app-logs hx-get=(d.logs_url) hx-trigger="load" hx-swap="outerHTML" {
                p.quiet { "Loading…" }
            }
        }
    }
}

/// The app dialog while a stop waits for the user's OK (the confirmation the tool asked for).
pub fn app_confirm(name: &str, question: &str, label: &str, approve: &str, deny: &str) -> Markup {
    html! {
        (dialog_head(name, None))
        section.approval.confirm {
            p { (question) }
            div.action-row {
                button.primary type="button" hx-post=(approve) hx-target="#app-dialog"
                    hx-swap="innerHTML" hx-disabled-elt="this" { (label) }
                button.secondary type="button" hx-post=(deny) hx-target="#app-dialog"
                    hx-swap="innerHTML" autofocus { "Cancel" }
            }
        }
    }
}

/// The app dialog after a force delete.
pub fn app_gone(name: &str, text: &str) -> Markup {
    html! {
        (dialog_head(name, None))
        p.sheet-msg role="status" { (text) }
    }
}

/// The logs region of the app dialog. Log text is untrusted: it is only ever escaped text.
pub struct AppLogs<'a> {
    pub url: &'a str,
    /// The containers to choose from (shown when there are several) and the current one.
    pub containers: &'a [String],
    pub container: Option<&'a str>,
    pub lines: &'a [&'a str],
    /// Shown instead of lines (no pods, an error).
    pub empty: Option<&'a str>,
}

pub fn app_logs(l: &AppLogs<'_>) -> Markup {
    html! {
        div #app-logs {
            form.action-row hx-get=(l.url) hx-target="#app-logs" hx-swap="outerHTML" {
                @if l.containers.len() > 1 {
                    label.inline-field {
                        span { "Container" }
                        select name="container" hx-get=(l.url) hx-trigger="change"
                            hx-target="#app-logs" hx-swap="outerHTML" hx-include="this" {
                            @for c in l.containers {
                                option value=(c) selected[l.container == Some(c.as_str())] { (c) }
                            }
                        }
                    }
                } @else if let Some(c) = l.container {
                    input type="hidden" name="container" value=(c);
                }
                button.secondary type="submit" { "Refresh" }
            }
            @if let Some(text) = l.empty {
                p.quiet { (text) }
            } @else {
                div.sheet-log tabindex="0" aria-label="Log lines" {
                    (log_view(l.lines.iter().map(|t| LogLine { text: t, is_error: false })))
                }
            }
        }
    }
}

/// The "Add an app" workspace: a link into the chat.
pub fn add_app_tile(href: &str) -> Markup {
    html! {
        li.hex.hex-add {
            a.hex-inner href=(href) {
                span.plus aria-hidden="true" { "+" }
                span.hex-name { "Add an app" }
            }
        }
    }
}

/// A app_grid of workspaces (from [`app_tile`] and [`add_app_tile`]).
pub fn app_grid(label: &str, workspaces: Markup) -> Markup {
    html! { ul.app_grid aria-label=(label) { (workspaces) } }
}

/// A mini workspace in the app strip.
pub struct StripTile<'a> {
    pub name: &'a str,
    pub health: Health,
}

/// The compact app-state strip at the top of the chat. Refreshes itself from `url`.
pub fn app_strip(
    url: &str,
    workspaces: &[StripTile<'_>],
    summary: &str,
    warning: Option<&str>,
) -> Markup {
    html! {
        section.strip #app-strip aria-label="Your apps" hx-get=(url) hx-trigger="every 15s" hx-swap="outerHTML" {
            @if !workspaces.is_empty() {
                ul.mini-tiles aria-hidden="true" {
                    @for c in workspaces {
                        li class={ "mini " (c.health.class()) } title=(c.name) {}
                    }
                }
            }
            p.strip-text {
                span { (summary) }
                @if let Some(w) = warning { span.strip-warn { (w) } }
            }
            a.strip-link href="/cluster" { "Apps and devices" }
        }
    }
}

// --- Devices ---------------------------------------------------------------------------

/// A usage meter (0.0 to 1.0) with a text label.
pub struct Meter<'a> {
    pub label: &'a str,
    pub fraction: f64,
    pub text: &'a str,
}

/// One device.
pub struct DeviceCard<'a> {
    pub name: &'a str,
    pub health: Health,
    pub status: &'a str,
    /// Facts such as "Fedora Linux 45", "4 CPUs".
    pub facts: &'a [String],
    pub meters: &'a [Meter<'a>],
    /// Something the owner should know (e.g. the disk is nearly full).
    pub warning: Option<&'a str>,
}

pub fn device_card(d: &DeviceCard<'_>) -> Markup {
    html! {
        article.device {
            header {
                span class={ "dot " (d.health.class()) } aria-hidden="true" {}
                h3 { (d.name) }
                span.device-status { (d.status) }
            }
            @if !d.facts.is_empty() {
                ul.device-facts { @for f in d.facts { li { (f) } } }
            }
            @for m in d.meters {
                @let pct = (m.fraction.clamp(0.0, 1.0) * 100.0).round() as u32;
                div.meter {
                    label { span { (m.label) } span.meter-text { (m.text) } }
                    meter min="0" max="100" value=(pct) high="85" optimum="0" { (pct) "%" }
                }
            }
            @if let Some(w) = d.warning { p.device-warn { (w) } }
        }
    }
}

// --- The chat ------------------------------------------------------------------------------

/// A live list: the chat feed or the approvals list. `entries` are [`feed_entry`] items;
/// the stream at `stream_url` appends new ones and updates existing ones in place. `empty`
/// shows while the list has no entries.
pub fn feed(id: &str, label: &str, stream_url: &str, entries: Markup, empty: Markup) -> Markup {
    html! {
        div.feed-wrap hx-ext="sse" sse-connect=(stream_url) {
            ol.feed id=(id) sse-swap="feed" hx-swap="beforeend" aria-live="polite" aria-label=(label) {
                li.feed-empty { (empty) }
                (entries)
            }
            // The server asks for a reload when the page is older than the event log
            // (e.g. after a restart); melissa.js does it.
            div.reload-hook sse-swap="reload" hx-swap="none" {}
        }
    }
}

/// A finished conversation (an archived chat): the same entries as [`feed`], read-only and
/// without live updates.
pub fn feed_archive(id: &str, label: &str, entries: Markup, empty: Markup) -> Markup {
    html! {
        div.feed-wrap {
            ol.feed id=(id) aria-label=(label) {
                li.feed-empty { (empty) }
                (entries)
            }
        }
    }
}

/// One item of the feed (or of the approvals list) with a stable id, so updates can
/// replace its content.
pub fn feed_entry(id: &str, content: Markup) -> Markup {
    html! { li.entry id=(id) { (content) } }
}

/// Out-of-band replacement of the *content* of the element with `id` (used in live
/// updates; the element itself stays).
pub fn oob_inner(id: &str, content: Markup) -> Markup {
    html! { div hx-swap-oob={ "innerHTML:#" (id) } { (content) } }
}

/// What the user typed, as paragraphs and simple bullet lists. Only splits on blank lines
/// and leading `- `/`* `, marks backtick code and links bare URLs; everything else is shown
/// as written, escaped. Agent prose uses [`markdown`].
pub fn plain_text(text: &str) -> Markup {
    let blocks = split_blocks(text);
    html! {
        @for block in &blocks {
            @match block {
                Block::Para(lines) => p { @for (i, l) in lines.iter().enumerate() { @if i > 0 { br; } (inline(l)) } },
                Block::List(items) => ul { @for it in items { li { (inline(it)) } } },
            }
        }
    }
}

/// One line: text between a pair of backticks becomes inline code (still escaped text);
/// bare URLs elsewhere become links.
fn inline(line: &str) -> Markup {
    let parts: Vec<&str> = line.split('`').collect();
    // `split` yields at least one part, so this can't underflow.
    let last = parts.len() - 1;
    html! {
        @for (i, part) in parts.iter().enumerate() {
            @if i % 2 == 0 {
                (linked(part))
            } @else if i == last {
                // A backtick without a partner stays as written.
                "`" (linked(part))
            } @else if part.is_empty() {
                "``"
            } @else {
                code { (part) }
            }
        }
    }
}

/// Text with its bare `http(s)://` URLs as links.
fn linked(text: &str) -> Markup {
    html! {
        @for seg in markdown::linkify(text) {
            @match seg {
                Segment::Text(t) => (t),
                Segment::Url(u) => (external_link(u, html! { (u) }, false)),
            }
        }
    }
}

/// A link that leaves Melissa: new tab, no opener, no referrer, not endorsed. `href` must
/// already be checked (`markdown::safe_href` or `markdown::linkify`).
fn external_link(href: &str, content: Markup, image: bool) -> Markup {
    html! {
        a href=(href) target="_blank" rel="noopener noreferrer nofollow" class=[image.then_some("md-image")] {
            @if image { span.image-tag { "Image: " } }
            (content)
        }
    }
}

/// Agent prose (the lead agent's or a helper's reply, a job, a job's summary): Markdown with
/// the GFM extensions, rendered from the safe tree of `crate::markdown`. Headings are
/// scaled down for chat (`#` becomes `h4`), code blocks get a copy button (shown by
/// `melissa.js` when the clipboard is available), wide tables scroll.
pub fn markdown(text: &str) -> Markup {
    html! { div.md { (md_nodes(&markdown::parse(text))) } }
}

fn md_nodes(nodes: &[Node]) -> Markup {
    html! { @for n in nodes { (md_node(n)) } }
}

fn md_node(node: &Node) -> Markup {
    match node {
        Node::Text(t) => html! { (t) },
        Node::Code(t) => html! { code { (t) } },
        Node::CodeBlock(t) => html! {
            div.md-code {
                pre tabindex="0" { code { (t) } }
                button.copy-code.secondary type="button" aria-label="Copy code" { "Copy" }
            }
        },
        Node::Link {
            href,
            image,
            children,
        } => external_link(href, md_nodes(children), *image),
        Node::Break => html! { br; },
        Node::Rule => html! { hr; },
        Node::Task(done) => html! {
            input.task type="checkbox" disabled checked[*done] aria-label=(if *done { "Done" } else { "Not done" });
            " "
        },
        Node::El(el, children) => {
            let inner = || md_nodes(children);
            match el {
                El::Para => html! { p { (inner()) } },
                El::Heading(1) => html! { h4.md-h1 { (inner()) } },
                El::Heading(2) => html! { h5.md-h2 { (inner()) } },
                El::Heading(_) => html! { h6.md-h3 { (inner()) } },
                El::Quote => html! { blockquote { (inner()) } },
                El::List { start: None } => html! { ul { (inner()) } },
                El::List { start: Some(1) } => html! { ol { (inner()) } },
                El::List { start: Some(n) } => html! { ol start=(n) { (inner()) } },
                El::Item => html! { li { (inner()) } },
                El::Table => {
                    let (head, rows) = match children.split_first() {
                        Some((h @ Node::El(El::TableHead, _), rows)) => (Some(h), rows),
                        _ => (None, &children[..]),
                    };
                    html! {
                        div.md-table tabindex="0" {
                            table {
                                @if let Some(h) = head { (md_node(h)) }
                                tbody { (md_nodes(rows)) }
                            }
                        }
                    }
                }
                El::TableHead => html! { thead { tr { (inner()) } } },
                El::TableRow => html! { tr { (inner()) } },
                El::TableCell { head, align } => {
                    let class = match align {
                        Align::None => None,
                        Align::Left => Some("align-left"),
                        Align::Center => Some("align-center"),
                        Align::Right => Some("align-right"),
                    };
                    if *head {
                        html! { th scope="col" class=[class] { (inner()) } }
                    } else {
                        html! { td class=[class] { (inner()) } }
                    }
                }
                El::Emphasis => html! { em { (inner()) } },
                El::Strong => html! { strong { (inner()) } },
                El::Strike => html! { del { (inner()) } },
            }
        }
    }
}

enum Block<'a> {
    Para(Vec<&'a str>),
    List(Vec<&'a str>),
}

fn split_blocks(text: &str) -> Vec<Block<'_>> {
    let mut out: Vec<Block<'_>> = Vec::new();
    let mut blank = true;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            blank = true;
            continue;
        }
        let bullet = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("• "));
        match (bullet, out.last_mut()) {
            (Some(item), Some(Block::List(items))) => items.push(item),
            (Some(item), _) => out.push(Block::List(vec![item])),
            (None, Some(Block::Para(lines))) if !blank => lines.push(trimmed),
            (None, _) => out.push(Block::Para(vec![trimmed])),
        }
        blank = false;
    }
    out
}

/// A message the user wrote (right-aligned, dark bubble).
pub fn user_message(text: &str) -> Markup {
    user_message_with(text, &[])
}

/// A file the user sent with a message, as the chat shows it.
pub struct SentFile {
    /// Where the UI serves it (same origin); None: removed from the file box (expired or
    /// deleted), shown as a plain "expired" chip.
    pub href: Option<String>,
    /// The cleaned file name.
    pub name: String,
    /// Human-readable size (`1.2 MiB`).
    pub size: String,
    /// Shown as a thumbnail (PNG, JPEG, GIF, WebP); anything else is a download chip.
    pub image: bool,
}

/// A message the user wrote with files: image thumbnails (open full size) and name/size
/// chips (download), then the text.
pub fn user_message_with(text: &str, files: &[SentFile]) -> Markup {
    html! {
        div.msg.msg-user {
            h3.visually-hidden { "You" }
            div.bubble {
                @if !files.is_empty() {
                    ul.sent-files {
                        @for f in files {
                            li {
                                @match &f.href {
                                    None => {
                                        span.file-chip.expired title="No longer kept" {
                                            (file_icon())
                                            span.name { (f.name) }
                                            span.size { "expired" }
                                        }
                                    }
                                    Some(href) if f.image => {
                                        a.sent-image href=(href) target="_blank" rel="noopener" title=(f.name) {
                                            img src=(href) alt=(f.name) loading="lazy" decoding="async";
                                        }
                                    }
                                    Some(href) => {
                                        a.file-chip href=(href) download=(f.name) {
                                            (file_icon())
                                            span.name { (f.name) }
                                            span.size { (f.size) }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                @if !text.trim().is_empty() { (plain_text(text)) }
            }
        }
    }
}

// --- Details panel (2026-11-20, owner decision) ------------------------------------------
//
// The right-hand panel of a chat page, about the agent whose chat it is (Melissa on home, a
// helper on its page): who it is and what it runs on, its apps, its schedules and its file
// box. Wide screens: a column beside the chat (melissa.js can hide it, remembered per
// browser). Narrow screens with script: a sheet over the chat, opened with the Details
// button (Esc or Close shut it; focus moves in and back). Without script it is always shown:
// beside the chat, or below it on narrow screens. Live: the stream swaps the file list and
// count, and the agent's status; the apps poll like the app strip.

/// DOM id of the details panel.
pub const DETAILS_ID: &str = "details-panel";
/// DOM id of the agent section (rename and delete swap it).
pub const DETAILS_AGENT_ID: &str = "details-agent";
/// DOM id of the agent's status badge (the stream replaces its content).
pub const DETAILS_STATUS_ID: &str = "details-status";
/// DOM id of the apps section (it polls).
pub const DETAILS_APPS_ID: &str = "details-apps";
/// DOM id of the schedules section (on/off swaps it).
pub const DETAILS_SCHEDULES_ID: &str = "details-schedules";
/// DOM id of the file panel's body (delete, upload, send and retention swap it; the stream
/// replaces its content).
pub const FILE_BODY_ID: &str = "file-box-body";
/// DOM id of the file count in the Files heading.
pub const FILE_COUNT_ID: &str = "file-count";

/// The agent section of the details panel.
pub struct AgentCard<'a> {
    /// The rename form's action (None: Melissa, whose name is fixed).
    pub rename: Option<&'a str>,
    pub name: &'a str,
    /// `Lead agent` or `Helper`.
    pub role: &'a str,
    /// "Claude · Opus 5.5 · High".
    pub runs_on: &'a str,
    /// The chat has the model picker: a Change button opens it.
    pub picker: bool,
    pub status: (&'a str, Tone),
    /// "By Melissa · 20 Nov", when known.
    pub created: Option<&'a str>,
    /// Delete's action (helpers only), unless `kept` says why it can't be deleted now.
    pub delete: Option<&'a str>,
    /// Why Delete isn't offered ("Looks after Nextcloud. …").
    pub kept: Option<&'a str>,
    pub error: Option<&'a str>,
}

/// One app in the details panel.
pub struct DetailApp {
    pub name: String,
    /// Its details ([`app_details`] into the page's [`app_dialog`]).
    pub details: String,
    pub health: Health,
    /// "Running".
    pub status: String,
    /// Who looks after it (Melissa's panel lists every app).
    pub keeper: Option<String>,
}

/// The apps section.
pub struct DetailApps<'a> {
    /// Where the section reloads itself from.
    pub url: &'a str,
    pub apps: &'a [DetailApp],
    pub empty: &'a str,
}

/// One schedule in the details panel.
pub struct DetailSchedule {
    pub name: String,
    /// "Every day at 03:00".
    pub when: String,
    /// "Next: 21 Nov 03:00" or "Off".
    pub next: String,
    pub enabled: bool,
    /// POST here with `enabled=on|off`.
    pub toggle: String,
}

/// The schedules section.
pub struct DetailSchedules<'a> {
    pub schedules: &'a [DetailSchedule],
    /// None: schedules don't run on this device.
    pub available: bool,
    pub flash: Option<(&'a str, Tone)>,
}

/// Everything the details panel shows.
pub struct DetailsPanel<'a> {
    pub agent: AgentCard<'a>,
    pub apps: DetailApps<'a>,
    pub schedules: DetailSchedules<'a>,
    pub files: &'a FilePanel<'a>,
}

/// The Details button of a chat page (shown by melissa.js; without script the panel is
/// always visible, so the button stays hidden).
pub fn details_toggle() -> Markup {
    html! {
        button.details-toggle type="button" aria-controls=(DETAILS_ID) aria-expanded="false"
            data-details-toggle hidden {
            (panel_icon())
            span.label { "Details" }
        }
    }
}

/// The details panel beside a chat.
pub fn details_panel(p: &DetailsPanel<'_>) -> Markup {
    html! {
        div.details-backdrop data-details-close hidden {}
        aside.details id=(DETAILS_ID) aria-labelledby="details-h" tabindex="-1" {
            div.details-head {
                h2 #details-h { "Details" }
                button.icon-button.details-close type="button" data-details-close
                    aria-label="Close details" hidden { (close_icon()) }
            }
            (agent_card(&p.agent))
            section.details-section aria-labelledby="details-apps-h" {
                h3 #details-apps-h { "Apps" }
                (detail_apps(&p.apps))
            }
            section.details-section aria-labelledby="details-schedules-h" {
                h3 #details-schedules-h { "Schedules" }
                (detail_schedules(&p.schedules))
            }
            section.details-section aria-labelledby="details-files-h" {
                h3 #details-files-h { "Files " (file_count(p.files.files.len(), false)) }
                (file_panel_body(p.files))
            }
        }
    }
}

/// The agent section (the response to rename and to a refused delete).
pub fn agent_card(a: &AgentCard<'_>) -> Markup {
    let target = format!("#{DETAILS_AGENT_ID}");
    html! {
        section.details-section.agent-card id=(DETAILS_AGENT_ID) aria-label="Agent" {
            div.agent-title {
                p.agent-name { (a.name) }
                span id=(DETAILS_STATUS_ID) { (badge(a.status.0, a.status.1)) }
            }
            @if let Some(e) = a.error { (notice(e, Tone::Error)) }
            dl.agent-facts {
                div { dt { "Role" } dd { (a.role) } }
                div {
                    dt { "Runs on" }
                    dd {
                        span { (a.runs_on) }
                        @if a.picker {
                            " "
                            button.link type="button" popovertarget=(MODEL_PICKER_ID)
                                aria-haspopup="dialog" { "Change" }
                        }
                    }
                }
                @if let Some(c) = a.created { div { dt { "Made" } dd { (c) } } }
            }
            @if let Some(action) = a.rename {
                details.disclosure.agent-rename {
                    summary { "Rename" }
                    form.one-field method="post" action=(action) hx-post=(action)
                        hx-target=(target) hx-swap="outerHTML" {
                        label.field {
                            span.visually-hidden { "New name" }
                            input type="text" name="name" value=(a.name) required maxlength="40"
                                autocomplete="off" spellcheck="false";
                        }
                        button.secondary type="submit" { "Save" }
                    }
                }
            }
            @if let Some(kept) = a.kept {
                p.quiet { (kept) }
            } @else if let Some(action) = a.delete {
                (action_button(
                    action,
                    DETAILS_AGENT_ID,
                    "Delete",
                    &format!("Delete {}? Its chat, file box and schedules go with it. This can't be undone.", a.name),
                ))
            }
        }
    }
}

/// The apps section's content, polling for changes like the app strip.
pub fn detail_apps(s: &DetailApps<'_>) -> Markup {
    html! {
        div id=(DETAILS_APPS_ID) hx-get=(s.url) hx-trigger="every 15s" hx-swap="outerHTML" {
            @if s.apps.is_empty() {
                p.quiet { (s.empty) }
            } @else {
                ul.detail-list {
                    @for a in s.apps {
                        li {
                            span class={ "dot " (a.health.class()) } aria-hidden="true" {}
                            // Without script: the apps page, where each app has its details.
                            a.detail-main href="/cluster" hx-get=(a.details) hx-target="#app-dialog"
                                hx-swap="innerHTML" aria-haspopup="dialog" {
                                span.name { (a.name) }
                                span.meta {
                                    (a.status)
                                    @if let Some(k) = &a.keeper { " · " (k) }
                                }
                            }
                        }
                    }
                }
            }
            a.detail-more href="/cluster" { "Apps and devices" }
        }
    }
}

/// The schedules section's content (the response to on/off).
pub fn detail_schedules(s: &DetailSchedules<'_>) -> Markup {
    let target = format!("#{DETAILS_SCHEDULES_ID}");
    html! {
        div id=(DETAILS_SCHEDULES_ID) {
            @if let Some((text, tone)) = s.flash { (notice(text, tone)) }
            @if !s.available {
                p.quiet { "Schedules aren't running on this device." }
            } @else if s.schedules.is_empty() {
                p.quiet { "No schedules." }
            } @else {
                ul.detail-list {
                    @for sc in s.schedules {
                        li {
                            div.detail-main {
                                span.name { (sc.name) }
                                span.meta { (sc.when) " · " (sc.next) }
                            }
                            form method="post" action=(sc.toggle) hx-post=(sc.toggle)
                                hx-target=(target) hx-swap="outerHTML" {
                                input type="hidden" name="enabled" value=(if sc.enabled { "off" } else { "on" });
                                button.secondary type="submit" aria-label={
                                    (if sc.enabled { "Turn off " } else { "Turn on " }) (sc.name)
                                } { (if sc.enabled { "Turn off" } else { "Turn on" }) }
                            }
                        }
                    }
                }
            }
            a.detail-more href="/settings/schedules" { "All schedules" }
        }
    }
}

/// One file in a chat's file panel.
pub struct PanelFile {
    /// Download URL (same origin).
    pub href: String,
    pub name: String,
    /// `1.2 MiB`.
    pub size: String,
    /// When it was added, for people (`20 Nov 14:03`).
    pub added: String,
    /// Who added it: `You`, the agent's name, or `From Ada`.
    pub by: String,
    /// POST here to delete it.
    pub delete: String,
    /// POST here (`to`, `mode=move`) to send it to another box.
    pub send: String,
    /// What "Reference in chat" puts in the message box: `[file box: report.pdf]`.
    pub reference: String,
    /// The chat with that text in the box (without script).
    pub reference_href: String,
}

/// A chat's file panel (the Files section of the details panel).
pub struct FilePanel<'a> {
    pub files: &'a [PanelFile],
    /// `3 files, 12 MiB`.
    pub usage: &'a str,
    /// `Kept for 5 days`.
    pub keep: &'a str,
    /// POST here to change how long this box keeps files (form field `keep`).
    pub keep_action: &'a str,
    /// (value, label): `default`, day counts, `forever`.
    pub keep_choices: &'a [(String, String)],
    pub keep_selected: &'a str,
    /// Whose box, for the empty text ("Melissa", "Ada").
    pub whose: &'a str,
    /// POST files here (multipart, field `files`) to add them to the box.
    pub upload: &'a str,
    /// The other boxes a file can be sent to: (key, name).
    pub send_to: &'a [(String, String)],
    /// What the last action did.
    pub flash: Option<(&'a str, Tone)>,
}

/// The file count in the Files heading (`oob`: as an out-of-band swap).
pub fn file_count(n: usize, oob: bool) -> Markup {
    html! {
        span.count id=(FILE_COUNT_ID) hx-swap-oob=[oob.then_some("true")] {
            (n) span.visually-hidden { (if n == 1 { " file" } else { " files" }) }
        }
    }
}

/// The file panel's body with its id (the response to its actions).
pub fn file_panel_body(p: &FilePanel<'_>) -> Markup {
    html! { div.file-box-body id=(FILE_BODY_ID) { (file_panel_inner(p)) } }
}

/// The file panel's body content (the stream replaces it).
pub fn file_panel_inner(p: &FilePanel<'_>) -> Markup {
    let target = format!("#{FILE_BODY_ID}");
    html! {
        @if let Some((text, tone)) = p.flash { (notice(text, tone)) }
        p.file-usage { (p.usage) " · " (p.keep) }
        form.file-upload method="post" action=(p.upload) enctype="multipart/form-data"
            hx-post=(p.upload) hx-target=(target) hx-swap="outerHTML" {
            label.field {
                span { "Add files" }
                input type="file" name="files" multiple required;
            }
            button.secondary type="submit" { "Upload" }
        }
        @if p.files.is_empty() {
            p.quiet { "Nothing here yet. Files you add or send " (p.whose) ", and files it saves, show up here." }
        } @else {
            ul.box-files {
                @for f in p.files {
                    li {
                        div.box-file-row {
                            a.box-file href=(f.href) download=(f.name) title={ "Download " (f.name) } {
                                span.name { (f.name) }
                                span.meta { (f.size) " · " (f.added) " · " (f.by) }
                            }
                            details.file-more {
                                summary.icon-button title={ "More for " (f.name) } {
                                    span.visually-hidden { "More for " (f.name) }
                                    (more_icon())
                                }
                                div.file-more-body {
                                    a.button-like href=(f.reference_href)
                                        data-insert=(f.reference) { "Reference in chat" }
                                    @if !p.send_to.is_empty() {
                                        form.file-send method="post" action=(f.send) hx-post=(f.send)
                                            hx-target=(target) hx-swap="outerHTML" {
                                            label.field {
                                                span { "Send to" }
                                                select name="to" {
                                                    @for (key, name) in p.send_to {
                                                        option value=(key) { (name) }
                                                    }
                                                }
                                            }
                                            label.check {
                                                input type="checkbox" name="mode" value="move";
                                                span { "Move (remove it here)" }
                                            }
                                            button.secondary type="submit" { "Send" }
                                        }
                                    }
                                    (action_button(
                                        &f.delete,
                                        FILE_BODY_ID,
                                        "Delete",
                                        &format!("Delete {}? It can't be brought back.", f.name),
                                    ))
                                }
                            }
                        }
                    }
                }
            }
        }
        details.disclosure.keep-files {
            summary { "Keep files for…" }
            form.one-field method="post" action=(p.keep_action) hx-post=(p.keep_action)
                hx-target=(target) hx-swap="outerHTML" {
                label.field {
                    span.visually-hidden { "Keep files for" }
                    select name="keep" {
                        @for (value, label) in p.keep_choices {
                            option value=(value) selected[value == p.keep_selected] { (label) }
                        }
                    }
                }
                button.secondary type="submit" { "Save" }
            }
        }
    }
}

fn panel_icon() -> Markup {
    html! {
        svg.file-icon viewBox="0 0 16 16" aria-hidden="true" focusable="false" {
            rect x="1.5" y="2.5" width="13" height="11" rx="1.5" {}
            path d="M10 2.5v11" {}
        }
    }
}

fn close_icon() -> Markup {
    html! {
        svg.file-icon viewBox="0 0 16 16" aria-hidden="true" focusable="false" {
            path d="M3.5 3.5l9 9M12.5 3.5l-9 9" {}
        }
    }
}

fn more_icon() -> Markup {
    html! {
        svg.file-icon viewBox="0 0 16 16" aria-hidden="true" focusable="false" {
            circle cx="3.5" cy="8" r="1.1" {}
            circle cx="8" cy="8" r="1.1" {}
            circle cx="12.5" cy="8" r="1.1" {}
        }
    }
}

fn file_icon() -> Markup {
    html! {
        svg.file-icon viewBox="0 0 16 16" aria-hidden="true" focusable="false" {
            path d="M4 1.5h5l3.5 3.5v9.5h-8.5zM9 1.5V5h3.5" {}
        }
    }
}

fn mic_icon() -> Markup {
    html! {
        svg viewBox="0 0 20 20" aria-hidden="true" focusable="false" {
            rect x="7" y="2.5" width="6" height="10" rx="3" {}
            path d="M4.5 9.5a5.5 5.5 0 0 0 11 0M10 15v2.5" {}
        }
    }
}

fn paperclip_icon() -> Markup {
    html! {
        svg viewBox="0 0 16 16" aria-hidden="true" focusable="false" {
            path d="M13.5 7.5 8.2 12.8a3.2 3.2 0 0 1-4.5-4.5l5.6-5.6a2.1 2.1 0 0 1 3 3L6.7 11.3a1 1 0 0 1-1.5-1.5l5-5" {}
        }
    }
}

/// A scheduled job's instruction, sent by Melissa in the user's place: the user's bubble with a
/// small "Scheduled: <name>" label above it.
pub fn scheduled_message(name: &str, text: &str) -> Markup {
    html! {
        div.msg.msg-user.msg-scheduled {
            h3.visually-hidden { "Scheduled job" }
            p.msg-tag { "Scheduled: " (name) }
            div.bubble { (plain_text(text)) }
        }
    }
}

/// Something Melissa passed into a helper's conversation (a job or a message): a quiet
/// block with who it is from. `text` is untrusted.
pub fn relayed_message(from: &str, text: &str) -> Markup {
    html! {
        div.msg.msg-relayed {
            h3.from { (from) }
            div.body { (markdown(text)) }
        }
    }
}

/// A message from the lead agent (left, honey hexagon avatar, no bubble).
pub fn lead_message(text: &str) -> Markup {
    html! {
        div.msg.msg-lead {
            (mark("avatar"))
            h3.visually-hidden { "Melissa" }
            div.body { (markdown(text)) }
        }
    }
}

fn activity_icon(state: ActivityState) -> Markup {
    match state {
        ActivityState::Working => html! {
            span.icon.spin aria-hidden="true" {}
        },
        ActivityState::Done => html! {
            svg.icon.ok viewBox="0 0 16 16" aria-hidden="true" focusable="false" {
                path d="M3.5 8.5l3 3 6-7" {}
            }
        },
        ActivityState::Failed => html! {
            svg.icon.fail viewBox="0 0 16 16" aria-hidden="true" focusable="false" {
                path d="M4.5 4.5l7 7M11.5 4.5l-7 7" {}
            }
        },
        ActivityState::Stopped => html! {
            svg.icon.stopped viewBox="0 0 16 16" aria-hidden="true" focusable="false" {
                path d="M4.5 8h7" {}
            }
        },
    }
}

/// A compact "what I did" row: icon, plain label (a link when `href` is given, e.g. to a
/// helper's conversation), short detail. `prose`: the detail is agent prose (a helper's
/// job), rendered as [`markdown`]; otherwise it is a short plain text (a tool's title).
pub fn activity_row(
    label: &str,
    detail: Option<&str>,
    prose: bool,
    state: ActivityState,
    href: Option<&str>,
) -> Markup {
    let state_text = match state {
        ActivityState::Working => "in progress",
        ActivityState::Done => "done",
        ActivityState::Failed => "failed",
        ActivityState::Stopped => "stopped",
    };
    html! {
        div.activity {
            (activity_icon(state))
            span.label {
                @if let Some(href) = href { a href=(href) { (label) } } @else { (label) }
                span.visually-hidden { " (" (state_text) ")" }
            }
            @if let Some(d) = detail {
                @if prose { div.detail { (markdown(d)) } } @else { span.detail { (d) } }
            }
        }
    }
}

/// The agent's thinking in the chat (Settings → Developer): a row like [`activity_row`]
/// that opens to the thought (agent prose, rendered as [`markdown`]). Without text yet (the
/// agent still thinks) it is a plain row.
pub fn thought_row(label: &str, text: &str, state: ActivityState) -> Markup {
    if text.is_empty() {
        return activity_row(label, None, false, state, None);
    }
    html! {
        details.thought {
            summary.activity {
                (activity_icon(state))
                span.label { (label) }
            }
            div.detail { (markdown(text)) }
        }
    }
}

/// A system line in the chat.
pub fn note(text: &str, detail: Option<&str>, tone: NoteTone) -> Markup {
    let class = match tone {
        NoteTone::Quiet => "note",
        NoteTone::Caution => "note caution",
        NoteTone::Guard => "note guard",
    };
    html! {
        p class=(class) {
            span { (text) }
            @if let Some(d) = detail { " " span.detail { (d) } }
        }
    }
}

/// A quiet divider across the chat with a short label (e.g. "Earlier").
pub fn chat_divider(text: &str) -> Markup {
    html! {
        div.divider role="separator" { span { (text) } }
    }
}

/// An approval card: what is asked, what happens, and two buttons.
pub struct ApprovalCard<'a> {
    pub id: u64,
    /// DOM id of the element whose content the buttons replace.
    pub target: &'a str,
    pub title: &'a str,
    pub consequence: &'a str,
    pub reason: &'a str,
    pub destructive: bool,
    pub permanent: bool,
    pub yes: &'a str,
    pub no: &'a str,
    pub state: ApprovalState,
    /// The "Always allow" question (the rule it saves); None: no such button.
    pub always: Option<&'a str>,
}

pub fn approval_card(c: &ApprovalCard<'_>) -> Markup {
    let class = match (c.state, c.destructive) {
        (ApprovalState::Pending, true) => "approval destructive",
        (ApprovalState::Pending, false) => "approval",
        _ => "approval decided",
    };
    let action = |verdict: &str| format!("/ui/confirmations/{}/{verdict}", c.id);
    let target = format!("#{}", c.target);
    html! {
        article class=(class) aria-labelledby={ "ap-" (c.id) } {
            h3 #{ "ap-" (c.id) } { (c.title) }
            p.consequence { (c.consequence) }
            @if c.permanent && c.state == ApprovalState::Pending {
                p.permanent { "Can't be undone." }
            }
            p.reason { "Asked because: " (c.reason) }
            @match c.state {
                ApprovalState::Pending => div.actions {
                    form method="post" action=(action("approve")) hx-post=(action("approve"))
                        hx-target=(target) hx-swap="innerHTML" {
                        button.primary.danger[c.destructive] type="submit" { (c.yes) }
                    }
                    form method="post" action=(action("deny")) hx-post=(action("deny"))
                        hx-target=(target) hx-swap="innerHTML" {
                        button.secondary type="submit" { (c.no) }
                    }
                    @if let Some(question) = c.always {
                        form method="post" action=(action("always")) hx-post=(action("always"))
                            hx-target=(target) hx-swap="innerHTML" {
                            button.secondary type="submit" title=(question) { "Always allow" }
                        }
                    }
                },
                ApprovalState::Approved => p.outcome.ok { "You allowed this." },
                ApprovalState::Denied => p.outcome { "You said no." },
                ApprovalState::Withdrawn => p.outcome { "No longer needed (it timed out or the job ended)." },
            }
        }
    }
}

/// Where a composer sends to: the lead agent or one helper.
pub struct ComposerTarget<'a> {
    /// POST URL for a message (form field `text`).
    pub send: &'a str,
    /// POST URL that stops the running turn.
    pub cancel: &'a str,
    pub placeholder: &'a str,
    /// POST URL that turns a recording into text (voice input on and ready), if any.
    pub voice: Option<&'a str>,
}

/// The message box at the bottom of the chat: the text on top, a bar below it with the
/// paperclip on the left and, on the right, `chip` (the model picker's chip, see
/// [`model_chip`]), the mic (voice on) and Send. `draft` pre-fills it, `error` explains why
/// the last message was not sent.
pub fn composer(
    to: &ComposerTarget<'_>,
    draft: &str,
    error: Option<&str>,
    chip: Markup,
    status: Markup,
) -> Markup {
    html! {
        form.composer id=(COMPOSER_ID) method="post" action=(to.send) hx-post=(to.send)
            enctype="multipart/form-data" hx-encoding="multipart/form-data"
            hx-target={ "#" (COMPOSER_ID) } hx-swap="outerHTML" {
            @if let Some(e) = error { p.composer-error role="alert" { (e) } }
            // Files picked, pasted or dropped: melissa.js fills this list from the template
            // (name, size, a thumbnail for images, a remove button) before sending.
            p.composer-error #composer-note role="alert" hidden {}
            ul.composer-files #composer-files aria-label="Files to send" {}
            template #file-chip {
                li.file-chip {
                    img.thumb alt="" hidden;
                    span.name {}
                    span.size {}
                    button.remove type="button" aria-label="Remove" { span aria-hidden="true" { "×" } }
                }
            }
            div.composer-box {
                label.visually-hidden for="message" { "Message" }
                textarea #message name="text" rows="1" maxlength="16384"
                    placeholder=(to.placeholder)
                    autofocus[draft.is_empty() && error.is_none()] { (draft) }
                div.composer-bar {
                    // After the text, so the server reads the text before the files.
                    input #attach .attach-input type="file" name="files" multiple
                        data-max-file=(MAX_FILE_BYTES) data-max-total=(MAX_MESSAGE_BYTES)
                        data-max-count=(MAX_FILES);
                    label.attach for="attach" title="Attach files" {
                        (paperclip_icon())
                        span.visually-hidden { "Attach files" }
                    }
                    span.bar-gap {}
                    span id=(MODEL_CHIP_ID) { (chip) }
                    @if let Some(url) = to.voice {
                        // Voice input: melissa.js shows it (recording needs script), records,
                        // sends the clip to `url` and puts the text in the box to edit; nothing
                        // is sent by itself. Where the browser can't record (plain http on the
                        // LAN) a click says why.
                        button.voice #voice type="button" hidden data-url=(url) aria-pressed="false"
                            title="Speak instead of typing" {
                            (mic_icon())
                            span.voice-time aria-hidden="true" {}
                            span.visually-hidden.voice-label { "Speak instead of typing" }
                        }
                    }
                    button.send type="submit" title="Send" {
                        svg viewBox="0 0 16 16" aria-hidden="true" focusable="false" { path d="M8 13V3M3.5 7.5 8 3l4.5 4.5" {} }
                        span.visually-hidden { "Send" }
                    }
                }
            }
            // The event stream owns the status (busy, Stop): it arrives in order with the
            // turn's events. The response to a send can land after the turn already ended, so
            // `hx-preserve` keeps the status on the page instead of this (stale) copy.
            div.composer-status id=(COMPOSER_STATUS_ID) hx-preserve aria-live="polite" { (status) }
        }
    }
}

/// The line under the composer: a stop button (posting to `cancel`) while a turn runs, what
/// the chat runs on when there is no model picker (`runs_on`), and that everything is logged.
pub fn composer_status(cancel: &str, busy: bool, runs_on: Option<&str>) -> Markup {
    html! {
        @if busy {
            span.working { span.icon.spin aria-hidden="true" {} "Working on it" }
            button.link type="submit" formaction=(cancel) hx-post=(cancel) hx-swap="none" formnovalidate { "Stop" }
            span.sep aria-hidden="true" { "·" }
        }
        span {
            @if let Some(b) = runs_on { "Runs on " (b) ". " }
            "Everything is logged."
        }
    }
}

/// DOM id of the slot that holds the model chip (updated live).
pub const MODEL_CHIP_ID: &str = "model-chip";
/// DOM id of the model picker popover.
pub const MODEL_PICKER_ID: &str = "model-picker";

/// One model in the picker.
pub struct PickerModel {
    /// Form value (`choice`): `<backend>` or `<backend> <model>`.
    pub value: String,
    pub label: String,
    pub current: bool,
}

/// One backend (provider) in the picker: its mark on the rail and its models.
pub struct PickerBackend {
    /// The backend's name: the rail's filter key, never shown.
    pub name: String,
    pub label: String,
    /// The letter in its mark.
    pub letter: String,
    pub models: Vec<PickerModel>,
}

/// One reasoning level of the current model.
pub struct PickerLevel {
    /// Form value (`effort`); empty: the agent's default.
    pub value: String,
    pub label: String,
    pub current: bool,
}

/// The model picker of a chat (2026-11-20, owner-requested): a chip in the composer bar
/// ("Opus 5.5 · High") opens a popover above it with a rail of backends (Favorites first),
/// a searchable list of the selected backend's models and the current model's reasoning
/// levels. Only usable backends are in it. Picking posts the picker's own form (`choice`, or
/// `effort` with the hidden `current`) to `action`; it applies from the next turn. Built from
/// the API's `BackendView`s in `agents.rs`.
pub struct ModelPicker {
    pub action: String,
    /// What the chat runs on, for the chip: "Opus 5.5 · High".
    pub chip: String,
    /// The current backend's mark letter (None: no backend is usable).
    pub letter: Option<String>,
    /// The `choice` of the current model (sent with a reasoning level).
    pub current: String,
    pub backends: Vec<PickerBackend>,
    /// The current model's reasoning levels (empty: none; the column is left out).
    pub levels: Vec<PickerLevel>,
    /// Why the last switch was refused.
    pub error: Option<String>,
}

/// The chip in the composer bar that opens the picker (Popover API: no script needed).
pub fn model_chip(p: &ModelPicker) -> Markup {
    html! {
        button.model-chip type="button" popovertarget=(MODEL_PICKER_ID) aria-haspopup="dialog"
            title="Choose the model" {
            @if let Some(l) = &p.letter { (provider_mark(l, false)) }
            span.visually-hidden { "Model: " }
            span.chip-text { (p.chip) }
            span.caret aria-hidden="true" {}
        }
    }
}

fn star_icon() -> Markup {
    html! {
        svg.star viewBox="0 0 16 16" aria-hidden="true" focusable="false" {
            path d="M8 1.8l1.85 3.9 4.25.55-3.1 2.95.8 4.2L8 11.35 4.2 13.4l.8-4.2L1.9 6.25l4.25-.55z" {}
        }
    }
}

fn check_icon() -> Markup {
    html! {
        svg.check viewBox="0 0 16 16" aria-hidden="true" focusable="false" {
            path d="M3 8.5 6.5 12 13 4.5" {}
        }
    }
}

/// The picker popover (see [`ModelPicker`]). Without script every backend's models show in
/// groups and each pick is a plain form post; melissa.js adds the rail filter, search,
/// favorites, Ctrl+1..9 and "Show more".
pub fn model_picker(p: &ModelPicker) -> Markup {
    let target = format!("#{MODEL_PICKER_ID}");
    html! {
        div.model-picker id=(MODEL_PICKER_ID) popover="auto" role="dialog" aria-label="Choose a model"
            data-error[p.error.is_some()] {
            form.picker method="post" action=(p.action) hx-post=(p.action) hx-target=(target)
                hx-swap="outerHTML" {
                input type="hidden" name="current" value=(p.current);
                @if !p.backends.is_empty() {
                    div.picker-rail role="group" aria-label="Providers" {
                        button.rail-item type="button" data-filter="favorites" aria-pressed="false" title="Favorites" {
                            (star_icon())
                            span.visually-hidden { "Favorites" }
                        }
                        @for b in &p.backends {
                            button.rail-item type="button" data-filter=(b.name)
                                aria-pressed=(if b.models.iter().any(|m| m.current) { "true" } else { "false" })
                                title=(b.label) {
                                (provider_mark(&b.letter, false))
                                span.visually-hidden { (b.label) }
                            }
                        }
                    }
                }
                div.picker-main {
                    @if let Some(e) = &p.error { p.picker-error role="alert" { (e) } }
                    @if p.backends.is_empty() {
                        p.picker-note {
                            "No provider is ready yet. Set one up in "
                            a href="/settings/providers" { "Settings → Providers" } "."
                        }
                    } @else {
                        label.visually-hidden for="picker-search" { "Search models" }
                        input.picker-search #picker-search type="search" placeholder="Search models…"
                            autocomplete="off" spellcheck="false";
                        div.picker-scroll {
                            @for b in &p.backends {
                                section.picker-group data-backend=(b.name) aria-label=(b.label) {
                                    h3.picker-group-head { (b.label) }
                                    ul.picker-list {
                                        @for m in &b.models {
                                            li.picker-row data-backend=(b.name) data-value=(m.value)
                                                data-name=(m.label.to_lowercase()) {
                                                button.pick type="submit" name="choice" value=(m.value)
                                                    aria-current=[m.current.then_some("true")] {
                                                    (provider_mark(&b.letter, false))
                                                    span.name { (m.label) }
                                                    @if m.current { (check_icon()) }
                                                    kbd.key aria-hidden="true" {}
                                                }
                                                button.fav type="button" data-fav=(m.value)
                                                    aria-pressed="false" title="Favorite" {
                                                    (star_icon())
                                                    span.visually-hidden { "Favorite " (m.label) }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            p.picker-empty data-empty="search" hidden { "No models match." }
                            p.picker-empty data-empty="favorites" hidden { "Star a model to keep it here." }
                        }
                        button.picker-more.link type="button" hidden { "Show more" }
                    }
                }
                @if !p.levels.is_empty() {
                    div.picker-levels role="group" aria-labelledby="picker-levels-head" {
                        h3 #picker-levels-head { "Reasoning" }
                        @for l in &p.levels {
                            button.level type="submit" name="effort" value=(l.value)
                                aria-current=[l.current.then_some("true")] {
                                span { (l.label) }
                                @if l.current { (check_icon()) }
                            }
                        }
                    }
                }
            }
        }
    }
}

// --- Generic pieces (task pages) -------------------------------------------------------

/// A titled card.
pub fn card(title: &str, content: Markup) -> Markup {
    html! {
        section.card {
            h2 { (title) }
            (content)
        }
    }
}

/// A data table. Shows `empty` when there are no rows.
pub fn table(headers: &[&str], rows: Vec<Vec<Markup>>, empty: &str) -> Markup {
    html! {
        @if rows.is_empty() {
            p.empty { (empty) }
        } @else {
            div.table-wrap {
                table {
                    thead { tr { @for h in headers { th scope="col" { (h) } } } }
                    tbody {
                        @for row in rows {
                            tr { @for workspace in row { td { (workspace) } } }
                        }
                    }
                }
            }
        }
    }
}

/// A breadcrumb ("Agent tasks / Task #3"): links to the pages above, then the current one.
/// Same look as the Settings breadcrumb; the way back on phones, where the rail is short.
pub fn crumbs(trail: &[(&str, &str)], current: &str) -> Markup {
    html! {
        nav.crumbs aria-label="Breadcrumb" {
            ol {
                @for (href, label) in trail { li { a href=(href) { (label) } } }
                li aria-current="page" { (current) }
            }
        }
    }
}

/// A short coloured status label.
pub fn badge(label: &str, tone: Tone) -> Markup {
    html! { span class={ "badge " (tone.class()) } { (label) } }
}

/// A one-line message box.
pub fn notice(text: &str, tone: Tone) -> Markup {
    html! { p class={ "notice " (tone.class()) } role="status" { (text) } }
}

/// Inline monospaced text (commands, identifiers).
pub fn code(text: &str) -> Markup {
    html! { code { (text) } }
}

/// A plain link.
pub fn link(href: &str, text: &str) -> Markup {
    html! { a href=(href) { (text) } }
}

/// Text that is cut to `max` characters, with the full text as a tooltip.
pub fn truncated(text: &str, max: usize) -> Markup {
    let short: String = text.chars().take(max).collect();
    let cut = short.len() < text.len();
    html! { span title=(text) { (short) @if cut { "…" } } }
}

/// A region that refreshes itself from `url` every `seconds` while `live` is true.
/// The server returns the same region again, so polling stops once `live` turns false.
pub fn live_region(id: &str, url: &str, seconds: u32, live: bool, content: Markup) -> Markup {
    html! {
        @if live {
            div id=(id) hx-get=(url) hx-trigger={ "every " (seconds) "s" } hx-swap="outerHTML" {
                (content)
            }
        } @else {
            div id=(id) { (content) }
        }
    }
}

/// A region that refreshes itself from `url` every `seconds`, and at once when the page
/// receives the event `event` (an `HX-Trigger` response header from another request).
pub fn live_region_on(id: &str, url: &str, seconds: u32, event: &str, content: Markup) -> Markup {
    html! {
        div id=(id) hx-get=(url) hx-trigger={ "every " (seconds) "s, " (event) " from:body" }
            hx-swap="outerHTML" {
            (content)
        }
    }
}

/// One line of a log view.
pub struct LogLine<'a> {
    pub text: &'a str,
    pub is_error: bool,
}

/// Scrollable monospaced output.
pub fn log_view<'a>(lines: impl IntoIterator<Item = LogLine<'a>>) -> Markup {
    html! {
        pre.log {
            @for line in lines {
                @if line.is_error { span.log-err { (line.text) } } @else { span { (line.text) } }
                "\n"
            }
        }
    }
}

/// An option in a [`select_field`].
pub struct Choice<'a> {
    pub value: &'a str,
    pub label: &'a str,
    pub enabled: bool,
}

/// A labelled drop-down.
pub fn select_field(name: &str, label: &str, choices: &[Choice<'_>]) -> Markup {
    select_field_at(name, label, choices, None)
}

/// [`select_field`] with the choice whose value is `selected` chosen (e.g. in an edit form).
pub fn select_field_at(
    name: &str,
    label: &str,
    choices: &[Choice<'_>],
    selected: Option<&str>,
) -> Markup {
    html! {
        label.field {
            span { (label) }
            select name=(name) required {
                @for c in choices {
                    option value=(c.value) disabled[!c.enabled]
                        selected[selected == Some(c.value)] { (c.label) }
                }
            }
        }
    }
}

/// A labelled multi-line text input.
pub fn textarea_field(name: &str, label: &str, placeholder: &str, max_len: usize) -> Markup {
    html! {
        label.field {
            span { (label) }
            textarea name=(name) rows="4" placeholder=(placeholder) maxlength=(max_len) required {}
        }
    }
}

/// A form that posts to `action`. With htmx it swaps the response into `#target`;
/// without JavaScript it falls back to a normal POST.
pub fn post_form(action: &str, target: &str, submit_label: &str, fields: Markup) -> Markup {
    html! {
        form.stack method="post" action=(action) hx-post=(action) hx-target={ "#" (target) } hx-swap="outerHTML" {
            (fields)
            div { button.primary type="submit" { (submit_label) } }
        }
    }
}

/// A single-button action, e.g. "Remove", confirmed in the page (never a browser dialog):
/// the button opens `confirm` with a "Yes, <label>" button that posts. No script needed.
pub fn action_button(action: &str, target: &str, label: &str, confirm: &str) -> Markup {
    inline_confirm(
        false,
        label,
        confirm,
        html! {
            form method="post" action=(action) hx-post=(action) hx-target={ "#" (target) }
                hx-swap="outerHTML" {
                button.primary.danger type="submit" { "Yes, " (label.to_lowercase()) }
            }
        },
    )
}

/// The in-page confirmation behind [`action_button`] and the variable list's Remove: a
/// `<details>` whose summary is the action and whose body asks `question` next to `form`
/// (which holds the real submit button). `link`: the summary looks like a text link.
fn inline_confirm(link: bool, label: &str, question: &str, form: Markup) -> Markup {
    html! {
        details.confirm-inline.link[link] {
            summary { (label) }
            div.confirm-inline-body {
                span { (question) }
                (form)
            }
        }
    }
}

// --- Settings (2026-11-19) ---------------------------------------------------------------
//
// The Settings pages: a list of sections on the left (tabs on phones), a breadcrumb
// header, and each section's content. Built for more sections than Providers and Updates.

/// One section in the Settings navigation.
pub struct SettingsSection<'a> {
    pub href: &'a str,
    pub label: &'a str,
    pub current: bool,
}

/// The Settings frame: section navigation, breadcrumb ("Settings / Providers") and content.
pub fn settings_frame(sections: &[SettingsSection<'_>], current: &str, content: Markup) -> Markup {
    settings_frame_at(sections, current, None, content)
}

/// [`settings_frame`] for a page below a section (e.g. one task under Developer): the
/// breadcrumb reads Settings / <section, a link> / `page`.
pub fn settings_frame_at(
    sections: &[SettingsSection<'_>],
    current: &str,
    page: Option<&str>,
    content: Markup,
) -> Markup {
    let section_href = sections.iter().find(|s| s.label == current).map(|s| s.href);
    html! {
        div.settings {
            nav.settings-nav aria-label="Settings sections" {
                p.settings-title { "Settings" }
                ul {
                    @for s in sections {
                        li { a href=(s.href) aria-current=[s.current.then_some("page")] { (s.label) } }
                    }
                }
            }
            div.settings-main {
                nav.crumbs aria-label="Breadcrumb" {
                    ol {
                        li { a href="/settings" { "Settings" } }
                        @match (page, section_href) {
                            (Some(page), Some(href)) => {
                                li { a href=(href) { (current) } }
                                li aria-current="page" { (page) }
                            }
                            _ => { li aria-current="page" { (current) } }
                        }
                    }
                }
                (content)
            }
        }
    }
}

/// A heading row with an action on the right, e.g. "Providers" and "Checked just now · Check
/// again". `action` posts to `action_url`, and htmx swaps the response into `#target`.
pub fn settings_head(
    title: &str,
    lead: &str,
    checked: &str,
    action_url: &str,
    target: &str,
    action: &str,
) -> Markup {
    html! {
        header.settings-head {
            div {
                h1 { (title) }
                p.lead { (lead) }
            }
            form.checked method="post" action=(action_url) hx-post=(action_url)
                hx-target={ "#" (target) } hx-swap="outerHTML" {
                span { (checked) }
                button.link type="submit" { (action) }
            }
        }
    }
}

/// Two panes in one card: a list to pick from and the picked item's details. Stacks on
/// phones. Has `id` so forms inside can swap the whole thing.
pub fn split_card(id: &str, label: &str, list: Markup, detail: Markup) -> Markup {
    html! {
        section.split-card id=(id) aria-label=(label) {
            div.split-list { (list) }
            div.split-detail { (detail) }
        }
    }
}

/// A provider's mark: a hexagon with a letter (no vendor logos).
pub fn provider_mark(letter: &str, muted: bool) -> Markup {
    html! {
        span class={ "pmark" @if muted { " muted" } } aria-hidden="true" {
            svg viewBox="0 0 24 24" focusable="false" {
                polygon points="12,1.5 21.1,6.75 21.1,17.25 12,22.5 2.9,17.25 2.9,6.75" {}
            }
            span { (letter) }
        }
    }
}

/// A styled native checkbox that saves on change (htmx) or with its own button (no JS).
pub struct Toggle<'a> {
    pub action: &'a str,
    pub target: &'a str,
    /// What it switches, for screen readers ("Use Claude Code").
    pub label: &'a str,
    pub on: bool,
    /// Why it can't be switched (shown as the tooltip), if it can't.
    pub locked: Option<&'a str>,
}

pub fn toggle(t: &Toggle<'_>) -> Markup {
    html! {
        form.toggle method="post" action=(t.action) hx-post=(t.action) hx-trigger="change"
            hx-target={ "#" (t.target) } hx-swap="outerHTML" title=[t.locked] {
            label {
                input type="checkbox" name="enabled" value="on" role="switch" checked[t.on]
                    disabled[t.locked.is_some()];
                span.visually-hidden { (t.label) }
            }
            button.toggle-save.secondary type="submit" disabled[t.locked.is_some()] { "Save" }
        }
    }
}

/// One entry in a [`split_card`] list.
pub struct PickRow<'a> {
    pub href: &'a str,
    pub mark: Markup,
    pub name: &'a str,
    /// e.g. "v2.1.3" (empty: none).
    pub version: &'a str,
    pub status: &'a str,
    pub tone: Tone,
    pub current: bool,
    pub toggle: Option<Markup>,
}

/// The list of a [`split_card`], with an optional footer (e.g. how to add one).
pub fn pick_list(label: &str, rows: &[PickRow<'_>], footer: Option<Markup>) -> Markup {
    html! {
        ul.pick-list aria-label=(label) {
            @for r in rows {
                li class={ "pick" @if r.current { " current" } } {
                    a href=(r.href) aria-current=[r.current.then_some("true")] {
                        (r.mark)
                        span.pick-text {
                            span.pick-name { (r.name) @if !r.version.is_empty() { " " span.pick-version { (r.version) } } }
                            span class={ "pick-status " (r.tone.class()) } { (r.status) }
                        }
                    }
                    @if let Some(t) = &r.toggle { (t) }
                }
            }
        }
        @if let Some(f) = footer { div.pick-foot { (f) } }
    }
}

/// A detail pane's header: mark, name, a quiet line and badges.
pub fn detail_head(mark: Markup, name: &str, sub: &str, badges: Markup) -> Markup {
    html! {
        header.detail-head {
            (mark)
            div {
                h2 { (name) }
                p.quiet { (sub) }
            }
            div.badges { (badges) }
        }
    }
}

/// Label/value rows.
pub fn facts(rows: &[(&str, Markup)]) -> Markup {
    html! {
        dl.facts {
            @for (k, v) in rows {
                div { dt { (k) } dd { (v) } }
            }
        }
    }
}

/// A titled group inside a detail pane ("Runtime", "Environment").
pub fn group(title: &str, hint: Option<&str>, content: Markup) -> Markup {
    html! {
        section.group {
            h3 { (title) }
            @if let Some(h) = hint { p.quiet { (h) } }
            (content)
        }
    }
}

/// A one-line text input. `kind`: `text` or `password`.
pub fn text_field(
    name: &str,
    label: &str,
    value: &str,
    placeholder: &str,
    kind: &str,
    hint: Option<&str>,
) -> Markup {
    html! {
        label.field {
            span { (label) }
            input type=(kind) name=(name) value=(value) placeholder=(placeholder)
                autocomplete="off" spellcheck="false" maxlength="4096";
            @if let Some(h) = hint { small.hint { (h) } }
        }
    }
}

/// A multi-line input with a value, one item per line.
pub fn lines_field(name: &str, label: &str, value: &str, hint: Option<&str>) -> Markup {
    html! {
        label.field {
            span { (label) }
            textarea name=(name) rows="3" spellcheck="false" autocomplete="off" { (value) }
            @if let Some(h) = hint { small.hint { (h) } }
        }
    }
}

/// A form that saves settings and swaps `#target` (htmx) or posts normally.
pub fn settings_form(action: &str, target: &str, submit: &str, fields: Markup) -> Markup {
    html! {
        form.stack.settings-form method="post" action=(action) hx-post=(action)
            hx-target={ "#" (target) } hx-swap="outerHTML" {
            (fields)
            div { button.secondary type="submit" { (submit) } }
        }
    }
}

/// An environment variable in a [`var_list`].
pub struct VarRow<'a> {
    pub name: &'a str,
    /// None: a secret (shown as "set", never echoed).
    pub value: Option<&'a str>,
    pub remove_action: &'a str,
}

/// Variables with a remove button each; secret values show only "set".
pub fn var_list(rows: &[VarRow<'_>], target: &str, empty: &str) -> Markup {
    html! {
        @if rows.is_empty() {
            p.quiet { (empty) }
        } @else {
            ul.vars {
                @for r in rows {
                    li {
                        code.var-name { (r.name) }
                        @match r.value {
                            Some(v) => span.var-value { (v) },
                            None => span.var-secret { "set" span.visually-hidden { " (hidden)" } },
                        }
                        div.var-remove {
                            (inline_confirm(true, "Remove", &format!("Remove {}?", r.name), html! {
                                form method="post" action=(r.remove_action) hx-post=(r.remove_action)
                                    hx-target={ "#" (target) } hx-swap="outerHTML" {
                                    input type="hidden" name="name" value=(r.name);
                                    button.primary.danger type="submit" {
                                        "Yes, remove" span.visually-hidden { " " (r.name) }
                                    }
                                }
                            }))
                        }
                    }
                }
            }
        }
    }
}

/// A command to type, with a copy button (shown where the clipboard works).
pub fn command_block(cmd: &str) -> Markup {
    html! {
        div.md-code.command {
            pre tabindex="0" { code { (cmd) } }
            button.copy-code.secondary type="button" aria-label="Copy command" { "Copy" }
        }
    }
}

/// An explicit yes for something big (a reboot): what happens, and two buttons. The
/// confirm button posts `confirm=yes`; "Keep as is" goes back to `cancel_href`.
pub struct ConfirmCard<'a> {
    pub title: &'a str,
    pub text: &'a str,
    pub action: &'a str,
    pub target: &'a str,
    pub confirm: &'a str,
    pub cancel_href: &'a str,
}

pub fn confirm_card(c: &ConfirmCard<'_>) -> Markup {
    html! {
        section.approval.destructive.confirm role="alertdialog" aria-labelledby="confirm-h" {
            h3 #confirm-h { (c.title) }
            p { (c.text) }
            div.actions {
                form.inline method="post" action=(c.action) hx-post=(c.action)
                    hx-target={ "#" (c.target) } hx-swap="outerHTML" {
                    input type="hidden" name="confirm" value="yes";
                    button.primary.danger type="submit" { (c.confirm) }
                }
                a.button-like href=(c.cancel_href) { "Keep as is" }
            }
        }
    }
}

/// A long step with a known size (a download): what it is, how far ("45 of 141 MB") and a
/// native progress bar.
pub fn progress_bar(label: &str, text: &str, done: u64, total: u64) -> Markup {
    html! {
        div.meter {
            div.meter-head { span { (label) } span.meter-text { (text) } }
            progress max=(total.max(1)) value=(done.min(total)) aria-label=(label) { (text) }
        }
    }
}

/// A form with one button (no confirmation), e.g. "Check for updates".
pub fn plain_action(action: &str, target: &str, label: &str, primary: bool) -> Markup {
    html! {
        form.inline method="post" action=(action) hx-post=(action)
            hx-target={ "#" (target) } hx-swap="outerHTML" {
            button class=(if primary { "primary" } else { "secondary" }) type="submit" { (label) }
        }
    }
}

/// A row of buttons.
pub fn actions(items: Markup) -> Markup {
    html! { div.action-row { (items) } }
}

/// A region with a stable id that forms swap.
pub fn region(id: &str, content: Markup) -> Markup {
    html! { div id=(id) { (content) } }
}

/// Something that stays out of the way until asked for ("Details", "Use an API key
/// instead"): a native `<details>`, keyboard and screen-reader friendly, no script.
pub fn disclosure(summary: &str, open: bool, content: Markup) -> Markup {
    html! {
        details.disclosure open[open] {
            summary { (summary) }
            div.disclosure-body { (content) }
        }
    }
}

/// A link to a vendor's page (a sign-in), styled as the primary button. Opens in a new tab
/// without handing over this page (`noopener noreferrer`). Only for http(s) URLs Melissa
/// checked (e.g. `install::claude_login_url`); anything else renders as plain text.
pub fn external_button(href: &str, label: &str) -> Markup {
    if !(href.starts_with("https://") || href.starts_with("http://")) {
        return html! { span { (label) } };
    }
    html! {
        a.button-like.primary href=(href) target="_blank" rel="noopener noreferrer" { (label) }
    }
}

/// One field and a primary button on one row, e.g. "Paste the code" + Continue. Swaps
/// `#target` (htmx) or posts normally.
pub fn one_field_form(action: &str, target: &str, submit: &str, field: Markup) -> Markup {
    html! {
        form.one-field method="post" action=(action) hx-post=(action)
            hx-target={ "#" (target) } hx-swap="outerHTML" {
            (field)
            button.primary type="submit" { (submit) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmations_stay_in_the_page() {
        let out = action_button("/x/remove", "t", "Remove", "Remove <this>?").into_string();
        assert!(!out.contains("hx-confirm"), "no browser dialogs: {out}");
        assert!(out.contains("<details class=\"confirm-inline\">"));
        assert!(out.contains("<summary>Remove</summary>"));
        assert!(out.contains("Remove &lt;this&gt;?"));
        assert!(out.contains("hx-post=\"/x/remove\""));
        assert!(out.contains(">Yes, remove</button>"));
    }

    #[test]
    fn text_is_escaped() {
        let out = card("<b>x</b>", notice("<script>alert(1)</script>", Tone::Error)).into_string();
        assert!(!out.contains("<script>"));
        assert!(!out.contains("<b>"));
        assert!(out.contains("&lt;script&gt;"));
    }

    #[test]
    fn agent_text_is_markdown_with_html_escaped() {
        let evil = "Hi <img src=x onerror=alert(1)>\n\n**bold** [link](http://example.com)\n- one\n- <b>two</b>";
        let out = lead_message(evil).into_string();
        assert!(!out.contains("<img"));
        assert!(!out.contains("<b>"));
        assert!(out.contains("&lt;img src=x onerror=alert(1)&gt;"), "{out}");
        assert!(out.contains("<strong>bold</strong>"), "{out}");
        assert!(out.contains(
            r#"<a href="http://example.com" target="_blank" rel="noopener noreferrer nofollow">link</a>"#
        ));
        assert!(
            out.contains("<li>one</li><li>&lt;b&gt;two&lt;/b&gt;</li>"),
            "{out}"
        );
        let out = relayed_message("A job from Melissa", "<b>x</b>").into_string();
        assert!(out.contains("&lt;b&gt;x&lt;/b&gt;"));
        assert!(out.contains("A job from Melissa"));
        let out = lead_message("run `<ls>` and `x").into_string();
        assert!(out.contains("run <code>&lt;ls&gt;</code> and `x"), "{out}");
    }

    /// Every real tag in `out` (text `<` is escaped, so each `<` starts one) is from the
    /// markdown allowlist, with allowed attributes only, and links go to http(s)/mailto.
    fn assert_inert(out: &str) {
        const TAGS: &[&str] = &[
            "div",
            "p",
            "br",
            "hr",
            "h4",
            "h5",
            "h6",
            "blockquote",
            "ul",
            "ol",
            "li",
            "pre",
            "code",
            "button",
            "a",
            "span",
            "em",
            "strong",
            "del",
            "table",
            "thead",
            "tbody",
            "tr",
            "th",
            "td",
            "input",
        ];
        const ATTRS: &[&str] = &[
            "class",
            "href",
            "target",
            "rel",
            "tabindex",
            "type",
            "aria-label",
            "start",
            "scope",
            "disabled",
            "checked",
        ];
        for tag in out.split('<').skip(1) {
            let tag = tag.split('>').next().unwrap_or_default();
            let tag = tag.strip_prefix('/').unwrap_or(tag);
            let (name, mut attrs) = tag.split_once(' ').unwrap_or((tag, ""));
            assert!(TAGS.contains(&name), "tag {name:?} in {out}");
            while !attrs.is_empty() {
                let end = attrs.find([' ', '=']).unwrap_or(attrs.len());
                let attr = &attrs[..end];
                assert!(ATTRS.contains(&attr), "attribute {attr:?} in {out}");
                attrs = &attrs[end..];
                if let Some(rest) = attrs.strip_prefix("=\"") {
                    let close = rest.find('"').unwrap_or(rest.len());
                    let value = &rest[..close];
                    if attr == "href" {
                        assert!(
                            ["http://", "https://", "mailto:"]
                                .iter()
                                .any(|p| value.to_ascii_lowercase().starts_with(p)),
                            "href {value:?} in {out}"
                        );
                    }
                    attrs = rest.get(close + 1..).unwrap_or_default();
                }
                attrs = attrs.trim_start();
            }
        }
    }

    #[test]
    fn markdown_xss_inputs_are_inert() {
        let inputs = [
            "<script>alert(1)</script>",
            "<div>\n<script>alert(1)</script>\n</div>",
            "<!-- x --><img src=x onerror=alert(1)>",
            "a <svg onload=alert(1)> b",
            "<style>body{display:none}</style>",
            "[x](javascript:alert(1))",
            "[x](JAVASCRIPT:alert(1))",
            "[x](  javascript:alert(1))",
            "[x](java&#x09;script:alert(1))",
            "[x](&#106;avascript:alert(1))",
            "[x](javascript&colon;alert(1))",
            "[x](data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==)",
            "[x](vbscript:msgbox)",
            "[x](/ui/cancel)",
            "[x](#f-1)",
            "[x][r]\n\n[r]: javascript:alert(1)",
            "<javascript:alert(1)>",
            "![x](https://example.com/a.png)",
            "![x](data:image/svg+xml,<svg onload=alert(1)>)",
            "[x](https://example.com \"title\\\" onmouseover=\\\"alert(1)\")",
            "[a\"b](https://example.com/\"onmouseover=\"alert(1))",
            "https://example.com/\"onmouseover=\"alert(1)",
            "`<script>`",
            "```html\n<script>alert(1)</script>\n```",
            "| <script> | b |\n|---|---|\n| <img src=x> | [x](javascript:1) |",
            "- [x] <b>done</b>",
            "> <iframe src=https://example.com>",
        ];
        for input in inputs {
            let out = markdown(input).into_string();
            assert_inert(&out);
        }
    }

    #[test]
    fn markdown_elements() {
        let out = markdown("# Title\n\n## Sub\n\n### Small").into_string();
        assert!(out.contains(r#"<h4 class="md-h1">Title</h4>"#), "{out}");
        assert!(out.contains(r#"<h5 class="md-h2">Sub</h5>"#));
        assert!(out.contains(r#"<h6 class="md-h3">Small</h6>"#));

        let out = markdown("```rust\nfn main() { 1 < 2 }\n```").into_string();
        assert!(
            out.contains("<pre tabindex=\"0\"><code>fn main() { 1 &lt; 2 }\n</code></pre>"),
            "{out}"
        );
        assert!(out.contains("copy-code"));

        let out = markdown("| a | b |\n|:-:|---|\n| 1 | 2 |").into_string();
        assert!(out.contains(r#"<div class="md-table" tabindex="0"><table><thead><tr><th scope="col" class="align-center">a</th><th scope="col">b</th></tr></thead><tbody><tr><td class="align-center">1</td><td>2</td></tr></tbody></table></div>"#), "{out}");

        let out =
            markdown("~~old~~ *em*\n> quote\n\n3. three\n4. four\n\n- [x] done\n- [ ] todo\n\n---")
                .into_string();
        assert!(out.contains("<del>old</del> <em>em</em>"), "{out}");
        assert!(
            out.contains("<blockquote><p>quote</p></blockquote>"),
            "{out}"
        );
        assert!(out.contains(r#"<ol start="3"><li>three</li>"#), "{out}");
        assert!(
            out.contains(
                r#"<input class="task" type="checkbox" disabled checked aria-label="Done">"#
            ),
            "{out}"
        );
        assert!(
            out.contains(r#"<input class="task" type="checkbox" disabled aria-label="Not done">"#)
        );
        assert!(out.contains("<hr>"));

        // Images are not loaded: a link with the alt text.
        let out = markdown("![the logo](https://example.com/logo.png)").into_string();
        assert!(out.contains(r#"<a href="https://example.com/logo.png" target="_blank" rel="noopener noreferrer nofollow" class="md-image"><span class="image-tag">Image: </span>the logo</a>"#), "{out}");

        // Line breaks inside a paragraph are kept (chat style).
        let out = markdown("a\nb").into_string();
        assert!(out.contains("<p>a<br>b</p>"), "{out}");
    }

    #[test]
    fn bare_urls_become_links() {
        let link = r#"<a href="http://nextcloud.example.com" target="_blank" rel="noopener noreferrer nofollow">http://nextcloud.example.com</a>"#;
        let out = markdown("Open http://nextcloud.example.com.").into_string();
        assert!(out.contains(&format!("Open {link}.")), "{out}");
        let out = markdown("`http://nextcloud.example.com`").into_string();
        assert!(!out.contains("<a "), "{out}");
        let out =
            user_message("see http://nextcloud.example.com, `http://x.example`").into_string();
        assert!(
            out.contains(&format!("see {link}, <code>http://x.example</code>")),
            "{out}"
        );
        let out = user_message("a\nb\n\nc <b>").into_string();
        assert!(out.contains("<p>a<br>b</p><p>c &lt;b&gt;</p>"), "{out}");
    }

    #[test]
    fn activity_detail_is_markdown_only_for_prose() {
        let out = activity_row(
            "Ada finished",
            Some("**done**"),
            true,
            ActivityState::Done,
            None,
        )
        .into_string();
        assert!(
            out.contains(
                r#"<div class="detail"><div class="md"><p><strong>done</strong></p></div></div>"#
            ),
            "{out}"
        );
        let out = activity_row(
            "Using a tool",
            Some("**x**"),
            false,
            ActivityState::Done,
            None,
        )
        .into_string();
        assert!(
            out.contains(r#"<span class="detail">**x**</span>"#),
            "{out}"
        );
    }

    #[test]
    fn live_region_stops_polling_when_not_live() {
        let live = live_region("t", "/x", 2, true, html! {}).into_string();
        let done = live_region("t", "/x", 2, false, html! {}).into_string();
        assert!(live.contains("hx-trigger=\"every 2s\""));
        assert!(!done.contains("hx-get"));
    }

    #[test]
    fn truncation_keeps_full_text_in_title() {
        let out = truncated("abcdef", 3).into_string();
        assert!(out.contains("title=\"abcdef\""));
        assert!(out.contains("abc…"));
    }

    fn card_for(state: ApprovalState, destructive: bool, permanent: bool) -> String {
        approval_card(&ApprovalCard {
            id: 7,
            target: "c-7",
            title: "Remove <Nextcloud>?",
            consequence: "Stops it.",
            reason: "destructive action",
            destructive,
            permanent,
            yes: "Remove",
            no: "Keep it",
            state,
            always: None,
        })
        .into_string()
    }

    #[test]
    fn approval_card_variants() {
        let pending = card_for(ApprovalState::Pending, false, false);
        assert!(pending.contains("class=\"approval\""));
        assert!(pending.contains("hx-post=\"/ui/confirmations/7/approve\""));
        assert!(pending.contains("hx-post=\"/ui/confirmations/7/deny\""));
        assert!(pending.contains("hx-target=\"#c-7\""));
        assert!(pending.contains("Remove &lt;Nextcloud&gt;?"));
        assert!(!pending.contains("Can't be undone"));
        assert!(!pending.contains("danger"));

        let destructive = card_for(ApprovalState::Pending, true, true);
        assert!(destructive.contains("approval destructive"));
        assert!(destructive.contains("Can't be undone."));
        assert!(destructive.contains("primary danger"));

        for (state, text) in [
            (ApprovalState::Approved, "You allowed this."),
            (ApprovalState::Denied, "You said no."),
            (ApprovalState::Withdrawn, "No longer needed"),
        ] {
            let out = card_for(state, true, true);
            assert!(out.contains(text));
            assert!(!out.contains("<button"), "{state:?}");
            assert!(!out.contains("Can't be undone"));
        }
    }

    #[test]
    fn app_tiles_are_hexagons_with_status() {
        let out = app_tile(&AppWorkspace {
            details: None,
            name: "Nextcloud",
            health: Health::Ok,
            status: "Running",
            detail: Some("Looked after by you"),
            url: Some("http://nextcloud.example.com"),
        })
        .into_string();
        assert!(out.starts_with("<li class=\"hex h-ok\">"));
        assert!(out.contains("href=\"http://nextcloud.example.com\""));
        assert!(out.contains("Looked after by you"));
        let add = add_app_tile("/?draft=Install").into_string();
        assert!(add.contains("hex hex-add"));
        assert!(add.contains("Add an app"));
        let strip = app_strip(
            "/ui/strip",
            &[StripTile {
                name: "Immich",
                health: Health::Busy,
            }],
            "Immich installing",
            Some("Disk 91% full"),
        )
        .into_string();
        assert!(strip.contains("mini h-busy"));
        assert!(strip.contains("hx-get=\"/ui/strip\""));
        assert!(strip.contains("Disk 91% full"));
    }

    #[test]
    fn oob_targets_the_inner_content() {
        let out = oob_inner("c-1", html! { "x" }).into_string();
        assert_eq!(out, "<div hx-swap-oob=\"innerHTML:#c-1\">x</div>");
    }
}
