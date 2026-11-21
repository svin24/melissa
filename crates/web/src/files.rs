//! File boxes in the UI and the JSON API: the Files section of each chat's details panel
//! (`crate::details`), serving, adding, sending and deleting files, and how long a box keeps
//! them (see `melissa_agent::files`).
//!
//! `{key}` is the box: `lead` (Melissa's) or an agent's id (the API and the panel's actions also
//! take an agent's name, or `melissa`); `{id}` a file's generated id.
//!
//! UI (same-origin checked like every POST; htmx gets the panel's body, a plain form post a
//! redirect back to the chat):
//! - `GET /ui/files/{key}/{id}`: the file. Sniffed images inline, everything else as a
//!   download; never script (CSP `sandbox`), never embedded by another site.
//! - `POST /ui/files/{key}/{id}/delete`: delete it (audited `files.deleted`, actor `ui`).
//! - `POST /ui/files/{key}/retention` (`keep` = `default`, a number of days or `forever`).
//! - `POST /ui/files/{key}/upload` (multipart `files`, the message limits): add files to the box
//!   from the panel (by the owner, no message).
//! - `POST /ui/files/{key}/{id}/send` (`to` = a box key or agent name, `mode=move` to move;
//!   copies otherwise): [`melissa_agent::files::FileBoxes::transfer`], actor `ui`.
//!
//! API:
//! - `GET /api/files/{key}` → `{"box", "path", "files": [{id, name, type, size, added, by}],
//!   "keep_days": n|null (null: forever), "override": bool, "used": bytes}`; 404 not a box.
//! - `GET /api/files/{key}/{id}` → the file (as above); 404 unknown.
//! - `POST /api/files/{key}/{id}/delete` → the deleted file; 404 unknown.
//! - `POST /api/files/{key}/retention` `{"keep_days": n|null}` or `{"default": true}` → the
//!   box's retention; 422 out of bounds (1–3650 days).
//! - `POST /api/files/{key}/upload` (multipart `files`) → `{"added": [file]}`; 422 refused.
//! - `POST /api/files/{key}/{id}/transfer` `{"to": "<key or name>", "move": false}` → `{"file":
//!   <the new file>, "from", "to", "moved"}`; 404 unknown file or box, 422 the same box, 507 the
//!   receiving box is full.
//! - `GET /api/settings/files` → `{"keep_days": n|null, "overrides": {key: n|null}}`;
//!   `POST /api/settings/files` `{"keep_days": n|null}` → the same.
//!
//! A file's JSON: `{id, name, type, size, added, by: owner|agent|sent, from, url, reference}`.

use std::io::Read;

use axum::{
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Form, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use maud::html;
use melissa_agent::{
    attachments::human_size,
    files::{box_key, owner_of_key, AddedBy, BoxFile, FileBoxError, Keep, Transfer, Transferred},
    tools::permissions::Principal,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    components::{file_count, file_panel_body, FilePanel, PanelFile, Tone},
    is_htmx,
    pages::{file_href, short_date, FileLinks, Who},
    ui::{NewMessage, Ui, MESSAGE_BODY_LIMIT},
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

/// Everything the file panel shows (owned; [`PanelData::view`] borrows it).
pub(crate) struct PanelData {
    files: Vec<PanelFile>,
    keep: String,
    keep_action: String,
    choices: Vec<(String, String)>,
    selected: String,
    whose: String,
    upload: String,
    send_to: Vec<(String, String)>,
    flash: Option<(String, Tone)>,
}

impl PanelData {
    pub(crate) fn view(&self) -> FilePanel<'_> {
        FilePanel {
            files: &self.files,
            keep: &self.keep,
            keep_action: &self.keep_action,
            keep_choices: &self.choices,
            keep_selected: &self.selected,
            whose: &self.whose,
            upload: &self.upload,
            send_to: &self.send_to,
            flash: self.flash.as_ref().map(|(t, tone)| (t.as_str(), *tone)),
        }
    }

    fn with_flash(mut self, flash: Option<(String, Tone)>) -> Self {
        self.flash = flash;
        self
    }
}

/// What "Reference in chat" puts in the message box for a file (the agent's prompt explains
/// it: `melissa_agent::lead` `file_box_words`).
pub(crate) fn reference(name: &str) -> String {
    format!("[file box: {name}]")
}

/// `text` for a query string (RFC 3986 unreserved characters kept, everything else `%XX`).
pub(crate) fn query_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl Ui {
    fn name_of(&self, owner: &Principal) -> String {
        match owner {
            Principal::Lead => self.lead.agents().lead_name(),
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

    /// The boxes a file of `owner`'s can be sent to: Melissa's and every other helper's.
    fn send_targets(&self, owner: &Principal) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if *owner != Principal::Lead {
            out.push(("lead".to_string(), self.lead.agents().lead_name()));
        }
        out.extend(
            self.lead
                .agents()
                .list()
                .into_iter()
                .filter(|a| !matches!(owner, Principal::Agent(id) if *id == a.id))
                .map(|a| (a.id, a.name)),
        );
        out
    }

    fn panel_of(&self, owner: &Principal) -> PanelData {
        let files = self.lead.files();
        let key = box_key(owner).unwrap_or_default();
        let list = files.list(owner);
        let name = self.name_of(owner);
        let chat = chat_url(owner);
        let rows = list
            .iter()
            .map(|f| {
                let reference = reference(&f.name);
                PanelFile {
                    href: file_href(&key, &f.id),
                    name: f.name.clone(),
                    size: human_size(f.size),
                    kind: kind_words(&f.name, &f.mime),
                    image: f.is_image(),
                    added: self.when(f.added),
                    by: match (f.by, f.from.as_deref().and_then(owner_of_key)) {
                        (AddedBy::Owner, _) => "You".into(),
                        (AddedBy::Agent, _) => name.clone(),
                        (AddedBy::Sent, Some(from)) => format!("From {}", self.name_of(&from)),
                        (AddedBy::Sent, None) => "Sent here".into(),
                    },
                    delete: format!("/ui/files/{key}/{}/delete", f.id),
                    send: format!("/ui/files/{key}/{}/send", f.id),
                    reference_href: format!("{chat}?draft={}", query_escape(&reference)),
                    reference,
                }
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
            keep: keep_words,
            keep_action: format!("/ui/files/{key}/retention"),
            choices: keep_choices(Some(files.retention().default), own),
            selected: own.map_or_else(|| "default".to_string(), Keep::value),
            whose: name,
            upload: format!("/ui/files/{key}/upload"),
            send_to: self.send_targets(owner),
            flash: None,
        }
    }

    /// The box named in a request: `lead`, an agent's id, or an agent's name.
    pub(crate) fn box_named(&self, key: &str) -> Option<Principal> {
        if self.lead.agents().registry().is_lead(key) {
            return Some(Principal::Lead);
        }
        if let Some(owner) = owner_of_key(key) {
            // An agent's box only while the agent exists (a deleted one's box is gone).
            return match &owner {
                Principal::Agent(id) if self.lead.agents().get(id).is_none() => None,
                _ => Some(owner),
            };
        }
        self.lead.agents().get(key).map(|a| Principal::Agent(a.id))
    }
}

pub(crate) fn routes() -> Router<Ui> {
    Router::new()
        .route("/ui/files/{key}/{id}", get(serve))
        .route("/ui/files/{key}/{id}/delete", post(delete))
        .route("/ui/files/{key}/retention", post(retention))
        .route(
            "/ui/files/{key}/upload",
            post(upload).layer(DefaultBodyLimit::max(MESSAGE_BODY_LIMIT)),
        )
        .route("/ui/files/{key}/{id}/send", post(send))
}

pub(crate) fn api_routes() -> Router<Ui> {
    Router::new()
        .route("/api/files/{key}", get(api_list))
        .route("/api/files/{key}/{id}", get(serve))
        .route("/api/files/{key}/{id}/delete", post(api_delete))
        .route("/api/files/{key}/retention", post(api_retention))
        .route(
            "/api/files/{key}/upload",
            post(api_upload).layer(DefaultBodyLimit::max(MESSAGE_BODY_LIMIT)),
        )
        .route("/api/files/{key}/{id}/transfer", post(api_transfer))
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
fn panel_reply(
    ui: &Ui,
    owner: &Principal,
    headers: &HeaderMap,
    flash: Option<(String, Tone)>,
) -> Response {
    if !is_htmx(headers) {
        return Redirect::to(&chat_url(owner)).into_response();
    }
    let data = ui.panel_of(owner).with_flash(flash);
    let view = data.view();
    html! {
        (file_panel_body(&view))
        (file_count(view.files.len(), true))
    }
    .into_response()
}

fn failed(text: String) -> Option<(String, Tone)> {
    Some((text, Tone::Error))
}

async fn delete(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Path((key, id)): Path<(String, String)>,
) -> Response {
    let Some(owner) = ui.box_named(&key) else {
        return not_found();
    };
    let files = ui.lead.files().clone();
    let o = owner.clone();
    let result = tokio::task::spawn_blocking(move || files.delete(&o, &id, "ui")).await;
    let flash = match result {
        Ok(Ok(_)) | Ok(Err(FileBoxError::NotFound)) => None,
        Ok(Err(e)) => failed(format!("Couldn't delete it: {e}")),
        Err(e) => failed(format!("Couldn't delete it: {e}")),
    };
    panel_reply(&ui, &owner, &headers, flash)
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
    let Some(owner) = ui.box_named(&key) else {
        return not_found();
    };
    let flash = parse_choice(&form.keep)
        .and_then(|k| ui.lead.files().set_override(&owner, k, "ui"))
        .err()
        .and_then(|e| failed(format!("Couldn't save that: {e}")));
    panel_reply(&ui, &owner, &headers, flash)
}

/// "Added report.pdf." / "Added 3 files."
fn added_words(names: &[String]) -> String {
    match names {
        [one] => format!("Added {one}."),
        many => format!("Added {} files.", many.len()),
    }
}

/// Store uploaded files in `owner`'s box (on the blocking pool).
async fn store(ui: &Ui, owner: &Principal, msg: NewMessage) -> Result<Vec<String>, String> {
    if let Some(e) = msg.refused {
        return Err(e.to_string());
    }
    if msg.files.is_empty() {
        return Err("Choose a file first.".into());
    }
    let files = ui.lead.files().clone();
    let o = owner.clone();
    match tokio::task::spawn_blocking(move || files.store_uploads(&o, msg.files)).await {
        Ok(Ok(added)) => Ok(added.into_iter().map(|a| a.name).collect()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// Files added from the panel (multipart `files`), as if sent in the chat but without a
/// message: the agent finds them in its box.
async fn upload(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Path(key): Path<String>,
    msg: NewMessage,
) -> Response {
    let Some(owner) = ui.box_named(&key) else {
        return not_found();
    };
    let flash = match store(&ui, &owner, msg).await {
        Ok(names) => Some((added_words(&names), Tone::Ok)),
        Err(e) => failed(format!("Couldn't add that: {e}")),
    };
    panel_reply(&ui, &owner, &headers, flash)
}

#[derive(Deserialize)]
struct SendForm {
    to: String,
    /// `move` to move; anything else (or missing) copies.
    #[serde(default)]
    mode: String,
}

/// Send a file to another box (the owner, from the panel): copy unless `move`.
async fn transfer_as_owner(
    ui: &Ui,
    from: &Principal,
    id: String,
    to: &Principal,
    mode: Transfer,
) -> Result<Transferred, FileBoxError> {
    let files = ui.lead.files().clone();
    let (f, t) = (from.clone(), to.clone());
    tokio::task::spawn_blocking(move || files.transfer(&f, &id, &t, mode, "ui"))
        .await
        .map_err(|e| FileBoxError::Io(e.to_string()))?
}

fn transfer_error(ui: &Ui, to: &Principal, e: &FileBoxError) -> String {
    match e {
        FileBoxError::Full(_) => format!("{}'s file box is full.", ui.name_of(to)),
        FileBoxError::NotFound => "That file is gone.".into(),
        other => format!("Couldn't send it: {other}"),
    }
}

async fn send(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Path((key, id)): Path<(String, String)>,
    Form(form): Form<SendForm>,
) -> Response {
    let Some(owner) = ui.box_named(&key) else {
        return not_found();
    };
    let Some(to) = ui.box_named(&form.to) else {
        return panel_reply(&ui, &owner, &headers, failed("Choose who gets it.".into()));
    };
    let mode = if form.mode == "move" {
        Transfer::Move
    } else {
        Transfer::Copy
    };
    let flash = match transfer_as_owner(&ui, &owner, id, &to, mode).await {
        Ok(t) => Some((
            format!(
                "{} {} to {}.",
                match t.mode {
                    Transfer::Copy => "Copied",
                    Transfer::Move => "Moved",
                },
                t.source.name,
                ui.name_of(&to)
            ),
            Tone::Ok,
        )),
        Err(e) => failed(transfer_error(&ui, &to, &e)),
    };
    panel_reply(&ui, &owner, &headers, flash)
}

fn file_json(key: &str, f: &BoxFile) -> Value {
    json!({
        "id": f.id,
        "name": f.name,
        "type": f.mime,
        "size": f.size,
        "added": f.added,
        "by": match f.by {
            AddedBy::Owner => "owner",
            AddedBy::Agent => "agent",
            AddedBy::Sent => "sent",
        },
        "from": f.from,
        "url": file_href(key, &f.id),
        "reference": reference(&f.name),
    })
}

pub(crate) fn box_json(ui: &Ui, owner: &Principal) -> Value {
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
    let Some(owner) = ui.box_named(&key) else {
        return api_error(StatusCode::NOT_FOUND, "no such file box");
    };
    let ui2 = ui.clone();
    match tokio::task::spawn_blocking(move || box_json(&ui2, &owner)).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_delete(State(ui): State<Ui>, Path((key, id)): Path<(String, String)>) -> Response {
    let Some(owner) = ui.box_named(&key) else {
        return api_error(StatusCode::NOT_FOUND, "no such file box");
    };
    let files = ui.lead.files().clone();
    let key = box_key(&owner).unwrap_or_default();
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
    let Some(owner) = ui.box_named(&key) else {
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
                "box": box_key(&owner),
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

/// `POST /api/files/{key}/upload` (multipart `files`) → `{"added": [file]}`.
async fn api_upload(State(ui): State<Ui>, Path(key): Path<String>, msg: NewMessage) -> Response {
    let Some(owner) = ui.box_named(&key) else {
        return api_error(StatusCode::NOT_FOUND, "no such file box");
    };
    let key = box_key(&owner).unwrap_or_default();
    match store(&ui, &owner, msg).await {
        Ok(names) => {
            let files = ui.lead.files().indexed(&owner);
            let added: Vec<Value> = files
                .iter()
                .filter(|f| names.contains(&f.name))
                .map(|f| file_json(&key, f))
                .collect();
            Json(json!({"added": added})).into_response()
        }
        Err(e) => api_error(StatusCode::UNPROCESSABLE_ENTITY, e),
    }
}

/// `{"to": "<box key or agent name>", "move": false}`.
#[derive(Deserialize)]
struct TransferBody {
    to: String,
    #[serde(default, rename = "move")]
    moved: bool,
}

/// `POST /api/files/{key}/{id}/transfer` → `{"file": <the new file>, "from", "to", "moved"}`.
async fn api_transfer(
    State(ui): State<Ui>,
    Path((key, id)): Path<(String, String)>,
    Json(body): Json<TransferBody>,
) -> Response {
    let Some(owner) = ui.box_named(&key) else {
        return api_error(StatusCode::NOT_FOUND, "no such file box");
    };
    let Some(to) = ui.box_named(&body.to) else {
        return api_error(StatusCode::NOT_FOUND, "no such file box to send it to");
    };
    let mode = if body.moved {
        Transfer::Move
    } else {
        Transfer::Copy
    };
    match transfer_as_owner(&ui, &owner, id, &to, mode).await {
        Ok(t) => {
            let (from_key, to_key) = (
                box_key(&owner).unwrap_or_default(),
                box_key(&to).unwrap_or_default(),
            );
            Json(json!({
                "file": file_json(&to_key, &t.file),
                "from": from_key,
                "to": to_key,
                "moved": t.mode == Transfer::Move,
            }))
            .into_response()
        }
        Err(FileBoxError::NotFound) => api_error(StatusCode::NOT_FOUND, "no such file"),
        Err(e @ FileBoxError::Full(_)) => {
            api_error(StatusCode::INSUFFICIENT_STORAGE, e.to_string())
        }
        Err(e @ FileBoxError::Invalid(_)) => {
            api_error(StatusCode::UNPROCESSABLE_ENTITY, e.to_string())
        }
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
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

/// A file's kind in a word for its card: `PNG`, `PDF` (from its name, else its sniffed type).
fn kind_words(name: &str, mime: &str) -> String {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e)
        .filter(|e| (1..=5).contains(&e.len()) && e.chars().all(|c| c.is_ascii_alphanumeric()));
    match ext {
        Some(e) => e.to_ascii_uppercase(),
        None => mime
            .split_once('/')
            .map(|(_, sub)| sub.split(['+', ';']).next().unwrap_or(sub))
            .filter(|s| s.len() <= 5 && *s != "octet-stream")
            .map(str::to_ascii_uppercase)
            .unwrap_or_default(),
    }
}
