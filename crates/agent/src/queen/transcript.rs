//! The Queen's transcript: every event except text pieces, as JSON Lines, in Melissa's
//! state directory (`$XDG_STATE_HOME/melissa/queen/transcript.jsonl`).
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

use super::events::{EventRecord, QueenEvent};

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
            tracing::warn!(path = %self.path.display(), "queen transcript write failed: {e}");
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

    fn rotated(&self) -> PathBuf {
        let mut name = self.path.as_os_str().to_os_string();
        name.push(".1");
        PathBuf::from(name)
    }

    /// The last conversation messages (user and Queen), oldest first, within about
    /// `max_chars` characters in total; long messages are shortened. For priming a fresh
    /// agent session after the previous one was lost.
    pub fn recent_messages(&self, max_chars: usize) -> Vec<(&'static str, String)> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut text = std::fs::read_to_string(self.rotated()).unwrap_or_default();
        text.push_str(&std::fs::read_to_string(&self.path).unwrap_or_default());
        let mut out = Vec::new();
        let mut used = 0;
        for line in text.lines().rev() {
            let Ok(record) = serde_json::from_str::<EventRecord>(line) else {
                continue;
            };
            let (who, msg) = match record.event {
                QueenEvent::UserMessage { text, .. } => ("user", text),
                QueenEvent::QueenMessage { text, .. } => ("queen", text),
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

/// At most `max` characters, with an ellipsis if cut.
pub(crate) fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max.saturating_sub(3)).collect();
        format!("{cut}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(seq: u64, event: QueenEvent) -> EventRecord {
        EventRecord { seq, ts: 0, event }
    }

    #[test]
    fn rotates_and_summarises() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("melissa-transcript-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let t = Transcript::new(dir.join("queen/transcript.jsonl"), 2048);
        for i in 0..40 {
            t.append(&record(
                i,
                QueenEvent::UserMessage {
                    turn: i,
                    text: format!("message {i}"),
                },
            ));
            t.append(&record(
                i,
                QueenEvent::QueenMessage {
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
        assert_eq!(recent.last().unwrap(), &("queen", "reply 39".to_string()));
        assert!(recent.len() < 10);
        let all = t.recent_messages(100_000);
        assert!(all.len() > recent.len());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn shorten_cuts_long_text() {
        assert_eq!(shorten("abc", 5), "abc");
        assert_eq!(shorten("abcdefgh", 6), "abc...");
    }
}
