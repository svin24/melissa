//! Append-only audit log (JSON Lines). Every mutating action on the agent layer is
//! recorded: dispatch, cancel and the final outcome of each task.

use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

/// One audit entry, written as a single JSON line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditRecord {
    /// Seconds since the Unix epoch.
    pub ts: u64,
    /// Who acted. `ui` for the local web UI until accounts exist.
    pub actor: String,
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
    /// The task that started this one (a bee's Queen).
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

    /// Append one record. Writes are small and serialized by a mutex.
    pub fn append(&self, record: &AuditRecord) -> std::io::Result<()> {
        let mut line = serde_json::to_string(record).map_err(std::io::Error::other)?;
        line.push('\n');
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(line.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_json_lines() {
        let dir = std::env::temp_dir().join(format!("melissa-audit-test-{}", std::process::id()));
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
}
