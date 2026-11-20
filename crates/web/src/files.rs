//! File boxes in the UI and the JSON API: each conversation's file panel, serving a file,
//! deleting one, and how long a box keeps files (see `melissa_agent::files`).
//!
//! `{key}` is the box: `lead` (Melissa's) or an agent's id; `{id}` a file's generated id.
//!
//! UI (same-origin checked like every POST; htmx gets the panel's body, a plain form post a
//! redirect back to the chat):
//! - `GET /ui/files/{key}/{id}`: the file. Sniffed images inline, everything else as a
//!   download; never script (CSP `sandbox`), never embedded by another site.
//! - `POST /ui/files/{key}/{id}/delete`: delete it (audited `files.deleted`, actor `ui`).
//! - `POST /ui/files/{key}/retention` (`keep` = `default`, a number of days or `forever`).
//!
//! API:
//! - `GET /api/files/{key}` → `{"box", "path", "files": [{id, name, type, size, added, by}],
//!   "keep_days": n|null (null: forever), "override": bool, "used": bytes}`; 404 not a box.
//! - `GET /api/files/{key}/{id}` → the file (as above); 404 unknown.
//! - `POST /api/files/{key}/{id}/delete` → the deleted file; 404 unknown.
//! - `POST /api/files/{key}/retention` `{"keep_days": n|null}` or `{"default": true}` → the
//!   box's retention; 422 out of bounds (1–3650 days).
//! - `GET /api/settings/files` → `{"keep_days": n|null, "overrides": {key: n|null}}`;
//!   `POST /api/settings/files` `{"keep_days": n|null}` → the same.

use std::io::Read;

use axum::{
    body::{Body, Bytes},
    extract::{Form, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use maud::html;
use melissa_agent::{
    attachments::human_size,
    files::{box_key, owner_of_key, AddedBy, BoxFile, FileBoxError, Keep},
    tools::permissions::Principal,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    components::{file_count, file_panel_body, FilePanel, PanelFile},
    is_htmx,
    pages::{file_href, short_date, FileLinks, Who},
    ui::Ui,
};

/// Retention choices offered in the panel and Settings (days), besides default and forever.
const DAY_CHOICES: [u32; 9] = [1, 2, 3, 5, 7, 14, 30, 90, 365];

pub(crate) fn owner_of(who: Who<'_>) -> Principal {
    match who {
        Who::Lead => Principal::Lead,
        Who::Agent(id) => Principal::Agent(id.to_string()),
    }
}

/// The chat page of a box's owner.
fn chat_url(owner: &Principal) -> String {
    match owner {
        Principal::Lead => "/".into(),
        Principal::Agent(id) => format!("/agents/{id}"),
    }
}

/// `5 days` → `Keep files for 5 days`-style labels.
fn day_label(n: u32) -> String {
    match n {
        7 => "1 week".into(),
        14 => "2 weeks".into(),
        365 => "1 year".into(),
        n => Keep::Days(n).words(),
    }
}

fn keep_label(k: Keep) -> String {
    match k {
        Keep::Days(n) => day_label(n),
        Keep::Forever => "Forever".into(),
    }
}

/// (value, label) choices for a retention select; `with_default` adds "Default (…)".
pub(crate) fn keep_choices(default: Option<Keep>, current: Option<Keep>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(d) = default {
        out.push(("default".into(), format!("Default ({})", keep_label(d))));
    }
    let mut days: Vec<u32> = DAY_CHOICES.to_vec();
    if let Some(Keep::Days(n)) = current {
        if !days.contains(&n) {
            days.push(n);
            days.sort_unstable();
        }
    }
    out.extend(days.into_iter().map(|n| (n.to_string(), day_label(n))));
    out.push(("forever".into(), "Forever".into()));
    out
}

fn usage_words(files: &[BoxFile]) -> String {
    let total: u64 = files.iter().map(|f| f.size).sum();
    match files.len() {
        0 => "No files".into(),
        1 => format!("1 file, {}", human_size(total)),
        n => format!("{n} files, {}", human_size(total)),
    }
}

/// Everything the file panel shows (owned; [`PanelData::view`] borrows it).
pub(crate) struct PanelData {
    files: Vec<PanelFile>,
    usage: String,
    keep: String,
    keep_action: String,
    choices: Vec<(String, String)>,
    selected: String,
    whose: String,
}

impl PanelData {
    pub(crate) fn view(&self) -> FilePanel<'_> {
        FilePanel {
            files: &self.files,
            usage: &self.usage,
            keep: &self.keep,
            keep_action: &self.keep_action,
            keep_choices: &self.choices,
            keep_selected: &self.selected,
            whose: &self.whose,
        }
    }
}

impl Ui {
    fn name_of(&self, owner: &Principal) -> String {
        match owner {
            Principal::Lead => "Melissa".into(),
            Principal::Agent(id) => self
                .lead
                .agents()
                .get(id)
                .map_or_else(|| id.clone(), |a| a.name),
        }
    }

    /// When something happened, for people: `20 Nov 14:03` (local time when the scheduler
    /// knows the zone, else UTC).
    fn when(&self, secs: u64) -> String {
        let time = match self.lead.scheduler() {
            Some(s) => s.schedules().local_time(secs),
            None => String::new(),
        };
        match time.rsplit_once(' ') {
            Some((_, hm)) => format!("{} {hm}", short_date(secs)),
            None => short_date(secs),
        }
    }

    /// Which of a conversation's sent files are still in its box.
    pub(crate) fn file_links(&self, who: Who<'_>) -> FileLinks {
        let owner = owner_of(who);
        match box_key(&owner) {
            Some(key) => FileLinks {
                key,
                present: self.lead.files().ids(&owner),
            },
            None => FileLinks::none(),
        }
    }

    /// [`Ui::file_panel`] by owner (callers run it on the blocking pool: it reads the box).
    pub(crate) fn panel_of_owner(&self, owner: &Principal) -> PanelData {
        self.panel_of(owner)
    }

    fn panel_of(&self, owner: &Principal) -> PanelData {
        let files = self.lead.files();
        let key = box_key(owner).unwrap_or_default();
        let list = files.list(owner);
        let name = self.name_of(owner);
        let rows = list
            .iter()
            .map(|f| PanelFile {
                href: file_href(&key, &f.id),
                name: f.name.clone(),
                size: human_size(f.size),
                added: self.when(f.added),
                by: match f.by {
                    AddedBy::Owner => "You".into(),
                    AddedBy::Agent => name.clone(),
                },
                delete: format!("/ui/files/{key}/{}/delete", f.id),
            })
            .collect();
        let own = files.override_for(owner);
        let keep = files.keep_for(owner);
        let keep_words = match (keep, own.is_some()) {
            (Keep::Forever, _) => "Kept forever".to_string(),
            (k, true) => format!("Kept for {}", k.words()),
            (k, false) => format!("Kept for {} (default)", k.words()),
        };
        PanelData {
            files: rows,
            usage: usage_words(&list),
            keep: keep_words,
            keep_action: format!("/ui/files/{key}/retention"),
            choices: keep_choices(Some(files.retention().default), own),
            selected: own.map_or_else(|| "default".to_string(), Keep::value),
            whose: name,
        }
    }
}

pub(crate) fn routes() -> Router<Ui> {
    Router::new()
        .route("/ui/files/{key}/{id}", get(serve))
        .route("/ui/files/{key}/{id}/delete", post(delete))
        .route("/ui/files/{key}/retention", post(retention))
}

pub(crate) fn api_routes() -> Router<Ui> {
    Router::new()
        .route("/api/files/{key}", get(api_list))
        .route("/api/files/{key}/{id}", get(serve))
        .route("/api/files/{key}/{id}/delete", post(api_delete))
        .route("/api/files/{key}/retention", post(api_retention))
        .route(
            "/api/settings/files",
            get(api_settings).post(api_set_default),
        )
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, crate::pages::not_found()).into_response()
}

/// A header-safe file name for `Content-Disposition` (ASCII; the rest becomes `_`).
fn ascii_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// One file of a box: sniffed images inline, everything else as a download. Read in chunks
/// from a file opened without following symlinks.
async fn serve(State(ui): State<Ui>, Path((key, id)): Path<(String, String)>) -> Response {
    let Some(owner) = owner_of_key(&key) else {
        return not_found();
    };
    let files = ui.lead.files().clone();
    let opened = tokio::task::spawn_blocking(move || files.open_file(&owner, &id)).await;
    let Ok(Ok((entry, file))) = opened else {
        return not_found();
    };
    let inline = entry.is_image();
    let content_type = if entry.mime == "text/plain" {
        "text/plain; charset=utf-8".to_string()
    } else {
        entry.mime.clone()
    };
    let disposition = if inline {
        "inline".to_string()
    } else {
        format!("attachment; filename=\"{}\"", ascii_name(&entry.name))
    };
    // Chunks are read on the blocking pool; the body streams them.
    let stream = futures::stream::unfold(Some(file), |file| async move {
        let mut file = file?;
        let read = tokio::task::spawn_blocking(move || {
            let mut buf = vec![0u8; 64 * 1024];
            let n = file.read(&mut buf)?;
            buf.truncate(n);
            Ok::<_, std::io::Error>((buf, file))
        })
        .await;
        match read {
            Ok(Ok((buf, _))) if buf.is_empty() => None,
            Ok(Ok((buf, file))) => Some((Ok(Bytes::from(buf)), Some(file))),
            Ok(Err(e)) => Some((Err(e), None)),
            Err(e) => Some((Err(std::io::Error::other(e)), None)),
        }
    });
    let mut res = Body::from_stream(stream).into_response();
    let h = res.headers_mut();
    for (name, value) in [
        (header::CONTENT_TYPE, content_type),
        (header::CONTENT_DISPOSITION, disposition),
        (header::CONTENT_LENGTH, entry.size.to_string()),
        // Agents may rewrite a file under the same id: always ask again.
        (header::CACHE_CONTROL, "private, no-cache".into()),
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; sandbox".into(),
        ),
        (
            header::HeaderName::from_static("cross-origin-resource-policy"),
            "same-origin".into(),
        ),
    ] {
        if let Ok(v) = header::HeaderValue::from_str(&value) {
            h.insert(name, v);
        }
    }
    res
}

/// The panel body after an action (htmx), else back to the chat.
fn panel_reply(ui: &Ui, owner: &Principal, headers: &HeaderMap, error: Option<String>) -> Response {
    if !is_htmx(headers) {
        return Redirect::to(&chat_url(owner)).into_response();
    }
    let data = ui.panel_of(owner);
    let view = data.view();
    html! {
        (file_panel_body(&view))
        (file_count(view.files.len(), true))
        @if let Some(e) = error { (crate::components::notice(&e, crate::components::Tone::Error)) }
    }
    .into_response()
}

async fn delete(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Path((key, id)): Path<(String, String)>,
) -> Response {
    let Some(owner) = owner_of_key(&key) else {
        return not_found();
    };
    let files = ui.lead.files().clone();
    let o = owner.clone();
    let result = tokio::task::spawn_blocking(move || files.delete(&o, &id, "ui")).await;
    let error = match result {
        Ok(Ok(_)) | Ok(Err(FileBoxError::NotFound)) => None,
        Ok(Err(e)) => Some(format!("Couldn't delete it: {e}")),
        Err(e) => Some(format!("Couldn't delete it: {e}")),
    };
    panel_reply(&ui, &owner, &headers, error)
}

#[derive(Deserialize)]
struct KeepForm {
    keep: String,
}

/// `default` → None (use the default); else the parsed retention.
fn parse_choice(value: &str) -> Result<Option<Keep>, FileBoxError> {
    if value.trim() == "default" {
        Ok(None)
    } else {
        Keep::parse(value).map(Some)
    }
}

async fn retention(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Path(key): Path<String>,
    Form(form): Form<KeepForm>,
) -> Response {
    let Some(owner) = owner_of_key(&key) else {
        return not_found();
    };
    let error = parse_choice(&form.keep)
        .and_then(|k| ui.lead.files().set_override(&owner, k, "ui"))
        .err()
        .map(|e| format!("Couldn't save that: {e}"));
    panel_reply(&ui, &owner, &headers, error)
}

fn file_json(key: &str, f: &BoxFile) -> Value {
    json!({
        "id": f.id,
        "name": f.name,
        "type": f.mime,
        "size": f.size,
        "added": f.added,
        "by": match f.by { AddedBy::Owner => "owner", AddedBy::Agent => "agent" },
        "url": file_href(key, &f.id),
    })
}

fn box_json(ui: &Ui, owner: &Principal) -> Value {
    let files = ui.lead.files();
    let key = box_key(owner).unwrap_or_default();
    let list = files.list(owner);
    json!({
        "box": key,
        "path": files.dir(owner).map(|d| d.display().to_string()),
        "files": list.iter().map(|f| file_json(&key, f)).collect::<Vec<_>>(),
        "used": list.iter().map(|f| f.size).sum::<u64>(),
        "keep_days": files.keep_for(owner).as_days(),
        "override": files.override_for(owner).is_some(),
    })
}

fn api_error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({"error": message.into()}))).into_response()
}

async fn api_list(State(ui): State<Ui>, Path(key): Path<String>) -> Response {
    let Some(owner) = owner_of_key(&key) else {
        return api_error(StatusCode::NOT_FOUND, "no such file box");
    };
    let ui2 = ui.clone();
    match tokio::task::spawn_blocking(move || box_json(&ui2, &owner)).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_delete(State(ui): State<Ui>, Path((key, id)): Path<(String, String)>) -> Response {
    let Some(owner) = owner_of_key(&key) else {
        return api_error(StatusCode::NOT_FOUND, "no such file box");
    };
    let files = ui.lead.files().clone();
    match tokio::task::spawn_blocking(move || files.delete(&owner, &id, "ui")).await {
        Ok(Ok(f)) => Json(json!({"deleted": file_json(&key, &f)})).into_response(),
        Ok(Err(FileBoxError::NotFound)) => api_error(StatusCode::NOT_FOUND, "no such file"),
        Ok(Err(e)) => api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `{"keep_days": n|null}` or `{"default": true}`.
#[derive(Deserialize)]
struct RetentionBody {
    #[serde(default)]
    keep_days: Option<u32>,
    #[serde(default)]
    default: bool,
}

fn keep_of(days: Option<u32>) -> Result<Keep, FileBoxError> {
    match days {
        Some(n) => Keep::days(n),
        None => Ok(Keep::Forever),
    }
}

async fn api_retention(
    State(ui): State<Ui>,
    Path(key): Path<String>,
    Json(body): Json<RetentionBody>,
) -> Response {
    let Some(owner) = owner_of_key(&key) else {
        return api_error(StatusCode::NOT_FOUND, "no such file box");
    };
    let keep = if body.default {
        Ok(None)
    } else {
        keep_of(body.keep_days).map(Some)
    };
    match keep.and_then(|k| ui.lead.files().set_override(&owner, k, "ui")) {
        Ok(()) => {
            let files = ui.lead.files();
            Json(json!({
                "box": key,
                "keep_days": files.keep_for(&owner).as_days(),
                "override": files.override_for(&owner).is_some(),
            }))
            .into_response()
        }
        Err(e) => api_error(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()),
    }
}

fn settings_json(ui: &Ui) -> Value {
    let r = ui.lead.files().retention();
    let overrides: serde_json::Map<String, Value> = r
        .overrides
        .iter()
        .map(|(k, v)| (k.clone(), json!(v.as_days())))
        .collect();
    json!({"keep_days": r.default.as_days(), "overrides": overrides})
}

async fn api_settings(State(ui): State<Ui>) -> Json<Value> {
    Json(settings_json(&ui))
}

#[derive(Deserialize)]
struct DefaultBody {
    keep_days: Option<u32>,
}

async fn api_set_default(State(ui): State<Ui>, Json(body): Json<DefaultBody>) -> Response {
    match keep_of(body.keep_days).and_then(|k| ui.lead.files().set_default(k, "ui")) {
        Ok(()) => Json(settings_json(&ui)).into_response(),
        Err(e) => api_error(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()),
    }
}

/// For Settings → Files: each box with its own retention (name, chat link, words).
pub(crate) fn overrides(ui: &Ui) -> Vec<(String, String, String)> {
    ui.lead
        .files()
        .retention()
        .overrides
        .into_iter()
        .filter_map(|(key, keep)| {
            let owner = owner_of_key(&key)?;
            Some((ui.name_of(&owner), chat_url(&owner), keep_label(keep)))
        })
        .collect()
}
