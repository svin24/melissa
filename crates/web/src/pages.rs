//! Pages and fragments, assembled only from `components`. No markup is written here.

use std::time::Duration;

use maud::{html, Markup};
use melissa_agent::{BackendInfo, Stream, TaskSnapshot, TaskStatus, MAX_PROMPT_BYTES};

use crate::{
    components::{
        action_button, activity_row, add_app_cell, app_cell, app_page, app_strip, approval_card,
        badge, card, code, composer, composer_status, device_card, empty_state, feed, feed_entry,
        honeycomb, link, live_region, log_view, note, oob_inner, page, page_header, post_form,
        queen_message, rail, section, select_field, table, textarea_field, truncated, user_message,
        waiting_count, AppCell, ApprovalCard, Choice, DeviceCard, Health, LogLine, Meter, Nav,
        RailDevice, StripCell, Tone, APPROVALS_ID, COMPOSER_STATUS_ID, FEED_ID,
    },
    feed::{Approval, Change, Entry, Feed, Item},
    state::{phase_words, Apps, Device, HiveSnapshot},
};

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

/// The rail with devices from `snap`.
pub fn rail_for(nav: Nav, waiting: usize, snap: &HiveSnapshot) -> Markup {
    let devices: Vec<RailDevice<'_>> = snap
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
    rail(nav, waiting, &devices)
}

/// DOM id of a feed entry.
pub fn entry_id(key: &str) -> String {
    format!("f-{key}")
}

/// The content of one feed entry (without its `li`).
pub fn entry_content(entry: &Entry) -> Markup {
    match &entry.item {
        Item::User { text } => user_message(text),
        Item::Reply { text } => queen_message(text),
        Item::Activity {
            label,
            detail,
            state,
        } => activity_row(label, detail.as_deref(), *state),
        Item::Approval(a) => approval_content(a, &entry_id(&entry.key)),
        Item::Note { text, detail, tone } => note(text, detail.as_deref(), *tone),
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
    backend: Option<&str>,
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
            (oob_inner(COMPOSER_STATUS_ID, composer_status(backend, feed.busy())))
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
    // Disk usage is not reported by any tool yet, so there is no warning to show.
    app_strip(STRIP_URL, &cells, &summary, None)
}

/// Everything the home page shows.
pub struct HomeView<'a> {
    pub feed: &'a Feed,
    pub snap: &'a HiveSnapshot,
    pub waiting: usize,
    pub backend: Option<&'a str>,
    pub busy: bool,
    pub draft: &'a str,
    pub error: Option<&'a str>,
}

pub fn home(v: &HomeView<'_>) -> Markup {
    let stream = format!("/ui/stream?since={}", v.feed.last_seq());
    app_page(
        "Chat",
        rail_for(Nav::Chat, v.waiting, v.snap),
        html! {
            div.chat {
                (strip(v.snap))
                (feed(
                    FEED_ID,
                    "Conversation",
                    &stream,
                    entries(v.feed.entries()),
                    empty_state(
                        "Hi, I'm Melissa.",
                        "I look after the apps on your devices. Ask me to install one, check on them, or explain what's going on.",
                    ),
                ))
                (composer_box(v.draft, v.error, v.backend, v.busy))
            }
        },
    )
}

/// The composer (also the response to sending a message).
pub fn composer_box(draft: &str, error: Option<&str>, backend: Option<&str>, busy: bool) -> Markup {
    composer(draft, error, composer_status(backend, busy))
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

fn format_uptime(secs: u64) -> String {
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
    let meters: Vec<Meter<'_>> = mem
        .iter()
        .map(|(fraction, text)| Meter {
            label: "Memory",
            fraction: *fraction,
            text,
        })
        .collect();
    device_card(&DeviceCard {
        name: &d.name,
        health,
        status,
        facts: &facts,
        meters: &meters,
    })
}

/// The apps region of the hive page (refreshes itself).
pub fn hive_apps(snap: &HiveSnapshot) -> Markup {
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
            html! {
                (honeycomb("Apps", html! {
                    @for (a, name) in installed.iter().zip(&names) {
                        @let (health, status) = phase_words(&a.phase);
                        (app_cell(&AppCell { name, health, status, detail: None, url: a.url.as_deref() }))
                    }
                    (add_app_cell("/?draft=Install%20"))
                }))
                @if !available.is_empty() {
                    p.quiet {
                        "Ready to install: "
                        @for (i, n) in available.iter().enumerate() {
                            @if i > 0 { ", " }
                            (link(&format!("/?draft=Install%20{n}"), &crate::feed::display_app(n)))
                        }
                    }
                }
            }
        }
    };
    live_region("hive-apps", "/ui/hive/apps", 15, true, content)
}

pub fn hive(snap: &HiveSnapshot, waiting: usize) -> Markup {
    app_page(
        "Apps and devices",
        rail_for(Nav::Hive, waiting, snap),
        html! {
            div.wide {
                (page_header("Apps and devices", Some("What runs where. Ask in the chat to change anything.")))
                (section("Apps", hive_apps(snap)))
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

pub fn approvals(list: &[Approval], since: u64, snap: &HiveSnapshot) -> Markup {
    let stream = format!("/ui/stream?since={since}&view=approvals");
    app_page(
        "Waiting for you",
        rail_for(Nav::Waiting, list.len(), snap),
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

pub fn dashboard(backends: &[BackendInfo], tasks: &[TaskSnapshot]) -> Markup {
    page(
        "Agent tasks",
        html! {
            (page_header("Agent tasks", Some("Run one agent on one prompt. For development; the chat is the normal way in.")))
            (card("Agents", agents_table(backends)))
            (card("New task", new_task_form(backends)))
            (card("Tasks", tasks_region(tasks, None)))
        },
    )
}

fn agents_table(backends: &[BackendInfo]) -> Markup {
    let rows = backends
        .iter()
        .map(|b| {
            vec![
                html! { (b.name) },
                html! { (b.kind) },
                if b.available {
                    badge("available", Tone::Ok)
                } else {
                    badge("not installed", Tone::Muted)
                },
                code(&b.detail),
            ]
        })
        .collect();
    table(
        &["Name", "Kind", "Status", "Command"],
        rows,
        "No agent backends configured.",
    )
}

fn new_task_form(backends: &[BackendInfo]) -> Markup {
    let choices: Vec<Choice<'_>> = backends
        .iter()
        .map(|b| Choice {
            value: &b.name,
            label: &b.name,
            enabled: b.available,
        })
        .collect();
    post_form(
        "/tasks",
        TASKS_ID,
        "Dispatch",
        html! {
            (select_field("backend", "Agent", &choices))
            (textarea_field("prompt", "Prompt", "What should the agent do?", MAX_PROMPT_BYTES))
        },
    )
}

/// The self-refreshing task list. `message` is shown above it (e.g. a dispatch error).
pub fn tasks_region(tasks: &[TaskSnapshot], message: Option<(&str, Tone)>) -> Markup {
    let rows = tasks
        .iter()
        .map(|t| {
            vec![
                link(&format!("/tasks/{}", t.id), &format!("#{}", t.id)),
                html! { (t.backend) },
                status_badge(t.status),
                truncated(&t.prompt, 60),
                html! { (format_elapsed(t.elapsed)) },
            ]
        })
        .collect();
    live_region(
        TASKS_ID,
        "/tasks/fragment",
        2,
        true,
        html! {
            @if let Some((text, tone)) = message { (crate::components::notice(text, tone)) }
            (table(&["Task", "Agent", "Status", "Prompt", "Time"], rows, "No tasks yet."))
        },
    )
}

pub fn task_page(task: &TaskSnapshot) -> Markup {
    page(
        &format!("Task #{}", task.id),
        card(&format!("Task #{}", task.id), task_region(task)),
    )
}

/// The task detail region; polls itself until the task finishes.
pub fn task_region(task: &TaskSnapshot) -> Markup {
    let running = task.status == TaskStatus::Running;
    live_region(
        TASK_ID,
        &format!("/tasks/{}/fragment", task.id),
        1,
        running,
        html! {
            (table(
                &["Agent", "Status", "Time"],
                vec![vec![
                    html! { (task.backend) },
                    status_badge(task.status),
                    html! { (format_elapsed(task.elapsed)) },
                ]],
                "",
            ))
            (log_view([LogLine { text: &task.prompt, is_error: false }]))
            @if let Some(error) = &task.error { (crate::components::notice(error, Tone::Error)) }
            @if task.truncated { (crate::components::notice("Output was truncated.", Tone::Warn)) }
            (log_view(task.output.iter().map(|l| LogLine {
                text: &l.text,
                is_error: l.stream == Stream::Stderr,
            })))
            @if running {
                (action_button(&format!("/tasks/{}/cancel", task.id), TASK_ID, "Cancel", "Cancel this task?"))
            }
            (link("/tasks", "All tasks"))
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
    badge(status.as_str(), tone)
}

fn format_elapsed(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        format!("{}m {:02}s", d.as_secs() / 60, d.as_secs() % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_formatting() {
        assert_eq!(format_elapsed(Duration::from_millis(1500)), "1.5s");
        assert_eq!(format_elapsed(Duration::from_secs(125)), "2m 05s");
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
                    },
                    App {
                        name: "immich".into(),
                        phase: "starting".into(),
                        url: None,
                    },
                ],
                available: vec![],
            },
            devices: vec![],
        };
        let out = strip(&snap).into_string();
        assert!(out.contains("Nextcloud running, Immich installing"));
        assert!(out.contains("mini h-ok"));
        assert!(out.contains("mini h-busy"));
    }
}
