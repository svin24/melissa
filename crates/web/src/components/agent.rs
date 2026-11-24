//! The agent workspace (2026-11-20, owner design "Melissa — Agent workspace"): one chat per
//! agent. The sidebar lists the agents grouped by state (Needs attention, Pinned, the idle
//! ones, Working last; 2026-11-21), each with its own icon and a "⋯" menu; the chat has a header with the agent, a
//! status and the same menu; an agent turn's tool steps are one checklist card; and the
//! permission card says what kind of access is asked for.
//!
//! Part of the fixed component library (`components.rs`); a separate file to keep it together.
//! Menus are native popovers (`popovertarget`): they open, close on Esc and on a click outside
//! without script; melissa.js only places them next to their button.

use maud::{html, Markup};

use super::{
    activity_icon, approval_state_outcome, markdown,
    mobile::{back_link, menu_cancel, menu_head, new_agent_round, MenuWho},
    panel::{glyph, section_bar, Glyph, UpdateHint},
    status_badge, ActivityState, ApprovalState, Btn, Health, Mark, Nav, Tone,
};

// --- Icons --------------------------------------------------------------------------------------

/// Who an icon is of: Melissa (her honey hexagon) or a helper (its shape and colour, by name;
/// see `melissa_agent::lead::icon`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Avatar<'a> {
    Melissa,
    Agent { shape: &'a str, color: &'a str },
}

/// The CSS class of an icon colour (unknown names: slate).
fn color_class(color: &str) -> &'static str {
    match color {
        "green" => "ic-green",
        "teal" => "ic-teal",
        "blue" => "ic-blue",
        "violet" => "ic-violet",
        "rose" => "ic-rose",
        "orange" => "ic-orange",
        "lime" => "ic-lime",
        _ => "ic-slate",
    }
}

/// One of the helpers' shapes (16-unit viewBox, line drawing; unknown names: a circle).
pub fn shape(name: &str) -> Markup {
    html! {
        @match name {
            "cube" => { path d="M8 1.75 13.5 4.9v6.2L8 14.25 2.5 11.1V4.9ZM2.5 4.9 8 8.05l5.5-3.15M8 8.05v6.2" {} }
            "square" => { rect x="2.5" y="2.5" width="11" height="11" rx="2.5" {} }
            "triangle" => { path d="M8 2.25 14 13.25H2Z" {} }
            "diamond" => { path d="M8 1.75 14.25 8 8 14.25 1.75 8Z" {} }
            "bolt" => { path d="M9.25 1.75 3.5 9h4.25l-1 5.25L12.5 7H8.25Z" {} }
            "leaf" => { path d="M13.5 2.5c-6.5 0-11 3-11 8 0 1.25.5 3 .5 3s1.75.5 3 .5c5 0 8-4.5 7.5-11.5ZM3 13l6-6" {} }
            "drop" => { path d="M8 1.75s-4.75 5.1-4.75 8.25a4.75 4.75 0 0 0 9.5 0C12.75 6.85 8 1.75 8 1.75Z" {} }
            "cloud" => { path d="M4.75 12.75h6.5a3 3 0 0 0 .4-5.97 4.1 4.1 0 0 0-7.9-.53 3.25 3.25 0 0 0 1 6.5Z" {} }
            "server" => {
                rect x="2.5" y="2.5" width="11" height="4.5" rx="1.25" {}
                rect x="2.5" y="9" width="11" height="4.5" rx="1.25" {}
                path d="M5 4.75h.01M5 11.25h.01" {}
            }
            "shield" => { path d="M8 1.75 13.25 3.75v4c0 3.25-2.25 5.5-5.25 6.5-3-1-5.25-3.25-5.25-6.5v-4Z" {} }
            "clock" => {
                circle cx="8" cy="8" r="5.75" {}
                path d="M8 4.75V8l2.25 1.5" {}
            }
            _ => { circle cx="8" cy="8" r="5.75" {} }
        }
    }
}

/// An agent's icon. `class` sizes it (`av-sm` in lists, `av` in headers).
pub fn avatar(a: &Avatar<'_>, class: &str) -> Markup {
    avatar_markup(a, class, None)
}

/// [`avatar`], with `hx-swap-oob` set to `oob` (a live update) if given.
fn avatar_markup(a: &Avatar<'_>, class: &str, oob: Option<&str>) -> Markup {
    match a {
        Avatar::Melissa => html! {
            svg class={ "avatar ic-honey " (class) } viewBox="0 0 16 16" aria-hidden="true" focusable="false"
                hx-swap-oob=[oob] {
                path d="M8 1.5 13.63 4.75v6.5L8 14.5l-5.63-3.25v-6.5Z" {}
            }
        },
        Avatar::Agent { shape: s, color } => html! {
            svg class={ "avatar " (color_class(color)) " " (class) } viewBox="0 0 16 16"
                aria-hidden="true" focusable="false" hx-swap-oob=[oob] {
                (shape(s))
            }
        },
    }
}

/// The icon picker of a form (new helper, Change icon): the shapes and the colours as two
/// groups of radio buttons (`shape`, `color`); the shapes take the chosen colour (CSS).
pub fn icon_picker(shapes: &[&str], colors: &[&str], shape_now: &str, color_now: &str) -> Markup {
    html! {
        div.icon-picker id="agent-icon" {
            fieldset.icon-shapes {
                legend { "Shape" }
                @for s in shapes {
                    label.icon-option title=(s) {
                        input type="radio" name="shape" value=(s) checked[*s == shape_now] required;
                        svg class={ "avatar av " (color_class(color_now)) } viewBox="0 0 16 16"
                            aria-hidden="true" focusable="false" { (shape(s)) }
                        span.visually-hidden { (s) }
                    }
                }
            }
            fieldset.icon-colors {
                legend { "Colour" }
                @for c in colors {
                    label.color-option title=(c) {
                        input type="radio" name="color" value=(c) checked[*c == color_now] required;
                        span class={ "swatch " (color_class(c)) } aria-hidden="true" {}
                        span.visually-hidden { (c) }
                    }
                }
            }
        }
    }
}

// --- The agent menu ("⋯") -----------------------------------------------------------------------

/// What an agent's menu offers (the sidebar row and the chat header show the same).
pub struct AgentMenu<'a> {
    /// Its name, for the button's label and the confirmation.
    pub name: &'a str,
    /// Agent settings (Settings → Agents).
    pub settings: &'a str,
    /// Rename and Change icon (helpers): where the forms are.
    pub rename: Option<&'a str>,
    pub icon: Option<&'a str>,
    /// Pin or unpin (helpers): the form's action and whether it is pinned now.
    pub pin: Option<(&'a str, bool)>,
    /// Fresh start: the form's action.
    pub fresh: &'a str,
    /// Earlier chats (Melissa's archived chats from before 2026-11-20), if there are any.
    pub earlier: Option<&'a str>,
    /// Delete (helpers): where it is (Agent settings, which says why when it can't be).
    pub delete: Option<&'a str>,
    /// On phones the menu is an action sheet with the agent at the top (None: no header).
    pub who: Option<MenuWho<'a>>,
}

fn menu_link(href: &str, icon: Glyph, label: &str, danger: bool) -> Markup {
    html! {
        a class={ "menu-item" @if danger { " danger" } } href=(href) {
            (glyph(icon)) span { (label) }
        }
    }
}

/// The "⋯" button and its menu (`id`: the popover's DOM id, unique on the page). The items are
/// [`agent_menu_items`], so another container (a sheet on phones) can show the same.
pub fn agent_menu(id: &str, m: &AgentMenu<'_>) -> Markup {
    html! {
        button.more-button.(Btn::ghost().icon()) type="button" popovertarget=(id) aria-haspopup="true"
            title={ "More for " (m.name) } {
            (glyph(Glyph::More))
            span.visually-hidden { "More for " (m.name) }
        }
        div.menu.agent-menu id=(id) popover="auto" aria-label={ "Menu for " (m.name) } {
            // The items in one block: on phones an action sheet's card, Cancel apart below.
            div.menu-sheet {
                @if let Some(who) = &m.who { (menu_head(who, m.name)) }
                (agent_menu_items(m))
            }
            (menu_cancel(id))
        }
    }
}

/// The items of an agent's menu, in order: Rename agent, Change icon, Agent settings,
/// Pin/Unpin, Fresh start (asks in place), Earlier chats, Delete helper.
pub fn agent_menu_items(m: &AgentMenu<'_>) -> Markup {
    html! {
            @if let Some(href) = m.rename { (menu_link(href, Glyph::Pencil, "Rename agent", false)) }
            @if let Some(href) = m.icon { (menu_link(href, Glyph::Shapes, "Change icon", false)) }
            (menu_link(m.settings, Glyph::Gear, "Agent settings", false))
            @if let Some((action, pinned)) = m.pin {
                form method="post" action=(action) {
                    (crate::components::csrf_field())
                    input type="hidden" name="pinned" value=(if pinned { "off" } else { "on" });
                    button.menu-item type="submit" {
                        (glyph(Glyph::Pin)) span { (if pinned { "Unpin" } else { "Pin" }) }
                    }
                }
            }
            details.menu-confirm {
                summary.menu-item { (glyph(Glyph::Restart)) span { "Fresh start" } }
                div.menu-confirm-body {
                    p { "Clear " (m.name) "'s memory? Its next message starts without the earlier conversation. The chat stays." }
                    form method="post" action=(m.fresh) {
                        (crate::components::csrf_field())
                        button.(Btn::primary()) type="submit" { "Clear memory" }
                    }
                }
            }
            @if let Some(href) = m.earlier { (menu_link(href, Glyph::History, "Earlier chats", false)) }
            @if let Some(href) = m.delete { (menu_link(href, Glyph::Trash, "Delete helper", true)) }
    }
}

// --- The sidebar --------------------------------------------------------------------------------

/// The state groups of the sidebar (shown in this order: Attention, Pinned, Idle, Working).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKind {
    /// Waiting for the owner: an approval, a failed turn, an app with a problem.
    Attention,
    Pinned,
    /// Done or resting: a plain list without a heading, most recently active first.
    Idle,
    /// A turn runs: at the bottom.
    Working,
}

impl GroupKind {
    fn label(self) -> &'static str {
        match self {
            Self::Attention => "Needs attention",
            Self::Pinned => "Pinned",
            Self::Idle => "Other helpers",
            Self::Working => "Working",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Attention => "attention",
            Self::Pinned => "pinned",
            Self::Idle => "idle",
            Self::Working => "working",
        }
    }
}

/// One agent in the sidebar.
pub struct SideAgent<'a> {
    /// Unique on the page (`lead` or the helper's id): the menu's id.
    pub key: &'a str,
    pub href: &'a str,
    pub name: &'a str,
    pub avatar: Avatar<'a>,
    /// Melissa: a star after the name.
    pub star: bool,
    /// What it is doing or needs ("Permission needed", "Setting up an app").
    pub status: Option<&'a str>,
    /// A quiet second line ("Lead agent", "melissa · Minecraft").
    pub sub: Option<&'a str>,
    /// A thin progress bar: Some(None) while working without a known share, Some(Some(p))
    /// at p percent.
    pub progress: Option<Option<u8>>,
    pub current: bool,
    pub menu: AgentMenu<'a>,
}

/// A group of agents.
pub struct SideGroup<'a> {
    pub kind: GroupKind,
    pub agents: Vec<SideAgent<'a>>,
}

/// One device in the device menu.
pub struct SideDevice<'a> {
    pub name: &'a str,
    pub health: Health,
    pub status: &'a str,
    pub current: bool,
}

/// Everything the sidebar shows.
pub struct Sidebar<'a> {
    pub devices: &'a [SideDevice<'a>],
    pub groups: &'a [SideGroup<'a>],
    pub current: Nav,
    pub update: &'a UpdateHint,
    /// Where "+" goes: the new-helper form.
    pub new_agent: &'a str,
    /// Pending approvals in all (a link to the list of them, when any).
    pub waiting: usize,
}

/// DOM id of the sidebar's agent groups (the live stream replaces it).
pub const AGENT_GROUPS_ID: &str = "agent-groups";
/// DOM id of the sidebar.
pub const SIDEBAR_ID: &str = "sidebar";

/// The sidebar (replaces the rail, 2026-11-20): the device menu and collapse control, the
/// agent search with "+", the agents grouped by state, and the sections bar. On phones it is
/// the agent list (`/`) and hidden on every other page (CSS). melissa.js filters it as you
/// type and collapses it on wide screens.
pub fn sidebar(s: &Sidebar<'_>) -> Markup {
    let this = s
        .devices
        .iter()
        .find(|d| d.current)
        .map_or("This device", |d| d.name);
    html! {
        nav.sidebar id=(SIDEBAR_ID) aria-label="Agents" {
            div.side-head {
                button.side-collapse.(Btn::ghost().icon()) type="button" data-sidebar-collapse data-script-only
                    title="Collapse sidebar" {
                    (glyph(Glyph::Sidebar))
                    span.visually-hidden { "Collapse sidebar" }
                }
                button.device-button.(Btn::ghost()) type="button" popovertarget="device-menu" aria-haspopup="true"
                    title="Devices" {
                    svg.mark viewBox="0 0 24 24" aria-hidden="true" focusable="false" {
                        polygon points="12,1.5 21.1,6.75 21.1,17.25 12,22.5 2.9,17.25 2.9,6.75" {}
                    }
                    span.brand-word { "Melissa" }
                    span.visually-hidden { ", on " (this) }
                    (glyph(Glyph::Chevron))
                }
                div.menu.device-menu #device-menu popover="auto" aria-label="Devices" {
                    p.menu-label { "Devices" }
                    @for d in s.devices {
                        p.menu-item.device-row aria-current=[d.current.then_some("true")] {
                            span class={ "dot " (health_class(d.health)) } aria-hidden="true" {}
                            span.name { (d.name) }
                            span.meta { (d.status) }
                        }
                    }
                    @if let Some(me) = crate::auth::signed_in() {
                        p.menu-label { "Signed in" }
                        a.menu-item href="/settings/account" {
                            (glyph(Glyph::Person)) span { (me.username) }
                        }
                        (super::people::sign_out_item())
                    }
                }
                (new_agent_round(s.new_agent))
            }
            div.side-search role="search" {
                label.visually-hidden for="agent-search" { "Search agents" }
                span.search-field {
                    (glyph(Glyph::Search))
                    input #agent-search type="search" placeholder="Search agents" autocomplete="off"
                        spellcheck="false" data-agent-search aria-controls=(AGENT_GROUPS_ID);
                }
                a.side-new.(Btn::ghost().icon()) href=(s.new_agent) title="New helper" {
                    (glyph(Glyph::Plus))
                    span.visually-hidden { "New helper" }
                }
            }
            div.side-main {
                (agent_groups(s.groups, s.waiting, false))
                p.side-none.quiet role="status" data-search-empty hidden { "No agents match." }
            }
            (section_bar(s.current, s.update))
        }
    }
}

fn health_class(h: Health) -> &'static str {
    h.class()
}

/// The agent groups (`oob`: as an out-of-band replacement in a live update). Needs attention
/// and Pinned have a heading; the idle agents follow as a plain list (its heading is for
/// screen readers only); Working, last, is open until the user closes it (melissa.js keeps
/// that across updates). Empty groups are left out, except Pinned. Each row carries its
/// agent's key (`data-agent`), so melissa.js can move it to its new place when it changes
/// group.
pub fn agent_groups(groups: &[SideGroup<'_>], waiting: usize, oob: bool) -> Markup {
    html! {
        div.agent-groups id=(AGENT_GROUPS_ID) hx-swap-oob=[oob.then_some("true")] {
            @for g in groups.iter().filter(|g| !g.agents.is_empty() || g.kind == GroupKind::Pinned) {
                @let head_id = format!("group-{}", g.kind.key());
                @match g.kind {
                    GroupKind::Attention | GroupKind::Pinned => {
                        section class={ "agent-group group-" (g.kind.key()) } aria-labelledby=(head_id) {
                            h2.group-head id=(head_id) {
                                @if g.kind == GroupKind::Attention && waiting > 0 {
                                    a href="/approvals" title="Everything waiting for you" { (g.kind.label()) }
                                } @else {
                                    span { (g.kind.label()) }
                                }
                                span.count { (g.agents.len()) }
                            }
                            (agent_list(&g.agents, g.kind))
                        }
                    }
                    GroupKind::Idle => {
                        section class={ "agent-group group-" (g.kind.key()) } aria-labelledby=(head_id) {
                            h2.visually-hidden id=(head_id) { (g.kind.label()) }
                            (agent_list(&g.agents, g.kind))
                        }
                    }
                    GroupKind::Working => {
                        details class={ "agent-group group-" (g.kind.key()) } data-group=(g.kind.key()) open {
                            summary.group-head id=(head_id) {
                                span { (g.kind.label()) }
                                span.count { (g.agents.len()) }
                                (glyph(Glyph::Chevron))
                            }
                            (agent_list(&g.agents, g.kind))
                        }
                    }
                }
            }
        }
    }
}

fn agent_list(agents: &[SideAgent<'_>], kind: GroupKind) -> Markup {
    html! {
        ul.agent-list {
            @for a in agents {
                li class={ "agent-row" @if kind == GroupKind::Attention { " attention" } }
                    data-name=(a.name.to_lowercase()) data-agent=(a.key) {
                    a.agent-link href=(a.href) aria-current=[a.current.then_some("page")] {
                        (avatar(&a.avatar, "av-sm"))
                        span.agent-text {
                            span.agent-name {
                                span.name { (a.name) }
                                @if a.star {
                                    svg.star-mark viewBox="0 0 16 16" aria-hidden="true" focusable="false" {
                                        path d="M8 1.8l1.85 3.9 4.25.55-3.1 2.95.8 4.2L8 11.35 4.2 13.4l.8-4.2L1.9 6.25l4.25-.55z" {}
                                    }
                                }
                            }
                            @if let Some(st) = a.status {
                                span class={ "agent-status" @if kind == GroupKind::Working { " working" } } { (st) }
                            }
                            @if let Some(sub) = a.sub { span.agent-sub { (sub) } }
                            @if let Some(p) = a.progress {
                                @match p {
                                    Some(p) => progress.agent-progress max="100" value=(p) { (p) "%" },
                                    None => progress.agent-progress { "Working" },
                                }
                            }
                        }
                    }
                    (agent_menu(&format!("menu-{}", a.key), &a.menu))
                }
            }
        }
    }
}

// --- The chat header ----------------------------------------------------------------------------

/// DOM id of the status in the chat header (the live stream replaces its content).
pub const CHAT_STATUS_ID: &str = "chat-status";

/// The chat header's status: what the agent needs or does (None: settled, nothing shown).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadStatus {
    NeedsPermission,
    /// Its last turn failed.
    Failed,
    Working,
}

/// The chat header's status badge (or nothing).
pub fn status_pill(s: Option<HeadStatus>) -> Markup {
    html! {
        @match s {
            Some(HeadStatus::NeedsPermission) => (status_badge("Needs permission", Tone::Attention, Mark::None)),
            Some(HeadStatus::Failed) => (status_badge("Needs a look", Tone::Attention, Mark::None)),
            Some(HeadStatus::Working) => (status_badge("Working", Tone::Neutral, Mark::Spin)),
            None => {},
        }
    }
}

/// The header of a chat page.
pub struct ChatHead<'a> {
    pub avatar: Avatar<'a>,
    /// The device it runs on ("melissa").
    pub device: &'a str,
    pub name: &'a str,
    pub status: Option<HeadStatus>,
    pub menu: AgentMenu<'a>,
}

/// The chat header: the agent's icon and "device / name", its status and its menu, and the
/// Details toggle. A button to show the sidebar again appears when it is collapsed.
pub fn chat_head(h: &ChatHead<'_>, details: Markup) -> Markup {
    html! {
        header.chat-head {
            (back_link("/", "Back to agents"))
            button.side-expand.(Btn::ghost().icon()) type="button" data-sidebar-expand
                hidden[!crate::looks::current().sidebar_collapsed] title="Show agents" {
                (glyph(Glyph::Sidebar))
                span.visually-hidden { "Show agents" }
            }
            (avatar(&h.avatar, "av"))
            div.chat-title { (chat_title(h.device, h.name)) }
            span.chat-status id=(CHAT_STATUS_ID) role="status" { (status_pill(h.status)) }
            (agent_menu("menu-head", &h.menu))
            (details)
        }
    }
}

fn chat_title(device: &str, name: &str) -> Markup {
    html! {
        span.chat-device { (device) }
        span.chat-sep aria-hidden="true" { "/" }
        h1 { (name) }
    }
}

/// A live update of the chat header's icon and "device / name" (the agent got a new name or
/// icon, or the device a new name, in Settings).
pub fn chat_who_update(avatar: &Avatar<'_>, device: &str, name: &str) -> Markup {
    html! {
        (avatar_markup(avatar, "av", Some("outerHTML:.chat-head > .avatar")))
        div hx-swap-oob="innerHTML:.chat-head > .chat-title" { (chat_title(device, name)) }
    }
}

/// A live update of this device's name in the sidebar's device menu (renamed in setup).
pub fn this_device_update(name: &str) -> Markup {
    html! {
        div hx-swap-oob="innerHTML:#device-menu .device-row[aria-current] .name" { (name) }
        div hx-swap-oob="innerHTML:.device-button .visually-hidden" { ", on " (name) }
    }
}

// --- Messages -----------------------------------------------------------------------------------

/// Who wrote an agent message, shown above the turn's first item.
pub struct MessageHead<'a> {
    pub avatar: Avatar<'a>,
    pub name: &'a str,
    /// "10:42".
    pub time: &'a str,
}

pub(super) fn message_head(h: &MessageHead<'_>) -> Markup {
    html! {
        div.msg-head {
            (avatar(&h.avatar, "av-sm"))
            h3.msg-name { (h.name) }
            span.msg-time { (h.time) }
        }
    }
}

/// An agent's message (untrusted prose, rendered as [`markdown`]); `head` on the turn's first.
pub fn agent_message(head: Option<&MessageHead<'_>>, name: &str, text: &str) -> Markup {
    html! {
        div.msg.msg-agent {
            @match head {
                Some(h) => (message_head(h)),
                None => h3.visually-hidden { (name) },
            }
            div.body { (markdown(text)) }
        }
    }
}

/// One line of a steps card.
pub struct StepLine<'a> {
    pub label: &'a str,
    pub detail: Option<&'a str>,
    pub state: ActivityState,
    /// The agent's thinking (Settings → Developer): opens to the thought.
    pub thought: Option<&'a str>,
}

fn state_words(state: ActivityState) -> &'static str {
    match state {
        ActivityState::Working => "Working",
        ActivityState::Done => "Done",
        ActivityState::Failed => "Failed",
        ActivityState::Stopped => "Stopped",
    }
}

/// The steps of an agent turn as one checklist card: each step's mark, its words, and its
/// state on the right ("Done"). A thought opens to its text.
pub fn steps_card(head: Option<&MessageHead<'_>>, steps: &[StepLine<'_>]) -> Markup {
    html! {
        div.msg.msg-steps {
            @if let Some(h) = head { (message_head(h)) }
            ul.steps-card aria-label="Steps" {
                @for st in steps {
                    li class={ "step state-" (state_words(st.state).to_lowercase()) } {
                        @match st.thought.filter(|t| !t.is_empty()) {
                            Some(text) => {
                                details.step-thought {
                                    summary.step-row {
                                        (activity_icon(st.state))
                                        span.step-label { (st.label) }
                                        span.step-state { (state_words(st.state)) }
                                    }
                                    div.detail { (markdown(text)) }
                                }
                            }
                            None => {
                                div.step-row {
                                    (activity_icon(st.state))
                                    span.step-label {
                                        (st.label)
                                        @if let Some(d) = st.detail { " " span.step-detail { (d) } }
                                    }
                                    span.step-state { (state_words(st.state)) }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

// --- The setup password card -------------------------------------------------------------------

/// What a setup password card shows (owner design 2026-11-21).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupPasswordState<'a> {
    /// As the chat first renders it: masked, loading its state from the server.
    Loading,
    /// Masked. `confirm`: asking "Forget it?" before a dismiss.
    Hidden { confirm: bool },
    /// The password in place, after the owner asked for it.
    Shown { password: &'a str },
    /// The owner dismissed it: Melissa can't show it again.
    Dismissed,
    /// The app is gone, or has no setup password any more.
    Gone,
    /// Masked, with a note that the last read failed.
    Failed,
}

/// The setup password card: an install generated an app's password.
pub struct SetupPasswordCard<'a> {
    /// "Nextcloud setup password".
    pub title: &'a str,
    pub login: Option<&'a str>,
    /// The card's URL (`GET`: the card; `/reveal` and `/dismiss` below it).
    pub url: &'a str,
    pub state: SetupPasswordState<'a>,
}

/// Styled like the permission card: a key mark and the title, the login and the password
/// (dots until asked; click shows it, Hide hides it), a one-line warning, then Copy (where
/// the clipboard works) and Dismiss, which asks "Forget it?" in the card. Dismissed or gone:
/// one quiet line. Every swap replaces the whole card (`closest .setup-password`).
pub fn setup_password_card(c: &SetupPasswordCard<'_>) -> Markup {
    let reveal = format!("{}/reveal", c.url);
    let dismiss = format!("{}/dismiss", c.url);
    let confirm = format!("{}?confirm=1", c.url);
    let trigger = match c.state {
        SetupPasswordState::Loading => "load, setup-refresh",
        _ => "setup-refresh",
    };
    match c.state {
        SetupPasswordState::Dismissed | SetupPasswordState::Gone => html! {
            p.setup-password.setup-ended {
                (glyph(Glyph::Key))
                span {
                    (c.title)
                    @if c.state == SetupPasswordState::Dismissed { " dismissed" }
                    @else { " no longer available" }
                }
            }
        },
        state => html! {
            article.approval.setup-password aria-label=(c.title) hx-get=(c.url)
                hx-trigger=(trigger) hx-target="this" hx-swap="outerHTML" {
                div.approval-head {
                    (glyph(Glyph::Key))
                    h3 { (c.title) }
                }
                dl.facts {
                    @if let Some(login) = c.login {
                        div { dt { "Login" } dd { (login) } }
                    }
                    div {
                        dt { "Password" }
                        dd {
                            @if let SetupPasswordState::Shown { password } = state {
                                code.secret-value { (password) }
                                button.(Btn::link()) type="button" hx-get=(c.url)
                                    hx-target="closest .setup-password" { "Hide" }
                            } @else {
                                button.secret.(Btn::ghost().sm()) type="button" hx-post=(reveal)
                                    hx-target="closest .setup-password" hx-disabled-elt="this"
                                    aria-label="Show password" { "••••••••" }
                            }
                        }
                    }
                }
                p.setup-warn.tone-attention { (glyph(Glyph::Warn)) span { "Change this password after you first sign in." } }
                @if state == SetupPasswordState::Failed {
                    p.sheet-msg.tone-caution role="status" { "Couldn't reach the app. Try again." }
                }
                div.actions {
                    button.copy-secret.(Btn::secondary().sm()) type="button" data-copy-url=(reveal) { "Copy" }
                    @if state == (SetupPasswordState::Hidden { confirm: true }) {
                        span.forget-question { "Forget it?" }
                        form method="post" action=(dismiss) hx-post=(dismiss)
                            hx-target="closest .setup-password" {
                            (crate::components::csrf_field())
                            button.(Btn::danger()) type="submit" { "Yes" }
                        }
                        button.(Btn::secondary()) type="button" hx-get=(c.url)
                            hx-target="closest .setup-password" autofocus { "No" }
                    } @else {
                        button.(Btn::secondary()) type="button" hx-get=(confirm)
                            hx-target="closest .setup-password" { "Dismiss" }
                    }
                }
            }
        },
    }
}

// --- The permission card ------------------------------------------------------------------------

/// A permission card (the approval card, owner design 2026-11-20).
pub struct PermissionCard<'a> {
    pub id: u64,
    /// DOM id of the element whose content the buttons replace.
    pub target: &'a str,
    /// The question ("Set up Minecraft?").
    pub title: &'a str,
    /// What happens, in plain words.
    pub consequence: &'a str,
    /// Melissa's reason for asking (from the policy).
    pub reason: &'a str,
    /// The tag line: the kind of request and a detail ("Network access", "TCP 25565").
    pub kind: &'a str,
    pub detail: Option<&'a str>,
    pub destructive: bool,
    pub permanent: bool,
    /// The verb for yes ("Set up", "Allow").
    pub yes: &'a str,
    pub no: &'a str,
    pub state: ApprovalState,
    /// The "Always allow" question (the rule it saves); None: no such button.
    pub always: Option<&'a str>,
    /// What the app may reach (Internet, Local network, Links), as a box under the
    /// consequence ([`super::network_box`]). Empty: none.
    pub network: &'a [(&'a str, &'a str)],
    /// The owner's own words the request cites, verified by Melissa: a "You asked: …" line.
    pub quote: Option<&'a str>,
}

/// The card: a warning mark and the question, what happens and why Melissa asks, the tag line,
/// then the verb (primary), Decline and a quiet Always allow, with "Waiting for your decision"
/// on the right. Decided: the outcome instead of the buttons.
pub fn permission_card(c: &PermissionCard<'_>) -> Markup {
    let class = match (c.state, c.destructive) {
        (ApprovalState::Pending, true) => "approval destructive",
        (ApprovalState::Pending, false) => "approval",
        _ => "approval decided",
    };
    let action = |verdict: &str| format!("/ui/confirmations/{}/{verdict}", c.id);
    let target = format!("#{}", c.target);
    html! {
        article class=(class) aria-labelledby={ "ap-" (c.id) } {
            div.approval-head {
                (glyph(Glyph::Warn))
                h3 #{ "ap-" (c.id) } { (c.title) }
            }
            p.consequence { (c.consequence) }
            @if !c.network.is_empty() { (super::network_box(c.network)) }
            @if let Some(q) = c.quote {
                // A 2px rule, then "You asked:" (muted) and the owner's words, two lines at most.
                blockquote.approval-quote {
                    p { span.quote-label { "You asked:" } " “" (q) "”" }
                }
            }
            @if c.permanent && c.state == ApprovalState::Pending {
                p.permanent { "Can't be undone." }
            }
            p.reason { "Asked because " (c.reason) "." }
            p.approval-tag {
                span.tag-kind { (c.kind) }
                @if let Some(d) = c.detail { span.tag-sep aria-hidden="true" { "·" } span.tag-detail { (d) } }
            }
            @match c.state {
                ApprovalState::Pending => div.actions {
                    form method="post" action=(action("approve")) hx-post=(action("approve"))
                        hx-target=(target) hx-swap="innerHTML" {
                        (crate::components::csrf_field())
                        button.(if c.destructive { Btn::danger() } else { Btn::primary() }) type="submit" {
                            (glyph(Glyph::Check)) span { (c.yes) }
                        }
                    }
                    form method="post" action=(action("deny")) hx-post=(action("deny"))
                        hx-target=(target) hx-swap="innerHTML" {
                        (crate::components::csrf_field())
                        button.(Btn::secondary()) type="submit" { (c.no) }
                    }
                    @if let Some(question) = c.always {
                        form method="post" action=(action("always")) hx-post=(action("always"))
                            hx-target=(target) hx-swap="innerHTML" {
                            (crate::components::csrf_field())
                            button.(Btn::ghost()) type="submit" title=(question) { "Always allow" }
                        }
                    }
                    span.waiting { "Waiting for your decision" }
                },
                state => (approval_state_outcome(state)),
            }
        }
    }
}
