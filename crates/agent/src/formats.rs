//! The format of every file this crate persists (melissa-statefile; inventory in
//! docs/design/STATE-INVENTORY-2026-11-28.md). Bump `current` and add a migration whenever an
//! older Melissa would misread what this one writes (see the melissa-statefile docs).
//!
//! `v1_marker: false`: format 1 is written without the `format` key because the images from
//! before formats existed would reject it (a JSON array, or a map keyed by data).

use melissa_statefile::{Format, JsonFormat};

const fn v1(name: &'static str, v1_marker: bool) -> JsonFormat {
    Format {
        name,
        current: 1,
        migrations: &[],
        v1_marker,
    }
}

/// `<state>/device.json`.
pub static DEVICE: JsonFormat = v1("device identity", true);
/// `<state>/agents/registry.json`.
pub static REGISTRY: JsonFormat = v1("agent registry", true);
/// `session.json` in a conversation's state directory.
pub static SESSION: JsonFormat = v1("saved agent session", true);
/// `backend.json` in a conversation's state directory.
pub static BACKEND_CHOICE: JsonFormat = v1("backend choice", true);
/// `<state>/lead/models.json` (a map keyed by backend).
pub static MODELS: JsonFormat = v1("model list", false);
/// `usage.json` in a conversation's state directory.
pub static USAGE: JsonFormat = v1("usage counters", true);
/// `<state>/schedules.json`.
pub static SCHEDULES: JsonFormat = v1("schedules", true);
/// `<state>/permissions.json`.
pub static PERMISSIONS: JsonFormat = v1("standing permissions", true);
/// `<state>/files/retention.json`.
pub static RETENTION: JsonFormat = v1("file retention", true);
/// `<state>/files/index/<box>.json` (a JSON array).
pub static FILE_INDEX: JsonFormat = v1("file box index", false);
/// `<state>/agent-users.json`.
pub static AGENT_USERS: JsonFormat = v1("agent user table", true);
/// `<state>/lead/provider-usage.json` (a map keyed by provider).
pub static PROVIDER_USAGE: JsonFormat = v1("provider usage", false);

/// Lines of `transcript.jsonl` (and its rotated copy and old `chats/*.jsonl` archives).
pub static TRANSCRIPT_LINE: JsonFormat = v1("transcript", true);
/// Lines of `threads.jsonl`.
pub static THREAD_LINE: JsonFormat = v1("thread list", true);
/// Lines of `<state>/audit.jsonl`.
pub static AUDIT_LINE: JsonFormat = v1("audit log", true);

#[cfg(test)]
mod tests;
