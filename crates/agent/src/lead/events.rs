//! A conversation's event stream (the lead agent's, or an agent's: same shape): typed, numbered events that clients (the CLI today, the web UI
//! later) read with a cursor, plus the JSONL transcript they are recorded in.
//!
//! Every event gets a sequence number (`seq`, starting at 1, never reused while the daemon
//! runs). Clients ask for "everything after `seq` N" and wait for new events (long poll).
//! The log keeps the newest [`EventLog::CAPACITY`] events in memory; a client that fell
//! further behind is told so (`missed`) and continues from the oldest kept event.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use super::transcript::Transcript;
use crate::TaskId;

/// Id of one user message and the lead agent's work on it.
pub type TurnId = u64;

/// Something that happened in the lead agent's conversation. Agent text is untrusted: clients
/// must never render it as HTML (the web UI uses a safe Markdown subset, `components::markdown`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LeadEvent {
    /// The user sent a message; a turn starts.
    UserMessage { turn: TurnId, text: String },
    /// A piece of the lead agent's reply (not line-buffered).
    Text { turn: TurnId, text: String },
    /// The agent's whole reply for a turn (sent at the end of the turn; the transcript keeps
    /// this instead of the pieces). Was `queen_message` before agents had conversations.
    #[serde(alias = "queen_message")]
    AgentMessage { turn: TurnId, text: String },
    /// The lead agent's agent started or finished a tool call (its own or one of Melissa's).
    ToolCall {
        turn: Option<TurnId>,
        id: String,
        title: String,
        /// `pending`, `running`, `done` or `failed`.
        status: String,
    },
    /// Melissa answered one of the agent's permission requests.
    Permission {
        turn: Option<TurnId>,
        title: String,
        /// `allow_once`, `reject_once` or `cancelled`.
        decision: String,
    },
    /// A tool call (of the lead agent or an agent) waits for a human: approve or deny it.
    /// Recorded in the lead agent's log and, when an agent asked, in that agent's log too.
    ConfirmationNeeded {
        id: u64,
        task: TaskId,
        tool: String,
        reason: String,
        /// The agent (id) that asked; None for the lead agent and the user's own calls.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
    },
    /// A confirmation was decided (`approved`, `denied`) or withdrawn (`withdrawn`).
    ConfirmationResolved { id: u64, outcome: String },
    /// The lead agent (or the user) created an agent.
    AgentCreated {
        agent: String,
        name: String,
        backend: String,
    },
    /// The lead agent gave an agent a job (the agent's turn runs in its own conversation).
    AgentStarted {
        agent: String,
        name: String,
        goal: String,
    },
    /// An agent's job ended (`succeeded`, `failed`, `cancelled`).
    AgentFinished {
        agent: String,
        name: String,
        status: String,
        error: Option<String>,
    },
    /// A workspace got a new owning agent (`from`: the previous agent, None if it had none).
    WorkspaceReassigned {
        workspace: String,
        from: Option<String>,
        to: String,
        by: String,
    },
    /// The user force-deleted a workspace and its data. `agent`: the agent that looked after it and
    /// was told (None: it had none, or that agent no longer exists).
    WorkspaceForceDeleted {
        workspace: String,
        agent: Option<String>,
    },
    /// The lead agent's agent session started. `resumed`: the agent's previous session (and its
    /// context) was resumed; otherwise the lead agent starts fresh.
    SessionOpened { backend: String, resumed: bool },
    /// The user switched the conversation to another backend (or model). It takes over at
    /// the next turn, with the conversation so far. `model_label`: the agent's own name
    /// for the model.
    BackendSwitched {
        backend: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_label: Option<String>,
    },
    /// A Lead agent backend could not start; the next one in the list is tried.
    BackendFailed { backend: String, error: String },
    /// The lead agent's agent session ended unexpectedly (e.g. the agent process died). Its
    /// context is lost unless the next session can resume it.
    SessionLost { reason: String },
    /// A turn ended. `stop_reason`: `end_turn`, `cancelled`, `refusal`, `max_tokens`,
    /// `max_turn_requests` or `error`.
    TurnFinished { turn: TurnId, stop_reason: String },
    /// Something went wrong (shown to the user).
    Error {
        turn: Option<TurnId>,
        message: String,
    },
    /// The conversation read untrusted content for the first time: from now on its changes
    /// need the user's OK. `tool`: what brought it in (`app_logs`, `agent_status`, `lead` for
    /// a job or message from a tainted lead agent), `subject`: the app or agent it was about.
    Tainted {
        tool: Option<String>,
        subject: Option<String>,
    },
    /// The user started a new chat: what came before is archived, the agent starts a fresh
    /// session and the conversation is untainted again.
    NewChat,
    /// Only in memory: the events up to [`LeadEvent::EarlierEnd`] were restored from the
    /// transcript after a daemon restart (the earlier conversation).
    EarlierStart,
    /// Only in memory: the end of the restored events.
    EarlierEnd,
}

impl LeadEvent {
    /// The turn this event belongs to, if any.
    pub fn turn(&self) -> Option<TurnId> {
        match self {
            Self::UserMessage { turn, .. }
            | Self::Text { turn, .. }
            | Self::AgentMessage { turn, .. }
            | Self::TurnFinished { turn, .. } => Some(*turn),
            Self::ToolCall { turn, .. }
            | Self::Permission { turn, .. }
            | Self::Error { turn, .. } => *turn,
            _ => None,
        }
    }
}

impl LeadEvent {
    /// Change this event's turn id (restored events get new ones).
    pub(crate) fn map_turn(&mut self, f: impl FnOnce(TurnId) -> Option<TurnId>) {
        match self {
            Self::UserMessage { turn, .. }
            | Self::Text { turn, .. }
            | Self::AgentMessage { turn, .. }
            | Self::TurnFinished { turn, .. } => *turn = f(*turn).unwrap_or(0),
            Self::ToolCall { turn, .. }
            | Self::Permission { turn, .. }
            | Self::Error { turn, .. } => *turn = turn.and_then(f),
            _ => {}
        }
    }
}

/// One event with its sequence number and time (Unix seconds).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRecord {
    pub seq: u64,
    pub ts: u64,
    #[serde(flatten)]
    pub event: LeadEvent,
}

/// A page of events for a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventPage {
    pub events: Vec<EventRecord>,
    /// Pass this as `since` next time.
    pub next: u64,
    /// Events after `since` were dropped from memory before the client asked.
    pub missed: bool,
}

struct Inner {
    buf: Mutex<VecDeque<EventRecord>>,
    next_seq: Mutex<u64>,
    notify: Notify,
    transcript: Option<Transcript>,
    /// Identifies this log (one daemon run): a client's cursor from another log is stale.
    epoch: u64,
    /// The highest turn id among restored events (new turns continue after it).
    restored_turns: TurnId,
}

/// The lead agent's event log. Cheap to clone.
#[derive(Clone)]
pub struct EventLog {
    inner: Arc<Inner>,
}

impl EventLog {
    /// Events kept in memory for clients.
    pub const CAPACITY: usize = 2000;

    /// Turns of the earlier conversation restored by [`EventLog::with_history`].
    pub const RESTORED_TURNS: usize = 30;

    /// A log that also records events in `transcript` (all but text pieces).
    pub fn new(transcript: Option<Transcript>) -> Self {
        Self::build(transcript, 0)
    }

    fn build(transcript: Option<Transcript>, restored_turns: TurnId) -> Self {
        // Nanoseconds since the epoch: different for every log a daemon makes in practice.
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        Self {
            inner: Arc::new(Inner {
                buf: Mutex::new(VecDeque::new()),
                next_seq: Mutex::new(1),
                notify: Notify::new(),
                transcript,
                epoch,
                restored_turns,
            }),
        }
    }

    /// A log that starts with the last [`EventLog::RESTORED_TURNS`] turns of the current
    /// chat from `transcript` (after a daemon restart), between `earlier_start` and
    /// `earlier_end` markers. Restored events get new sequence numbers (and turn ids) and
    /// are not written to the transcript again.
    pub fn with_history(transcript: Transcript) -> Self {
        let earlier = transcript.load_chat(Self::RESTORED_TURNS);
        let turns = earlier
            .iter()
            .filter_map(|r| r.event.turn())
            .max()
            .unwrap_or(0);
        let log = Self::build(Some(transcript), turns);
        if !earlier.is_empty() {
            log.push_memory(LeadEvent::EarlierStart, None);
            for r in earlier {
                log.push_memory(r.event, Some(r.ts));
            }
            log.push_memory(LeadEvent::EarlierEnd, None);
        }
        log
    }

    pub fn transcript(&self) -> Option<&Transcript> {
        self.inner.transcript.as_ref()
    }

    /// Identifies this log; a client cursor taken from a log with another epoch (before a
    /// daemon restart) does not point into this one.
    pub fn epoch(&self) -> u64 {
        self.inner.epoch
    }

    /// The highest turn id of the restored earlier conversation (0: none).
    pub fn restored_turns(&self) -> TurnId {
        self.inner.restored_turns
    }

    /// Append an event; returns its sequence number.
    pub fn push(&self, event: LeadEvent) -> u64 {
        let record = self.push_memory(event, None);
        if let (Some(t), false) = (
            &self.inner.transcript,
            matches!(
                record.event,
                LeadEvent::Text { .. } | LeadEvent::EarlierStart | LeadEvent::EarlierEnd
            ),
        ) {
            t.append(&record);
        }
        self.inner.notify.notify_waiters();
        record.seq
    }

    /// Append an event to memory only (`ts`: keep this time instead of now).
    fn push_memory(&self, event: LeadEvent, ts: Option<u64>) -> EventRecord {
        let ts = ts.unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        });
        {
            // Number and append under one lock so the buffer stays in seq order.
            let mut buf = lock(&self.inner.buf);
            let mut next = lock(&self.inner.next_seq);
            let record = EventRecord {
                seq: *next,
                ts,
                event,
            };
            *next += 1;
            if buf.len() == Self::CAPACITY {
                buf.pop_front();
            }
            buf.push_back(record.clone());
            record
        }
    }

    /// The newest sequence number handed out (0: none yet).
    pub fn last_seq(&self) -> u64 {
        *lock(&self.inner.next_seq) - 1
    }

    /// Events after `since`, at most `limit`.
    pub fn since(&self, since: u64, limit: usize) -> EventPage {
        let buf = lock(&self.inner.buf);
        let oldest = buf.front().map(|r| r.seq);
        let missed = oldest.is_some_and(|o| since + 1 < o);
        let events: Vec<EventRecord> = buf
            .iter()
            .filter(|r| r.seq > since)
            .take(limit)
            .cloned()
            .collect();
        let next = events.last().map_or(since, |r| r.seq);
        EventPage {
            events,
            next,
            missed,
        }
    }

    /// Like [`EventLog::since`], waiting up to `wait` for the first new event.
    pub async fn wait_since(&self, since: u64, limit: usize, wait: Duration) -> EventPage {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let notified = self.inner.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let page = self.since(since, limit);
            if !page.events.is_empty() || tokio::time::Instant::now() >= deadline {
                return page;
            }
            tokio::select! {
                () = notified => {}
                () = tokio::time::sleep_until(deadline) => return self.since(since, limit),
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // Poisoning only means another thread panicked mid-update; the data is still usable.
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(turn: TurnId, t: &str) -> LeadEvent {
        LeadEvent::Text {
            turn,
            text: t.into(),
        }
    }

    #[test]
    fn events_serialize_with_a_type_tag() {
        let rec = EventRecord {
            seq: 3,
            ts: 10,
            event: LeadEvent::AgentFinished {
                agent: "d-0a1b2c3d.a7".into(),
                name: "Ada".into(),
                status: "succeeded".into(),
                error: None,
            },
        };
        let json = serde_json::to_value(&rec).unwrap();
        assert_eq!(json["type"], "agent_finished");
        assert_eq!(json["seq"], 3);
        assert_eq!(json["agent"], "d-0a1b2c3d.a7");
        let back: EventRecord = serde_json::from_value(json).unwrap();
        assert_eq!(back, rec);
    }

    #[test]
    fn since_pages_and_reports_missed_events() {
        let log = EventLog::new(None);
        for i in 0..5 {
            log.push(text(1, &i.to_string()));
        }
        assert_eq!(log.last_seq(), 5);
        let page = log.since(0, 2);
        assert_eq!(page.events.len(), 2);
        assert_eq!(page.next, 2);
        assert!(!page.missed);
        let page = log.since(page.next, 100);
        assert_eq!(page.events.len(), 3);
        assert_eq!(page.next, 5);
        let empty = log.since(5, 100);
        assert!(empty.events.is_empty());
        assert_eq!(empty.next, 5);

        for i in 0..EventLog::CAPACITY {
            log.push(text(2, &i.to_string()));
        }
        let page = log.since(0, 1);
        assert!(page.missed);
        assert_eq!(page.events[0].seq, 6);
    }

    #[tokio::test]
    async fn wait_since_wakes_on_new_events() {
        let log = EventLog::new(None);
        let waiter = {
            let log = log.clone();
            tokio::spawn(async move { log.wait_since(0, 10, Duration::from_secs(5)).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        log.push(text(1, "hi"));
        let page = waiter.await.unwrap();
        assert_eq!(page.events.len(), 1);
        // Times out empty.
        let page = log.wait_since(1, 10, Duration::from_millis(10)).await;
        assert!(page.events.is_empty());
    }
}
