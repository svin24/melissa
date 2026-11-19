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

/// Status colour of an app cell or a device dot.
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
    /// A conversation with a helper (bee).
    Helper,
    Hive,
    Waiting,
    Tasks,
    /// An archived chat with Melissa.
    PastChat,
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
                (item(Nav::Hive, "/hive", "Apps and devices", html! {}))
                (item(Nav::Waiting, "/approvals", "Waiting for you", waiting_count(waiting, false)))
            }
            section.rail-section aria-labelledby="conv-h" {
                h2 #conv-h { "Conversations" }
                ul.conversations {
                    li { a href="/" aria-current=[(current == Nav::Chat).then_some("page")] { "Melissa" } }
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
            a.rail-foot href="/tasks" aria-current=[(current == Nav::Tasks).then_some("page")] { "Agent tasks" }
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

// --- Apps as honeycomb cells ---------------------------------------------------------

/// One app as a hexagon cell.
pub struct AppCell<'a> {
    pub name: &'a str,
    pub health: Health,
    /// Short status, e.g. "Running".
    pub status: &'a str,
    /// Second line, e.g. "Looked after by you" or a problem.
    pub detail: Option<&'a str>,
    /// Where the app is served, if known.
    pub url: Option<&'a str>,
}

/// A hexagon cell for an app.
pub fn app_cell(cell: &AppCell<'_>) -> Markup {
    html! {
        li class={ "hex " (cell.health.class()) } {
            div.hex-inner {
                @if let Some(url) = cell.url {
                    a.hex-name href=(url) rel="noreferrer" { (cell.name) }
                } @else {
                    span.hex-name { (cell.name) }
                }
                span.hex-status { (cell.status) }
                @if let Some(detail) = cell.detail { span.hex-detail { (detail) } }
            }
        }
    }
}

/// The "Add an app" cell: a link into the chat.
pub fn add_app_cell(href: &str) -> Markup {
    html! {
        li.hex.hex-add {
            a.hex-inner href=(href) {
                span.plus aria-hidden="true" { "+" }
                span.hex-name { "Add an app" }
            }
        }
    }
}

/// A honeycomb of cells (from [`app_cell`] and [`add_app_cell`]).
pub fn honeycomb(label: &str, cells: Markup) -> Markup {
    html! { ul.honeycomb aria-label=(label) { (cells) } }
}

/// A mini cell in the app strip.
pub struct StripCell<'a> {
    pub name: &'a str,
    pub health: Health,
}

/// The compact app-state strip at the top of the chat. Refreshes itself from `url`.
pub fn app_strip(
    url: &str,
    cells: &[StripCell<'_>],
    summary: &str,
    warning: Option<&str>,
) -> Markup {
    html! {
        section.strip #app-strip aria-label="Your apps" hx-get=(url) hx-trigger="every 15s" hx-swap="outerHTML" {
            @if !cells.is_empty() {
                ul.mini-cells aria-hidden="true" {
                    @for c in cells {
                        li class={ "mini " (c.health.class()) } title=(c.name) {}
                    }
                }
            }
            p.strip-text {
                span { (summary) }
                @if let Some(w) = warning { span.strip-warn { (w) } }
            }
            a.strip-link href="/hive" { "Apps and devices" }
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
                ul.facts { @for f in d.facts { li { (f) } } }
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

/// Agent prose (the Queen's or a helper's reply, a job, a job's summary): Markdown with
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
    html! {
        div.msg.msg-user {
            h3.visually-hidden { "You" }
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

/// A message from the Queen (left, honey hexagon avatar, no bubble).
pub fn queen_message(text: &str) -> Markup {
    html! {
        div.msg.msg-queen {
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
                },
                ApprovalState::Approved => p.outcome.ok { "You allowed this." },
                ApprovalState::Denied => p.outcome { "You said no." },
                ApprovalState::Withdrawn => p.outcome { "No longer needed (it timed out or the job ended)." },
            }
        }
    }
}

/// Where a composer sends to: the Queen or one helper.
pub struct ComposerTarget<'a> {
    /// POST URL for a message (form field `text`).
    pub send: &'a str,
    /// POST URL that stops the running turn.
    pub cancel: &'a str,
    pub placeholder: &'a str,
}

/// The message box at the bottom of the chat. `draft` pre-fills it, `error` explains why
/// the last message was not sent.
pub fn composer(
    to: &ComposerTarget<'_>,
    draft: &str,
    error: Option<&str>,
    status: Markup,
) -> Markup {
    html! {
        form.composer id=(COMPOSER_ID) method="post" action=(to.send) hx-post=(to.send)
            hx-target={ "#" (COMPOSER_ID) } hx-swap="outerHTML" {
            @if let Some(e) = error { p.composer-error role="alert" { (e) } }
            div.composer-box {
                label.visually-hidden for="message" { "Message" }
                textarea #message name="text" rows="2" maxlength="16384" required
                    placeholder=(to.placeholder)
                    autofocus[draft.is_empty() && error.is_none()] { (draft) }
                button.send type="submit" {
                    svg viewBox="0 0 16 16" aria-hidden="true" focusable="false" { path d="M2.5 8h10M8.5 3.5L13 8l-4.5 4.5" {} }
                    span.visually-hidden { "Send" }
                }
            }
            div.composer-status id=(COMPOSER_STATUS_ID) aria-live="polite" { (status) }
        }
    }
}

/// The line under the composer: which backend, everything is logged, and a stop button
/// (posting to `cancel`) while a turn runs.
pub fn composer_status(cancel: &str, backend: Option<&str>, busy: bool) -> Markup {
    html! {
        @if busy {
            span.working { span.icon.spin aria-hidden="true" {} "Working on it" }
            button.link type="submit" formaction=(cancel) hx-post=(cancel) hx-swap="none" formnovalidate { "Stop" }
            span.sep aria-hidden="true" { "·" }
        }
        span {
            @match backend {
                Some(b) => { "Runs on " (b) ". " },
                None => { "Starts when you send a message. " },
            }
            "Everything is logged."
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
                            tr { @for cell in row { td { (cell) } } }
                        }
                    }
                }
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
    html! {
        label.field {
            span { (label) }
            select name=(name) required {
                @for c in choices {
                    option value=(c.value) disabled[!c.enabled] { (c.label) }
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

/// A single-button form, e.g. "Cancel". Asks for confirmation with htmx.
pub fn action_button(action: &str, target: &str, label: &str, confirm: &str) -> Markup {
    html! {
        form.inline method="post" action=(action) hx-post=(action) hx-target={ "#" (target) }
            hx-swap="outerHTML" hx-confirm=(confirm) {
            button.secondary type="submit" { (label) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let out = queen_message(evil).into_string();
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
        let out = queen_message("run `<ls>` and `x").into_string();
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
    fn app_cells_are_hexagons_with_status() {
        let out = app_cell(&AppCell {
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
        let add = add_app_cell("/?draft=Install").into_string();
        assert!(add.contains("hex hex-add"));
        assert!(add.contains("Add an app"));
        let strip = app_strip(
            "/ui/strip",
            &[StripCell {
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
