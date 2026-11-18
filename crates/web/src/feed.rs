//! The chat feed: folds the Queen's event log into the items the chat shows, and maps
//! internal names (event types, tool names, backend names) to plain words.
//!
//! **All naming lives here.** Tool names (`give_bee_job`, `install_app`, ...), event names
//! and backend names are internal; the UI says "helper" for a bee and "app" for a cell. When
//! they change, only the tables and the `match` in [`Feed::apply`] need updating. Nothing in
//! this module renders markup: `pages.rs` turns [`Item`]s into components.
//!
//! The fold is deterministic: folding the same events gives the same items with the same
//! keys, so the page render and a later live stream (which re-folds from the start) agree on
//! which DOM element each update belongs to.

use std::collections::{HashMap, HashSet};

use melissa_agent::queen::events::{EventRecord, QueenEvent, TurnId};
use serde_json::Value;

use crate::components::{ActivityState, ApprovalState, NoteTone};

/// One thing shown in the chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// What the user wrote.
    User { text: String },
    /// A piece of the Queen's reply (untrusted text). A reply is split into several items
    /// when tool activity happens in between.
    Reply { text: String },
    /// Something the Queen or a helper did: a tool call or a helper's job.
    Activity {
        label: String,
        detail: Option<String>,
        state: ActivityState,
        /// The helper (bee id) this is about, for a link to its conversation.
        bee: Option<String>,
    },
    /// A confirmation the user decides on.
    Approval(Approval),
    /// A short system line (session opened, turn stopped, errors).
    Note {
        text: String,
        detail: Option<String>,
        tone: NoteTone,
    },
}

/// The plain-language view of a confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    pub id: u64,
    pub title: String,
    pub consequence: String,
    /// Melissa's reason for asking (from the policy; not agent text).
    pub reason: String,
    /// Shown in the caution colour.
    pub destructive: bool,
    /// Can't be undone (e.g. deleting data).
    pub permanent: bool,
    pub yes: &'static str,
    pub no: &'static str,
    pub state: ApprovalState,
}

/// A feed item with its stable key (used as the DOM id suffix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub item: Item,
}

/// What an event changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// `entries()[i]` is new.
    Added(usize),
    /// `entries()[i]` changed.
    Updated(usize),
    /// The turn state or the backend changed (composer status line).
    Status,
    /// The set of pending confirmations changed.
    Waiting,
}

/// The folded chat.
#[derive(Debug, Default)]
pub struct Feed {
    entries: Vec<Entry>,
    by_key: HashMap<String, usize>,
    /// The reply item the current turn is writing into.
    open_reply: Option<(TurnId, usize)>,
    turns_with_reply: HashSet<TurnId>,
    errored_turns: HashSet<TurnId>,
    /// Agent tool-call ids → our own keys (agent ids never reach DOM ids or selectors).
    tool_keys: HashMap<String, String>,
    /// Agent tool-call ids not shown.
    hidden_tools: HashSet<String>,
    /// Bee id → key of its running job's row.
    open_jobs: HashMap<String, String>,
    busy: Option<TurnId>,
    backend: Option<String>,
    last_seq: u64,
}

impl Feed {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold a list of events (e.g. everything in the log).
    pub fn from_events<'a>(
        events: impl IntoIterator<Item = &'a EventRecord>,
        look: &dyn Lookup,
    ) -> Self {
        let mut feed = Self::new();
        for e in events {
            feed.apply(e, look);
        }
        feed
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The turn running according to the events seen so far.
    pub fn busy(&self) -> bool {
        self.busy.is_some()
    }

    /// The Queen's backend (display name) according to the events seen so far.
    pub fn backend(&self) -> Option<&str> {
        self.backend.as_deref()
    }

    /// The newest sequence number folded in.
    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// Fold one event in and say what changed.
    pub fn apply(&mut self, rec: &EventRecord, look: &dyn Lookup) -> Vec<Change> {
        if rec.seq <= self.last_seq {
            return Vec::new();
        }
        self.last_seq = rec.seq;
        match &rec.event {
            QueenEvent::UserMessage { turn, text } => {
                self.busy = Some(*turn);
                let added = self.add(format!("u{}", rec.seq), Item::User { text: text.clone() });
                vec![added, Change::Status]
            }
            QueenEvent::Text { turn, text } => self.reply_text(rec.seq, *turn, text),
            QueenEvent::AgentMessage { turn, text } => {
                if self.turns_with_reply.contains(turn) || text.trim().is_empty() {
                    return Vec::new();
                }
                self.turns_with_reply.insert(*turn);
                vec![self.add(format!("r{}", rec.seq), Item::Reply { text: text.clone() })]
            }
            QueenEvent::ToolCall {
                id, title, status, ..
            } => self.tool_call(id, title, status),
            QueenEvent::Permission {
                title, decision, ..
            } => {
                if decision != "reject_once" {
                    return Vec::new();
                }
                let tool = tool_words(title);
                vec![self.add(
                    format!("p{}", rec.seq),
                    Item::Activity {
                        label: format!("Not allowed: {}", lower_first(tool.doing)),
                        detail: None,
                        state: ActivityState::Failed,
                        bee: None,
                    },
                )]
            }
            QueenEvent::ConfirmationNeeded {
                id, tool, reason, ..
            } => {
                let approval = approval_words(
                    *id,
                    tool,
                    reason,
                    look.confirmation_args(*id).as_ref(),
                    look,
                );
                vec![
                    self.add(format!("c{id}"), Item::Approval(approval)),
                    Change::Waiting,
                ]
            }
            QueenEvent::ConfirmationResolved { id, outcome } => {
                let state = match outcome.as_str() {
                    "approved" => ApprovalState::Approved,
                    "denied" => ApprovalState::Denied,
                    _ => ApprovalState::Withdrawn,
                };
                let mut out = vec![Change::Waiting];
                if let Some(&i) = self.by_key.get(&format!("c{id}")) {
                    if let Item::Approval(a) = &mut self.entries[i].item {
                        a.state = state;
                        out.insert(0, Change::Updated(i));
                    }
                }
                out
            }
            QueenEvent::BeeCreated { bee, name, .. } => vec![self.add(
                format!("n{}", rec.seq),
                Item::Activity {
                    label: format!("New helper: {name}"),
                    detail: None,
                    state: ActivityState::Done,
                    bee: Some(bee.clone()),
                },
            )],
            QueenEvent::BeeStarted { bee, name, goal } => {
                let key = format!("j{}", rec.seq);
                self.open_jobs.insert(bee.clone(), key.clone());
                vec![self.add(
                    key,
                    Item::Activity {
                        label: format!("{name} is looking into it"),
                        detail: Some(goal.clone()),
                        state: ActivityState::Working,
                        bee: Some(bee.clone()),
                    },
                )]
            }
            QueenEvent::BeeFinished {
                bee,
                name,
                status,
                error,
            } => {
                let (label, state) = match status.as_str() {
                    "succeeded" => (format!("{name} finished"), ActivityState::Done),
                    "cancelled" => (format!("{name} was stopped"), ActivityState::Stopped),
                    _ => (format!("{name} couldn't finish"), ActivityState::Failed),
                };
                // The job's row is updated; a finish without a start (e.g. older than the
                // log) gets its own row.
                let key = self
                    .open_jobs
                    .remove(bee)
                    .unwrap_or_else(|| format!("j{}", rec.seq));
                let detail = error.clone().or_else(|| self.detail_of(&key));
                vec![self.upsert(
                    key,
                    Item::Activity {
                        label,
                        detail,
                        state,
                        bee: Some(bee.clone()),
                    },
                )]
            }
            QueenEvent::CellReassigned { cell, to, .. } => {
                let who = look.bee_name(to).unwrap_or_else(|| to.clone());
                vec![self.add(
                    format!("n{}", rec.seq),
                    Item::Activity {
                        label: format!("{who} looks after {} now", display_app(cell)),
                        detail: None,
                        state: ActivityState::Done,
                        bee: Some(to.clone()),
                    },
                )]
            }
            QueenEvent::SessionOpened { backend, resumed } => {
                self.backend = Some(backend_label(backend).to_string());
                let text = if *resumed {
                    format!(
                        "Connected to {}, picking up where we left off",
                        backend_label(backend)
                    )
                } else {
                    format!("Connected to {}", backend_label(backend))
                };
                vec![
                    self.add(
                        format!("n{}", rec.seq),
                        Item::Note {
                            text,
                            detail: None,
                            tone: NoteTone::Quiet,
                        },
                    ),
                    Change::Status,
                ]
            }
            QueenEvent::BackendFailed { backend, error } => vec![self.add(
                format!("n{}", rec.seq),
                Item::Note {
                    text: format!(
                        "Couldn't start {}; trying the next one",
                        backend_label(backend)
                    ),
                    detail: Some(error.clone()),
                    tone: NoteTone::Caution,
                },
            )],
            QueenEvent::SessionLost { reason } => {
                self.backend = None;
                vec![
                    self.add(
                        format!("n{}", rec.seq),
                        Item::Note {
                            text: "Lost the connection to the assistant. Your next message starts it again.".into(),
                            detail: Some(reason.clone()),
                            tone: NoteTone::Caution,
                        },
                    ),
                    Change::Status,
                ]
            }
            QueenEvent::TurnFinished { turn, stop_reason } => {
                if self.busy == Some(*turn) {
                    self.busy = None;
                }
                self.open_reply = None;
                let mut out = vec![Change::Status];
                let text = match stop_reason.as_str() {
                    "end_turn" => None,
                    "cancelled" => Some("Stopped."),
                    "refusal" => Some("The assistant declined to do that."),
                    "max_tokens" => Some("The reply hit its length limit."),
                    "max_turn_requests" => Some("Stopped after too many steps."),
                    _ if self.errored_turns.contains(turn) => None,
                    _ => Some("That didn't finish."),
                };
                if let Some(text) = text {
                    let tone = if stop_reason == "cancelled" {
                        NoteTone::Quiet
                    } else {
                        NoteTone::Caution
                    };
                    out.insert(
                        0,
                        self.add(
                            format!("n{}", rec.seq),
                            Item::Note {
                                text: text.into(),
                                detail: None,
                                tone,
                            },
                        ),
                    );
                }
                out
            }
            QueenEvent::Error { turn, message } => {
                if let Some(t) = turn {
                    self.errored_turns.insert(*t);
                }
                vec![self.add(
                    format!("n{}", rec.seq),
                    Item::Note {
                        text: "Something went wrong.".into(),
                        detail: Some(message.clone()),
                        tone: NoteTone::Caution,
                    },
                )]
            }
        }
    }

    fn add(&mut self, key: String, item: Item) -> Change {
        let i = self.entries.len();
        self.by_key.insert(key.clone(), i);
        self.entries.push(Entry { key, item });
        Change::Added(i)
    }

    fn upsert(&mut self, key: String, item: Item) -> Change {
        match self.by_key.get(&key) {
            Some(&i) => {
                self.entries[i].item = item;
                Change::Updated(i)
            }
            None => self.add(key, item),
        }
    }

    fn detail_of(&self, key: &str) -> Option<String> {
        match &self.entries[*self.by_key.get(key)?].item {
            Item::Activity { detail, .. } => detail.clone(),
            _ => None,
        }
    }

    fn reply_text(&mut self, seq: u64, turn: TurnId, text: &str) -> Vec<Change> {
        self.turns_with_reply.insert(turn);
        if let Some((t, i)) = self.open_reply {
            if t == turn && i + 1 == self.entries.len() {
                if let Item::Reply { text: so_far } = &mut self.entries[i].item {
                    so_far.push_str(text);
                    return vec![Change::Updated(i)];
                }
            }
        }
        if text.trim().is_empty() {
            // Don't open a new bubble for whitespace between tool calls.
            return Vec::new();
        }
        let change = self.add(
            format!("r{seq}"),
            Item::Reply {
                text: text.trim_start().to_string(),
            },
        );
        self.open_reply = Some((turn, self.entries.len() - 1));
        vec![change]
    }

    fn tool_call(&mut self, id: &str, title: &str, status: &str) -> Vec<Change> {
        let words = tool_words(title);
        // A call first reported under a hidden title stays hidden when its title changes
        // (Grok: `search_tool`, then `Search tools: "..."`).
        if words.hidden || self.hidden_tools.contains(id) {
            if !self.tool_keys.contains_key(id) {
                self.hidden_tools.insert(id.to_string());
            }
            return Vec::new();
        }
        let state = match status {
            "done" => ActivityState::Done,
            "failed" => ActivityState::Failed,
            _ => ActivityState::Working,
        };
        let label = match state {
            ActivityState::Done => words.done.to_string(),
            ActivityState::Failed => format!("Couldn't finish: {}", lower_first(words.doing)),
            _ => words.doing.to_string(),
        };
        // Unknown tools: the agent's own title as the detail (escaped text), except for an
        // agent's wrapper tool, which says nothing (Grok: `use_tool`, renamed to the real
        // tool when it finishes).
        let detail =
            (words.unknown && !WRAPPER_TOOLS.contains(&title.trim())).then(|| shorten(title, 80));
        let next = self.tool_keys.len() + 1;
        let key = self
            .tool_keys
            .entry(id.to_string())
            .or_insert_with(|| format!("t{next}"))
            .clone();
        vec![self.upsert(
            key,
            Item::Activity {
                label,
                detail,
                state,
                bee: None,
            },
        )]
    }
}

/// What the fold needs to know besides the events. Everything returned is shown only as
/// escaped text.
pub trait Lookup {
    /// A pending confirmation's arguments (agent-supplied).
    fn confirmation_args(&self, _id: u64) -> Option<Value> {
        None
    }
    /// A bee's display name by id.
    fn bee_name(&self, _bee: &str) -> Option<String> {
        None
    }
}

/// No extra knowledge.
#[cfg(test)]
pub struct NoLookup;

#[cfg(test)]
impl Lookup for NoLookup {}

/// Plain words for a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolWords {
    pub doing: &'static str,
    pub done: &'static str,
    /// Not worth a row (e.g. the agent looking up its own tool list, or starting a helper,
    /// which gets its own helper row).
    pub hidden: bool,
    /// Not one of Melissa's tools.
    pub unknown: bool,
}

/// Melissa's tools: (internal name, doing, done). Keep in sync with the tool registry.
const TOOLS: &[(&str, &str, &str)] = &[
    ("bee_info", "Checking this device", "Checked this device"),
    (
        "cluster_health",
        "Checking the cluster",
        "Checked the cluster",
    ),
    (
        "list_catalog",
        "Looking at the app catalog",
        "Looked at the app catalog",
    ),
    ("list_apps", "Listing your apps", "Listed your apps"),
    (
        "install_app",
        "Starting an app install",
        "Started an app install",
    ),
    ("app_status", "Checking an app", "Checked an app"),
    ("app_logs", "Reading an app's logs", "Read an app's logs"),
    ("uninstall_app", "Removing an app", "Removed an app"),
    ("create_bee", "Setting up a helper", "Set up a helper"),
    (
        "give_bee_job",
        "Giving a helper a job",
        "Gave a helper a job",
    ),
    ("bee_status", "Checking on a helper", "Checked on a helper"),
    ("list_bees", "Listing helpers", "Listed helpers"),
    ("stop_bee", "Stopping a helper", "Stopped a helper"),
    (
        "message_bee",
        "Passing a helper a message",
        "Passed a helper a message",
    ),
    (
        "reassign_cell",
        "Handing an app to a helper",
        "Handed an app to a helper",
    ),
];

/// Agents' own tool calls not shown as rows: looking up their tool list.
const HIDDEN_TOOLS: &[&str] = &["search_tool"];
/// Agents' wrapper tools that call one of Melissa's tools (the real name comes later).
const WRAPPER_TOOLS: &[&str] = &["use_tool"];

/// Map an agent's tool title (`melissa__bee_info`, `mcp__melissa__bee_info`, `bee_info`,
/// or an agent's own tool such as `Read`) to plain words.
pub fn tool_words(title: &str) -> ToolWords {
    let name = tool_name(title);
    // Grok's finished tool search is titled `Search tools: "<query>"`; the query may name
    // Melissa's tools, so check this before the name.
    if HIDDEN_TOOLS.contains(&name) || title.trim_start().starts_with("Search tools") {
        return ToolWords {
            doing: "",
            done: "",
            hidden: true,
            unknown: false,
        };
    }
    match TOOLS.iter().find(|(n, _, _)| *n == name) {
        Some((_, doing, done)) => ToolWords {
            doing,
            done,
            hidden: false,
            unknown: false,
        },
        None => ToolWords {
            doing: "Using a tool",
            done: "Used a tool",
            hidden: false,
            unknown: true,
        },
    }
}

/// The bare tool name inside an agent's tool title.
fn tool_name(title: &str) -> &str {
    let t = title.trim();
    // `mcp__melissa__x`, `melissa__x`, `melissa_x`: the part after Melissa's server name.
    for prefix in ["mcp__melissa__", "melissa__", "melissa_"] {
        if let Some(rest) = t.strip_prefix(prefix) {
            return rest;
        }
    }
    // Titles like "use_tool: melissa__bee_info" or "bee_info (melissa)": a known name inside.
    for (n, _, _) in TOOLS {
        if t.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .any(|w| w == *n || w.ends_with(&format!("__{n}")))
        {
            return n;
        }
    }
    t
}

/// The plain-language confirmation card for `tool` (see [`TOOLS`] for names).
pub fn approval_words(
    id: u64,
    tool: &str,
    reason: &str,
    args: Option<&Value>,
    look: &dyn Lookup,
) -> Approval {
    let arg = |k: &str| args.and_then(|a| a.get(k)).and_then(Value::as_str);
    let app = arg("app").or_else(|| arg("cell")).map(display_app);
    // A bee argument may be an id or a name; show the name when we know it.
    let bee = |k: &str| arg(k).map(|b| look.bee_name(b).unwrap_or_else(|| b.to_string()));
    let destructive_reason = reason.contains("destructive");
    let mut a = Approval {
        id,
        title: String::new(),
        consequence: String::new(),
        reason: reason.to_string(),
        destructive: destructive_reason,
        permanent: false,
        yes: "Allow",
        no: "Don't allow",
        state: ApprovalState::Pending,
    };
    match tool_name(tool) {
        "install_app" => {
            a.title = match &app {
                Some(app) => format!("Install {app}?"),
                None => "Install an app?".into(),
            };
            a.consequence = "Downloads the app and starts it on this device.".into();
            a.yes = "Install";
            a.no = "Not now";
        }
        "uninstall_app" => {
            let delete = args
                .and_then(|a| a.get("delete_data"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            a.title = match &app {
                Some(app) => format!("Remove {app}?"),
                None => "Remove an app?".into(),
            };
            a.consequence = if delete {
                "Stops the app and deletes its data for good.".into()
            } else {
                "Stops the app. Its data is kept, so you can install it again later.".into()
            };
            a.destructive = true;
            a.permanent = delete;
            a.yes = if delete {
                "Remove and delete data"
            } else {
                "Remove"
            };
            a.no = "Keep it";
        }
        "create_bee" => {
            a.title = match arg("name") {
                Some(n) => format!("Set up a new helper, {}?", shorten(n, 40)),
                None => "Set up a new helper?".into(),
            };
            a.consequence = "A helper is an assistant that works on its own, in its own sandbox, and looks after the apps it is given.".into();
        }
        "give_bee_job" => {
            a.title = match bee("bee") {
                Some(b) => format!("Give {b} a job?"),
                None => "Give a helper a job?".into(),
            };
            a.consequence = match arg("goal") {
                Some(goal) => format!("The job: {}", shorten(goal, 160)),
                None => "The helper works on its own, in its own sandbox.".into(),
            };
        }
        "message_bee" => {
            a.title = match bee("bee") {
                Some(b) => format!("Pass {b} a message?"),
                None => "Pass a helper a message?".into(),
            };
            a.consequence = match arg("text") {
                Some(t) => format!("The message: {}", shorten(t, 160)),
                None => "The helper reads it as information, not as orders.".into(),
            };
        }
        "stop_bee" => {
            a.title = match bee("bee") {
                Some(b) => format!("Stop {b}?"),
                None => "Stop a helper?".into(),
            };
            a.consequence =
                "Cancels its current job. It starts again with its next job or message.".into();
            a.yes = "Stop";
            a.no = "Let it work";
        }
        "reassign_cell" => {
            let to = bee("to");
            a.title = match (&app, &to) {
                (Some(app), Some(to)) => format!("Hand {app} to {to}?"),
                _ => "Hand an app to another helper?".into(),
            };
            a.consequence = match &to {
                Some(to) => format!(
                    "{to} looks after it from now on; the helper before can't change it any more."
                ),
                None => "Another helper looks after it from now on.".into(),
            };
            a.yes = "Hand it over";
            a.no = "Keep as is";
        }
        "agent:execute" => {
            a.title = "Run a command?".into();
            a.consequence = match arg("title") {
                Some(t) => shorten(t, 160),
                None => "The assistant wants to run a command on this device.".into(),
            };
            a.destructive = true;
        }
        "app_logs" => {
            a.title = match &app {
                Some(app) => format!("Read {app}'s logs?"),
                None => "Read an app's logs?".into(),
            };
            a.consequence = "Lets the assistant read the app's recent output.".into();
        }
        other => {
            let words = tool_words(other);
            a.title = if words.unknown {
                format!("Allow {}?", shorten(other, 60))
            } else {
                format!("Allow: {}?", lower_first(words.doing))
            };
            a.consequence = "The assistant waits for your answer.".into();
        }
    }
    a
}

/// A catalog app's display name (`nextcloud` → `Nextcloud`).
pub fn display_app(name: &str) -> String {
    let mut c = name.chars();
    match c.next() {
        Some(first) => first.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// A backend's display name (`grok-acp` → `Grok`).
pub fn backend_label(backend: &str) -> &str {
    match backend {
        "grok-acp" => "Grok",
        "claude-session" | "claude" => "Claude",
        "opencode-acp" => "OpenCode",
        other => other,
    }
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(first) => first.to_lowercase().chain(c).collect(),
        None => String::new(),
    }
}

fn shorten(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn rec(seq: u64, event: QueenEvent) -> EventRecord {
        EventRecord { seq, ts: 0, event }
    }

    /// Test knowledge: confirmation 4 removes Nextcloud with its data; bee `c.b1` is Ada.
    struct Known;
    impl Lookup for Known {
        fn confirmation_args(&self, id: u64) -> Option<Value> {
            (id == 4).then(|| json!({"app": "nextcloud", "delete_data": true}))
        }
        fn bee_name(&self, bee: &str) -> Option<String> {
            (bee == "c.b1").then(|| "Ada".to_string())
        }
    }

    #[test]
    fn tool_titles_map_to_plain_words() {
        assert_eq!(tool_words("melissa__bee_info").done, "Checked this device");
        assert_eq!(
            tool_words("mcp__melissa__install_app").doing,
            "Starting an app install"
        );
        assert_eq!(
            tool_words("use_tool: melissa__list_apps").done,
            "Listed your apps"
        );
        assert!(!tool_words("melissa__give_bee_job").hidden);
        assert_eq!(tool_words("mcp__melissa__list_bees").done, "Listed helpers");
        assert!(tool_words("search_tool").hidden);
        assert!(tool_words("Search tools: \"melissa start_worker bee_info\"").hidden);
        let other = tool_words("Read");
        assert!(other.unknown);
        assert_eq!(other.done, "Used a tool");
    }

    #[test]
    fn a_turn_folds_into_messages_activity_and_status() {
        let mut feed = Feed::new();
        let events = [
            rec(
                1,
                QueenEvent::UserMessage {
                    turn: 1,
                    text: "Install Nextcloud".into(),
                },
            ),
            rec(
                2,
                QueenEvent::SessionOpened {
                    backend: "grok-acp".into(),
                    resumed: false,
                },
            ),
            rec(
                3,
                QueenEvent::Text {
                    turn: 1,
                    text: "Sure, ".into(),
                },
            ),
            rec(
                4,
                QueenEvent::Text {
                    turn: 1,
                    text: "checking.".into(),
                },
            ),
            rec(
                5,
                QueenEvent::ToolCall {
                    turn: Some(1),
                    id: "a".into(),
                    title: "melissa__bee_info".into(),
                    status: "running".into(),
                },
            ),
            rec(
                6,
                QueenEvent::ToolCall {
                    turn: Some(1),
                    id: "a".into(),
                    title: "melissa__bee_info".into(),
                    status: "done".into(),
                },
            ),
            rec(
                7,
                QueenEvent::Text {
                    turn: 1,
                    text: "Done.".into(),
                },
            ),
            rec(
                8,
                QueenEvent::AgentMessage {
                    turn: 1,
                    text: "Sure, checking.Done.".into(),
                },
            ),
            rec(
                9,
                QueenEvent::TurnFinished {
                    turn: 1,
                    stop_reason: "end_turn".into(),
                },
            ),
        ];
        let mut changes = Vec::new();
        for e in &events {
            changes.push(feed.apply(e, &NoLookup));
            if e.seq == 1 {
                assert!(feed.busy());
            }
        }
        assert!(!feed.busy());
        assert_eq!(feed.backend(), Some("Grok"));
        let keys: Vec<&str> = feed.entries().iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, ["u1", "n2", "r3", "t1", "r7"]);
        assert_eq!(
            feed.entries()[2].item,
            Item::Reply {
                text: "Sure, checking.".into()
            }
        );
        assert_eq!(changes[3], [Change::Updated(2)]);
        assert_eq!(changes[5], [Change::Updated(3)]);
        assert!(matches!(
            &feed.entries()[3].item,
            Item::Activity { label, state: ActivityState::Done, .. } if label == "Checked this device"
        ));
        // The whole message at the end is not shown twice.
        assert!(changes[7].is_empty());
        // Replaying the same events is a no-op.
        assert!(feed.apply(&events[0], &NoLookup).is_empty());
    }

    #[test]
    fn confirmations_and_helpers() {
        let args = Known;
        let mut feed = Feed::new();
        feed.apply(
            &rec(
                1,
                QueenEvent::BeeStarted {
                    bee: "c.b1".into(),
                    name: "Ada".into(),
                    goal: "Wait until ready".into(),
                },
            ),
            &args,
        );
        let c = feed.apply(
            &rec(
                2,
                QueenEvent::ConfirmationNeeded {
                    id: 4,
                    task: 1,
                    tool: "uninstall_app".into(),
                    reason: "destructive action".into(),
                },
            ),
            &args,
        );
        assert_eq!(c, [Change::Added(1), Change::Waiting]);
        let Item::Approval(a) = &feed.entries()[1].item else {
            panic!()
        };
        assert_eq!(a.title, "Remove Nextcloud?");
        assert!(a.destructive && a.permanent);
        let c = feed.apply(
            &rec(
                3,
                QueenEvent::ConfirmationResolved {
                    id: 4,
                    outcome: "denied".into(),
                },
            ),
            &args,
        );
        assert_eq!(c, [Change::Updated(1), Change::Waiting]);
        let Item::Approval(a) = &feed.entries()[1].item else {
            panic!()
        };
        assert_eq!(a.state, ApprovalState::Denied);
        feed.apply(
            &rec(
                4,
                QueenEvent::BeeFinished {
                    bee: "c.b1".into(),
                    name: "Ada".into(),
                    status: "succeeded".into(),
                    error: None,
                },
            ),
            &args,
        );
        assert_eq!(
            feed.entries()[0].item,
            Item::Activity {
                label: "Ada finished".into(),
                detail: Some("Wait until ready".into()),
                state: ActivityState::Done,
                bee: Some("c.b1".into()),
            }
        );
        feed.apply(
            &rec(
                5,
                QueenEvent::CellReassigned {
                    cell: "nextcloud".into(),
                    from: None,
                    to: "c.b1".into(),
                    by: "the user".into(),
                },
            ),
            &args,
        );
        assert!(matches!(
            &feed.entries()[2].item,
            Item::Activity { label, bee: Some(b), .. } if label == "Ada looks after Nextcloud now" && b == "c.b1"
        ));
        let a = approval_words(
            9,
            "reassign_cell",
            "x",
            Some(&json!({"cell": "immich", "to": "c.b1"})),
            &args,
        );
        assert_eq!(a.title, "Hand Immich to Ada?");
    }

    #[test]
    fn grok_tool_titles_change_while_running() {
        let call = |seq, id: &str, title: &str, status: &str| {
            rec(
                seq,
                QueenEvent::ToolCall {
                    turn: Some(1),
                    id: id.into(),
                    title: title.into(),
                    status: status.into(),
                },
            )
        };
        let mut feed = Feed::new();
        feed.apply(&call(1, "a", "search_tool", "pending"), &NoLookup);
        feed.apply(
            &call(2, "a", "Search tools: \"melissa give_bee_job\"", "done"),
            &NoLookup,
        );
        assert!(feed.entries().is_empty());
        feed.apply(&call(3, "b", "use_tool", "pending"), &NoLookup);
        assert_eq!(
            feed.entries()[0].item,
            Item::Activity {
                label: "Using a tool".into(),
                detail: None,
                state: ActivityState::Working,
                bee: None,
            }
        );
        let c = feed.apply(&call(4, "b", "melissa__give_bee_job", "done"), &NoLookup);
        assert_eq!(c, [Change::Updated(0)]);
        assert_eq!(
            feed.entries()[0].item,
            Item::Activity {
                label: "Gave a helper a job".into(),
                detail: None,
                state: ActivityState::Done,
                bee: None,
            }
        );
    }

    #[test]
    fn errors_and_stops_become_notes() {
        let mut feed = Feed::new();
        feed.apply(
            &rec(
                1,
                QueenEvent::UserMessage {
                    turn: 1,
                    text: "hi".into(),
                },
            ),
            &NoLookup,
        );
        feed.apply(
            &rec(
                2,
                QueenEvent::Error {
                    turn: Some(1),
                    message: "no backend".into(),
                },
            ),
            &NoLookup,
        );
        feed.apply(
            &rec(
                3,
                QueenEvent::TurnFinished {
                    turn: 1,
                    stop_reason: "error".into(),
                },
            ),
            &NoLookup,
        );
        // One note for the error, none for the error stop.
        assert_eq!(feed.entries().len(), 2);
        feed.apply(
            &rec(
                4,
                QueenEvent::UserMessage {
                    turn: 2,
                    text: "again".into(),
                },
            ),
            &NoLookup,
        );
        feed.apply(
            &rec(
                5,
                QueenEvent::TurnFinished {
                    turn: 2,
                    stop_reason: "cancelled".into(),
                },
            ),
            &NoLookup,
        );
        assert!(matches!(&feed.entries()[3].item, Item::Note { text, .. } if text == "Stopped."));
    }

    #[test]
    fn approval_words_fall_back_for_unknown_tools() {
        let a = approval_words(
            1,
            "touch",
            "changes state while nobody is watching",
            None,
            &NoLookup,
        );
        assert_eq!(a.title, "Allow touch?");
        assert!(!a.destructive);
        let a = approval_words(
            2,
            "install_app",
            "x",
            Some(&json!({"app": "immich"})),
            &NoLookup,
        );
        assert_eq!(a.title, "Install Immich?");
        assert_eq!(a.yes, "Install");
    }
}
