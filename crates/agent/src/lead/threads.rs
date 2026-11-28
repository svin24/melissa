//! The record of agent-to-agent talk (owner request, 2026-11-24): one thread per pair of
//! agents (the lead agent counts as one, `lead`), every message passed between them with who
//! sent it to whom, when, the text, and how it was delivered (`data`: straight from the
//! sender, framed as data; `relayed`: another agent's message the lead agent passed on).
//! Helpers never talk to each other directly; a thread between two helpers holds only what
//! the lead agent relayed.
//!
//! Stored like the transcripts: JSON Lines, mode 0600, in Melissa's state directory
//! (`<state>/threads/<a>__<b>.jsonl`, the two ids sorted), rotated to `.1` past a size limit.
//! Read-only for clients (`GET /api/threads`, owner only); nothing here reaches an agent.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};

use super::transcript::shorten;

/// The lead agent in a thread.
pub const LEAD: &str = "lead";
/// Characters of a message kept in a thread.
const TEXT_CHARS: usize = 8000;
/// Size at which a thread file is rotated.
const MAX_BYTES: u64 = 1 << 20;

/// One message between two agents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadEntry {
    /// Unix seconds.
    pub ts: u64,
    /// Who wrote it: `lead` or an agent id.
    pub from: String,
    /// Who got it.
    pub to: String,
    /// What it was: `ask` (a helper's `ask_lead`), `message` (`message_agent`) or `reply` (the
    /// answer to a message, sent back on its own).
    pub kind: String,
    /// `data` (from the sender, framed as data) or `relayed` (passed on by the lead agent).
    pub delivery: String,
    /// The text (untrusted, agent-written; shortened).
    pub text: String,
    /// The owner message a helper's request cited, and whether Melissa verified it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub verified: bool,
}

impl ThreadEntry {
    pub fn new(from: &str, to: &str, kind: &str, delivery: &str, text: &str) -> Self {
        Self {
            ts: now(),
            from: from.into(),
            to: to.into(),
            kind: kind.into(),
            delivery: delivery.into(),
            text: shorten(text, TEXT_CHARS),
            on_behalf_of: None,
            verified: false,
        }
    }
}

/// A thread as listed: the pair, how many messages, when the last one was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThreadSummary {
    pub a: String,
    pub b: String,
    pub messages: usize,
    pub last: u64,
}

/// The threads of this device. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Threads {
    dir: PathBuf,
    lock: Arc<Mutex<()>>,
}

/// `lead` or an agent id: the only names that become file names.
fn valid_party(id: &str) -> bool {
    id == LEAD || crate::device::valid_agent_id(id)
}

impl Threads {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            lock: Arc::new(Mutex::new(())),
        }
    }

    fn path(&self, a: &str, b: &str) -> Option<PathBuf> {
        if !valid_party(a) || !valid_party(b) || a == b {
            return None;
        }
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        Some(self.dir.join(format!("{a}__{b}.jsonl")))
    }

    /// Add `entry` to the thread of its sender and recipient. Best effort: failures are
    /// logged, never fatal.
    pub fn record(&self, entry: &ThreadEntry) {
        let Some(path) = self.path(&entry.from, &entry.to) else {
            tracing::warn!(from = %entry.from, to = %entry.to, "no thread for this pair");
            return;
        };
        if let Err(e) = self.append(&path, entry) {
            tracing::warn!(path = %path.display(), "thread write failed: {e}");
        }
    }

    fn append(&self, path: &Path, entry: &ThreadEntry) -> std::io::Result<()> {
        // Each line carries its format, like the transcripts.
        let line = melissa_statefile::encode_line(&crate::formats::THREAD_LINE, entry)
            .map_err(std::io::Error::other)?;
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        std::fs::create_dir_all(&self.dir)?;
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if size > 0 && size + line.len() as u64 + 1 > MAX_BYTES {
            std::fs::rename(path, rotated(path))?;
        }
        melissa_statefile::append_line(path, &line, 0o600)
    }

    /// The thread between `a` and `b`, oldest first (empty if they never talked; None if
    /// either is not `lead` or an agent id).
    pub fn read(&self, a: &str, b: &str) -> Option<Vec<ThreadEntry>> {
        let path = self.path(a, b)?;
        let text = {
            let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
            let mut text = std::fs::read_to_string(rotated(&path)).unwrap_or_default();
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
            text
        };
        Some(melissa_statefile::decode_lines(
            &crate::formats::THREAD_LINE,
            &text,
            &path,
        ))
    }

    /// Every thread, the most recent first.
    pub fn list(&self) -> Vec<ThreadSummary> {
        let Ok(dir) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut out: Vec<ThreadSummary> = dir
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let (a, b) = name.strip_suffix(".jsonl")?.split_once("__")?;
                let entries = self.read(a, b)?;
                Some(ThreadSummary {
                    a: a.into(),
                    b: b.into(),
                    messages: entries.len(),
                    last: entries.last().map_or(0, |e| e.ts),
                })
            })
            .collect();
        out.sort_by(|x, y| y.last.cmp(&x.last).then_with(|| x.a.cmp(&y.a)));
        out
    }
}

fn rotated(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".1");
    PathBuf::from(name)
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_thread_per_pair_in_either_order() {
        let dir = crate::test_support::fresh_path("threads");
        let t = Threads::new(&dir);
        let a = "d-0123abcd.a1";
        t.record(&ThreadEntry::new(a, LEAD, "ask", "data", "can you ask Bo?"));
        t.record(&ThreadEntry::new(LEAD, a, "message", "data", "Bo says yes"));
        let thread = t.read(LEAD, a).unwrap();
        assert_eq!(thread.len(), 2);
        assert_eq!(thread[0].from, a);
        assert_eq!(t.read(a, LEAD).unwrap(), thread);
        let list = t.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].messages, 2);
        // Only ids become paths.
        assert!(t.read("../x", LEAD).is_none());
        assert!(t.read(LEAD, LEAD).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
