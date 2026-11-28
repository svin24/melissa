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

/// The tone of a badge, a notice or a status word (COMPONENT-SPEC 2.7): neutral, info, ok,
/// attention (honey, "look here") and caution (errors, danger). Colour only helps; the word
/// always carries the meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Info,
    Ok,
    Attention,
    Caution,
}

impl Tone {
    pub(crate) fn class(self) -> &'static str {
        match self {
            Self::Neutral => "tone-neutral",
            Self::Info => "tone-info",
            Self::Ok => "tone-ok",
            Self::Attention => "tone-attention",
            Self::Caution => "tone-caution",
        }
    }
}

/// What leads a badge's word: nothing, a dot in the tone's colour, or a spinner (working).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    None,
    Dot,
    Spin,
}

/// What a button looks like (docs/design/COMPONENT-SPEC.md 2.1, Penpot board DS-01).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonVariant {
    /// The one main action of a view or card.
    Primary,
    /// Other actions.
    Secondary,
    /// Toolbars, low-weight and icon-only actions: no fill until hovered.
    Ghost,
    /// The confirming step of a destructive action.
    Danger,
    /// The first click of a destructive action (it opens the in-page confirm).
    DangerQuiet,
    /// An inline, low-emphasis action that reads as a link.
    Link,
}

/// The size of a control: 28, 32 (the default) or 40px high.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    Sm,
    Md,
    Lg,
}

/// The one button component: its look, rendered as the element's classes so the same look
/// fits `<button>`, `<a>` (navigation), `<summary>` (an in-page confirm) and `<label>` (a file
/// input), each keeping its own attributes:
/// `button.(Btn::secondary()) type="submit" { "Save" }`. Simple buttons use [`submit_button`]
/// and [`link_button`]. Icon-only buttons ([`Btn::icon`], [`Btn::round`]) need an
/// `aria-label` or a visually hidden label, and a `title`. The CSS is `.btn` in melissa.css.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Btn {
    variant: ButtonVariant,
    size: Size,
    shape: BtnShape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BtnShape {
    Text,
    /// Square, a glyph only.
    Icon,
    /// Round, a glyph only.
    Round,
    /// Fully rounded ends around text: a chip (a suggestion, a picker trigger).
    Chip,
}

impl Btn {
    pub const fn new(variant: ButtonVariant) -> Self {
        Self {
            variant,
            size: Size::Md,
            shape: BtnShape::Text,
        }
    }
    pub const fn primary() -> Self {
        Self::new(ButtonVariant::Primary)
    }
    pub const fn secondary() -> Self {
        Self::new(ButtonVariant::Secondary)
    }
    pub const fn ghost() -> Self {
        Self::new(ButtonVariant::Ghost)
    }
    pub const fn danger() -> Self {
        Self::new(ButtonVariant::Danger)
    }
    pub const fn danger_quiet() -> Self {
        Self::new(ButtonVariant::DangerQuiet)
    }
    pub const fn link() -> Self {
        Self::new(ButtonVariant::Link)
    }
    pub const fn size(self, size: Size) -> Self {
        Self { size, ..self }
    }
    pub const fn sm(self) -> Self {
        self.size(Size::Sm)
    }
    pub const fn lg(self) -> Self {
        self.size(Size::Lg)
    }
    /// A square button with a glyph only.
    pub const fn icon(self) -> Self {
        Self {
            shape: BtnShape::Icon,
            ..self
        }
    }
    /// A round button with a glyph only.
    pub const fn round(self) -> Self {
        Self {
            shape: BtnShape::Round,
            ..self
        }
    }
    /// A chip (COMPONENT-SPEC 2.7): an interactive pill, usually small.
    pub const fn chip(self) -> Self {
        Self {
            shape: BtnShape::Chip,
            ..self
        }
    }

    fn classes(self) -> String {
        let variant = match self.variant {
            ButtonVariant::Primary => "btn-primary",
            ButtonVariant::Secondary => "btn-secondary",
            ButtonVariant::Ghost => "btn-ghost",
            ButtonVariant::Danger => "btn-danger",
            ButtonVariant::DangerQuiet => "btn-danger-quiet",
            ButtonVariant::Link => "btn-link",
        };
        let size = match self.size {
            Size::Sm => " btn-sm",
            Size::Md => "",
            Size::Lg => " btn-lg",
        };
        let shape = match self.shape {
            BtnShape::Text => "",
            BtnShape::Icon => " btn-icon",
            BtnShape::Round => " btn-round",
            BtnShape::Chip => " btn-chip",
        };
        format!("btn {variant}{size}{shape}")
    }
}

impl maud::Render for Btn {
    fn render_to(&self, buffer: &mut String) {
        buffer.push_str(&self.classes());
    }
}

/// A submit button for the enclosing form.
pub fn submit_button(look: Btn, label: &str) -> Markup {
    html! { button.(look) type="submit" { (label) } }
}

/// A link that looks like a button (navigation is always a link).
pub fn link_button(look: Btn, href: &str, label: &str) -> Markup {
    html! { a.(look) href=(href) { (label) } }
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
    /// The status dot's class (`dot h-ok`).
    pub(crate) fn class(self) -> &'static str {
        match self {
            Self::Ok => "h-ok",
            Self::Busy => "h-busy",
            Self::Problem => "h-problem",
            Self::Idle => "h-idle",
        }
    }

    /// How a badge shows it: running is ok with a dot, busy works (a spinner), a problem
    /// asks for attention, idle is neutral.
    pub(crate) fn tone_mark(self) -> (Tone, Mark) {
        match self {
            Self::Ok => (Tone::Ok, Mark::Dot),
            Self::Busy => (Tone::Attention, Mark::Spin),
            Self::Problem => (Tone::Attention, Mark::Dot),
            Self::Idle => (Tone::Neutral, Mark::Dot),
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
    /// Usage and resources.
    Usage,
}

/// DOM id of the composer's status line (updated live).
pub const COMPOSER_STATUS_ID: &str = "composer-status";
/// DOM id of the composer form.
pub const COMPOSER_ID: &str = "composer";
/// DOM id of the chat feed list.
pub const FEED_ID: &str = "feed";
/// DOM id of the list on the approvals page.
pub const APPROVALS_ID: &str = "approvals";
/// DOM id of the main column (in-page navigation swaps its content, see `crate::panel`).
pub const MAIN_ID: &str = "main";

fn head(title: &str) -> Markup {
    html! {
        meta charset="utf-8";
        meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover";
        // The light/dark choice (`looks`), else both: the browser follows the system.
        meta name="color-scheme" content=(crate::looks::current().theme.map_or("light dark", |t| t.as_str()));
        // Keep htmx from injecting inline styles, so the CSP can forbid them.
        meta name="htmx-config" content=r#"{"includeIndicatorStyles":false,"allowEval":false}"#;
        // melissa.js sends it with every htmx and fetch request (X-CSRF-Token).
        @if let Some(t) = crate::auth::csrf_token() { meta name="csrf-token" content=(t); }
        title { (title) " · Melissa" }
        link rel="preload" href="/assets/fonts/AtkinsonHyperlegibleNext-Variable.woff2" as="font" type="font/woff2" crossorigin;
        link rel="stylesheet" href="/assets/melissa.css";
        link rel="icon" href="/assets/mark.svg" type="image/svg+xml";
        script src="/assets/htmx.min.js" defer {}
        script src="/assets/htmx-ext-sse.min.js" defer {}
        script src="/assets/melissa.js" defer {}
    }
}

/// `data-theme` on `<html>`: the light/dark choice, absent to follow the system.
fn theme_attr() -> Option<&'static str> {
    crate::looks::current()
        .theme
        .map(crate::looks::Theme::as_str)
}

/// A full page with the side rail: banner, rail, main column.
pub fn app_page(title: &str, rail: Markup, main: Markup) -> Markup {
    html! {
        (DOCTYPE)
        // The sidebar's collapsed state and the light/dark choice come with the page
        // (`looks`), so nothing moves or flashes.
        html lang="en" data-theme=[theme_attr()]
            data-sidebar=[crate::looks::current().sidebar_collapsed.then_some("collapsed")] {
            head { (head(title)) }
            body {
                a.skip href="#main" { "Skip to content" }
                (banner())
                div.shell {
                    (rail)
                    main id=(MAIN_ID) tabindex="-1" { (main) }
                }
                (overlay_dialog())
            }
        }
    }
}

/// The main column's content alone, for in-page navigation (melissa.js swaps it into
/// `#main`): the page's title (htmx sets it), the content, and `rail_update`, the sidebar's
/// out-of-band update (the agent groups, with the open chat selected).
pub fn main_part(title: &str, rail_update: Markup, main: Markup) -> Markup {
    html! {
        title { (title) " · Melissa" }
        (main)
        (rail_update)
    }
}

/// A full page without the rail (task pages, errors).
pub fn page(title: &str, content: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" data-theme=[theme_attr()] {
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

/// The slim pre-alpha notice at the top of every page. Its close button hides it in this
/// browser (melissa.js remembers it in a cookie, so later pages come without it and nothing
/// flashes; without JavaScript it stays).
pub fn banner() -> Markup {
    if crate::looks::current().banner_dismissed {
        return html! {};
    }
    html! {
        div.banner #prealpha role="note" {
            p {
                strong { "Pre-alpha." }
                " Things will break. Don't trust Melissa with data you can't lose yet."
            }
            button.banner-close.(Btn::ghost().icon().sm()) type="button" data-dismiss="prealpha" title="Dismiss" data-script-only {
                span aria-hidden="true" { "×" }
                span.visually-hidden { "Dismiss" }
            }
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

/// Shown in place of the composer when the conversation cannot go on (its helper was
/// deleted): a short notice and a way back. `oob`: replaces the composer out of band.
pub fn composer_closed(text: &str, back: &str, back_label: &str, oob: bool) -> Markup {
    let swap = oob.then(|| format!("outerHTML:#{COMPOSER_ID}"));
    html! {
        div.composer.composer-closed hx-swap-oob=[swap] {
            (notice(text, Tone::Attention))
            (link(back, back_label))
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
/// shows while the list has no entries. `queue`: the chat's queued messages
/// ([`queued_messages`]), kept after the entries.
pub fn feed(
    id: &str,
    label: &str,
    stream_url: &str,
    entries: Markup,
    empty: Markup,
    queue: Option<Markup>,
) -> Markup {
    html! {
        div.feed-wrap hx-ext="sse" sse-connect=(stream_url) {
            ol.feed id=(id) sse-swap="feed" hx-swap="beforeend" aria-live="polite" aria-label=(label) {
                li.feed-empty { (empty) }
                (entries)
            }
            @if let Some(q) = queue {
                div.chat-queue id=(QUEUE_ID) aria-live="polite" { (q) }
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

/// The top of a chat page's feed when older entries are not loaded yet: melissa.js loads them
/// from `fetch` when it scrolls into view (or is clicked) and puts them in its place, with a new
/// marker if there are more. Without script the link opens the chat with every entry (`page`).
pub fn older_marker(fetch: &str, page: &str) -> Markup {
    html! {
        li.feed-older data-older=(fetch) {
            a href=(page) { "Load earlier messages" }
        }
    }
}

/// The bottom of a page of earlier messages (the older marker's link, without script): a link
/// to the next newer page.
pub fn newer_marker(page: &str) -> Markup {
    html! {
        li.feed-newer {
            a href=(page) { "Load newer messages" }
        }
    }
}

/// One of Melissa's earlier (archived) chats.
pub struct PastChatRow {
    pub href: String,
    pub title: String,
    pub when: String,
    pub turns: usize,
}

/// The list of earlier chats.
pub fn past_chat_list(rows: &[PastChatRow]) -> Markup {
    html! {
        ul.detail-list.past-chats {
            @for r in rows {
                li {
                    a.detail-main href=(r.href) {
                        span.name { (r.title) }
                        span.meta {
                            (r.when) " · " (r.turns)
                            (if r.turns == 1 { " message" } else { " messages" })
                        }
                    }
                }
            }
        }
    }
}

/// DOM id of the chat's queued messages (the stream replaces its content).
pub const QUEUE_ID: &str = "chat-queue";

/// A message the user sent while the agent works: it waits for the running turn to end.
pub struct QueuedView<'a> {
    pub text: &'a str,
    /// Names of the files that go with it.
    pub files: Vec<&'a str>,
    /// POST: into the running turn now ("Send now").
    pub steer: String,
    /// POST: back into the composer to change it (None: it has files, which can't go back).
    pub edit: Option<String>,
    /// POST: drop it.
    pub remove: String,
}

/// The user's queued messages, after the chat (owner design, 2026-11-21): each is the user's
/// bubble with a dashed outline and muted text (not sent yet), and under it one quiet row:
/// "Send now" (into the running turn), Edit (back into the composer) and Remove. Nothing
/// when none waits. Without script each is a plain form post.
pub fn queued_messages(items: &[QueuedView<'_>]) -> Markup {
    html! {
        @for q in items {
            div.msg.msg-user.msg-queued {
                h3.visually-hidden { "Queued message" }
                div.bubble {
                    @if !q.files.is_empty() {
                        ul.sent-files {
                            @for f in &q.files {
                                li { span.file-chip { (file_icon()) span.name { (f) } } }
                            }
                        }
                    }
                    @if !q.text.trim().is_empty() { (plain_text(q.text)) }
                }
                div.queue-actions {
                    form method="post" action=(q.steer) hx-post=(q.steer) hx-swap="none" {
                        (crate::components::csrf_field())
                        button.queue-send.(Btn::ghost().sm()) type="submit" { (arrow_up_icon()) "Send now" }
                    }
                    @if let Some(edit) = &q.edit {
                        form method="post" action=(edit) hx-post=(edit)
                            hx-target={ "#" (COMPOSER_ID) } hx-swap="outerHTML" {
                            (crate::components::csrf_field())
                            button.queue-icon.(Btn::ghost().icon().sm()) type="submit" title="Edit" aria-label="Edit" { (pencil_icon()) }
                        }
                    }
                    form method="post" action=(q.remove) hx-post=(q.remove) hx-swap="none" {
                        (crate::components::csrf_field())
                        button.queue-icon.(Btn::ghost().icon().sm()) type="submit" title="Remove" aria-label="Remove" { (cross_icon()) }
                    }
                }
            }
        }
    }
}

fn arrow_up_icon() -> Markup {
    html! {
        svg viewBox="0 0 16 16" aria-hidden="true" focusable="false" { path d="M8 13V3M3.5 7.5 8 3l4.5 4.5" {} }
    }
}

fn pencil_icon() -> Markup {
    html! {
        svg viewBox="0 0 16 16" aria-hidden="true" focusable="false" {
            path d="M10.5 2.5 13.5 5.5 5.5 13.5H2.5V10.5zM9 4l3 3" {}
        }
    }
}

fn cross_icon() -> Markup {
    html! {
        svg viewBox="0 0 16 16" aria-hidden="true" focusable="false" { path d="M4 4l8 8M12 4l-8 8" {} }
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

/// [`markdown`] for a document wrapped at a fixed width (Settings → About's credits): a single
/// line break in a paragraph reads as a space. The same safe tree and look.
pub fn markdown_document(text: &str) -> Markup {
    html! { div.md { (md_nodes(&markdown::parse_document(text))) } }
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
                button.copy-code.(Btn::secondary().sm()) type="button" aria-label="Copy code" { "Copy" }
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
            h3.msg-who { "You" }
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

// --- Details panel (2026-11-20, owner decision; restructured to the owner's design the same day)
//
// The right-hand panel of a chat page, "Agent details": the agent's on-device services (its
// apps and this device), its uploaded files, its permissions (approvals waiting and its
// "Always allow" rules), its recent activity, and its schedules when it has any. Wide screens:
// a column beside the chat (its collapse button hides it, remembered per browser). Narrow
// screens with script: a sheet over the chat, opened with the Details button (Esc or the
// collapse button shut it; focus moves in and back). Without script it is always shown: beside
// the chat, or below it on narrow screens. Live: the chat's stream swaps the file list and
// count, and the content of the services, permissions, activity and schedules sections when
// they change (the services also poll, for app status from the cluster).

/// DOM id of the details panel.
pub const DETAILS_ID: &str = "details-panel";
/// DOM id of the services section (it polls; the stream replaces its content).
pub const DETAILS_APPS_ID: &str = "details-apps";
/// DOM id of the schedules section (on/off swaps it; the stream replaces its content).
pub const DETAILS_SCHEDULES_ID: &str = "details-schedules";
/// DOM id of the permissions section (removing a rule swaps it; the stream replaces it).
pub const DETAILS_PERMISSIONS_ID: &str = "details-permissions";
/// DOM id of the recent activity section (the stream replaces it).
pub const DETAILS_ACTIVITY_ID: &str = "details-activity";
/// DOM id of the file panel's body (delete, upload, send and retention swap it; the stream
/// replaces its content).
pub const FILE_BODY_ID: &str = "file-box-body";
/// DOM id of the file count in the Files heading.
pub const FILE_COUNT_ID: &str = "file-count";

/// One app in the details panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailApp {
    pub name: String,
    /// Its details page (`/cluster/apps/{app}`).
    pub details: String,
    pub health: Health,
    /// "Running".
    pub status: String,
    /// Key facts ("TCP 25565", "Port 8080").
    pub facts: Option<String>,
    /// Who looks after it (Melissa's panel lists every app, grouped by this).
    pub keeper: Option<String>,
}

/// This device in the services section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailDevice {
    pub name: String,
    /// "Linux · This device".
    pub sub: String,
    pub health: Health,
    /// "Online".
    pub status: String,
    /// Storage almost full, ...
    pub warning: Option<String>,
}

/// The services section.
pub struct DetailApps<'a> {
    /// Where the section reloads itself from.
    pub url: &'a str,
    pub apps: &'a [DetailApp],
    pub empty: &'a str,
    pub device: Option<&'a DetailDevice>,
}

/// One schedule in the details panel.
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// An approval this agent waits for, in the Permissions section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailWaiting {
    /// "Network access".
    pub kind: String,
    /// "TCP 25565 · waiting for you".
    pub sub: String,
    /// The card in the chat.
    pub href: String,
}

/// An "Always allow" rule of this agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailRule {
    /// "Update Minecraft".
    pub what: String,
    /// POST here to remove it.
    pub remove: String,
}

/// The permissions section.
pub struct DetailPermissions<'a> {
    pub waiting: &'a [DetailWaiting],
    pub rules: &'a [DetailRule],
    pub flash: Option<(&'a str, Tone)>,
}

/// One line of recent activity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityLine {
    pub text: String,
    /// "10:42" (today) or "18 Nov".
    pub when: String,
    /// Attention (honey dot) rather than done (green).
    pub attention: bool,
}

/// Everything the details panel shows.
pub struct DetailsPanel<'a> {
    pub apps: DetailApps<'a>,
    pub files: &'a FilePanel<'a>,
    pub permissions: DetailPermissions<'a>,
    pub activity: &'a [ActivityLine],
    pub schedules: DetailSchedules<'a>,
    /// The agents this one talked with (the owner's; empty for anyone else).
    pub conversations: &'a [ConversationRow<'a>],
    /// A thread open in the panel's place (a page opened with `?thread=`, without script).
    pub thread: Option<&'a ThreadPanel<'a>>,
}

/// The Details button of a chat page (works with melissa.js; without script the panel is
/// always visible, so the button is hidden: `data-script-only`).
pub fn details_toggle() -> Markup {
    let open = !crate::looks::current().details_closed;
    html! {
        button.details-toggle.(Btn::ghost().icon()) type="button" aria-controls=(DETAILS_ID)
            aria-expanded=(if open { "true" } else { "false" })
            data-details-toggle data-script-only title="Agent details" {
            (glyph(Glyph::Sidebar))
            span.visually-hidden { "Agent details" }
        }
    }
}

/// The details panel beside a chat. On phones it is a bottom sheet with a grab handle, and
/// its header shows `who` (the agent's icon and a line under the title).
pub fn details_panel(p: &DetailsPanel<'_>, who: Option<&SheetWho<'_>>) -> Markup {
    html! {
        div.details-backdrop data-details-close hidden {}
        // Hidden on wide screens when this browser closed it (`looks`; phones ignore it).
        aside.details id=(DETAILS_ID) tabindex="-1"
            aria-labelledby=(if p.thread.is_some() { "thread-h" } else { "details-h" })
            data-view=[p.thread.is_some().then_some("thread")]
            data-closed[crate::looks::current().details_closed && p.thread.is_none()] {
            div.details-view {
            div.details-head {
                (sheet_handle())
                @if let Some(w) = who { (sheet_avatar(w)) }
                div.details-title {
                    h2 #details-h { "Agent details" }
                    @if let Some(w) = who { (sheet_sub(w)) }
                }
                button.details-close.(Btn::ghost().icon()) type="button" data-details-close
                    data-script-only title="Hide details" {
                    span.close-wide { (glyph(Glyph::Sidebar)) }
                    span.close-phone { (glyph(Glyph::Close)) }
                    span.visually-hidden { "Hide details" }
                }
            }
            (detail_conversations(p.conversations, false))
            section.details-section aria-labelledby="details-apps-h" {
                (detail_apps(&p.apps))
            }
            section.details-section aria-labelledby="details-files-h" {
                h3.details-label #details-files-h { "Uploaded files " (file_count(p.files.files.len(), false)) }
                (file_panel_body(p.files))
            }
            section.details-section aria-labelledby="details-permissions-h" {
                h3.details-label #details-permissions-h { "Permissions" }
                (detail_permissions(&p.permissions))
            }
            section.details-section aria-labelledby="details-activity-h" {
                h3.details-label #details-activity-h { "Recent activity" }
                (detail_activity(p.activity))
            }
            @if !p.schedules.schedules.is_empty() || p.schedules.flash.is_some() {
                section.details-section aria-labelledby="details-schedules-h" {
                    h3.details-label #details-schedules-h { "Schedules" }
                    (detail_schedules(&p.schedules))
                }
            }
            }
            // A thread opens here, in the panel's place (agent threads, 2026-11-24).
            div.thread-slot id=(THREAD_SLOT_ID) {
                @if let Some(t) = p.thread { (thread_panel(t)) }
            }
        }
    }
}

/// The services section's content (one card: the apps, then this device), polling for
/// changes like the app strip.
pub fn detail_apps(s: &DetailApps<'_>) -> Markup {
    html! {
        div id=(DETAILS_APPS_ID) hx-get=(s.url) hx-trigger="every 15s" hx-swap="outerHTML" {
            (detail_apps_inner(s))
        }
    }
}

/// What [`detail_apps`] holds (a live update replaces it: an app handed over, a helper gone).
pub fn detail_apps_inner(s: &DetailApps<'_>) -> Markup {
    let mut groups: Vec<Option<&str>> = Vec::new();
    for a in s.apps {
        if !groups.contains(&a.keeper.as_deref()) {
            groups.push(a.keeper.as_deref());
        }
    }
    html! {
            div.services-card {
                h3.services-head #details-apps-h {
                    span { "On-device services" }
                    span.count { (s.apps.len() + usize::from(s.device.is_some())) }
                }
                @if s.apps.is_empty() {
                    p.service-empty { (s.empty) }
                }
                @for g in &groups {
                    @if let Some(keeper) = g { p.service-group { (keeper) } }
                    ul.service-list {
                        @for a in s.apps.iter().filter(|a| a.keeper.as_deref() == *g) {
                            li.service {
                                (glyph(Glyph::Hexagon))
                                div.service-text {
                                    p.service-name { (a.name) }
                                    p.service-status {
                                        span class={ "dot " (a.health.class()) } aria-hidden="true" {}
                                        (a.status)
                                    }
                                    @if let Some(f) = &a.facts { p.service-facts { (f) } }
                                }
                                // The app's details (melissa.js opens them in the Apps modal).
                                a.more-button.(Btn::ghost().icon()) href=(a.details)
                                    title={ "More for " (a.name) } {
                                    (glyph(Glyph::More))
                                    span.visually-hidden { "More for " (a.name) }
                                }
                            }
                        }
                    }
                }
                @if let Some(d) = s.device {
                    div.service.device-service {
                        (glyph(Glyph::Server))
                        div.service-text {
                            p.service-name { (d.name) }
                            p.service-facts { (d.sub) }
                            @if let Some(w) = &d.warning { p.device-warn { (w) } }
                        }
                        span.service-state {
                            span class={ "dot " (d.health.class()) } aria-hidden="true" {}
                            (d.status)
                        }
                    }
                }
            }
    }
}

/// The permissions section's content (the response to removing a rule).
pub fn detail_permissions(p: &DetailPermissions<'_>) -> Markup {
    html! { div id=(DETAILS_PERMISSIONS_ID) { (detail_permissions_inner(p)) } }
}

/// What [`detail_permissions`] holds (a live update replaces it).
pub fn detail_permissions_inner(p: &DetailPermissions<'_>) -> Markup {
    let target = format!("#{DETAILS_PERMISSIONS_ID}");
    html! {
            @if let Some((text, tone)) = p.flash { (notice(text, tone)) }
            @if p.waiting.is_empty() && p.rules.is_empty() {
                p.quiet { "Nothing asked or allowed ahead." }
            }
            @if !p.waiting.is_empty() {
                ul.detail-list.permission-list {
                    @for w in p.waiting {
                        li.permission.waiting {
                            (glyph(Glyph::Warn))
                            div.detail-main {
                                span.name { (w.kind) }
                                span.meta { (w.sub) }
                            }
                            // To the permission card in the chat.
                            a.review href=(w.href) data-review { "Review" }
                        }
                    }
                }
            }
            @if !p.rules.is_empty() {
                ul.detail-list.permission-list {
                    @for r in p.rules {
                        li.permission {
                            (glyph(Glyph::Check))
                            div.detail-main {
                                span.name { (r.what) }
                                span.meta { "Always allowed" }
                            }
                            form method="post" action=(r.remove) hx-post=(r.remove)
                                hx-target=(target) hx-swap="outerHTML" {
                                (crate::components::csrf_field())
                                button.(Btn::link()) type="submit" aria-label={ "Stop always allowing: " (r.what) } { "Remove" }
                            }
                        }
                    }
                }
            }
    }
}

/// The recent activity section's content.
pub fn detail_activity(lines: &[ActivityLine]) -> Markup {
    html! { div id=(DETAILS_ACTIVITY_ID) { (detail_activity_inner(lines)) } }
}

/// What [`detail_activity`] holds (a live update replaces it).
pub fn detail_activity_inner(lines: &[ActivityLine]) -> Markup {
    html! {
            @if lines.is_empty() {
                p.quiet { "Nothing yet." }
            } @else {
                ul.activity-list {
                    @for l in lines {
                        li class={ "activity-line" @if l.attention { " attention" } } {
                            span.dot aria-hidden="true" {}
                            span.text { (l.text) }
                            span.when { (l.when) }
                        }
                    }
                }
            }
    }
}

/// The schedules section's content (the response to on/off).
pub fn detail_schedules(s: &DetailSchedules<'_>) -> Markup {
    html! { div id=(DETAILS_SCHEDULES_ID) { (detail_schedules_inner(s)) } }
}

/// What [`detail_schedules`] holds (a live update replaces it).
pub fn detail_schedules_inner(s: &DetailSchedules<'_>) -> Markup {
    let target = format!("#{DETAILS_SCHEDULES_ID}");
    html! {
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
                                (crate::components::csrf_field())
                                input type="hidden" name="enabled" value=(if sc.enabled { "off" } else { "on" });
                                button.(Btn::secondary()) type="submit" aria-label={
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

/// One file in a chat's file panel.
pub struct PanelFile {
    /// Download URL (same origin).
    pub href: String,
    pub name: String,
    /// `1.2 MiB`.
    pub size: String,
    /// `PNG`, `PDF`, or empty.
    pub kind: String,
    /// Shown as a preview (PNG, JPEG, GIF, WebP).
    pub image: bool,
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

/// Who added a file, as the card's note.
fn added_words(by: &str, whose: &str) -> String {
    if by == "You" {
        "Uploaded in this conversation".to_string()
    } else if by == whose {
        format!("Saved by {by}")
    } else {
        by.to_string()
    }
}

/// The file panel's body content (the stream replaces it): the upload button and how long
/// files are kept, then a card per file (a preview for images).
pub fn file_panel_inner(p: &FilePanel<'_>) -> Markup {
    let target = format!("#{FILE_BODY_ID}");
    html! {
        @if let Some((text, tone)) = p.flash { (notice(text, tone)) }
        // One row, no prose: the upload icon and how long files are kept (both act on change,
        // melissa.js `data-autosubmit`; without JavaScript their buttons do it).
        div.file-tools {
            form.file-upload method="post" action=(p.upload) enctype="multipart/form-data"
                hx-post=(p.upload) hx-target=(target) hx-swap="outerHTML" {
                (crate::components::csrf_field())
                input #box-upload .attach-input type="file" name="files" multiple required
                    data-autosubmit;
                label.attach.upload.(Btn::secondary().icon()) for="box-upload" title="Upload files" {
                    (upload_icon())
                    span.visually-hidden { "Upload files" }
                }
                noscript { button.(Btn::secondary()) type="submit" { "Upload" } }
            }
            form.file-keep method="post" action=(p.keep_action) hx-post=(p.keep_action)
                hx-target=(target) hx-swap="outerHTML" title=(p.keep) {
                (crate::components::csrf_field())
                label.select-wrap {
                    span.visually-hidden { "Keep files for" }
                    select.input name="keep" data-autosubmit {
                        @for (value, label) in p.keep_choices {
                            option value=(value) selected[value == p.keep_selected] { (label) }
                        }
                    }
                }
                noscript { button.(Btn::secondary()) type="submit" { "Save" } }
            }
        }
        // No empty-state prose (owner, 2026-11-20): the count in the heading says it.
        @if !p.files.is_empty() {
            ul.box-files {
                @for f in p.files {
                    li.file-card {
                        @if f.image {
                            a.file-preview href=(f.href) target="_blank" rel="noopener"
                                title={ "Open " (f.name) } {
                                img src=(f.href) alt="" loading="lazy" decoding="async";
                                span.file-expand aria-hidden="true" { (glyph(Glyph::Expand)) }
                                span.visually-hidden { "Open " (f.name) " full size" }
                            }
                        }
                        div.box-file-row {
                            (glyph(if f.image { Glyph::Image } else { Glyph::File }))
                            a.box-file href=(f.href) download=(f.name) title={ "Download " (f.name) } {
                                span.name { (f.name) }
                                span.meta {
                                    @if !f.kind.is_empty() { (f.kind) " · " }
                                    (f.size)
                                }
                            }
                            details.file-more {
                                summary.(Btn::ghost().icon()) title={ "More for " (f.name) } {
                                    span.visually-hidden { "More for " (f.name) }
                                    (glyph(Glyph::More))
                                }
                                div.file-more-body {
                                    a.(Btn::secondary()) href=(f.reference_href)
                                        data-insert=(f.reference) { "Reference in chat" }
                                    @if !p.send_to.is_empty() {
                                        form.file-send method="post" action=(f.send) hx-post=(f.send)
                                            hx-target=(target) hx-swap="outerHTML" {
                                            (crate::components::csrf_field())
                                            label.field {
                                                span { "Send to" }
                                                span.select-wrap {
                                                    select.input name="to" {
                                                        @for (key, name) in p.send_to {
                                                            option value=(key) { (name) }
                                                        }
                                                    }
                                                }
                                            }
                                            label.check {
                                                input type="checkbox" name="mode" value="move";
                                                span { "Move (remove it here)" }
                                            }
                                            button.(Btn::secondary()) type="submit" { "Send" }
                                        }
                                    }
                                    (action_button(
                                        Btn::danger_quiet(),
                                        &f.delete,
                                        FILE_BODY_ID,
                                        "Delete",
                                        &format!("Delete {}? It can't be brought back.", f.name),
                                    ))
                                }
                            }
                        }
                        p.file-note { (added_words(&f.by, p.whose)) " · " (f.added) }
                    }
                }
            }
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

fn upload_icon() -> Markup {
    html! {
        svg viewBox="0 0 16 16" aria-hidden="true" focusable="false" {
            path d="M8 10.5V2.5M4.8 5.7 8 2.5l3.2 3.2M2.5 10.5v2a1 1 0 0 0 1 1h9a1 1 0 0 0 1-1v-2" {}
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

pub(crate) fn activity_icon(state: ActivityState) -> Markup {
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

/// The date and time above a part of the chat ("Today, 10:42"): a quiet line, no rule.
pub fn chat_date(text: &str) -> Markup {
    html! { p.chat-date { (text) } }
}

/// A quiet divider across the chat with a short label (e.g. "Earlier").
pub fn chat_divider(text: &str) -> Markup {
    html! {
        div.divider role="separator" { span { (text) } }
    }
}

/// What a decided permission card says instead of its buttons.
pub(crate) fn approval_state_outcome(state: ApprovalState) -> Markup {
    html! {
        @match state {
            ApprovalState::Pending => {},
            ApprovalState::Approved => p.outcome.ok { "You allowed this." },
            ApprovalState::Denied => p.outcome { "You declined." },
            ApprovalState::Withdrawn => p.outcome { "No longer needed (it timed out or the job ended)." },
        }
    }
}

/// Where a composer sends to: the lead agent or one helper.
pub struct ComposerTarget<'a> {
    /// POST URL for a message (form field `text`).
    pub send: &'a str,
    /// Names the chat for its unsent text (`data-draft`; melissa.js keeps it per browser).
    pub draft_key: &'a str,
    /// While the agent works, Enter steers the message into the running turn (double Enter
    /// queues it); false: Enter queues (double Enter steers). Settings → Developer → Chat.
    pub enter_steers: bool,
    pub placeholder: &'a str,
    /// POST URL that turns a recording into text (voice input on and ready), if any.
    pub voice: Option<&'a str>,
    /// A line under the box on phones ([`composer_caption`]), if any.
    pub caption: Option<&'a str>,
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
            data-draft=(to.draft_key)
            enctype="multipart/form-data" hx-encoding="multipart/form-data"
            hx-target={ "#" (COMPOSER_ID) } hx-swap="outerHTML" {
            (crate::components::csrf_field())
            // While the agent works: `queue` or `steer` (melissa.js sets it for Enter and
            // double Enter); empty follows the owner's setting.
            input type="hidden" name="mode" value="";
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
                    button.remove.(Btn::ghost().icon().sm()) type="button" aria-label="Remove" { span aria-hidden="true" { "×" } }
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
                    label.attach.(Btn::ghost().icon().lg()) for="attach" title="Attach files" {
                        (paperclip_icon())
                        span.visually-hidden { "Attach files" }
                    }
                    span.bar-gap {}
                    span id=(MODEL_CHIP_ID) { (chip) }
                    (composer_prefs(to.enter_steers, to.voice, false))
                    button.send.(Btn::primary().round().lg()) type="submit" title="Send" {
                        svg viewBox="0 0 16 16" aria-hidden="true" focusable="false" { path d="M8 13V3M3.5 7.5 8 3l4.5 4.5" {} }
                        span.visually-hidden { "Send" }
                    }
                }
            }
            // The event stream owns the status (busy, Stop): it arrives in order with the
            // turn's events. The response to a send can land after the turn already ended, so
            // `hx-preserve` keeps the status on the page instead of this (stale) copy.
            div.composer-status id=(COMPOSER_STATUS_ID) hx-preserve aria-live="polite" { (status) }
            div id=(COMPOSER_CAPTION_ID) { @if let Some(c) = to.caption { (composer_caption(c)) } }
        }
    }
}

/// DOM id of the composer's settings: what Enter does while the agent works and the mic
/// (a live update replaces it when they change in Settings).
pub const COMPOSER_PREFS_ID: &str = "composer-prefs";
/// DOM id of the slot for the line under the composer ([`composer_caption`]).
pub const COMPOSER_CAPTION_ID: &str = "composer-caption";
/// DOM id of the slot for the notice above the composer ([`composer_notice`]).
pub const COMPOSER_NOTICE_ID: &str = "composer-notice";

/// The composer's settings (in its bar, no box of its own): `data-enter` says what Enter does
/// while the agent works (`steer` or `queue`; melissa.js reads it on each Enter), and the mic
/// when voice input is on and ready (`voice`: where recordings go). `oob`: as an out-of-band
/// replacement in a live update.
pub fn composer_prefs(enter_steers: bool, voice: Option<&str>, oob: bool) -> Markup {
    html! {
        span id=(COMPOSER_PREFS_ID) data-enter=(if enter_steers { "steer" } else { "queue" })
            hx-swap-oob=[oob.then_some("true")] {
            @if let Some(url) = voice {
                // Voice input: melissa.js shows it (recording needs script), records,
                // sends the clip to `url` and puts the text in the box to edit; nothing
                // is sent by itself. Where the browser can't record (plain http on the
                // LAN) a click says why.
                button.voice.(Btn::ghost().lg()) #voice type="button" data-script-only data-url=(url) aria-pressed="false"
                    title="Speak instead of typing" {
                    (mic_icon())
                    span.voice-time aria-hidden="true" {}
                    span.visually-hidden.voice-label { "Speak instead of typing" }
                }
            }
        }
    }
}

/// The slot above the composer for a notice about it (no provider ready), empty otherwise.
/// A live update fills or empties it ([`oob_inner`]).
pub fn composer_notice(notice: Option<Markup>) -> Markup {
    html! { div id=(COMPOSER_NOTICE_ID) { @if let Some(n) = notice { (n) } } }
}

/// The line under the composer: a stop button (posting to `cancel`) while a turn runs, and
/// what the chat runs on when there is no model picker (`runs_on`). Empty otherwise.
pub fn composer_status(cancel: &str, busy: bool, runs_on: Option<&str>) -> Markup {
    html! {
        @if busy {
            span.working { span.icon.spin aria-hidden="true" {} "Working on it" }
            button.(Btn::link()) type="submit" formaction=(cancel) hx-post=(cancel) hx-swap="none" formnovalidate { "Stop" }
        }
        @if let Some(b) = runs_on {
            @if busy { span.sep aria-hidden="true" { "·" } }
            span { "Runs on " (b) }
        }
    }
}

/// DOM id of the slot that holds the model chip (updated live).
pub const MODEL_CHIP_ID: &str = "model-chip";
/// DOM id of the model picker popover.
pub const MODEL_PICKER_ID: &str = "model-picker";

/// One model in the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerModel {
    /// Form value (`choice`): `<backend>` or `<backend> <model>`.
    pub value: String,
    pub label: String,
    pub current: bool,
}

/// One backend (provider) in the picker: its mark on the rail and its models.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerBackend {
    /// The backend's name: the rail's filter key, never shown.
    pub name: String,
    pub label: String,
    /// The letter in its mark.
    pub letter: String,
    pub models: Vec<PickerModel>,
}

/// One reasoning level of the current model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerLevel {
    /// Form value (`effort`); empty: the agent's default.
    pub value: String,
    pub label: String,
    pub current: bool,
}

/// The access level in the picker (shown only with the developer option "Full access per
/// agent").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerAccess {
    /// The agent is on Full access (else Ask).
    pub full: bool,
}

/// The model picker of a chat (2026-11-20, owner-requested): a chip in the composer bar
/// ("Opus 5.5 · High") opens a popover above it with a rail of backends (Favorites first),
/// a searchable list of the selected backend's models, the current model's reasoning
/// levels, and a footer with the other options the model supports (Speed) and the agent's
/// access level. Each group shows only when it applies. Only usable backends are in it.
/// Picking posts the picker's own form (`choice`, or `effort`/`fast` with the hidden
/// `current`, or `access`) to `action`; a model option applies from the next turn. Built
/// from the API's `BackendView`s in `agents.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Speed (Standard / Fast) when the current model has a fast mode: whether Fast is on.
    /// None: the group is left out.
    pub speed: Option<bool>,
    /// Fast is on (a "Fast" mark on the chip).
    pub fast: bool,
    /// The agent's access level; None: the group is left out (developer option off).
    pub access: Option<PickerAccess>,
    /// The agent is on Full access (a warning mark on the chip).
    pub full_access: bool,
    /// Why the last switch was refused.
    pub error: Option<String>,
}

/// The chip in the composer bar that opens the picker (Popover API: no script needed).
pub fn model_chip(p: &ModelPicker) -> Markup {
    html! {
        button.model-chip.(Btn::ghost().sm().chip()) type="button" popovertarget=(MODEL_PICKER_ID) aria-haspopup="dialog"
            title="Choose the model" {
            @if let Some(l) = &p.letter {
                (provider_logo(p.current.split(' ').next().unwrap_or_default(), l, false))
            }
            span.visually-hidden { "Model: " }
            span.chip-text { (p.chip) }
            @if p.fast { span.chip-tag.badge.tone-attention { "Fast" } }
            @if p.full_access {
                span.chip-warn title="Full access: this agent's own requests run without asking you" {
                    (warn_icon())
                    span.visually-hidden { ", full access" }
                }
            }
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

fn warn_icon() -> Markup {
    html! {
        svg.warn viewBox="0 0 16 16" aria-hidden="true" focusable="false" {
            path d="M8 1.8 15 14H1z" {}
            path.mark d="M8 6v3.6M8 11.6v.4" {}
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
    picker_markup(p, false)
}

/// [`model_picker`] as an out-of-band replacement in a live update (the providers or their
/// models changed in Settings).
pub fn model_picker_oob(p: &ModelPicker) -> Markup {
    picker_markup(p, true)
}

fn picker_markup(p: &ModelPicker, oob: bool) -> Markup {
    let target = format!("#{MODEL_PICKER_ID}");
    html! {
        div.model-picker id=(MODEL_PICKER_ID) popover="auto" role="dialog" aria-label="Choose a model"
            hx-swap-oob=[oob.then_some("true")] data-error[p.error.is_some()] {
            form.picker method="post" action=(p.action) hx-post=(p.action) hx-target=(target)
                hx-swap="outerHTML" {
                (crate::components::csrf_field())
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
                                (provider_logo(&b.name, &b.letter, false))
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
                        input.picker-search.input #picker-search type="search" placeholder="Search models…"
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
                                                    (provider_logo(&b.name, &b.letter, false))
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
                        button.picker-more.(Btn::link()) type="button" hidden { "Show more" }
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
                @if p.speed.is_some() || p.access.is_some() {
                    div.picker-footer {
                        @if let Some(fast) = p.speed {
                            div.picker-option role="group" aria-labelledby="picker-speed-head" {
                                span.picker-option-head #picker-speed-head { "Speed" }
                                div.segmented {
                                    button.seg type="submit" name="fast" value="off"
                                        aria-pressed=(if fast { "false" } else { "true" }) { "Standard" }
                                    button.seg type="submit" name="fast" value="on"
                                        aria-pressed=(if fast { "true" } else { "false" })
                                        title="Faster replies; may use more of your plan" { "Fast" }
                                }
                            }
                        }
                        @if let Some(a) = &p.access {
                            div.picker-option role="group" aria-labelledby="picker-access-head" {
                                span.picker-option-head #picker-access-head { "Access" }
                                div.segmented {
                                    button.seg type="submit" name="access" value="ask"
                                        aria-pressed=(if a.full { "false" } else { "true" }) { "Ask" }
                                    @if a.full {
                                        button.seg.danger type="submit" name="access" value="full"
                                            aria-pressed="true" { "Full access" }
                                    } @else {
                                        details.seg-confirm {
                                            summary.seg { "Full access" }
                                            div.seg-confirm-body {
                                                p { "This agent's own requests run without asking you. Other agents' apps, schedules and moves still ask. All of it is logged." }
                                                button.(Btn::danger()) type="submit" name="access" value="full" { "Allow full access" }
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

/// A short status label (COMPONENT-SPEC 2.7, Penpot DS-08): the word on its tone's soft
/// colour, 20px high. Not a button.
pub fn badge(label: &str, tone: Tone) -> Markup {
    status_badge(label, tone, Mark::None)
}

/// A badge led by a dot or a spinner ("Running", "Working").
pub fn status_badge(label: &str, tone: Tone, mark: Mark) -> Markup {
    html! {
        span class={ "badge " (tone.class()) } {
            @match mark {
                Mark::None => {},
                Mark::Dot => span.badge-dot aria-hidden="true" {},
                Mark::Spin => span.icon.spin aria-hidden="true" {},
            }
            (label)
        }
    }
}

/// A one-line message box (COMPONENT-SPEC 2.10, Penpot DS-11): the text on its tone's soft
/// colour, led by the tone's glyph (ok: a check; attention and caution: a warning).
pub fn notice(text: &str, tone: Tone) -> Markup {
    html! { p class={ "notice " (tone.class()) } role="status" { (notice_glyph(tone)) (text) } }
}

/// The glyph in front of a [`notice`] (none for neutral and info).
pub(crate) fn notice_glyph(tone: Tone) -> Markup {
    match tone {
        Tone::Ok => glyph(Glyph::Check),
        Tone::Attention | Tone::Caution => glyph(Glyph::Warn),
        Tone::Neutral | Tone::Info => html! {},
    }
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
            span.select-wrap {
                select.input name=(name) required {
                    @for c in choices {
                        option value=(c.value) disabled[!c.enabled]
                            selected[selected == Some(c.value)] { (c.label) }
                    }
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
            textarea.input name=(name) rows="4" placeholder=(placeholder) maxlength=(max_len) required {}
        }
    }
}

/// A form that posts to `action`. With htmx it swaps the response into `#target`;
/// without JavaScript it falls back to a normal POST.
pub fn post_form(action: &str, target: &str, submit_label: &str, fields: Markup) -> Markup {
    html! {
        form.stack method="post" action=(action) hx-post=(action) hx-target={ "#" (target) } hx-swap="outerHTML" {
            (crate::components::csrf_field())
            (fields)
            div { button.(Btn::primary()) type="submit" { (submit_label) } }
        }
    }
}

/// A single-button action, e.g. "Remove", confirmed in the page (never a browser dialog):
/// the button opens `confirm` with a "Yes, <label>" button that posts. No script needed.
/// `look`: the button's look, `Btn::danger_quiet()` when it deletes something.
pub fn action_button(look: Btn, action: &str, target: &str, label: &str, confirm: &str) -> Markup {
    inline_confirm(
        look,
        label,
        confirm,
        html! {
            form method="post" action=(action) hx-post=(action) hx-target={ "#" (target) }
                hx-swap="outerHTML" {
                (crate::components::csrf_field())
                button.(Btn::danger()) type="submit" { "Yes, " (label.to_lowercase()) }
            }
        },
    )
}

/// Something that can't be undone (Settings → Reset): one button that opens an in-page card
/// with what is erased and what is kept, and a field where the word must be typed before the
/// final button posts `confirm=<typed>`. No script needed; never a browser dialog.
pub struct DangerConfirm<'a> {
    /// The button, the card's heading and the final button ("Reset Melissa").
    pub label: &'a str,
    pub erased: &'a str,
    pub kept: &'a str,
    /// The word to type.
    pub word: &'a str,
    pub action: &'a str,
    pub target: &'a str,
    /// Why the last try was refused (the card is open then).
    pub error: Option<&'a str>,
}

pub fn danger_confirm(c: &DangerConfirm<'_>) -> Markup {
    html! {
        details.confirm-inline.danger-confirm open[c.error.is_some()] {
            summary.(Btn::danger_quiet()) { (c.label) }
            section.approval.destructive.confirm aria-label=(c.label) {
                (facts(&[("Erased", html! { (c.erased) }), ("Kept", html! { (c.kept) })]))
                form.stack method="post" action=(c.action) hx-post=(c.action)
                    hx-target={ "#" (c.target) } hx-swap="outerHTML" hx-disabled-elt="find button" {
                    (crate::components::csrf_field())
                    label.field {
                        span { "Type " strong { (c.word) } " to confirm" }
                        input.input type="text" name="confirm" required autocomplete="off"
                            autocapitalize="off" spellcheck="false" pattern=(c.word)
                            aria-invalid=[c.error.map(|_| "true")];
                    }
                    @if let Some(e) = c.error { (notice(e, Tone::Caution)) }
                    div { button.(Btn::danger()) type="submit" { (c.label) } }
                }
            }
        }
    }
}

/// While Melissa is away on purpose (a reset): what is happening, and a quiet poll of `poll`,
/// which sends the browser on (`HX-Redirect`) once she is back. Without script: a link.
pub fn away_card(title: &str, text: &str, poll: &str, link_to: (&str, &str)) -> Markup {
    html! {
        section.approval.confirm role="status" aria-live="polite"
            hx-get=(poll) hx-trigger="every 3s" hx-swap="none" {
            h3 { (title) }
            p { (text) }
            noscript { p { a href=(link_to.0) { (link_to.1) } } }
        }
    }
}

/// The in-page confirmation behind [`action_button`] and the variable list's Remove: a
/// `<details>` whose summary is the action and whose body asks `question` next to `form`
/// (which holds the real submit button). `link`: the summary looks like a text link.
fn inline_confirm(look: Btn, label: &str, question: &str, form: Markup) -> Markup {
    let link = look == Btn::link();
    html! {
        details.confirm-inline.link[link] {
            summary.(look) { (label) }
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

pub use panel::{
    glyph, inline_form, inline_number, inline_select, inline_text_form, overlay_dialog, panel,
    section_bar, setting_card, setting_row, setting_row_details, setting_row_full,
    setting_row_with, settings_frame_at, settings_tools_oob, usage_table, Glyph, SettingsHead,
    SettingsSection, UpdateHint, UsageRow, OVERLAY_BODY_ID, OVERLAY_ID, SETTINGS_MAIN_ID,
    SETTINGS_TOOLS_ID,
};

/// When a section last looked, and the button that looks again, e.g. "Checked just now ·
/// Check again" (for the head bar's tools). Posts to `action_url`; htmx swaps the answer into
/// `#target`. Empty `checked`: the button alone.
pub fn settings_head(
    checked: &str,
    action_url: &str,
    target: &str,
    action: &str,
    look: Btn,
) -> Markup {
    html! {
        form.checked method="post" action=(action_url) hx-post=(action_url)
            hx-target={ "#" (target) } hx-swap="outerHTML" {
            (crate::components::csrf_field())
            @if !checked.is_empty() { span { (checked) } }
            button.(look) type="submit" { (action) }
        }
    }
}

/// One thing a row's "⋯" menu does: a form with one button, or one that asks first.
pub struct RowAction<'a> {
    pub label: &'a str,
    pub icon: Glyph,
    pub action: &'a str,
    pub target: &'a str,
    /// Asks this in the menu before it posts (a destructive action, drawn in caution).
    pub confirm: Option<&'a str>,
}

/// A row's "⋯" button and its menu (Settings → People: Reset password, Remove), so a row
/// keeps one small control instead of buttons that wrap under its name on a phone (owner
/// board P2, 2026-11-23). The agent menu's native popover (`id`: unique on the page) and its
/// in-menu confirmation; no script needed.
pub fn row_menu(id: &str, name: &str, items: &[RowAction<'_>]) -> Markup {
    html! {
        button.more-button.(Btn::ghost().icon()) type="button" popovertarget=(id) aria-haspopup="true"
            title={ "More for " (name) } {
            (glyph(Glyph::More))
            span.visually-hidden { "More for " (name) }
        }
        div.menu id=(id) popover="auto" aria-label={ "Menu for " (name) } {
            @for a in items {
                @let form = html! {
                    form method="post" action=(a.action) hx-post=(a.action)
                        hx-target={ "#" (a.target) } hx-swap="outerHTML" {
                        (crate::components::csrf_field())
                        @match a.confirm {
                            Some(_) => button.(Btn::danger()) type="submit" { "Yes, " (a.label.to_lowercase()) },
                            None => button.menu-item type="submit" { (glyph(a.icon)) span { (a.label) } },
                        }
                    }
                };
                @match a.confirm {
                    Some(q) => details.menu-confirm {
                        summary.menu-item.danger { (glyph(a.icon)) span { (a.label) } }
                        div.menu-confirm-body { p { (q) } (form) }
                    },
                    None => (form),
                }
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

mod agent;
pub use agent::{
    agent_groups, agent_menu, agent_menu_items, agent_message, avatar, chat_head, chat_who_update,
    icon_picker, permission_card, setup_password_card, shape, sidebar, status_pill, steps_card,
    this_device_update, AgentMenu, Avatar, ChatHead, GroupKind, HeadStatus, MessageHead,
    PermissionCard, SetupPasswordCard, SetupPasswordState, SideAgent, SideDevice, SideGroup,
    Sidebar, StepLine, AGENT_GROUPS_ID, CHAT_STATUS_ID, SIDEBAR_ID,
};
mod apps;
pub use apps::{
    app_details_panel, app_details_region, app_head_icon_oob, app_icon, app_icon_card, app_list,
    app_logs_region, app_status, AppAsk, AppDetailsView, AppFact, AppGroup, AppList, AppLogsView,
    AppRow, AppsEmpty, ShowChoice, APPS_LIST_ID, APP_DETAILS_ID,
};
mod network;
pub use network::{
    add_link_form, link_row, network_box, network_card, AddLinkForm, NetworkCard, NetworkLink,
    NetworkSwitch, APP_NETWORK_ID,
};
mod mobile;
pub use mobile::{
    back_link, composer_caption, menu_cancel, menu_head, new_agent_round, phone_view, sheet_avatar,
    sheet_handle, sheet_sub, MenuWho, PhoneView, SheetWho,
};
mod panel;
pub mod people;
pub use people::csrf_field;
mod provider_add;
pub use provider_add::{
    add_panel, provider_tiles, registry_list, search_box, stepper, ProviderTile, RegistryAction,
    RegistryRow, TileLink,
};
mod usage;
pub use usage::{
    usage_checked, usage_lines, usage_providers, usage_view, LimitCard, LimitLevel, LineMark,
    ProviderLimits, ProviderLimitsBody, ProviderTokens, ProvidersPart, TokenRow, UsageBar,
    UsageLine, UsageView, LOW_AT, NEARLY_GONE_AT, USAGE_CHECKED_ID, USAGE_PROVIDERS_ID,
    USAGE_VIEW_ID,
};
mod threads;
pub use threads::{
    detail_conversations, event_line, relayed_mark, thread_panel, ConversationRow, EventLine,
    ThreadLink, ThreadMessage, ThreadPanel, DETAILS_CONVERSATIONS_ID, THREAD_SLOT_ID,
};
mod onboarding;
pub use onboarding::{
    notice_link, setup_actions, setup_checklist, setup_page, setup_submit, suggestion_chips,
    ChecklistItem, Suggestion, CHECKLIST_ID,
};

/// A provider's logo where Melissa ships one for `name` (a backend name, provider id or ACP
/// registry id; see `crate::logos`), else its letter mark. The logo is drawn from the
/// vendored file's checked paths, in one colour (black, white in dark mode).
pub fn provider_logo(name: &str, letter: &str, muted: bool) -> Markup {
    let Some(logo) = crate::logos::logo(name) else {
        return provider_mark(letter, muted);
    };
    html! {
        span class={ "pmark plogo" @if muted { " muted" } } aria-hidden="true" {
            svg viewBox=(logo.view_box) focusable="false" {
                @for p in &logo.paths {
                    path d=(p.d) fill-rule=[p.evenodd.then_some("evenodd")] {}
                }
            }
        }
    }
}

/// A provider's mark: a hexagon with a letter (no vendor logo; see [`provider_logo`]).
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
            (crate::components::csrf_field())
            label {
                input type="checkbox" name="enabled" value="on" role="switch" checked[t.on]
                    disabled[t.locked.is_some()];
                span.visually-hidden { (t.label) }
            }
            button.toggle-save.(Btn::secondary().sm()) type="submit" disabled[t.locked.is_some()] { "Save" }
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
            input.input type=(kind) name=(name) value=(value) placeholder=(placeholder)
                autocomplete="off" spellcheck="false" maxlength="4096";
            @if let Some(h) = hint { small.hint { (h) } }
        }
    }
}

/// [`text_field`] (text) with the server's verdict (COMPONENT-SPEC 2.16, invalid): with an
/// `error`, the field is `aria-invalid`, takes the focus, and the error line (caution glyph)
/// shows in place of the hint (owner's Penpot draft, 2026-11-28). The typed value is kept.
pub fn text_field_checked(
    name: &str,
    label: &str,
    value: &str,
    hint: Option<&str>,
    error: Option<&str>,
) -> Markup {
    html! {
        label.field {
            span { (label) }
            input.input type="text" name=(name) value=(value) autocomplete="off"
                autocapitalize="none" spellcheck="false" maxlength="4096"
                aria-invalid=[error.map(|_| "true")] autofocus[error.is_some()];
            (field_note(hint, error))
        }
    }
}

/// Under a field: its error (caution glyph) or else its hint.
pub(crate) fn field_note(hint: Option<&str>, error: Option<&str>) -> Markup {
    html! {
        @if let Some(e) = error {
            small.field-error { (glyph(Glyph::Warn)) (e) }
        } @else if let Some(h) = hint {
            small.hint { (h) }
        }
    }
}

/// A button for a job the server is doing ("Turning on…"; COMPONENT-SPEC 2.1, busy): the
/// `-ing…` label, `aria-disabled`, not clickable.
pub fn busy_button(look: Btn, label: &str) -> Markup {
    html! {
        button.(look) type="button" disabled aria-disabled="true" { (label) }
    }
}

/// A multi-line input with a value, one item per line.
pub fn lines_field(name: &str, label: &str, value: &str, hint: Option<&str>) -> Markup {
    html! {
        label.field {
            span { (label) }
            textarea.input.input-mono name=(name) rows="3" spellcheck="false" autocomplete="off" { (value) }
            @if let Some(h) = hint { small.hint { (h) } }
        }
    }
}

/// A form that saves settings and swaps `#target` (htmx) or posts normally.
/// `cancel`: a form opened in place of a button (owner decision 3, 2026-11-23): its submit is
/// the card's one primary button, and Cancel (a link to the page without it) closes it.
pub fn settings_form(
    action: &str,
    target: &str,
    submit: &str,
    cancel: Option<&str>,
    fields: Markup,
) -> Markup {
    html! {
        form.stack.settings-form method="post" action=(action) hx-post=(action)
            hx-target={ "#" (target) } hx-swap="outerHTML" {
            (crate::components::csrf_field())
            (fields)
            @match cancel {
                Some(href) => div.action-row {
                    button.(Btn::primary()) type="submit" { (submit) }
                    a.(Btn::ghost()) href=(href) { "Cancel" }
                },
                None => div { button.(Btn::secondary()) type="submit" { (submit) } },
            }
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
                            (inline_confirm(Btn::link(), "Remove", &format!("Remove {}?", r.name), html! {
                                form method="post" action=(r.remove_action) hx-post=(r.remove_action)
                                    hx-target={ "#" (target) } hx-swap="outerHTML" {
                                    (crate::components::csrf_field())
                                    input type="hidden" name="name" value=(r.name);
                                    button.(Btn::danger()) type="submit" {
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
            button.copy-code.(Btn::secondary().sm()) type="button" aria-label="Copy command" { "Copy" }
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
                    (crate::components::csrf_field())
                    input type="hidden" name="confirm" value="yes";
                    button.(Btn::danger()) type="submit" { (c.confirm) }
                }
                a.(Btn::secondary()) href=(c.cancel_href) { "Keep as is" }
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
            (crate::components::csrf_field())
            button.(if primary { Btn::primary() } else { Btn::secondary() }) type="submit" { (label) }
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
        a.(Btn::primary()) href=(href) target="_blank" rel="noopener noreferrer" { (label) }
    }
}

/// A link that opens in a new tab, in a button's look, with an optional glyph before its label
/// (Settings → About: Sponsor, Source code). Screen readers hear that it opens a new tab;
/// `noopener noreferrer`: the other page gets neither this page nor its address. Only for
/// http(s) URLs Melissa chose and paths on this site; anything else renders as plain text.
pub fn new_tab_button(look: Btn, href: &str, label: &str, icon: Option<Glyph>) -> Markup {
    let local = href.starts_with('/') && !href.starts_with("//");
    if !(local || href.starts_with("https://") || href.starts_with("http://")) {
        return html! { span { (label) } };
    }
    html! {
        a.(look) href=(href) target="_blank" rel="noopener noreferrer" {
            @if let Some(g) = icon { (glyph(g)) }
            (label)
            span.visually-hidden { " (opens in a new tab)" }
        }
    }
}

/// The first row of Settings → About's card: Melissa's mark, her name and one quiet line (the
/// version), laid out like a setting row (2026-11-23).
pub fn about_head(name: &str, detail: &str) -> Markup {
    html! {
        div.setting-row.about-head {
            div.about-name {
                (mark("mark"))
                div.setting-text {
                    p.setting-title { (name) }
                    p.setting-desc { (detail) }
                }
            }
        }
    }
}

/// A license or notice text as written (line breaks kept), in a box that scrolls; focusable,
/// so a keyboard can scroll it too, and named by `label` for screen readers.
pub fn license_text(label: &str, text: &str) -> Markup {
    html! { pre.license-text tabindex="0" aria-label=(label) { (text) } }
}

/// One field and a primary button on one row, e.g. "Paste the code" + Continue. Swaps
/// `#target` (htmx) or posts normally.
pub fn one_field_form(action: &str, target: &str, submit: &str, field: Markup) -> Markup {
    html! {
        form.one-field method="post" action=(action) hx-post=(action)
            hx-target={ "#" (target) } hx-swap="outerHTML" {
            (crate::components::csrf_field())
            (field)
            button.(Btn::primary()) type="submit" { (submit) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmations_stay_in_the_page() {
        let out = action_button(
            Btn::secondary(),
            "/x/remove",
            "t",
            "Remove",
            "Remove <this>?",
        )
        .into_string();
        assert!(!out.contains("hx-confirm"), "no browser dialogs: {out}");
        assert!(out.contains("<details class=\"confirm-inline\">"));
        assert!(out.contains("<summary class=\"btn btn-secondary\">Remove</summary>"));
        assert!(out.contains("Remove &lt;this&gt;?"));
        assert!(out.contains("hx-post=\"/x/remove\""));
        assert!(out.contains(">Yes, remove</button>"));
    }

    #[test]
    fn text_is_escaped() {
        let out = card(
            "<b>x</b>",
            notice("<script>alert(1)</script>", Tone::Caution),
        )
        .into_string();
        assert!(!out.contains("<script>"));
        assert!(!out.contains("<b>"));
        assert!(out.contains("&lt;script&gt;"));
    }

    #[test]
    fn agent_text_is_markdown_with_html_escaped() {
        let evil = "Hi <img src=x onerror=alert(1)>\n\n**bold** [link](http://example.com)\n- one\n- <b>two</b>";
        let out = agent_message(None, "Melissa", evil).into_string();
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
        let out = agent_message(None, "Melissa", "run `<ls>` and `x").into_string();
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
        // Images are never loaded: a link with the alt text.
        let out = markdown("![the logo](https://example.com/logo.png)").into_string();
        assert!(!out.contains("<img"), "{out}");
        assert!(
            out.contains(r#"href="https://example.com/logo.png""#),
            "{out}"
        );
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

    fn card_for(state: ApprovalState, destructive: bool, permanent: bool) -> String {
        permission_card(&PermissionCard {
            id: 7,
            target: "c-7",
            title: "Remove <Nextcloud>?",
            consequence: "Stops it.",
            reason: "destructive action",
            kind: "Network access",
            detail: Some("TCP 25565"),
            destructive,
            permanent,
            yes: "Remove",
            no: "Decline",
            state,
            always: Some("Always let Ada update Minecraft?"),
            quote: None,
            network: &[],
        })
        .into_string()
    }

    #[test]
    fn approval_cards_quote_the_owners_words_on_a_line_of_their_own() {
        let card = |quote| {
            permission_card(&PermissionCard {
                id: 9,
                target: "c-9",
                title: "Let immich reach the internet?",
                consequence: "immich can reach any internet address.",
                reason: "agents can't change network access alone",
                kind: "Network access",
                detail: Some("Internet"),
                destructive: false,
                permanent: false,
                yes: "Allow",
                no: "Deny",
                state: ApprovalState::Pending,
                always: None,
                quote,
                network: &[],
            })
            .into_string()
        };
        let quoted = card(Some("Turn on the <map>"));
        assert!(
            quoted.contains(
                "<blockquote class=\"approval-quote\"><p><span class=\"quote-label\">You asked:\
                 </span> “Turn on the &lt;map&gt;”</p></blockquote>"
            ),
            "{quoted}"
        );
        assert!(
            quoted.contains("<p class=\"consequence\">immich can reach any internet address.</p>")
        );
        assert!(!card(None).contains("You asked"));
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
        assert!(pending.contains("class=\"btn btn-primary\""));
        // The tag line, the quiet third action and the waiting note (owner design).
        assert!(pending.contains("<span class=\"tag-kind\">Network access</span>"));
        assert!(pending.contains("<span class=\"tag-detail\">TCP 25565</span>"));
        assert!(pending.contains("button class=\"btn btn-ghost\""));
        assert!(pending.contains("Waiting for your decision"));
        assert!(pending.contains(">Decline<"));

        let destructive = card_for(ApprovalState::Pending, true, true);
        assert!(destructive.contains("approval destructive"));
        assert!(destructive.contains("Can't be undone."));
        assert!(destructive.contains("btn btn-danger"));

        for (state, text) in [
            (ApprovalState::Approved, "You allowed this."),
            (ApprovalState::Denied, "You declined."),
            (ApprovalState::Withdrawn, "No longer needed"),
        ] {
            let out = card_for(state, true, true);
            assert!(out.contains(text));
            assert!(!out.contains("<button"), "{state:?}");
            assert!(!out.contains("Can't be undone"));
            assert!(!out.contains("Waiting for your decision"));
        }
    }
}
