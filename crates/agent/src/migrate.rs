//! One-time migration from the old role names (docs/DECISIONS.md, 2026-11-19: generic terms).
//!
//! Before the rename, a device was a "comb" (`c-<hex>`), an agent a "bee" (`<comb>.b<n>`), the
//! lead agent the "queen" and a workspace a "cell". [`migrate_state`] moves an existing state
//! directory to the new layout on startup, before anything opens it:
//!
//! | Old | New |
//! |---|---|
//! | `comb.json` (`"id": "c-<hex>"`) | `device.json` (`"id": "d-<hex>"`) |
//! | `bees/registry.json` (`comb`, `bees`, ids `c-<hex>.b<n>`) | `agents/registry.json` (`device`, `agents`, ids `d-<hex>.a<n>`) |
//! | `bees/c-<hex>.b<n>/` | `agents/d-<hex>.a<n>/` |
//! | `queen/` | `lead/` |
//! | `work/queen/bees/c-<hex>.b<n>/` | `work/lead/agents/d-<hex>.a<n>/` |
//!
//! Transcripts (`transcript.jsonl`, its rotated copy and `chats/*.jsonl`) and `session.json`
//! get the new ids, event names (`bee_created` → `agent_created`, `cell_reassigned` →
//! `workspace_reassigned`, …), field names (`bee` → `agent`, `cell` → `workspace`) and tool
//! names ([`legacy_tool_name`]). The audit log is history and stays as it is.
//!
//! Every step is idempotent: it only acts while the old name exists, writes the new file
//! before removing the old one, and a second run finds nothing to do. [`migrate_config`] does
//! the same for `queen.toml` → `server.toml`. Namespace labels in the cluster are migrated by
//! the cluster crate (`melissa.io/comb` → `melissa.io/device`, …) with [`new_device_id`] and
//! [`new_agent_id`].

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::device::{valid_agent_id, valid_device_id, write_private};

/// Old tool names and their new names.
const TOOLS: [(&str, &str); 10] = [
    ("create_bee", "create_agent"),
    ("give_bee_job", "give_agent_job"),
    ("bee_status", "agent_status"),
    ("list_bees", "list_agents"),
    ("stop_bee", "stop_agent"),
    ("start_bee", "start_agent"),
    ("message_bee", "message_agent"),
    ("reassign_cell", "reassign_workspace"),
    ("comb_info", "device_info"),
    ("bee_info", "device_info"),
];

/// Old transcript event types and their new names.
const EVENTS: [(&str, &str); 5] = [
    ("bee_created", "agent_created"),
    ("bee_started", "agent_started"),
    ("bee_finished", "agent_finished"),
    ("cell_reassigned", "workspace_reassigned"),
    ("cell_force_deleted", "workspace_force_deleted"),
];

/// The new name of a renamed tool (None: not an old name).
pub fn legacy_tool_name(name: &str) -> Option<&'static str> {
    TOOLS
        .iter()
        .find(|(old, _)| *old == name)
        .map(|(_, new)| *new)
}

/// `c-<hex>` → `d-<hex>` (None: not an old device id).
pub fn new_device_id(old: &str) -> Option<String> {
    let hex = old.strip_prefix("c-")?;
    let new = format!("d-{hex}");
    valid_device_id(&new).then_some(new)
}

/// `c-<hex>.b<n>` → `d-<hex>.a<n>` (None: not an old agent id).
pub fn new_agent_id(old: &str) -> Option<String> {
    let (device, n) = old.split_once(".b")?;
    let new = format!("{}.a{n}", new_device_id(device)?);
    valid_agent_id(&new).then_some(new)
}

/// What [`migrate_state`] or [`migrate_config`] changed, for the log.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub steps: Vec<String>,
}

impl Report {
    fn did(&mut self, step: impl Into<String>) {
        self.steps.push(step.into());
    }
}

#[derive(Debug, thiserror::Error)]
#[error("migrating {path}: {reason}")]
pub struct MigrateError {
    pub path: String,
    pub reason: String,
}

fn err(path: &Path, reason: impl ToString) -> MigrateError {
    MigrateError {
        path: path.display().to_string(),
        reason: reason.to_string(),
    }
}

/// Rename `old` to `new` if `old` exists and `new` does not. Both existing: `new` wins and
/// `old` is left alone (a warning in the report).
fn rename(old: &Path, new: &Path, report: &mut Report) -> Result<bool, MigrateError> {
    if !old.exists() {
        return Ok(false);
    }
    if new.exists() {
        report.did(format!(
            "kept {} (both it and {} exist; the new one is used)",
            old.display(),
            new.display()
        ));
        return Ok(false);
    }
    if let Some(parent) = new.parent() {
        std::fs::create_dir_all(parent).map_err(|e| err(parent, e))?;
    }
    std::fs::rename(old, new).map_err(|e| err(old, e))?;
    report.did(format!("renamed {} to {}", old.display(), new.display()));
    Ok(true)
}

/// Move the state directory `state` (`$XDG_STATE_HOME/melissa`) from the old layout.
pub fn migrate_state(state: &Path) -> Result<Report, MigrateError> {
    let mut report = Report::default();
    let ids = migrate_device_file(state, &mut report)?;
    rename(&state.join("bees"), &state.join("agents"), &mut report)?;
    rename(&state.join("queen"), &state.join("lead"), &mut report)?;
    let work = state.join("work");
    rename(&work.join("queen"), &work.join("lead"), &mut report)?;
    rename(
        &work.join("lead").join("bees"),
        &work.join("lead").join("agents"),
        &mut report,
    )?;
    let Some(ids) = ids else {
        return Ok(report);
    };
    migrate_registry(&state.join("agents"), &ids, &mut report)?;
    rename_id_dirs(&state.join("agents"), &mut report)?;
    rename_id_dirs(&work.join("lead").join("agents"), &mut report)?;
    rewrite_transcripts(&state.join("lead"), &ids, &mut report)?;
    if let Ok(entries) = std::fs::read_dir(state.join("agents")) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                rewrite_transcripts(&entry.path(), &ids, &mut report)?;
            }
        }
    }
    Ok(report)
}

/// The old and new device id, for rewriting ids in files.
struct Ids {
    old: String,
    new: String,
}

impl Ids {
    /// Replace this device's old ids in `text` (`c-<hex>.b<n>` first, then `c-<hex>`).
    fn rewrite(&self, text: &str) -> String {
        text.replace(&format!("{}.b", self.old), &format!("{}.a", self.new))
            .replace(&self.old, &self.new)
    }
}

/// `comb.json` → `device.json` with the new id. Returns the ids if this device had an old id
/// (now or in an earlier, interrupted run: `device.json` exists and old ids may remain).
fn migrate_device_file(state: &Path, report: &mut Report) -> Result<Option<Ids>, MigrateError> {
    let old_path = state.join("comb.json");
    let new_path = state.join("device.json");
    if old_path.exists() && !new_path.exists() {
        let text = std::fs::read_to_string(&old_path).map_err(|e| err(&old_path, e))?;
        let mut json: Value = serde_json::from_str(&text).map_err(|e| err(&old_path, e))?;
        let old = json["id"].as_str().unwrap_or_default().to_string();
        let new = new_device_id(&old).ok_or_else(|| err(&old_path, "no old device id in it"))?;
        json["id"] = Value::String(new);
        let out = serde_json::to_vec_pretty(&json).map_err(|e| err(&new_path, e))?;
        write_private(&new_path, &out).map_err(|e| err(&new_path, e))?;
        report.did(format!(
            "wrote {} with id {}",
            new_path.display(),
            json["id"]
        ));
    }
    if old_path.exists() && new_path.exists() {
        // Written above or by an interrupted run: the old file only names the old id now.
        let text = std::fs::read_to_string(&old_path).map_err(|e| err(&old_path, e))?;
        let old: Value = serde_json::from_str(&text).unwrap_or_default();
        std::fs::remove_file(&old_path).map_err(|e| err(&old_path, e))?;
        report.did(format!("removed {}", old_path.display()));
        if let Some(old) = old["id"].as_str() {
            if let Some(new) = new_device_id(old) {
                return Ok(Some(Ids {
                    old: old.to_string(),
                    new,
                }));
            }
        }
    }
    // Already migrated: the old id is the new one with `c-`, so leftovers still get fixed.
    let text = match std::fs::read_to_string(&new_path) {
        Ok(text) => text,
        Err(_) => return Ok(None),
    };
    let json: Value = serde_json::from_str(&text).map_err(|e| err(&new_path, e))?;
    Ok(json["id"].as_str().and_then(|new| {
        let hex = new.strip_prefix("d-")?;
        Some(Ids {
            old: format!("c-{hex}"),
            new: new.to_string(),
        })
    }))
}

/// `registry.json`: `comb` → `device`, `bees` → `agents`, new ids and tool names.
fn migrate_registry(dir: &Path, ids: &Ids, report: &mut Report) -> Result<(), MigrateError> {
    let path = dir.join("registry.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let mut json: Value = serde_json::from_str(&ids.rewrite(&text)).map_err(|e| err(&path, e))?;
    let Some(obj) = json.as_object_mut() else {
        return Err(err(&path, "not a JSON object"));
    };
    let mut changed = ids.rewrite(&text) != text;
    for (old, new) in [("comb", "device"), ("bees", "agents")] {
        if let Some(v) = obj.remove(old) {
            obj.entry(new).or_insert(v);
            changed = true;
        }
    }
    if let Some(Value::Array(agents)) = obj.get_mut("agents") {
        for agent in agents {
            if let Some(Value::Array(tools)) = agent.get_mut("tools") {
                for tool in tools {
                    if let Some(new) = tool.as_str().and_then(legacy_tool_name) {
                        *tool = Value::String(new.into());
                        changed = true;
                    }
                }
            }
            if let Some(Value::String(by)) = agent.get_mut("created_by") {
                let new = by.replace("the Queen", "the lead agent");
                if new != *by {
                    *by = new;
                    changed = true;
                }
            }
        }
    }
    if changed {
        let out = serde_json::to_vec_pretty(&json).map_err(|e| err(&path, e))?;
        write_private(&path, &out).map_err(|e| err(&path, e))?;
        report.did(format!("updated {}", path.display()));
    }
    Ok(())
}

/// Rename `<dir>/c-<hex>.b<n>` entries to `<dir>/d-<hex>.a<n>`.
fn rename_id_dirs(dir: &Path, report: &mut Report) -> Result<(), MigrateError> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    let mut old: Vec<(PathBuf, String)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(new) = new_agent_id(&name) {
            old.push((entry.path(), new));
        }
    }
    old.sort();
    for (path, new) in old {
        rename(&path, &dir.join(new), report)?;
    }
    Ok(())
}

/// Rewrite the transcripts and `session.json` in an agent's (or the lead agent's) state dir.
fn rewrite_transcripts(dir: &Path, ids: &Ids, report: &mut Report) -> Result<(), MigrateError> {
    let mut files: Vec<PathBuf> = Vec::new();
    for d in [dir.to_path_buf(), dir.join("chats")] {
        if let Ok(entries) = std::fs::read_dir(&d) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if entry.path().is_file() && (name.contains(".jsonl") || name == "session.json") {
                    files.push(entry.path());
                }
            }
        }
    }
    files.sort();
    for path in files {
        let text = std::fs::read_to_string(&path).map_err(|e| err(&path, e))?;
        let new = if path.file_name().is_some_and(|n| n == "session.json") {
            ids.rewrite(&text)
        } else {
            let mut out = String::with_capacity(text.len());
            for line in text.split_inclusive('\n') {
                let (body, nl) = match line.strip_suffix('\n') {
                    Some(b) => (b, "\n"),
                    None => (line, ""),
                };
                out.push_str(&rewrite_event_line(body, ids));
                out.push_str(nl);
            }
            out
        };
        if new != text {
            write_private(&path, new.as_bytes()).map_err(|e| err(&path, e))?;
            report.did(format!("updated {}", path.display()));
        }
    }
    Ok(())
}

/// One transcript line: new ids, event type, field and tool names. Lines that are not JSON
/// objects only get the new ids.
fn rewrite_event_line(line: &str, ids: &Ids) -> String {
    let line = ids.rewrite(line);
    let Ok(Value::Object(mut obj)) = serde_json::from_str::<Value>(&line) else {
        return line;
    };
    let mut changed = false;
    if let Some(Value::String(kind)) = obj.get_mut("type") {
        if let Some((_, new)) = EVENTS.iter().find(|(old, _)| old == kind) {
            *kind = (*new).to_string();
            changed = true;
        }
    }
    for (old, new) in [("bee", "agent"), ("cell", "workspace")] {
        if let Some(v) = obj.remove(old) {
            obj.entry(new).or_insert(v);
            changed = true;
        }
    }
    for key in ["title", "tool"] {
        if let Some(Value::String(s)) = obj.get_mut(key) {
            let new = rename_tools(s);
            if new != *s {
                *s = new;
                changed = true;
            }
        }
    }
    if changed {
        serde_json::to_string(&obj).unwrap_or(line)
    } else {
        line
    }
}

/// Replace old tool names inside a tool title (`melissa__create_bee`, `create_bee (melissa)`).
fn rename_tools(title: &str) -> String {
    let mut out = title.to_string();
    for (old, new) in TOOLS {
        let mut from = 0;
        while let Some(i) = out[from..].find(old).map(|i| i + from) {
            let end = i + old.len();
            let before = out[..i].chars().next_back();
            let after = out[end..].chars().next();
            let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
            if !word(before) && !word(after) && after != Some('_') {
                out.replace_range(i..end, new);
                from = i + new.len();
            } else {
                from = end;
            }
        }
    }
    out
}

/// `queen.toml` → `server.toml` in the config directory (`$XDG_CONFIG_HOME/melissa`).
pub fn migrate_config(config: &Path) -> Result<Report, MigrateError> {
    let mut report = Report::default();
    rename(
        &config.join("queen.toml"),
        &config.join("server.toml"),
        &mut report,
    )?;
    Ok(report)
}

#[cfg(test)]
mod tests;
