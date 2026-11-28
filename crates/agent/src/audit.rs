//! Append-only audit log (JSON Lines). Every mutating action on the agent layer is
//! recorded: dispatch, cancel and the final outcome of each task.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

tokio::task_local! {
    /// Who is signed in for the request being handled (the web layer sets it around each
    /// request), so every audit record made while handling it names the person.
    pub static PERSON: String;
    /// The signed-in person's account id for the same request (the web layer sets it with
    /// [`PERSON`]; `ToolRegistry::call` sets it again from [`crate::tools::TaskContext::person`]
    /// for a turn a person started). Apps record it as who they were installed for.
    pub static PERSON_ID: String;
    /// The signed-in person for the same request is the owner (the web layer sets it with
    /// [`PERSON_ID`]; the host's own CLI counts as the owner). Never set inside a turn: only
    /// a message the owner typed gets an owner message id ([`crate::tools::owner`]).
    pub static PERSON_IS_OWNER: bool;
}

/// The account id of the person the current request or turn acts for, if known.
pub fn requester() -> Option<String> {
    PERSON_ID.try_with(Clone::clone).ok()
}

/// Is the request being handled the owner's own (see [`PERSON_IS_OWNER`])? False when unknown.
pub fn requester_is_owner() -> bool {
    PERSON_IS_OWNER.try_with(|o| *o).unwrap_or(false)
}

/// One audit entry, written as a single JSON line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditRecord {
    /// Seconds since the Unix epoch.
    pub ts: u64,
    /// Who acted: `ui` for the web UI and the API, `agent`, `melissa`, ...
    pub actor: String,
    /// The signed-in person behind a `ui` action (their username; `host` for the host's own
    /// CLI on the local socket). Set from [`PERSON`] when the record is made.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
    /// What happened, e.g. `task.dispatch`, `task.cancel`, `task.finish`.
    pub action: String,
    pub task: u64,
    pub backend: String,
    /// Free-form detail (prompt on dispatch, outcome on finish).
    pub detail: String,
    /// Tool name, for `tool.*` actions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// SHA-256 of the tool's canonical JSON arguments (arguments themselves are not logged).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args_sha256: Option<String>,
    /// The task that started this one (an agent's lead agent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<u64>,
}

impl AuditRecord {
    pub fn now(action: &str, task: u64, backend: &str, detail: impl Into<String>) -> Self {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            ts,
            actor: "ui".into(),
            person: PERSON.try_with(Clone::clone).ok(),
            action: action.into(),
            task,
            backend: backend.into(),
            detail: detail.into(),
            tool: None,
            args_sha256: None,
            parent: None,
        }
    }

    pub fn by(mut self, actor: &str) -> Self {
        self.actor = actor.into();
        self
    }

    pub fn with_tool(mut self, tool: &str, args_sha256: &str) -> Self {
        self.tool = Some(tool.into());
        self.args_sha256 = Some(args_sha256.into());
        self
    }
}

/// Appends [`AuditRecord`]s to a file, creating parent directories as needed.
/// Clones share one lock, so writers never interleave lines.
#[derive(Debug, Clone)]
pub struct AuditLog {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl AuditLog {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one record. Writes are small and serialized by a mutex. Each line carries its
    /// format ([`crate::formats::AUDIT_LINE`]); a new file is mode 0640 (as the factory reset
    /// writes it).
    pub fn append(&self, record: &AuditRecord) -> std::io::Result<()> {
        let line = melissa_statefile::encode_line(&crate::formats::AUDIT_LINE, record)
            .map_err(std::io::Error::other)?;
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        melissa_statefile::append_line(&self.path, &line, 0o640)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_json_lines() {
        let dir = crate::test_support::fresh_path("audit-test");
        let log = AuditLog::new(dir.join("nested/audit.jsonl"));
        log.append(&AuditRecord::now("task.dispatch", 1, "echo", "hi"))
            .unwrap();
        log.append(&AuditRecord::now("task.finish", 1, "echo", "succeeded"))
            .unwrap();
        let text = std::fs::read_to_string(log.path()).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["action"], "task.dispatch");
        assert_eq!(lines[1]["detail"], "succeeded");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn records_name_the_signed_in_person() {
        let outside = AuditRecord::now("x", 0, "", "");
        assert_eq!(outside.person, None);
        let inside = PERSON
            .scope("ana".to_string(), async {
                AuditRecord::now("x", 0, "", "")
            })
            .await;
        assert_eq!(inside.person.as_deref(), Some("ana"));
        let line = serde_json::to_value(&inside).unwrap();
        assert_eq!(line["person"], "ana");
        assert!(serde_json::to_value(&outside)
            .unwrap()
            .get("person")
            .is_none());
    }
}
