//! Pages and fragments, assembled only from `components`. No markup is written here.

use std::{collections::HashSet, time::Duration};

use maud::{html, Markup};
use melissa_agent::{BackendInfo, Stream, TaskSnapshot, TaskStatus, MAX_PROMPT_BYTES};

use crate::{
    components::{
        action_button, activity_row, add_app_tile, agent_message, app_dialog, app_grid, app_page,
        app_tile, badge, card, chat_divider, composer, composer_status, crumbs, device_card,
        empty_state, facts, feed, feed_entry, link, live_region, live_region_on, log_view,
        markdown, note, oob_inner, page, page_header, permission_card, plain_text, post_form,
        relayed_message, scheduled_message, section, select_field, steps_card, table,
        textarea_field, truncated, user_message_with, AppWorkspace, Avatar, Choice, DeviceCard,
        Health, LogLine, MessageHead, Meter, Nav, PermissionCard, StepLine, Tone, APPROVALS_ID,
        COMPOSER_STATUS_ID, FEED_ID,
    },
    components::{
        chat_head, composer_closed, details_panel, details_toggle, feed_archive, file_count,
        file_panel_inner, model_chip, model_picker, panel, setting_card, status_pill, usage_table,
        ChatHead, ComposerTarget, DetailsPanel, FilePanel, HeadStatus, ModelPicker, SentFile,
        UpdateHint, UsageRow, CHAT_STATUS_ID, FILE_BODY_ID, MODEL_CHIP_ID,
    },
    components::{phone_view, queued_messages, PhoneView, QueuedView, SheetWho, QUEUE_ID},
    feed::{Approval, Change, Clock, Entry, Feed, Item},
    state::{phase_words, Apps, ClusterSnapshot, Device, Disk, Keeper},
    workspace::{groups_update, sidebar_markup, SideData},
};
use melissa_agent::{
    attachments::{human_size, Attachment},
    lead::transcript::PastChat,
};

pub const TASKS_ID: &str = "tasks";
pub const TASK_ID: &str = "task";

// --- Shared pieces --------------------------------------------------------------------------

fn device_health(d: &Device) -> (Health, &'static str) {
    match d.ready {
        Some(true) => (Health::Ok, "Online"),
        Some(false) => (Health::Problem, "Not ready"),
        None => (Health::Idle, "Status unknown"),
    }
}

/// What every page with the sidebar needs.
pub struct Frame<'a> {
    /// No permission prompts is on (developer mode).
    pub bypass: bool,
    /// The developer option "Full access per agent" is on.
    pub full_access: bool,
    pub snap: &'a ClusterSnapshot,
    /// The sidebar: agents by state, devices.
    pub side: &'a SideData,
    /// Archived chats with Melissa (from before one chat per agent), newest first.
    pub past: &'a [PastChat],
    /// The `Host` the visitor used (for app links), if the page needs it.
    pub host: Option<&'a str>,
    /// The bottom bar's update button.
    pub update: UpdateHint,
}

impl Frame<'_> {
    /// The lead agent's display name ("Melissa" until the user renames it).
    pub fn lead_name(&self) -> &str {
        self.side.lead_name()
    }
}

/// A page with the sidebar. (No permission prompts shows no banner: the owner turned it on
/// knowingly; Settings → Developer shows it.)
pub fn shell(_f: &Frame<'_>, title: &str, rail: Markup, main: Markup) -> Markup {
    app_page(title, rail, main)
}

/// A page that is a panel (Settings, Apps, Usage and resources): the panel alone when the
/// modal asks for it (see `crate::panel`), else the full page with the sidebar.
pub fn panel_shell(f: &Frame<'_>, title: &str, rail: Markup, panel: Markup) -> Markup {
    if crate::panel::requested() {
        panel
    } else {
        shell(f, title, rail, panel)
    }
}

/// An archived chat's page.
pub fn past_chat_href(id: u64) -> String {
    format!("/chats/{id}")
}

/// A short date ("18 Nov") for Unix seconds, in UTC (no time zone data on the box yet).
pub fn short_date(secs: u64) -> String {
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let days = (secs / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let _year = era * 400 + yoe + i64::from(month <= 2);
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let name = MONTHS.get((month - 1) as usize).copied().unwrap_or("");
    format!("{day} {name}")
}

/// Melissa's chat. `/` shows it too, except on phones, where `/` is the agent list
/// (`components::mobile`).
pub const MELISSA_CHAT_URL: &str = "/chat";

tokio::task_local! {
    /// The page is the home page `/`, which phones show as the agent list.
    static PHONE_HOME: bool;
}

/// Render `page` as the home page: on phones the agent list instead of Melissa's chat.
pub(crate) async fn as_phone_home<F: std::future::Future>(page: F) -> F::Output {
    PHONE_HOME.scope(true, page).await
}

fn phone_view_now() -> PhoneView {
    if PHONE_HOME.try_with(|h| *h).unwrap_or(false) {
        PhoneView::List
    } else {
        PhoneView::Page
    }
}

/// A helper's page.
pub fn agent_href(id: &str) -> String {
    format!("/agents/{id}")
}

/// The sidebar. `current_agent`: the helper whose conversation is open (Melissa's: `nav` is
/// [`Nav::Chat`]).
pub fn rail_for(nav: Nav, f: &Frame<'_>, current_agent: Option<&str>) -> Markup {
    let current = match (nav, current_agent) {
        (_, Some(id)) => Some(id),
        (Nav::Chat, None) => Some("lead"),
        _ => None,
    };
    sidebar_markup(nav, f.side, current, &f.update)
}

/// A live update of the sidebar's agent groups (agents came, went, moved between groups or
/// changed), for a page that shows `current`'s chat (`lead` or an id).
pub fn sidebar_update(side: &SideData, current: Option<&str>) -> Markup {
    groups_update(side, current)
}

/// A live update for the open chat of a helper that was just deleted: the composer gives
/// way to a notice and a link back to the lead agent (called `lead`).
pub fn helper_deleted(name: &str, lead: &str) -> Markup {
    composer_closed(
        &format!("{name} was deleted."),
        MELISSA_CHAT_URL,
        &format!("Back to {lead}'s chat"),
        true,
    )
}

/// The page for a helper that isn't on this device (deleted, or a wrong address).
pub fn helper_gone(f: &Frame<'_>) -> Markup {
    shell(
        f,
        "Conversation not found",
        rail_for(Nav::Helper, f, None),
        html! {
            div.page-pad {
                (page_header("Conversation not found", None))
                (crate::components::notice("This helper isn't on this device any more.", Tone::Warn))
                (link(MELISSA_CHAT_URL, &format!("Back to {}'s chat", f.lead_name())))
            }
        },
    )
}

/// Whose conversation a chat page or stream shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Who<'a> {
    Lead,
    /// A helper (agent) by id.
    Agent(&'a str),
}

impl Who<'_> {
    pub fn send_url(self) -> String {
        match self {
            Self::Lead => "/ui/messages".into(),
            Self::Agent(id) => format!("/ui/agents/{id}/messages"),
        }
    }

    /// `POST`: `steer`, `edit` or `remove` queued message `id`.
    pub fn queue_url(self, id: u64, action: &str) -> String {
        match self {
            Self::Lead => format!("/ui/queue/{id}/{action}"),
            Self::Agent(agent) => format!("/ui/agents/{agent}/queue/{id}/{action}"),
        }
    }

    pub fn cancel_url(self) -> String {
        match self {
            Self::Lead => "/ui/cancel".into(),
            Self::Agent(id) => format!("/ui/agents/{id}/cancel"),
        }
    }

    fn stream_url(self, since: u64, epoch: u64) -> String {
        match self {
            Self::Lead => format!("/ui/stream?since={since}&epoch={epoch}"),
            Self::Agent(id) => format!("/ui/agents/{id}/stream?since={since}&epoch={epoch}"),
        }
    }

    /// Where older entries load from (lazy loading).
    pub fn older_url(self, before: usize, epoch: u64) -> String {
        match self {
            Self::Lead => format!("/ui/older?before={before}&epoch={epoch}"),
            Self::Agent(id) => format!("/ui/agents/{id}/older?before={before}&epoch={epoch}"),
        }
    }

    /// Names the chat's unsent text in the browser (the composer's `data-draft`).
    pub fn draft_key(self) -> String {
        match self {
            Self::Lead => "lead".into(),
            Self::Agent(id) => format!("agent.{id}"),
        }
    }

    /// The chat page.
    pub fn chat_url(self) -> String {
        match self {
            Self::Lead => "/".into(),
            Self::Agent(id) => agent_href(id),
        }
    }

    /// `lead` or the helper's id (the sidebar's key, the file box's key).
    pub fn key(self) -> String {
        match self {
            Self::Lead => "lead".into(),
            Self::Agent(id) => id.to_string(),
        }
    }
}

/// DOM id of a feed entry.
pub fn entry_id(key: &str) -> String {
    format!("f-{key}")
}

/// Where the UI serves a file of a box (`key`: `lead` or an agent id; see `ui::box_file`).
pub fn file_href(key: &str, id: &str) -> String {
    format!("/ui/files/{key}/{id}")
}

/// Which of a conversation's sent files are still in its file box, and where they are.
pub struct FileLinks {
    /// The box key: `lead` or the agent's id.
    pub key: String,
    /// Ids of the files in the box.
    pub present: HashSet<String>,
}

impl FileLinks {
    /// No box (nothing is shown as available).
    pub fn none() -> Self {
        Self {
            key: String::new(),
            present: HashSet::new(),
        }
    }

    /// A message's files as the chat shows them; those no longer in the box as expired.
    fn sent(&self, files: &[Attachment]) -> Vec<SentFile> {
        files
            .iter()
            .map(|a| {
                let id = melissa_agent::files::attachment_id(&a.file);
                SentFile {
                    href: self.present.contains(id).then(|| file_href(&self.key, id)),
                    name: a.name.clone(),
                    size: human_size(a.size),
                    image: a.is_image(),
                }
            })
            .collect()
    }
}

/// Who the agent of a chat is, for its messages' heads.
#[derive(Debug, Clone, Copy)]
pub struct Speaker<'a> {
    pub name: &'a str,
    pub avatar: Avatar<'a>,
}

impl<'a> Speaker<'a> {
    /// The lead agent, called `name`.
    pub const fn lead(name: &'a str) -> Self {
        Speaker {
            name,
            avatar: Avatar::Melissa,
        }
    }
}

/// What rendering an entry needs besides the entry.
pub struct EntryCtx<'a> {
    pub links: &'a FileLinks,
    pub speaker: Speaker<'a>,
    pub clock: &'a Clock,
}

/// The content of one feed entry (without its `li`).
pub fn entry_content(entry: &Entry, x: &EntryCtx<'_>) -> Markup {
    let head = |at: u64, head: bool| -> Option<String> { head.then(|| x.clock.time(at)) };
    match &entry.item {
        Item::User {
            text,
            files,
            scheduled: None,
            ..
        } => user_message_with(text, &x.links.sent(files)),
        Item::User {
            text,
            scheduled: Some(name),
            ..
        } => scheduled_message(name, text),
        Item::Relayed { from, text } => relayed_message(from, text),
        Item::Reply { text, at, head: h } => {
            let time = head(*at, *h);
            let mh = time.as_deref().map(|t| MessageHead {
                avatar: x.speaker.avatar,
                name: x.speaker.name,
                time: t,
            });
            agent_message(mh.as_ref(), x.speaker.name, text)
        }
        Item::Steps { steps, at, head: h } => {
            let time = head(*at, *h);
            let mh = time.as_deref().map(|t| MessageHead {
                avatar: x.speaker.avatar,
                name: x.speaker.name,
                time: t,
            });
            let lines: Vec<StepLine<'_>> = steps
                .iter()
                .map(|s| StepLine {
                    label: &s.label,
                    detail: s.detail.as_deref(),
                    state: s.state,
                    thought: s.thought.as_deref(),
                })
                .collect();
            steps_card(mh.as_ref(), &lines)
        }
        Item::Activity {
            label,
            detail,
            state,
            agent,
        } => activity_row(
            label,
            detail.as_deref(),
            // A helper's job: the detail is the lead agent's words (Markdown). Tool calls carry
            // a short title as plain text.
            agent.is_some(),
            *state,
            agent.as_deref().map(agent_href).as_deref(),
        ),
        Item::Approval(a) => approval_content(a, &entry_id(&entry.key)),
        Item::SetupPassword { app, login } => crate::setup_password::card(
            app,
            login.as_deref(),
            crate::components::SetupPasswordState::Loading,
        ),
        Item::Note { text, detail, tone } => note(text, detail.as_deref(), *tone),
        Item::Divider { text } => chat_divider(text),
        Item::Date { text } => crate::components::chat_date(text),
    }
}

/// A permission card whose buttons replace the content of `target`.
pub fn approval_content(a: &Approval, target: &str) -> Markup {
    permission_card(&PermissionCard {
        id: a.id,
        target,
        title: &a.title,
        consequence: &a.consequence,
        reason: &a.reason,
        kind: a.kind,
        detail: a.detail.as_deref(),
        destructive: a.destructive,
        permanent: a.permanent,
        yes: a.yes,
        no: a.no,
        state: a.state,
        always: a.always.as_deref(),
    })
}

/// Shown when "Always allow" could not save its rule (nothing was approved).
pub fn approval_error(text: &str) -> Markup {
    note(
        "Couldn't save that. Nothing was allowed.",
        Some(text),
        crate::components::NoteTone::Caution,
    )
}

/// Shown when an approve/deny button is pressed for a confirmation that is already gone.
pub fn approval_gone() -> Markup {
    note(
        "Already decided, or no longer needed.",
        None,
        crate::components::NoteTone::Quiet,
    )
}

fn entries<'a>(list: impl IntoIterator<Item = &'a Entry>, x: &EntryCtx<'_>) -> Markup {
    html! {
        @for e in list { (feed_entry(&entry_id(&e.key), entry_content(e, x))) }
    }
}

/// Entries a chat page renders at first; older ones load as the user scrolls up.
pub const PAGE_ENTRIES: usize = 60;

/// The newest [`PAGE_ENTRIES`] entries before `before` (all entries: `entries().len()`),
/// with a marker at the top that loads the ones before them, if any. `all`: every entry (the
/// marker's link without script).
pub fn feed_page(
    feed: &Feed,
    before: usize,
    who: Who<'_>,
    epoch: u64,
    all: bool,
    x: &EntryCtx<'_>,
) -> Markup {
    let list = feed.entries();
    let end = before.min(list.len());
    let start = if all {
        0
    } else {
        end.saturating_sub(PAGE_ENTRIES)
    };
    html! {
        @if start > 0 {
            (crate::components::older_marker(
                &who.older_url(start, epoch),
                &format!("{}?all=1", who.chat_url()),
            ))
        }
        (entries(&list[start..end], x))
    }
}

/// Which live list a stream feeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// The chat: every entry, the composer status and the waiting count.
    Chat,
    /// The approvals page: approval entries and the waiting count.
    Approvals,
}

impl View {
    fn shows(self, item: &Item) -> bool {
        match self {
            Self::Chat => true,
            Self::Approvals => matches!(item, Item::Approval(_)),
        }
    }
}

/// What a live update needs besides the feed.
pub struct StreamExtras<'a> {
    pub backend: Option<&'a str>,
    pub picker: Option<&'a ModelPicker>,
    pub entry: &'a EntryCtx<'a>,
    /// The file panel, when the box changed (chat view).
    pub files: Option<&'a FilePanel<'a>>,
    /// The chat header's status, when it may have changed (chat view).
    pub status: Option<Option<HeadStatus>>,
}

/// The HTML for one live update: new entries (appended by the client) and out-of-band
/// replacements for changed entries, the composer status, the header's status and the file
/// panel. Empty if nothing visible changed.
pub fn stream_update(
    feed: &Feed,
    changes: &[Change],
    view: View,
    who: Who<'_>,
    x: &StreamExtras<'_>,
) -> String {
    let (backend, picker) = (x.backend, x.picker);
    let mut added = Vec::new();
    let mut updated = Vec::new();
    let (mut status, mut files, mut queue_changed) = (false, false, false);
    for c in changes {
        match *c {
            Change::Added(i) if !added.contains(&i) => added.push(i),
            Change::Updated(i) if !updated.contains(&i) => updated.push(i),
            Change::Status => status = true,
            Change::Files => files = true,
            Change::Queue => queue_changed = true,
            _ => {}
        }
    }
    let list = feed.entries();
    let shown = |i: &usize| list.get(*i).is_some_and(|e| view.shows(&e.item));
    let out = html! {
        @for i in added.iter().filter(|i| shown(i)) {
            (feed_entry(&entry_id(&list[*i].key), entry_content(&list[*i], x.entry)))
        }
        // An entry added in this batch is already rendered in its latest state.
        @for i in updated.iter().filter(|i| shown(i) && !added.contains(i)) {
            (oob_inner(&entry_id(&list[*i].key), entry_content(&list[*i], x.entry)))
        }
        @if let (true, View::Chat, Some(panel)) = (files, view, x.files) {
            (oob_inner(FILE_BODY_ID, file_panel_inner(panel)))
            (file_count(panel.files.len(), true))
            // Sent files that left the box show as expired.
            @for (i, e) in list.iter().enumerate().filter(|(i, e)| {
                matches!(&e.item, Item::User { files, .. } if !files.is_empty())
                    && !added.contains(i)
                    && !updated.contains(i)
            }) {
                (oob_inner(&entry_id(&list[i].key), entry_content(e, x.entry)))
            }
        }
        @if queue_changed && view == View::Chat {
            (oob_inner(QUEUE_ID, queue(feed, who)))
        }
        @if let (View::Chat, Some(s)) = (view, x.status) {
            (oob_inner(CHAT_STATUS_ID, status_pill(s)))
        }
        @if status && view == View::Chat {
            (oob_inner(COMPOSER_STATUS_ID, composer_status(&who.cancel_url(), feed.busy(), backend.filter(|_| picker.is_none()))))
            @if let Some(p) = picker { (oob_inner(MODEL_CHIP_ID, model_chip(p))) }
        }
    };
    out.into_string()
}

// --- Home: the chat ---------------------------------------------------------------------------

/// "Storage almost full" when a device's data disk is above [`Disk::WARN_PERCENT`].
pub(crate) fn disk_warning(snap: &ClusterSnapshot) -> Option<String> {
    let d = snap
        .devices
        .iter()
        .find(|d| d.disk.as_ref().is_some_and(Disk::nearly_full))?;
    let disk = d.disk.as_ref()?;
    let left = format_bytes(disk.available_bytes);
    Some(if snap.devices.len() > 1 {
        format!("{}: storage almost full ({left} left)", d.name)
    } else {
        format!("Storage almost full ({left} left)")
    })
}

fn format_bytes(bytes: u64) -> String {
    format_kib(bytes / 1024)
}

/// Everything a chat page shows.
pub struct ChatView<'a> {
    pub who: Who<'a>,
    /// The event log's epoch (the stream reloads the page if the log changed).
    pub epoch: u64,
    pub feed: &'a Feed,
    pub backend: Option<&'a str>,
    /// Who the chat is with ("Melissa" or the helper's name, its icon).
    pub speaker: Speaker<'a>,
    /// The model picker (switch backend, model or reasoning level), if it can be read.
    pub picker: Option<&'a ModelPicker>,
    pub busy: bool,
    pub draft: &'a str,
    pub error: Option<&'a str>,
    /// Voice input's transcribe URL when it is on and ready (a mic in the composer).
    pub voice: Option<&'static str>,
    /// Which sent files are still in the box.
    pub links: &'a FileLinks,
    /// The conversation's details panel (None: only the composer is rendered).
    pub details: Option<&'a DetailsPanel<'a>>,
    /// Render every entry (no lazy loading; the older marker's link without script).
    pub all: bool,
    /// Say under the composer (phones) that device changes need the user's OK: false when
    /// No permission prompts is on.
    pub caption: bool,
    /// While the agent works, Enter steers (else it queues; Settings → Developer → Chat).
    pub enter_steers: bool,
}

/// A chat page: Melissa's (home) or a helper's. `head`: its header.
pub fn chat(f: &Frame<'_>, v: &ChatView<'_>, head: &ChatHead<'_>) -> Markup {
    let (nav, current, title) = match v.who {
        Who::Lead => (Nav::Chat, None, "Chat"),
        Who::Agent(id) => (Nav::Helper, Some(id), v.speaker.name),
    };
    let x = EntryCtx {
        links: v.links,
        speaker: v.speaker,
        clock: v.feed.clock(),
    };
    let empty = match v.who {
        Who::Lead => empty_state(
            &format!("Hi, I'm {}.", f.lead_name()),
            "I look after the apps on your devices. Ask me to install one, check on them, or explain what's going on.",
        ),
        Who::Agent(_) => empty_state(
            &format!("Nothing here yet with {}.", v.speaker.name),
            &format!("Its jobs from {} and your messages show up here.", f.lead_name()),
        ),
    };
    // The details sheet's line on phones: "Minecraft agent · melissa".
    let sheet_sub = format!("{} · {}", head.name, head.device);
    let label = match v.who {
        Who::Lead => "Conversation".to_string(),
        Who::Agent(_) => format!("Conversation with {}", v.speaker.name),
    };
    shell(
        f,
        title,
        rail_for(nav, f, current),
        html! {
            (phone_view(phone_view_now()))
            div.chat-layout {
                div.chat {
                    (chat_head(head, details_toggle()))
                    (feed(
                        FEED_ID,
                        &label,
                        &v.who.stream_url(v.feed.last_seq(), v.epoch),
                        feed_page(v.feed, v.feed.entries().len(), v.who, v.epoch, v.all, &x),
                        empty,
                        Some(queue(v.feed, v.who)),
                    ))
                    (composer_box(v))
                    @if let Some(p) = v.picker { (model_picker(p)) }
                }
                @if let Some(p) = v.details {
                    (details_panel(p, Some(&SheetWho { avatar: head.avatar, sub: &sheet_sub })))
                }
            }
            (app_dialog())
        },
    )
}

/// The user's queued messages of a chat ([`queued_messages`]).
pub fn queue(feed: &Feed, who: Who<'_>) -> Markup {
    let items: Vec<QueuedView<'_>> = feed
        .queued()
        .iter()
        .map(|q| QueuedView {
            text: &q.text,
            files: q.files.iter().map(|f| f.name.as_str()).collect(),
            steer: who.queue_url(q.id, "steer"),
            edit: q.files.is_empty().then(|| who.queue_url(q.id, "edit")),
            remove: who.queue_url(q.id, "remove"),
        })
        .collect();
    queued_messages(&items)
}

/// The composer (also the response to sending a message).
pub fn composer_box(v: &ChatView<'_>) -> Markup {
    let (send, cancel) = (v.who.send_url(), v.who.cancel_url());
    let placeholder = format!("Message {}…", v.speaker.name);
    let draft_key = v.who.draft_key();
    composer(
        &ComposerTarget {
            send: &send,
            draft_key: &draft_key,
            enter_steers: v.enter_steers,
            placeholder: &placeholder,
            voice: v.voice,
            caption: v
                .caption
                .then_some("Device changes require your permission."),
        },
        v.draft,
        v.error,
        v.picker.map(model_chip).unwrap_or_default(),
        composer_status(&cancel, v.busy, v.backend.filter(|_| v.picker.is_none())),
    )
}

/// An archived chat with Melissa: read-only, no composer.
pub fn past_chat(f: &Frame<'_>, chat: &PastChat, feed: &Feed, links: &FileLinks) -> Markup {
    let name = f.lead_name();
    let lead = format!(
        "An earlier chat with {name}, kept read-only. Started {}, closed {}.",
        short_date(chat.started),
        short_date(chat.ended)
    );
    let x = EntryCtx {
        links,
        speaker: Speaker::lead(name),
        clock: feed.clock(),
    };
    shell(
        f,
        "Earlier chat",
        rail_for(Nav::PastChat, f, None),
        html! {
            div.chat.page-pad {
                (crumbs(&[("/", name), ("/chats", "Earlier chats")], &short_date(chat.ended)))
                (page_header("Earlier chat", Some(&lead)))
                (feed_archive(
                    FEED_ID,
                    "Earlier chat",
                    entries(feed.entries(), &x),
                    empty_state("Nothing in this chat.", "It was closed before anything was said."),
                ))
                p.quiet { (link(MELISSA_CHAT_URL, &format!("Back to {name}'s chat"))) }
            }
        },
    )
}

/// The list of Melissa's earlier chats (archived by "New chat" before one chat per agent).
pub fn past_chats(f: &Frame<'_>) -> Markup {
    shell(
        f,
        "Earlier chats",
        rail_for(Nav::PastChat, f, None),
        html! {
            div.page-pad {
                (crumbs(&[("/", f.lead_name())], "Earlier chats"))
                (page_header("Earlier chats", Some(&format!("Chats with {} from before one chat per agent. Read-only.", f.lead_name()))))
                @if f.past.is_empty() {
                    (empty_state("No earlier chats.", &format!("{} keeps one chat; there were none before it.", f.lead_name())))
                } @else {
                    (crate::components::past_chat_list(&f.past.iter().map(|p| crate::components::PastChatRow {
                        href: past_chat_href(p.id),
                        title: if p.title.is_empty() { "A chat".to_string() } else { p.title.clone() },
                        when: short_date(p.ended),
                        turns: p.turns,
                    }).collect::<Vec<_>>()))
                }
            }
        },
    )
}

// --- The cluster: apps and devices ---------------------------------------------------------

fn format_kib(kib: u64) -> String {
    let gib = kib as f64 / (1024.0 * 1024.0);
    if gib >= 10.0 {
        format!("{gib:.0} GB")
    } else {
        format!("{gib:.1} GB")
    }
}

pub(crate) fn format_uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, (secs % 86_400) / 3600, (secs % 3600) / 60);
    match (d, h) {
        (0, 0) => format!("Up {m} min"),
        (0, h) => format!("Up {h} h {m} min"),
        (d, h) => format!("Up {d} d {h} h"),
    }
}

fn device(d: &Device) -> Markup {
    let (health, status) = device_health(d);
    let mut facts = Vec::new();
    if let Some(os) = &d.os {
        facts.push(os.clone());
    }
    if let Some(cpus) = &d.cpus {
        facts.push(match cpus.as_str() {
            "1" => "1 CPU".to_string(),
            n => format!("{n} CPUs"),
        });
    }
    if let Some(k) = &d.kernel {
        facts.push(format!("Kernel {k}"));
    }
    if let Some(up) = d.uptime_secs {
        facts.push(format_uptime(up));
    }
    let mem = match (d.mem_total_kib, d.mem_available_kib) {
        (Some(total), Some(avail)) if total > 0 => {
            let used = total.saturating_sub(avail);
            Some((
                used as f64 / total as f64,
                format!("{} of {} used", format_kib(used), format_kib(total)),
            ))
        }
        _ => None,
    };
    let disk = d.disk.as_ref().map(|k| {
        let used = k.total_bytes.saturating_sub(k.available_bytes);
        (
            k.used_percent as f64 / 100.0,
            format!(
                "{} of {} used",
                format_bytes(used),
                format_bytes(k.total_bytes)
            ),
        )
    });
    let meters: Vec<Meter<'_>> = mem
        .iter()
        .map(|(fraction, text)| Meter {
            label: "Memory",
            fraction: *fraction,
            text,
        })
        .chain(disk.iter().map(|(fraction, text)| Meter {
            label: "Storage",
            fraction: *fraction,
            text,
        }))
        .collect();
    let warning = d.disk.as_ref().filter(|k| k.nearly_full()).map(|k| {
        format!(
            "Storage is almost full: {} left. New apps may not fit; ask Melissa what to clean up.",
            format_bytes(k.available_bytes)
        )
    });
    device_card(&DeviceCard {
        name: &d.name,
        health,
        status,
        facts: &facts,
        meters: &meters,
        warning: warning.as_deref(),
    })
}

/// The apps region of the cluster page (refreshes itself). App links use `host`, the address
/// the visitor reached the UI at.
pub fn cluster_apps(snap: &ClusterSnapshot, host: Option<&str>) -> Markup {
    let content = match &snap.apps {
        Apps::Unknown => html! {
            (app_grid("Apps", add_app_tile("/?draft=Install%20")))
            p.quiet { "Couldn't read the app list yet." }
        },
        Apps::Unreachable => html! {
            (app_grid("Apps", add_app_tile("/?draft=Install%20")))
            p.quiet { "The app platform isn't answering right now. Melissa keeps trying." }
        },
        Apps::Known { installed } => {
            let names: Vec<String> = installed.iter().map(|a| a.display_name()).collect();
            let details: Vec<String> = installed
                .iter()
                .map(|a| format!("/ui/apps/{}", a.name))
                .collect();
            let links: Vec<Option<String>> = installed.iter().map(|a| a.link(host)).collect();
            // An app without a web link shows where to reach it instead (`host:port (TCP)`).
            let keepers: Vec<Option<String>> = installed
                .iter()
                .map(|a| {
                    a.address(host).or_else(|| match &a.keeper {
                        Some(Keeper::Agent { name, .. }) => Some(format!("Looked after by {name}")),
                        Some(Keeper::Owner) => Some("Looked after by you".to_string()),
                        None => None,
                    })
                })
                .collect();
            html! {
                (app_grid("Apps", html! {
                    @for ((((a, name), keeper), link), details) in installed.iter().zip(&names).zip(&keepers).zip(&links).zip(&details) {
                        @let (health, status) = phase_words(&a.phase);
                        (app_tile(&AppWorkspace { name, health, status, detail: keeper.as_deref(), url: link.as_deref(), details: Some(details) }))
                    }
                    (add_app_tile("/?draft=Install%20"))
                }))
            }
        }
    };
    // Also refreshed at once after a change from the app dialog (`cluster-changed`).
    live_region_on(
        "cluster-apps",
        "/ui/cluster/apps",
        15,
        "cluster-changed",
        content,
    )
}

/// Apps (the rail's hexagon): every app with its status and who looks after it. Devices and
/// their resources are in Usage and resources.
pub fn cluster(f: &Frame<'_>) -> Markup {
    panel_shell(
        f,
        "Apps",
        rail_for(Nav::Cluster, f, None),
        panel(
            "Apps",
            html! {
                p.lead { "What runs on your devices. Ask in the chat to change anything." }
                (cluster_apps(f.snap, f.host))
                (app_dialog())
            },
        ),
    )
}

/// Usage and resources (the rail's chart): what each agent used, then the devices.
pub fn usage(f: &Frame<'_>, rows: &[UsageRow<'_>], since: &str) -> Markup {
    let snap = f.snap;
    let running = match &snap.apps {
        Apps::Known { installed } => Some(installed.iter().filter(|a| a.phase == "ready").count()),
        _ => None,
    };
    panel_shell(
        f,
        "Usage and resources",
        rail_for(Nav::Usage, f, None),
        panel(
            "Usage and resources",
            html! {
                (section("Agents", usage_table(rows, since)))
                (section("Devices", html! {
                    p.quiet {
                        @match running {
                            Some(1) => "1 app running.",
                            Some(n) => { (n) " apps running." }
                            None => "Couldn't read the app list yet.",
                        }
                    }
                    @if snap.devices.is_empty() {
                        p.quiet { "Couldn't read device details yet." }
                    } @else {
                        div.devices-grid { @for d in &snap.devices { (device(d)) } }
                    }
                }))
            },
        ),
    )
}

// --- Waiting for you ---------------------------------------------------------------------------

pub fn approvals(f: &Frame<'_>, list: &[Approval], since: u64, epoch: u64) -> Markup {
    let stream = format!("/ui/stream?since={since}&epoch={epoch}&view=approvals");
    shell(
        f,
        "Waiting for you",
        rail_for(Nav::Waiting, f, None),
        html! {
            div.chat {
                (page_header("Waiting for you", Some("Things Melissa won't do without your yes.")))
                (feed(
                    APPROVALS_ID,
                    "Waiting for you",
                    &stream,
                    html! {
                        @for a in list {
                            @let id = entry_id(&format!("c{}", a.id));
                            (feed_entry(&id, approval_content(a, &id)))
                        }
                    },
                    empty_state("Nothing waiting.", "When Melissa needs your yes, it shows up here and in the chat."),
                    None,
                ))
            }
        },
    )
}

// --- Agent tasks (developer view) -----------------------------------------------------------

/// Agents offered on the task page. The built-in one-shot `echo` (no LLM, always there) is a
/// test double, not an agent people pick; an echo the user configured has its own name and
/// is offered like any other agent.
fn offered(b: &BackendInfo) -> bool {
    !(b.kind == "echo" && b.name == "echo")
}

/// A task page: in Settings → Developer when `f` is given (the app with a lead agent), else the
/// bare page (the task UI without a lead agent). `sub`: the page below the section (a task).
fn tasks_page(f: Option<&Frame<'_>>, title: &str, sub: bool, content: Markup) -> Markup {
    match f {
        Some(f) => crate::settings::pages::frame_at(f, "Developer", sub.then_some(title), content),
        None => page(title, content),
    }
}

/// The task page; in Settings → Developer also the chat preferences.
pub fn dashboard(
    f: Option<&Frame<'_>>,
    backends: &[BackendInfo],
    tasks: &[TaskSnapshot],
    prefs: melissa_settings::prefs::Prefs,
) -> Markup {
    let offered: Vec<&BackendInfo> = backends.iter().filter(|b| offered(b)).collect();
    let lead = "Try one agent with one prompt, outside the chat.";
    let content = if f.is_some() {
        html! {
            (page_header("Developer", Some(lead)))
            (setting_card(Some("Developer mode"), html! {
                (crate::settings::pages::bypass_region(f.is_some_and(|f| f.bypass), false, None))
                (crate::settings::pages::full_access_region(f.is_some_and(|f| f.full_access), None))
            }))
            (setting_card(Some("Chat"), crate::settings::pages::chat_prefs_region(prefs, None)))
            (card("New agent task", new_task_form(&offered)))
            (card("Agent tasks", tasks_region(tasks, None)))
        }
    } else {
        html! {
            (page_header("Agent tasks", Some(lead)))
            (card("New task", new_task_form(&offered)))
            (card("Tasks", tasks_region(tasks, None)))
        }
    };
    tasks_page(f, "Agent tasks", false, content)
}

fn new_task_form(backends: &[&BackendInfo]) -> Markup {
    if !backends.iter().any(|b| b.available) {
        return html! {
            (empty_state("No agent to try yet.", "Set one up in Providers."))
            (link("/settings/providers", "Open Providers"))
        };
    }
    let labels: Vec<String> = backends
        .iter()
        .map(|b| {
            if b.available {
                b.name.clone()
            } else {
                format!("{} (not installed)", b.name)
            }
        })
        .collect();
    // Usable agents first, so the default pick is one that runs.
    let mut order: Vec<usize> = (0..backends.len()).collect();
    order.sort_by_key(|&i| !backends[i].available);
    let choices: Vec<Choice<'_>> = order
        .iter()
        .map(|&i| Choice {
            value: &backends[i].name,
            label: &labels[i],
            enabled: backends[i].available,
        })
        .collect();
    post_form(
        "/tasks",
        TASKS_ID,
        "Run",
        html! {
            (select_field("backend", "Agent", &choices))
            (textarea_field("prompt", "Prompt", "What should the agent do?", MAX_PROMPT_BYTES))
        },
    )
}

/// The task list. Polls itself while a task runs. `message` is shown above it (e.g. why a
/// task could not start).
pub fn tasks_region(tasks: &[TaskSnapshot], message: Option<(&str, Tone)>) -> Markup {
    let running = tasks.iter().any(|t| t.status == TaskStatus::Running);
    let rows = tasks
        .iter()
        .map(|t| {
            vec![
                link(&format!("/tasks/{}", t.id), &format!("#{}", t.id)),
                status_badge(t.status),
                truncated(&t.prompt, 60),
                html! { (t.backend) },
                html! { (format_elapsed(t.elapsed)) },
            ]
        })
        .collect();
    live_region(
        TASKS_ID,
        "/tasks/fragment",
        2,
        running,
        html! {
            @if let Some((text, tone)) = message { (crate::components::notice(text, tone)) }
            (table(
                &["Task", "Status", "Prompt", "Agent", "Time"],
                rows,
                "No tasks yet. Start one above.",
            ))
        },
    )
}

pub fn task_page(f: Option<&Frame<'_>>, task: &TaskSnapshot) -> Markup {
    let title = format!("Task #{}", task.id);
    let content = html! {
        @if f.is_none() { (crumbs(&[("/tasks", "Agent tasks")], &title)) }
        (page_header(&title, None))
        (task_region(task))
    };
    tasks_page(f, &title, true, content)
}

/// The task detail region; polls itself until the task finishes.
pub fn task_region(task: &TaskSnapshot) -> Markup {
    let running = task.status == TaskStatus::Running;
    let time = if running {
        format!("Running for {}", format_elapsed(task.elapsed))
    } else {
        format!("Took {}", format_elapsed(task.elapsed))
    };
    let reply: String = task
        .output
        .iter()
        .filter(|l| l.stream == Stream::Stdout)
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let failed = task.status == TaskStatus::Failed;
    let log: Vec<LogLine<'_>> = task
        .output
        .iter()
        .filter(|l| l.stream == Stream::Stderr)
        .map(|l| LogLine {
            text: &l.text,
            is_error: failed,
        })
        .collect();
    live_region(
        TASK_ID,
        &format!("/tasks/{}/fragment", task.id),
        1,
        running,
        html! {
            (card("About", html! {
                (facts(&[
                    ("Agent", html! { (task.backend) }),
                    ("Status", status_badge(task.status)),
                    ("Time", html! { (time) }),
                ]))
                @if let Some(error) = &task.error {
                    (crate::components::notice(&format!("The agent failed: {error}."), Tone::Error))
                }
                @if task.truncated {
                    (crate::components::notice("The output was too long; the end is cut off.", Tone::Warn))
                }
                @if running {
                    (action_button(&format!("/tasks/{}/cancel", task.id), TASK_ID, "Stop", "Stop this task?"))
                }
            }))
            (card("Prompt", plain_text(&task.prompt)))
            (card("Reply", html! {
                @if !reply.trim().is_empty() {
                    (markdown(&reply))
                } @else if running {
                    p.empty { "Waiting for the agent…" }
                } @else {
                    p.empty { "No reply." }
                }
            }))
            @if !log.is_empty() {
                (card("Agent log", log_view(log)))
            }
        },
    )
}

pub fn error_page(title: &str, message: &str) -> Markup {
    page(
        title,
        card(
            title,
            html! { (crate::components::notice(message, Tone::Error)) (link("/", "Back to the chat")) },
        ),
    )
}

pub fn not_found() -> Markup {
    page(
        "Not found",
        card(
            "Not found",
            html! { (crate::components::notice("No such page.", Tone::Warn)) (link("/", "Back to the chat")) },
        ),
    )
}

fn status_badge(status: TaskStatus) -> Markup {
    let tone = match status {
        TaskStatus::Running => Tone::Info,
        TaskStatus::Succeeded => Tone::Ok,
        TaskStatus::Failed => Tone::Error,
        TaskStatus::Cancelled => Tone::Muted,
    };
    let label = match status {
        TaskStatus::Running => "Running",
        TaskStatus::Succeeded => "Done",
        TaskStatus::Failed => "Failed",
        TaskStatus::Cancelled => "Stopped",
    };
    badge(label, tone)
}

fn format_elapsed(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 0.1 {
        "under 0.1 s".into()
    } else if secs < 60.0 {
        format!("{secs:.1} s")
    } else {
        format!("{} min {:02} s", d.as_secs() / 60, d.as_secs() % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_formatting() {
        assert_eq!(format_elapsed(Duration::from_millis(20)), "under 0.1 s");
        assert_eq!(format_elapsed(Duration::from_millis(1500)), "1.5 s");
        assert_eq!(format_elapsed(Duration::from_secs(125)), "2 min 05 s");
        assert_eq!(format_uptime(3 * 86_400 + 7200), "Up 3 d 2 h");
        assert_eq!(format_kib(8 * 1024 * 1024), "8.0 GB");
    }
}
