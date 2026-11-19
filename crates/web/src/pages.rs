//! Pages and fragments, assembled only from `components`. No markup is written here.

use std::time::Duration;

use maud::{html, Markup};
use melissa_agent::{BackendInfo, Stream, TaskSnapshot, TaskStatus, MAX_PROMPT_BYTES};

use crate::{
    components::{
        action_button, activity_row, add_app_cell, app_cell, app_dialog, app_page, app_strip,
        approval_card, badge, card, chat_divider, composer, composer_status, crumbs, device_card,
        empty_state, facts, feed, feed_entry, honeycomb, link, live_region, live_region_on,
        log_view, markdown, note, oob_inner, page, page_header, plain_text, post_form,
        queen_message, rail, relayed_message, section, select_field, table, textarea_field,
        truncated, user_message, waiting_count, AppCell, ApprovalCard, Choice, DeviceCard, Health,
        LogLine, Meter, Nav, RailDevice, StripCell, Tone, APPROVALS_ID, COMPOSER_STATUS_ID,
        FEED_ID,
    },
    components::{feed_archive, AgentMenu, ComposerTarget, RailHelper, RailPastChat},
    feed::{display_app, Approval, Change, Entry, Feed, Item},
    state::{phase_words, Apps, Device, Disk, HiveSnapshot, Keeper},
};
use melissa_agent::queen::{bees::BeeView, transcript::PastChat};

pub const TASKS_ID: &str = "tasks";
pub const TASK_ID: &str = "task";

/// URL of the app strip fragment.
pub const STRIP_URL: &str = "/ui/strip";

// --- Shared pieces --------------------------------------------------------------------------

fn device_health(d: &Device) -> (Health, &'static str) {
    match d.ready {
        Some(true) => (Health::Ok, "Online"),
        Some(false) => (Health::Problem, "Not ready"),
        None => (Health::Idle, "Status unknown"),
    }
}

/// What every page with the rail needs.
pub struct Frame<'a> {
    pub waiting: usize,
    pub snap: &'a HiveSnapshot,
    pub bees: &'a [BeeView],
    /// Archived chats with Melissa, newest first.
    pub past: &'a [PastChat],
    /// The `Host` the visitor used (for app links), if the page needs it.
    pub host: Option<&'a str>,
}

/// An archived chat's page.
pub fn past_chat_href(id: u64) -> String {
    format!("/chats/{id}")
}

/// Past chats shown in the rail.
const RAIL_PAST_CHATS: usize = 5;

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

/// A helper's page.
pub fn bee_href(id: &str) -> String {
    format!("/bees/{id}")
}

fn bee_health(status: &str) -> (Health, &'static str) {
    match status {
        "working" => (Health::Busy, "working"),
        "stopped" => (Health::Idle, "stopped"),
        _ => (Health::Ok, "idle"),
    }
}

/// The rail. `current_bee`: the helper whose conversation is open.
pub fn rail_for(nav: Nav, f: &Frame<'_>, current_bee: Option<&str>) -> Markup {
    rail_with(nav, f, current_bee, None)
}

/// The rail; `current_past`: the archived chat that is open.
fn rail_with(
    nav: Nav,
    f: &Frame<'_>,
    current_bee: Option<&str>,
    current_past: Option<u64>,
) -> Markup {
    let devices: Vec<RailDevice<'_>> = f
        .snap
        .devices
        .iter()
        .map(|d| {
            let (health, status) = device_health(d);
            RailDevice {
                name: &d.name,
                health,
                status,
            }
        })
        .collect();
    let hrefs: Vec<String> = f.bees.iter().map(|b| bee_href(&b.id)).collect();
    let helpers: Vec<RailHelper<'_>> = f
        .bees
        .iter()
        .zip(&hrefs)
        .map(|(b, href)| {
            let (health, status) = bee_health(b.status);
            RailHelper {
                href,
                name: &b.name,
                health,
                status,
                current: current_bee == Some(b.id.as_str()),
            }
        })
        .collect();
    let shown = &f.past[..f.past.len().min(RAIL_PAST_CHATS)];
    let past_words: Vec<(String, String, String)> = shown
        .iter()
        .map(|p| {
            let title = if p.title.is_empty() {
                "A chat".to_string()
            } else {
                p.title.clone()
            };
            (past_chat_href(p.id), title, short_date(p.ended))
        })
        .collect();
    let past: Vec<RailPastChat<'_>> = shown
        .iter()
        .zip(&past_words)
        .map(|(p, (href, title, when))| RailPastChat {
            href,
            title,
            when,
            current: current_past == Some(p.id),
        })
        .collect();
    rail(nav, f.waiting, &devices, &helpers, &past)
}

/// Whose conversation a chat page or stream shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Who<'a> {
    Queen,
    /// A helper (bee) by id.
    Bee(&'a str),
}

impl Who<'_> {
    pub fn send_url(self) -> String {
        match self {
            Self::Queen => "/ui/messages".into(),
            Self::Bee(id) => format!("/ui/bees/{id}/messages"),
        }
    }

    pub fn cancel_url(self) -> String {
        match self {
            Self::Queen => "/ui/cancel".into(),
            Self::Bee(id) => format!("/ui/bees/{id}/cancel"),
        }
    }

    fn stream_url(self, since: u64, epoch: u64) -> String {
        match self {
            Self::Queen => format!("/ui/stream?since={since}&epoch={epoch}"),
            Self::Bee(id) => format!("/ui/bees/{id}/stream?since={since}&epoch={epoch}"),
        }
    }
}

/// DOM id of a feed entry.
pub fn entry_id(key: &str) -> String {
    format!("f-{key}")
}

/// The content of one feed entry (without its `li`).
pub fn entry_content(entry: &Entry) -> Markup {
    match &entry.item {
        Item::User { text } => user_message(text),
        Item::Relayed { from, text } => relayed_message(from, text),
        Item::Reply { text } => queen_message(text),
        Item::Activity {
            label,
            detail,
            state,
            bee,
        } => activity_row(
            label,
            detail.as_deref(),
            // A helper's job: the detail is the Queen's words (Markdown). Tool calls carry
            // a short title as plain text.
            bee.is_some(),
            *state,
            bee.as_deref().map(bee_href).as_deref(),
        ),
        Item::Approval(a) => approval_content(a, &entry_id(&entry.key)),
        Item::Note { text, detail, tone } => note(text, detail.as_deref(), *tone),
        Item::Divider { text } => chat_divider(text),
    }
}

/// An approval card whose buttons replace the content of `target`.
pub fn approval_content(a: &Approval, target: &str) -> Markup {
    approval_card(&ApprovalCard {
        id: a.id,
        target,
        title: &a.title,
        consequence: &a.consequence,
        reason: &a.reason,
        destructive: a.destructive,
        permanent: a.permanent,
        yes: a.yes,
        no: a.no,
        state: a.state,
    })
}

/// Shown when an approve/deny button is pressed for a confirmation that is already gone.
pub fn approval_gone() -> Markup {
    note(
        "Already decided, or no longer needed.",
        None,
        crate::components::NoteTone::Quiet,
    )
}

fn entries<'a>(list: impl IntoIterator<Item = &'a Entry>) -> Markup {
    html! {
        @for e in list { (feed_entry(&entry_id(&e.key), entry_content(e))) }
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

/// The HTML for one live update: new entries (appended by the client) and out-of-band
/// replacements for changed entries, the composer status and the waiting count. Empty if
/// nothing visible changed.
pub fn stream_update(
    feed: &Feed,
    changes: &[Change],
    view: View,
    who: Who<'_>,
    backend: Option<&str>,
    agents: Option<&AgentMenu>,
    waiting: usize,
) -> String {
    let mut added = Vec::new();
    let mut updated = Vec::new();
    let (mut status, mut count) = (false, false);
    for c in changes {
        match *c {
            Change::Added(i) if !added.contains(&i) => added.push(i),
            Change::Updated(i) if !updated.contains(&i) => updated.push(i),
            Change::Status => status = true,
            Change::Waiting => count = true,
            _ => {}
        }
    }
    let list = feed.entries();
    let shown = |i: &usize| list.get(*i).is_some_and(|e| view.shows(&e.item));
    let out = html! {
        @for i in added.iter().filter(|i| shown(i)) {
            (feed_entry(&entry_id(&list[*i].key), entry_content(&list[*i])))
        }
        // An entry added in this batch is already rendered in its latest state.
        @for i in updated.iter().filter(|i| shown(i) && !added.contains(i)) {
            (oob_inner(&entry_id(&list[*i].key), entry_content(&list[*i])))
        }
        @if status && view == View::Chat {
            (oob_inner(COMPOSER_STATUS_ID, composer_status(&who.cancel_url(), backend, feed.busy(), agents)))
        }
        @if count { (waiting_count(waiting, true)) }
    };
    out.into_string()
}

// --- Home: the chat ---------------------------------------------------------------------------

/// The app strip from a snapshot.
pub fn strip(snap: &HiveSnapshot) -> Markup {
    let words: Vec<(String, Health, &str)> = match &snap.apps {
        Apps::Known { installed, .. } => installed
            .iter()
            .map(|a| {
                let (h, w) = phase_words(&a.phase);
                (a.display_name(), h, w)
            })
            .collect(),
        _ => Vec::new(),
    };
    let summary = match &snap.apps {
        Apps::Unknown => "Checking your apps…".to_string(),
        Apps::Unreachable => "Can't reach the app platform right now.".to_string(),
        Apps::Known { installed, .. } if installed.is_empty() => {
            "No apps yet. Ask me to install one.".to_string()
        }
        Apps::Known { .. } => words
            .iter()
            .map(|(name, _, w)| format!("{name} {}", w.to_lowercase()))
            .collect::<Vec<_>>()
            .join(", "),
    };
    let cells: Vec<StripCell<'_>> = words
        .iter()
        .map(|(name, health, _)| StripCell {
            name,
            health: *health,
        })
        .collect();
    let warning = disk_warning(snap);
    app_strip(STRIP_URL, &cells, &summary, warning.as_deref())
}

/// "Storage almost full" when a device's data disk is above [`Disk::WARN_PERCENT`].
fn disk_warning(snap: &HiveSnapshot) -> Option<String> {
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
    /// The agent menu (switch backend or model), if the conversation has choices.
    pub agents: Option<&'a AgentMenu>,
    pub busy: bool,
    pub draft: &'a str,
    pub error: Option<&'a str>,
}

/// The chat with Melissa (the Queen): home.
pub fn home(f: &Frame<'_>, v: &ChatView<'_>) -> Markup {
    app_page(
        "Chat",
        rail_for(Nav::Chat, f, None),
        html! {
            div.chat {
                (strip(f.snap))
                (feed(
                    FEED_ID,
                    "Conversation",
                    &v.who.stream_url(v.feed.last_seq(), v.epoch),
                    entries(v.feed.entries()),
                    empty_state(
                        "Hi, I'm Melissa.",
                        "I look after the apps on your devices. Ask me to install one, check on them, or explain what's going on.",
                    ),
                ))
                (composer_box(v))
            }
        },
    )
}

/// The chat with one helper.
pub fn bee_chat(f: &Frame<'_>, bee: &BeeView, v: &ChatView<'_>) -> Markup {
    let cells: Vec<String> = match &f.snap.apps {
        Apps::Known { installed, .. } => installed
            .iter()
            .filter(|a| matches!(&a.keeper, Some(Keeper::Bee { id, .. }) if *id == bee.id))
            .map(|a| a.display_name())
            .collect(),
        _ => Vec::new(),
    };
    let lead = match cells.len() {
        0 => "A helper on this device. Melissa gives it jobs; you can talk to it here.".to_string(),
        _ => format!("A helper on this device. Looks after {}.", cells.join(", ")),
    };
    app_page(
        &bee.name,
        rail_for(Nav::Helper, f, Some(&bee.id)),
        html! {
            div.chat {
                (page_header(&bee.name, Some(&lead)))
                (feed(
                    FEED_ID,
                    &format!("Conversation with {}", bee.name),
                    &v.who.stream_url(v.feed.last_seq(), v.epoch),
                    entries(v.feed.entries()),
                    empty_state(
                        &format!("Nothing here yet with {}.", bee.name),
                        "Its jobs from Melissa and your messages show up here.",
                    ),
                ))
                (composer_box(v))
            }
        },
    )
}

/// The composer (also the response to sending a message).
pub fn composer_box(v: &ChatView<'_>) -> Markup {
    let (send, cancel) = (v.who.send_url(), v.who.cancel_url());
    let placeholder = match v.who {
        Who::Queen => "Ask Melissa to install, check or fix something",
        Who::Bee(_) => "Message this helper",
    };
    composer(
        &ComposerTarget {
            send: &send,
            cancel: &cancel,
            placeholder,
        },
        v.draft,
        v.error,
        composer_status(&cancel, v.backend, v.busy, v.agents),
    )
}

/// An archived chat with Melissa: read-only, no composer.
pub fn past_chat(f: &Frame<'_>, chat: &PastChat, feed: &Feed) -> Markup {
    let lead = format!(
        "An earlier chat with Melissa, kept read-only. Started {}, closed {}.",
        short_date(chat.started),
        short_date(chat.ended)
    );
    app_page(
        "Past chat",
        rail_with(Nav::PastChat, f, None, Some(chat.id)),
        html! {
            div.chat {
                (page_header("Past chat", Some(&lead)))
                (feed_archive(
                    FEED_ID,
                    "Past chat",
                    entries(feed.entries()),
                    empty_state("Nothing in this chat.", "It was closed before anything was said."),
                ))
                p.quiet { (link("/", "Back to the current chat")) }
            }
        },
    )
}

// --- The hive: apps and devices ---------------------------------------------------------

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

/// The apps region of the hive page (refreshes itself). App links use `host`, the address
/// the visitor reached the UI at.
pub fn hive_apps(snap: &HiveSnapshot, host: Option<&str>) -> Markup {
    let content = match &snap.apps {
        Apps::Unknown => html! {
            (honeycomb("Apps", add_app_cell("/?draft=Install%20")))
            p.quiet { "Couldn't read the app list yet." }
        },
        Apps::Unreachable => html! {
            (honeycomb("Apps", add_app_cell("/?draft=Install%20")))
            p.quiet { "The app platform isn't answering right now. Melissa keeps trying." }
        },
        Apps::Known {
            installed,
            available,
        } => {
            let names: Vec<String> = installed.iter().map(|a| a.display_name()).collect();
            let details: Vec<String> = installed
                .iter()
                .map(|a| format!("/ui/apps/{}", a.name))
                .collect();
            let links: Vec<Option<String>> = installed.iter().map(|a| a.link(host)).collect();
            let keepers: Vec<Option<String>> = installed
                .iter()
                .map(|a| match &a.keeper {
                    Some(Keeper::Bee { name, .. }) => Some(format!("Looked after by {name}")),
                    Some(Keeper::Owner) => Some("Looked after by you".to_string()),
                    None => None,
                })
                .collect();
            html! {
                (honeycomb("Apps", html! {
                    @for ((((a, name), keeper), link), details) in installed.iter().zip(&names).zip(&keepers).zip(&links).zip(&details) {
                        @let (health, status) = phase_words(&a.phase);
                        (app_cell(&AppCell { name, health, status, detail: keeper.as_deref(), url: link.as_deref(), details: Some(details) }))
                    }
                    (add_app_cell("/?draft=Install%20"))
                }))
                @if !available.is_empty() {
                    p.quiet {
                        "Ready to install: "
                        @for (i, n) in available.iter().enumerate() {
                            @if i > 0 { ", " }
                            (link(&format!("/?draft=Install%20{n}"), &display_app(n)))
                        }
                    }
                }
            }
        }
    };
    // Also refreshed at once after a change from the app dialog (`hive-changed`).
    live_region_on("hive-apps", "/ui/hive/apps", 15, "hive-changed", content)
}

pub fn hive(f: &Frame<'_>) -> Markup {
    let snap = f.snap;
    app_page(
        "Apps and devices",
        rail_for(Nav::Hive, f, None),
        html! {
            div.wide {
                (page_header("Apps and devices", Some("What runs where. Ask in the chat to change anything.")))
                (section("Apps", hive_apps(snap, f.host)))
                (app_dialog())
                (section("Devices", html! {
                    @if snap.devices.is_empty() {
                        p.quiet { "Couldn't read device details yet." }
                    } @else {
                        div.devices-grid { @for d in &snap.devices { (device(d)) } }
                    }
                }))
            }
        },
    )
}

// --- Waiting for you ---------------------------------------------------------------------------

pub fn approvals(f: &Frame<'_>, list: &[Approval], since: u64, epoch: u64) -> Markup {
    let stream = format!("/ui/stream?since={since}&epoch={epoch}&view=approvals");
    app_page(
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

/// A task page: in Settings → Developer when `f` is given (the app with a Queen), else the
/// bare page (the task UI without a Queen). `sub`: the page below the section (a task).
fn tasks_page(f: Option<&Frame<'_>>, title: &str, sub: bool, content: Markup) -> Markup {
    match f {
        Some(f) => crate::settings::pages::frame_at(f, "Developer", sub.then_some(title), content),
        None => page(title, content),
    }
}

pub fn dashboard(
    f: Option<&Frame<'_>>,
    backends: &[BackendInfo],
    tasks: &[TaskSnapshot],
) -> Markup {
    let offered: Vec<&BackendInfo> = backends.iter().filter(|b| offered(b)).collect();
    let lead = "Give one agent one prompt and see its reply, outside the chat. For trying out \
                agents: they can look but not change apps, and the list is cleared when Melissa \
                restarts.";
    let content = if f.is_some() {
        html! {
            (page_header("Developer", Some(&format!("Agent tasks. {lead}"))))
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
            (empty_state("No agent to try yet.", "Turn one on in Settings, under Providers."))
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

    #[test]
    fn strip_summarises_apps() {
        use crate::state::App;
        let snap = HiveSnapshot {
            apps: Apps::Known {
                installed: vec![
                    App {
                        name: "nextcloud".into(),
                        phase: "ready".into(),
                        url: None,
                        port: None,
                        keeper: None,
                    },
                    App {
                        name: "immich".into(),
                        phase: "starting".into(),
                        url: None,
                        port: None,
                        keeper: None,
                    },
                ],
                available: vec![],
            },
            devices: vec![],
        };
        let out = strip(&snap).into_string();
        assert!(out.contains("Nextcloud running, Immich starting"));
        assert!(out.contains("mini h-ok"));
        assert!(out.contains("mini h-busy"));
    }
}
