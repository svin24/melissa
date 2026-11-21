//! A conversation's transcript (the lead agent's or an agent's): every event except text pieces, as
//! JSON Lines, in Melissa's state directory (`$XDG_STATE_HOME/melissa/lead/transcript.jsonl`,
//! `$XDG_STATE_HOME/melissa/agents/<id>/transcript.jsonl`).
//!
//! Bounded: when the file grows past its limit it is renamed to `transcript.jsonl.1`
//! (replacing the previous one) and a new file starts, so at most twice the limit is kept.
//! Files are mode 0600: they hold the conversation.

use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use std::collections::HashMap;

use serde::Serialize;

use super::events::{EventRecord, LeadEvent, TurnId};

/// A chat archived by "New chat" before 2026-11-20 (read-only; one chat per agent since then,
/// so no new archives are made).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PastChat {
    /// The archive's id (Unix seconds when it was archived, made unique).
    pub id: u64,
    /// When the chat started (Unix seconds, 0 if unknown).
    pub started: u64,
    /// When it was archived (Unix seconds).
    pub ended: u64,
    /// The user's first message, shortened (untrusted text; empty if none).
    pub title: String,
    /// Messages from the user.
    pub turns: usize,
}

/// Past chats listed at most.
const PAST_CHATS: usize = 30;

/// Appends events to the transcript file. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Transcript {
    path: PathBuf,
    max_bytes: u64,
    lock: Arc<Mutex<()>>,
}

impl Transcript {
    pub fn new(path: impl Into<PathBuf>, max_bytes: u64) -> Self {
        Self {
            path: path.into(),
            max_bytes: max_bytes.max(1024),
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one record. Best effort: failures are logged, never fatal.
    pub fn append(&self, record: &EventRecord) {
        if let Err(e) = self.try_append(record) {
            tracing::warn!(path = %self.path.display(), "lead transcript write failed: {e}");
        }
    }

    fn try_append(&self, record: &EventRecord) -> std::io::Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let mut line = serde_json::to_string(record).map_err(std::io::Error::other)?;
        line.push('\n');
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let size = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        if size > 0 && size + line.len() as u64 > self.max_bytes {
            std::fs::rename(&self.path, self.rotated())?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)?;
        file.write_all(line.as_bytes())
    }

    /// Where "New chat" archived this transcript (before 2026-11-20).
    fn archive_dir(&self) -> PathBuf {
        self.path
            .parent()
            .map_or_else(|| PathBuf::from("chats"), |p| p.join("chats"))
    }

    /// Both files' contents, oldest first. Caller holds the lock.
    fn read_all(&self) -> String {
        let mut text = std::fs::read_to_string(self.rotated()).unwrap_or_default();
        text.push_str(&std::fs::read_to_string(&self.path).unwrap_or_default());
        text
    }

    /// The current chat (since the last `new_chat`), limited to its last `max_turns`
    /// turns, with turn ids renumbered from 1 (earlier daemon runs reused them).
    pub fn load_chat(&self, max_turns: usize) -> Vec<EventRecord> {
        let text = {
            let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
            self.read_all()
        };
        parse_chat(&text, max_turns)
    }

    /// Archived chats, newest first.
    pub fn past_chats(&self) -> Vec<PastChat> {
        let Ok(dir) = std::fs::read_dir(self.archive_dir()) else {
            return Vec::new();
        };
        let mut ids: Vec<u64> = dir
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                name.strip_suffix(".jsonl")?.parse().ok()
            })
            .collect();
        ids.sort_unstable_by(|a, b| b.cmp(a));
        ids.truncate(PAST_CHATS);
        ids.into_iter()
            .filter_map(|id| {
                let events = self.past_chat(id)?;
                let title = events
                    .iter()
                    .find_map(|r| match &r.event {
                        LeadEvent::UserMessage {
                            text, attachments, ..
                        } => Some(shorten(&with_file_names(text, attachments), 60)),
                        _ => None,
                    })
                    .unwrap_or_default();
                Some(PastChat {
                    id,
                    started: events.first().map_or(0, |r| r.ts),
                    ended: id,
                    title,
                    turns: events
                        .iter()
                        .filter(|r| matches!(r.event, LeadEvent::UserMessage { .. }))
                        .count(),
                })
            })
            .collect()
    }

    /// The events of an archived chat (renumbered like [`Transcript::load_chat`]).
    pub fn past_chat(&self, id: u64) -> Option<Vec<EventRecord>> {
        // `id` is a number, so the path cannot leave the archive directory.
        let text = std::fs::read_to_string(self.archive_dir().join(format!("{id}.jsonl"))).ok()?;
        Some(parse_chat(&text, usize::MAX))
    }

    fn rotated(&self) -> PathBuf {
        let mut name = self.path.as_os_str().to_os_string();
        name.push(".1");
        PathBuf::from(name)
    }

    /// What a new agent session is primed with (it starts without the earlier conversation):
    /// the last messages (user and agent) since the last fresh start, oldest first, within
    /// about `max_chars` characters (long messages shortened), and the summary Melissa made
    /// when it last condensed the context, if that came after the fresh start (then only the
    /// messages after it). See [`LeadEvent::FreshStart`] and [`LeadEvent::ContextCompacted`].
    pub fn recap(&self, max_chars: usize) -> Recap {
        let records = self.since_boundary();
        let mut out = Recap::default();
        let mut used = 0;
        for record in records.iter().rev() {
            let (who, msg) = match &record.event {
                LeadEvent::ContextCompacted { summary } => {
                    out.summary = Some(summary.clone());
                    break;
                }
                LeadEvent::UserMessage {
                    text, attachments, ..
                } => ("user", with_file_names(text, attachments)),
                LeadEvent::HelperReport { name, status, .. } => (
                    "melissa",
                    format!("helper {name}'s job ended ({status}); its report reached you as data"),
                ),
                // Seen from the agent being primed: its own earlier replies.
                LeadEvent::AgentMessage { text, .. } => ("you", text.clone()),
                _ => continue,
            };
            let msg = shorten(&msg, 600);
            used += msg.len();
            if used > max_chars && !out.messages.is_empty() {
                // Older messages than this are left out; keep looking for the summary.
                if let Some(summary) = records.iter().rev().find_map(|r| match &r.event {
                    LeadEvent::ContextCompacted { summary } if r.seq < record.seq => {
                        Some(summary.clone())
                    }
                    _ => None,
                }) {
                    out.summary = Some(summary);
                }
                break;
            }
            out.messages.push((who, msg));
        }
        out.messages.reverse();
        out
    }

    /// The summary of the conversation since the last fresh start, for condensing the
    /// context: the previous summary (if any), then each older message shortened to its
    /// first lines, leaving out the last `keep` messages (those go to the new session
    /// whole). At most about `max_chars` characters: the oldest parts go first. None when
    /// there is nothing older than the kept messages.
    pub fn condense(&self, keep: usize, max_chars: usize) -> Option<String> {
        let records = self.since_boundary();
        let mut previous: Option<String> = None;
        let mut messages: Vec<(&'static str, String)> = Vec::new();
        for r in &records {
            match &r.event {
                LeadEvent::ContextCompacted { summary } => {
                    previous = Some(summary.clone());
                    messages.clear();
                }
                LeadEvent::UserMessage {
                    text, attachments, ..
                } => messages.push(("user", with_file_names(text, attachments))),
                LeadEvent::AgentMessage { text, .. } => messages.push(("you", text.clone())),
                _ => {}
            }
        }
        if messages.len() <= keep {
            return None;
        }
        let older = &messages[..messages.len() - keep];
        let mut lines: Vec<String> = older
            .iter()
            .map(|(who, m)| format!("{who}: {}", shorten(&one_line(m), 240)))
            .collect();
        // Newest first within the budget, so what is left out is the oldest.
        let budget = max_chars.saturating_sub(previous.as_ref().map_or(0, String::len));
        let mut used = 0;
        let mut kept = Vec::new();
        while let Some(line) = lines.pop() {
            used += line.len() + 1;
            if used > budget && !kept.is_empty() {
                break;
            }
            kept.push(line);
        }
        kept.reverse();
        let mut out = String::new();
        if let Some(p) = previous {
            let p = shorten(&p, max_chars / 2);
            out.push_str(&p);
            out.push('\n');
        }
        out.push_str(&kept.join("\n"));
        Some(out)
    }

    /// Every record since the last fresh start (or new chat), oldest first.
    fn since_boundary(&self) -> Vec<EventRecord> {
        let text = {
            let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
            self.read_all()
        };
        let mut records: Vec<EventRecord> = text
            .lines()
            .filter_map(|l| serde_json::from_str::<EventRecord>(l).ok())
            .collect();
        if let Some(i) = records
            .iter()
            .rposition(|r| matches!(r.event, LeadEvent::FreshStart | LeadEvent::NewChat))
        {
            records.drain(..=i);
        }
        records
    }
}

/// What a new session is primed with (see [`Transcript::recap`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recap {
    /// Melissa's summary of the older conversation, if it was condensed.
    pub summary: Option<String>,
    /// The last messages: (`user` or `you`, text), oldest first.
    pub messages: Vec<(&'static str, String)>,
}

/// The text on one line: line breaks and runs of spaces become single spaces.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Events kept when a chat is read back (the in-memory log's size).
const MAX_EVENTS: usize = 1500;

/// The events of one chat from transcript lines: those after the last `new_chat`, from the
/// start of the last `max_turns` turns, at most [`MAX_EVENTS`]. Sequence numbers are
/// renumbered from 1 and turn ids from 1 (each daemon run counted turns from 1 again).
fn parse_chat(text: &str, max_turns: usize) -> Vec<EventRecord> {
    let mut records: Vec<EventRecord> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<EventRecord>(l).ok())
        .filter(|r| {
            !matches!(
                r.event,
                LeadEvent::Text { .. }
                    | LeadEvent::EarlierStart
                    | LeadEvent::EarlierEnd
                    | LeadEvent::MessageQueued { .. }
                    | LeadEvent::MessageUnqueued { .. }
            )
        })
        .collect();
    if let Some(i) = records
        .iter()
        .rposition(|r| matches!(r.event, LeadEvent::NewChat))
    {
        records.drain(..=i);
    }
    let starts: Vec<usize> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            matches!(
                r.event,
                LeadEvent::UserMessage { .. } | LeadEvent::HelperReport { .. }
            )
        })
        .map(|(i, _)| i)
        .collect();
    if starts.len() > max_turns {
        records.drain(..starts[starts.len() - max_turns]);
    }
    if records.len() > MAX_EVENTS {
        records.drain(..records.len() - MAX_EVENTS);
    }
    let mut turns: HashMap<TurnId, TurnId> = HashMap::new();
    let mut next: TurnId = 0;
    for (i, r) in records.iter_mut().enumerate() {
        r.seq = i as u64 + 1;
        if let LeadEvent::UserMessage { turn, .. } | LeadEvent::HelperReport { turn, .. } = &r.event
        {
            next += 1;
            turns.insert(*turn, next);
        }
        // A turn whose start is not in the window has no turn id any more.
        r.event.map_turn(|t| turns.get(&t).copied());
    }
    records
}

/// At most `max` characters, with an ellipsis if cut.
pub(crate) fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max.saturating_sub(3)).collect();
        format!("{cut}...")
    }
}

/// A user message's text with its files named (`[files: a.png, b.pdf]`), for titles and
/// the conversation replayed into a new session (the files themselves are not resent).
fn with_file_names(text: &str, files: &[crate::attachments::Attachment]) -> String {
    let text = text.trim();
    if files.is_empty() {
        return text.to_string();
    }
    let names: Vec<&str> = files.iter().map(|a| a.name.as_str()).collect();
    let files = format!("[files: {}]", names.join(", "));
    if text.is_empty() {
        files
    } else {
        format!("{text} {files}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(seq: u64, event: LeadEvent) -> EventRecord {
        EventRecord { seq, ts: 0, event }
    }

    #[test]
    fn rotates_and_summarises() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("melissa-transcript-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let t = Transcript::new(dir.join("lead/transcript.jsonl"), 2048);
        for i in 0..40 {
            t.append(&record(
                i,
                LeadEvent::UserMessage {
                    turn: i,
                    text: format!("message {i}"),
                    attachments: vec![],
                    scheduled: None,
                },
            ));
            t.append(&record(
                i,
                LeadEvent::AgentMessage {
                    turn: i,
                    text: format!("reply {i}"),
                },
            ));
        }
        let size = std::fs::metadata(t.path()).unwrap().len();
        assert!(size <= 2048, "{size}");
        assert!(t.rotated().exists());
        assert_eq!(
            std::fs::metadata(t.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let recent = t.recap(40).messages;
        assert_eq!(recent.last().unwrap(), &("you", "reply 39".to_string()));
        assert!(recent.len() < 10);
        let all = t.recap(100_000).messages;
        assert!(all.len() > recent.len());
        // Transcripts from before agents had conversations still read.
        std::fs::write(
            t.path(),
            "{\"seq\":1,\"ts\":0,\"type\":\"queen_message\",\"turn\":1,\"text\":\"old\"}\n",
        )
        .unwrap();
        assert_eq!(
            t.recap(100_000).messages.last().unwrap(),
            &("you", "old".to_string())
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn load_chat_keeps_the_last_turns_and_renumbers() {
        let lines: Vec<String> = [
            // An earlier chat, closed by "New chat".
            LeadEvent::UserMessage {
                turn: 1,
                text: "old".into(),
                attachments: vec![],
                scheduled: None,
            },
            LeadEvent::NewChat,
            // Two daemon runs, both counting turns from 1.
            LeadEvent::UserMessage {
                turn: 1,
                text: "a".into(),
                attachments: vec![],
                scheduled: None,
            },
            LeadEvent::AgentMessage {
                turn: 1,
                text: "ra".into(),
            },
            LeadEvent::UserMessage {
                turn: 1,
                text: "b".into(),
                attachments: vec![],
                scheduled: None,
            },
            LeadEvent::ToolCall {
                turn: Some(1),
                id: "x".into(),
                title: "t".into(),
                status: "done".into(),
                ms: Some(12),
            },
            LeadEvent::UserMessage {
                turn: 2,
                text: "c".into(),
                attachments: vec![],
                scheduled: None,
            },
            LeadEvent::TurnFinished {
                turn: 2,
                stop_reason: "end_turn".into(),
            },
        ]
        .into_iter()
        .enumerate()
        .map(|(i, e)| serde_json::to_string(&record(i as u64 + 7, e)).unwrap())
        .collect();
        let text = lines.join("\n");
        let all = parse_chat(&text, 10);
        assert_eq!(all.len(), 6, "only the current chat");
        assert_eq!(
            all.iter().map(|r| r.seq).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 6]
        );
        let turns: Vec<Option<TurnId>> = all.iter().map(|r| r.event.turn()).collect();
        assert_eq!(
            turns,
            [Some(1), Some(1), Some(2), Some(2), Some(3), Some(3)]
        );
        let last_two = parse_chat(&text, 2);
        assert_eq!(last_two.len(), 4);
        assert_eq!(
            last_two[0].event,
            LeadEvent::UserMessage {
                turn: 1,
                text: "b".into(),
                attachments: vec![],
                scheduled: None,
            }
        );
    }

    fn user(text: &str) -> LeadEvent {
        LeadEvent::UserMessage {
            turn: 1,
            text: text.into(),
            attachments: vec![],
            scheduled: None,
        }
    }

    fn reply(text: &str) -> LeadEvent {
        LeadEvent::AgentMessage {
            turn: 1,
            text: text.into(),
        }
    }

    #[test]
    fn a_fresh_start_and_a_summary_bound_the_recap() {
        let dir = std::env::temp_dir().join(format!("melissa-recap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let t = Transcript::new(dir.join("transcript.jsonl"), 1 << 20);
        for (i, e) in [user("before"), reply("old answer"), LeadEvent::FreshStart]
            .into_iter()
            .enumerate()
        {
            t.append(&record(i as u64, e));
        }
        assert_eq!(
            t.recap(10_000),
            Recap::default(),
            "nothing since the fresh start"
        );
        assert_eq!(t.condense(2, 1000), None);
        for i in 0..6u64 {
            t.append(&record(10 + 2 * i, user(&format!("ask {i}\nsecond line"))));
            t.append(&record(11 + 2 * i, reply(&format!("answer {i}"))));
        }
        // Keep the last 4 messages whole; the 8 before them are condensed, one line each.
        let summary = t.condense(4, 10_000).unwrap();
        assert!(
            summary.starts_with("user: ask 0 second line\nyou: answer 0"),
            "{summary}"
        );
        assert!(summary.contains("you: answer 3"), "{summary}");
        assert!(!summary.contains("ask 4"), "{summary}");
        assert!(
            !summary.contains("before"),
            "not from before the fresh start"
        );
        t.append(&record(
            30,
            LeadEvent::ContextCompacted {
                summary: summary.clone(),
            },
        ));
        t.append(&record(31, user("new question")));
        let recap = t.recap(10_000);
        assert_eq!(recap.summary.as_deref(), Some(summary.as_str()));
        assert_eq!(recap.messages, [("user", "new question".to_string())]);
        // A later condense starts from the previous summary.
        for i in 0..4u64 {
            t.append(&record(40 + i, reply(&format!("later {i}"))));
        }
        let again = t.condense(2, 10_000).unwrap();
        assert!(again.starts_with(&summary), "{again}");
        assert!(again.contains("user: new question"));
        // Over budget: the oldest lines go first.
        let short = t.condense(2, 60).unwrap();
        assert!(short.contains("later 1"), "{short}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn shorten_cuts_long_text() {
        assert_eq!(shorten("abc", 5), "abc");
        assert_eq!(shorten("abcdefgh", 6), "abc...");
    }
}
