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

/// A chat archived by "New chat" (read-only).
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

    /// Where "New chat" archives this transcript.
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

    /// Move the current chat to the archive (`chats/<id>.jsonl`, mode 0600) and start an
    /// empty transcript. Returns the archive's id, or None if there was nothing to keep.
    pub fn archive(&self) -> std::io::Result<Option<u64>> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let text = self.read_all();
        if !text.lines().any(|l| {
            serde_json::from_str::<EventRecord>(l)
                .is_ok_and(|r| matches!(r.event, LeadEvent::UserMessage { .. }))
        }) {
            // Nothing said yet: nothing worth keeping.
            let _ = std::fs::remove_file(self.rotated());
            let _ = std::fs::remove_file(&self.path);
            return Ok(None);
        }
        let dir = self.archive_dir();
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        let mut id = now_secs();
        let mut file = loop {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(dir.join(format!("{id}.jsonl")))
            {
                Ok(f) => break f,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => id += 1,
                Err(e) => return Err(e),
            }
        };
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        let _ = std::fs::remove_file(self.rotated());
        std::fs::remove_file(&self.path).or_else(|e| match e.kind() {
            std::io::ErrorKind::NotFound => Ok(()),
            _ => Err(e),
        })?;
        Ok(Some(id))
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

    /// The last conversation messages (user and lead agent), oldest first, within about
    /// `max_chars` characters in total; long messages are shortened. For priming a fresh
    /// agent session after the previous one was lost.
    pub fn recent_messages(&self, max_chars: usize) -> Vec<(&'static str, String)> {
        let text = {
            let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
            self.read_all()
        };
        let mut out = Vec::new();
        let mut used = 0;
        for line in text.lines().rev() {
            let Ok(record) = serde_json::from_str::<EventRecord>(line) else {
                continue;
            };
            let (who, msg) = match record.event {
                LeadEvent::UserMessage {
                    text, attachments, ..
                } => ("user", with_file_names(&text, &attachments)),
                // Seen from the agent being primed: its own earlier replies.
                LeadEvent::AgentMessage { text, .. } => ("you", text),
                _ => continue,
            };
            let msg = shorten(&msg, 600);
            used += msg.len();
            if used > max_chars && !out.is_empty() {
                break;
            }
            out.push((who, msg));
        }
        out.reverse();
        out
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
                LeadEvent::Text { .. } | LeadEvent::EarlierStart | LeadEvent::EarlierEnd
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
        .filter(|(_, r)| matches!(r.event, LeadEvent::UserMessage { .. }))
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
        if let LeadEvent::UserMessage { turn, .. } = &r.event {
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
        let recent = t.recent_messages(40);
        assert_eq!(recent.last().unwrap(), &("you", "reply 39".to_string()));
        assert!(recent.len() < 10);
        let all = t.recent_messages(100_000);
        assert!(all.len() > recent.len());
        // Transcripts from before agents had conversations still read.
        std::fs::write(
            t.path(),
            "{\"seq\":1,\"ts\":0,\"type\":\"queen_message\",\"turn\":1,\"text\":\"old\"}\n",
        )
        .unwrap();
        assert_eq!(
            t.recent_messages(100_000).last().unwrap(),
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
            },
            LeadEvent::NewChat,
            // Two daemon runs, both counting turns from 1.
            LeadEvent::UserMessage {
                turn: 1,
                text: "a".into(),
                attachments: vec![],
            },
            LeadEvent::AgentMessage {
                turn: 1,
                text: "ra".into(),
            },
            LeadEvent::UserMessage {
                turn: 1,
                text: "b".into(),
                attachments: vec![],
            },
            LeadEvent::ToolCall {
                turn: Some(1),
                id: "x".into(),
                title: "t".into(),
                status: "done".into(),
            },
            LeadEvent::UserMessage {
                turn: 2,
                text: "c".into(),
                attachments: vec![],
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
                attachments: vec![]
            }
        );
    }

    #[test]
    fn shorten_cuts_long_text() {
        assert_eq!(shorten("abc", 5), "abc");
        assert_eq!(shorten("abcdefgh", 6), "abc...");
    }
}
