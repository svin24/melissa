//! The lead agent's web UI: the chat (home), apps and devices, the approvals list, and a chat
//! with each helper (agent), with live updates over server-sent events.
//!
//! - `GET /` the chat with Melissa (`?draft=` pre-fills the message box), `GET /cluster`,
//!   `GET /approvals`, `GET /agents/{agent}` (a helper's conversation; by id or name).
//! - `GET /ui/stream?since=<seq>&view=chat|approvals` and `GET /ui/agents/{agent}/stream?since=`
//!   (SSE). Each `feed` event carries server-rendered HTML: new entries (the htmx SSE
//!   extension appends them to the list) and out-of-band replacements for entries that
//!   changed (a growing reply, a finished tool call, a decided approval), the composer status
//!   and the waiting count. The event id is the conversation's event sequence number; on
//!   reconnect the browser sends it back as `Last-Event-ID` and the stream continues from
//!   there. The stream re-folds the log up to the cursor first, so its keys match the page.
//! - `GET /ui/strip`, `GET /ui/cluster/apps`: fragments that poll (apps change slowly).
//! - `POST /ui/messages` / `POST /ui/agents/{agent}/messages` (multipart: `text` and any
//!   number of `files`; or a urlencoded `text`), `POST /ui/cancel` /
//!   `POST /ui/agents/{agent}/cancel`, `POST /ui/confirmations/{id}/approve|deny`: the same calls
//!   as the JSON API. htmx gets a fragment; a plain form post gets a redirect.
//! - `POST /ui/backend` / `POST /ui/agents/{agent}/backend` (form `choice`: `<backend>` or
//!   `<backend> <model>`, posted with the composer like Stop): switch the agent from the
//!   next turn. htmx gets the composer status (with the menu, or why it was refused);
//!   a plain form post gets a redirect.
//! - `GET /ui/attachments/{file}`: a file sent with a message (images inline, the rest as
//!   downloads; see `melissa_agent::attachments`).
//! - `POST /ui/new-chat`: a new chat with Melissa (plain form, redirects to `/`);
//!   `GET /chats/{id}`: an archived chat, read-only.
//! - Stream cursors carry the log's epoch (`?epoch=`, SSE ids `<epoch>-<seq>`): a page from
//!   before a daemon restart gets `reload`, and so does every page when a new chat starts.

use std::{convert::Infallible, time::Duration};

use axum::{
    extract::{DefaultBodyLimit, Form, FromRequest, Multipart, Path, Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Redirect, Response,
    },
    routing::{get, post},
    Router,
};
use futures::Stream;
use maud::Markup;
use melissa_agent::{
    attachments::{self, AttachmentError, Upload, MAX_FILES, MAX_FILE_BYTES, MAX_MESSAGE_BYTES},
    lead::{agents::AgentView, events::EventLog, Lead, LeadError, LeadStatus},
    tools::Verdict,
};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    agents,
    components::{composer_status, AgentMenu, ApprovalState},
    feed::{approval_words, asked_by, backend_label, Approval, Change, Feed, Lookup},
    is_htmx, pages,
    pages::{ChatView, Frame, View, Who},
    state::ClusterCache,
};

#[derive(Clone)]
pub(crate) struct Ui {
    pub(crate) lead: Lead,
    pub(crate) cluster: ClusterCache,
    pub(crate) settings: crate::settings::Settings,
    /// One-shot agent tasks (Settings → Developer), to tell whether Melissa is busy.
    pub(crate) board: Option<melissa_agent::TaskBoard>,
}

impl Ui {
    pub(crate) fn new(
        lead: Lead,
        cluster: ClusterCache,
        settings: crate::settings::Settings,
    ) -> Self {
        Self {
            lead,
            cluster,
            settings,
            board: None,
        }
    }

    pub(crate) fn with_board(mut self, board: melissa_agent::TaskBoard) -> Self {
        self.board = Some(board);
        self
    }

    /// Something is running that a restart would cut off: a chat turn (Melissa's or a
    /// helper's), a helper's job or an agent task.
    pub(crate) fn busy(&self) -> bool {
        self.lead.status().turn.is_some()
            || self
                .lead
                .agents()
                .list()
                .iter()
                .any(|b| b.status == "working")
            || self.board.as_ref().is_some_and(|b| {
                b.list()
                    .iter()
                    .any(|t| t.status == melissa_agent::TaskStatus::Running)
            })
    }
}

pub(crate) fn routes(ui: Ui) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/cluster", get(cluster_page))
        .route("/approvals", get(approvals_page))
        .route("/agents/{agent}", get(agent_page))
        .route("/ui/stream", get(lead_stream))
        .route("/ui/agents/{agent}/stream", get(agent_stream))
        .route("/ui/strip", get(strip))
        .route("/ui/cluster/apps", get(cluster_apps))
        .route(
            "/ui/messages",
            post(lead_message).layer(DefaultBodyLimit::max(MESSAGE_BODY_LIMIT)),
        )
        .route(
            "/ui/agents/{agent}/messages",
            post(agent_message).layer(DefaultBodyLimit::max(MESSAGE_BODY_LIMIT)),
        )
        .route("/ui/attachments/{file}", get(attachment))
        .route("/ui/cancel", post(lead_cancel))
        .route("/ui/agents/{agent}/cancel", post(agent_cancel))
        .route("/ui/confirmations/{id}/{verdict}", post(resolve))
        .route("/ui/new-chat", post(new_chat))
        .route("/ui/backend", post(lead_backend))
        .route("/ui/agents/{agent}/backend", post(agent_backend))
        .route("/chats/{id}", get(past_chat_page))
        .merge(crate::settings::ui_routes())
        .with_state(ui)
}

impl Lookup for Ui {
    fn confirmation_args(&self, id: u64) -> Option<Value> {
        self.lead.confirmations().get(id).map(|c| c.args)
    }

    fn agent_name(&self, agent: &str) -> Option<String> {
        self.lead.agents().get(agent).map(|b| b.name)
    }
}

impl Ui {
    /// A conversation's event log.
    fn log(&self, who: Who<'_>) -> Result<EventLog, LeadError> {
        match who {
            Who::Lead => Ok(self.lead.events().clone()),
            Who::Agent(id) => self.lead.agents().events_of(id),
        }
    }

    fn status(&self, who: Who<'_>) -> Option<LeadStatus> {
        match who {
            Who::Lead => Some(self.lead.status()),
            Who::Agent(id) => self.lead.agents().conversation_status(id).ok(),
        }
    }

    /// A conversation folded from every event in its log.
    fn feed(&self, who: Who<'_>, log: &EventLog) -> Feed {
        let page = log.since(0, EventLog::CAPACITY);
        Feed::for_helper(matches!(who, Who::Agent(_)))
            .with_steps(self.settings.prefs().show_steps)
            .fold(&page.events, self)
    }

    fn backend(&self, who: Who<'_>, feed: &Feed) -> Option<String> {
        self.status(who)
            .and_then(|s| s.backend)
            .map(|b| backend_label(&b).to_string())
            .or_else(|| feed.backend().map(str::to_string))
    }

    /// The agent menu of a conversation (None if it cannot be read).
    fn agents(&self, who: Who<'_>, error: Option<String>) -> Option<AgentMenu> {
        let (views, action) = match who {
            Who::Lead => (self.lead.backends(), "/ui/backend".to_string()),
            Who::Agent(id) => (
                self.lead.agents().backends_of(id).ok()?,
                format!("/ui/agents/{id}/backend"),
            ),
        };
        let host = self.settings.host();
        (!views.is_empty()).then(|| agents::menu(&views, action, error, host))
    }

    /// Name the helper that asked for confirmation `task` in its card (views that show every
    /// helper's: Melissa's chat, Waiting for you).
    fn label_asker(&self, a: &mut Approval, task: melissa_agent::TaskId) {
        if let Some(agent) = self.lead.agents().agent_of_task(task) {
            asked_by(a, &agent, self);
        }
    }

    fn waiting(&self) -> usize {
        self.lead.confirmations().pending().len()
    }

    /// The agent for a path segment (id or name).
    fn agent(&self, agent: &str) -> Option<AgentView> {
        self.lead.agents().get(agent)
    }

    /// Render a page with the rail.
    pub(crate) async fn framed(&self, page: impl FnOnce(&Frame<'_>) -> Markup) -> Markup {
        self.framed_at(None, page).await
    }

    /// Render a page with the rail for a visitor who came in at `host` (app links).
    async fn framed_at(
        &self,
        host: Option<&str>,
        page: impl FnOnce(&Frame<'_>) -> Markup,
    ) -> Markup {
        let snap = self.cluster.get().await;
        let agents = self.lead.agents().list();
        let past = self.lead.past_chats();
        page(&Frame {
            waiting: self.waiting(),
            snap: &snap,
            agents: &agents,
            past: &past,
            host,
        })
    }

    /// A chat page (home or a helper's).
    async fn chat_page(
        &self,
        who: Who<'_>,
        agent: Option<&AgentView>,
        draft: &str,
        error: Option<&str>,
    ) -> Result<Markup, LeadError> {
        let log = self.log(who)?;
        let feed = self.feed(who, &log);
        let backend = self.backend(who, &feed);
        // Learn the models of usable agents that never had a chat (rate-limited, no prompt);
        // they show in the menu from the next render.
        self.lead.refresh_models_soon();
        let agents = self.agents(who, None);
        let view = ChatView {
            who,
            epoch: log.epoch(),
            feed: &feed,
            backend: backend.as_deref(),
            agents: agents.as_ref(),
            busy: self.status(who).is_some_and(|s| s.turn.is_some()),
            draft,
            error,
        };
        Ok(self
            .framed(|f| match agent {
                Some(agent) => pages::agent_chat(f, agent, &view),
                None => pages::home(f, &view),
            })
            .await)
    }
}

#[derive(Deserialize)]
struct HomeQuery {
    #[serde(default)]
    draft: String,
}

async fn home(State(ui): State<Ui>, Query(q): Query<HomeQuery>) -> Response {
    // A draft is only a suggestion for the box; it is never sent by itself.
    let draft: String = q.draft.chars().take(200).collect();
    match ui.chat_page(Who::Lead, None, &draft, None).await {
        Ok(page) => page.into_response(),
        Err(e) => error_page(e),
    }
}

async fn agent_page(State(ui): State<Ui>, Path(agent): Path<String>) -> Response {
    let Some(agent) = ui.agent(&agent) else {
        return (StatusCode::NOT_FOUND, pages::not_found()).into_response();
    };
    match ui
        .chat_page(Who::Agent(&agent.id), Some(&agent), "", None)
        .await
    {
        Ok(page) => page.into_response(),
        Err(e) => error_page(e),
    }
}

fn error_page(e: LeadError) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        pages::error_page("Couldn't open the conversation", &e.to_string()),
    )
        .into_response()
}

/// The `Host` header, for app links built from the address the visitor used.
pub(crate) fn host(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::HOST).and_then(|h| h.to_str().ok())
}

async fn cluster_page(State(ui): State<Ui>, headers: HeaderMap) -> Markup {
    ui.framed_at(host(&headers), pages::cluster).await
}

async fn cluster_apps(State(ui): State<Ui>, headers: HeaderMap) -> Markup {
    pages::cluster_apps(&*ui.cluster.get().await, host(&headers))
}

async fn strip(State(ui): State<Ui>) -> Markup {
    pages::strip(&*ui.cluster.get().await)
}

async fn approvals_page(State(ui): State<Ui>) -> Markup {
    let since = ui.lead.events().last_seq();
    let list: Vec<_> = ui
        .lead
        .confirmations()
        .pending()
        .into_iter()
        .map(|c| {
            let mut a = approval_words(c.id, &c.tool, &c.reason, Some(&c.args), &ui);
            ui.label_asker(&mut a, c.task);
            a
        })
        .collect();
    let epoch = ui.lead.events().epoch();
    ui.framed(|f| pages::approvals(f, &list, since, epoch))
        .await
}

#[derive(Deserialize)]
struct StreamQuery {
    #[serde(default)]
    since: u64,
    #[serde(default)]
    view: Option<String>,
    /// The epoch of the log the page was rendered from.
    #[serde(default)]
    epoch: Option<u64>,
}

/// How long one wait for new events lasts before it is started again.
const STREAM_WAIT: Duration = Duration::from_secs(25);

struct StreamState {
    ui: Ui,
    log: EventLog,
    /// The helper whose conversation this is (None: the lead agent's).
    agent: Option<String>,
    feed: Feed,
    cursor: u64,
    view: View,
    reload: bool,
}

impl StreamState {
    fn who(&self) -> Who<'_> {
        match &self.agent {
            Some(id) => Who::Agent(id),
            None => Who::Lead,
        }
    }
}

async fn lead_stream(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Query(q): Query<StreamQuery>,
) -> Response {
    let view = match q.view.as_deref() {
        Some("approvals") => View::Approvals,
        _ => View::Chat,
    };
    let log = ui.lead.events().clone();
    let at = cursor(&headers, &q, log.epoch());
    stream(ui, log, None, view, at).into_response()
}

async fn agent_stream(
    State(ui): State<Ui>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    Query(q): Query<StreamQuery>,
) -> Response {
    let Some(agent) = ui.agent(&agent) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let log = match ui.log(Who::Agent(&agent.id)) {
        Ok(log) => log,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let at = cursor(&headers, &q, log.epoch());
    stream(ui, log, Some(agent.id), View::Chat, at).into_response()
}

/// Where a stream starts: a reconnecting browser says where it got to (`<epoch>-<seq>`).
/// None: the page came from another log (before a daemon restart), so it must reload.
fn cursor(headers: &HeaderMap, q: &StreamQuery, epoch: u64) -> Option<u64> {
    let last = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| match v.split_once('-') {
            Some((e, s)) => Some((Some(e.parse::<u64>().ok()?), s.parse::<u64>().ok()?)),
            // Ids from before epochs.
            None => Some((None, v.parse::<u64>().ok()?)),
        });
    match (last, q.epoch) {
        (Some((Some(e), seq)), _) => (e == epoch).then_some(seq),
        (Some((None, seq)), _) => Some(seq),
        (None, Some(e)) => (e == epoch).then_some(q.since),
        // Pages from before epochs: only the sequence number to go by.
        (None, None) => Some(q.since),
    }
}

fn stream(
    ui: Ui,
    log: EventLog,
    agent: Option<String>,
    view: View,
    cursor: Option<u64>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // The page is from another log or newer than this one (the daemon restarted): its DOM
    // can't be updated.
    let reload = cursor.is_none_or(|c| c > log.last_seq());
    let cursor = cursor.unwrap_or(0);
    let helper = agent.is_some();
    let feed = {
        let page = log.since(0, EventLog::CAPACITY);
        Feed::for_helper(helper)
            .with_steps(ui.settings.prefs().show_steps)
            .fold(page.events.iter().filter(|r| r.seq <= cursor), &ui)
    };
    let state = StreamState {
        ui,
        log,
        agent,
        feed,
        cursor,
        view,
        reload,
    };
    let events = futures::stream::unfold(state, |mut st| async move {
        if st.reload {
            if st.cursor == u64::MAX {
                return None;
            }
            st.cursor = u64::MAX;
            return Some((Ok(Event::default().event("reload").data("")), st));
        }
        loop {
            let page = st.log.wait_since(st.cursor, 200, STREAM_WAIT).await;
            if page.events.is_empty() {
                continue;
            }
            let mut changes = Vec::new();
            for rec in &page.events {
                changes.extend(st.feed.apply(rec, &st.ui));
            }
            st.cursor = page.next;
            if changes.contains(&Change::Reset) {
                // A new chat: the page starts over.
                st.reload = true;
                st.cursor = u64::MAX;
                return Some((Ok(Event::default().event("reload").data("")), st));
            }
            let backend = st.ui.backend(st.who(), &st.feed);
            let agents = changes
                .contains(&Change::Status)
                .then(|| st.ui.agents(st.who(), None))
                .flatten();
            let html = pages::stream_update(
                &st.feed,
                &changes,
                st.view,
                st.who(),
                backend.as_deref(),
                agents.as_ref(),
                st.ui.waiting(),
            );
            if html.is_empty() {
                continue;
            }
            let event = Event::default()
                .event("feed")
                .id(format!("{}-{}", st.log.epoch(), st.cursor))
                .data(html);
            return Some((Ok(event), st));
        }
    });
    Sse::new(events).keep_alive(KeepAlive::default())
}

/// Largest message request body: the files' limit plus room for the text and the form's
/// framing. Bigger bodies are cut off while they stream in.
const MESSAGE_BODY_LIMIT: usize = MAX_MESSAGE_BYTES + 1024 * 1024;

#[derive(Deserialize)]
struct TextOnly {
    text: String,
}

/// A message from the composer: its text and files. The composer posts
/// `multipart/form-data` (fields `text` and `files`); a urlencoded form has text only.
struct NewMessage {
    text: String,
    files: Vec<Upload>,
    /// Why the files were refused while reading them (too large, too many, cut off).
    refused: Option<LeadError>,
}

impl NewMessage {
    fn had_files(&self) -> bool {
        !self.files.is_empty() || self.refused.is_some()
    }
}

impl<S: Send + Sync> FromRequest<S> for NewMessage {
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Response> {
        let multipart = req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("multipart/form-data"));
        if !multipart {
            let Form(form) = Form::<TextOnly>::from_request(req, state)
                .await
                .map_err(IntoResponse::into_response)?;
            return Ok(Self {
                text: form.text,
                files: Vec::new(),
                refused: None,
            });
        }
        let parts = Multipart::from_request(req, state)
            .await
            .map_err(IntoResponse::into_response)?;
        Ok(read_message(parts).await)
    }
}

/// Read a multipart message. Files are checked while they stream in, so an oversized one
/// is refused without holding more than the limit in memory.
async fn read_message(mut parts: Multipart) -> NewMessage {
    let mut msg = NewMessage {
        text: String::new(),
        files: Vec::new(),
        refused: None,
    };
    let mut total = 0usize;
    loop {
        let mut field = match parts.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                msg.refused = Some(cut_off(&e));
                break;
            }
        };
        match field.name() {
            Some("text") => match field.text().await {
                Ok(t) => msg.text = t,
                Err(e) => {
                    msg.refused = Some(cut_off(&e));
                    break;
                }
            },
            Some("files") => {
                let name = field.file_name().unwrap_or_default().to_string();
                let mut bytes = Vec::new();
                let mut failed = None;
                loop {
                    match field.chunk().await {
                        Ok(Some(chunk)) => {
                            bytes.extend_from_slice(&chunk);
                            if bytes.len() > MAX_FILE_BYTES {
                                failed = Some(
                                    AttachmentError::FileTooLarge {
                                        name: attachments::safe_name(&name),
                                    }
                                    .into(),
                                );
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            failed = Some(cut_off(&e));
                            break;
                        }
                    }
                }
                if let Some(e) = failed {
                    msg.refused = Some(e);
                    break;
                }
                // A file input with nothing chosen sends one empty, unnamed part.
                if name.is_empty() && bytes.is_empty() {
                    continue;
                }
                total += bytes.len();
                if msg.files.len() == MAX_FILES {
                    msg.refused = Some(AttachmentError::TooMany.into());
                    break;
                }
                if total > MAX_MESSAGE_BYTES {
                    msg.refused = Some(AttachmentError::MessageTooLarge.into());
                    break;
                }
                msg.files.push(Upload { name, bytes });
            }
            _ => {}
        }
    }
    msg
}

/// A multipart body that could not be read: almost always the size limit.
fn cut_off(e: &axum::extract::multipart::MultipartError) -> LeadError {
    tracing::debug!("reading a message upload: {e}");
    if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
        AttachmentError::MessageTooLarge.into()
    } else {
        LeadError::Invalid("the upload was cut off".into())
    }
}

async fn lead_message(State(ui): State<Ui>, headers: HeaderMap, msg: NewMessage) -> Response {
    let had_files = msg.had_files();
    let result = match msg.refused {
        Some(e) => Err(e),
        None if msg.files.is_empty() => ui.lead.send(&msg.text).map(|_| ()),
        None => {
            // Storing the files writes to disk: off the async workers.
            let (lead, text, files) = (ui.lead.clone(), msg.text.clone(), msg.files);
            tokio::task::spawn_blocking(move || lead.send_with(&text, files).map(|_| ()))
                .await
                .unwrap_or_else(|e| Err(LeadError::Failed(e.to_string())))
        }
    };
    message_reply(&ui, Who::Lead, None, &headers, &msg.text, had_files, result).await
}

async fn agent_message(
    State(ui): State<Ui>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    msg: NewMessage,
) -> Response {
    let Some(agent) = ui.agent(&agent) else {
        return (StatusCode::NOT_FOUND, pages::not_found()).into_response();
    };
    let had_files = msg.had_files();
    let result = match msg.refused {
        Some(e) => Err(e),
        None => {
            let (lead, id, text, files) = (
                ui.lead.clone(),
                agent.id.clone(),
                msg.text.clone(),
                msg.files,
            );
            tokio::task::spawn_blocking(move || {
                lead.agents().user_send_with(&id, &text, files).map(|_| ())
            })
            .await
            .unwrap_or_else(|e| Err(LeadError::Failed(e.to_string())))
        }
    };
    message_reply(
        &ui,
        Who::Agent(&agent.id),
        Some(&agent),
        &headers,
        &msg.text,
        had_files,
        result,
    )
    .await
}

/// A file sent with a message (any conversation's; names are unguessable). Images are shown
/// inline, everything else is a download. The file may not run script or be embedded by
/// another site.
async fn attachment(State(ui): State<Ui>, Path(file): Path<String>) -> Response {
    let not_found = || (StatusCode::NOT_FOUND, pages::not_found()).into_response();
    let Some(path) = ui.lead.attachments().path(&file) else {
        return not_found();
    };
    let Ok(bytes) = tokio::fs::read(path).await else {
        return not_found();
    };
    let (content_type, inline) = attachments::content_type(&file);
    let disposition = if inline { "inline" } else { "attachment" };
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CONTENT_DISPOSITION, disposition),
            (
                header::CACHE_CONTROL,
                "private, max-age=31536000, immutable",
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; sandbox",
            ),
            (
                header::HeaderName::from_static("cross-origin-resource-policy"),
                "same-origin",
            ),
        ],
        bytes,
    )
        .into_response()
}

/// The answer to a sent message: the composer (cleared, or with the text and why it wasn't
/// sent) for htmx; a redirect or the page with the error otherwise.
async fn message_reply(
    ui: &Ui,
    who: Who<'_>,
    agent: Option<&AgentView>,
    headers: &HeaderMap,
    text: &str,
    had_files: bool,
    result: Result<(), LeadError>,
) -> Response {
    let mut error = match &result {
        Ok(()) => None,
        Err(LeadError::Busy) => {
            Some("Still working on your last message. Wait for it, or press Stop.".to_string())
        }
        Err(LeadError::Empty) => Some("Type a message first.".to_string()),
        Err(e) => Some(format!("Couldn't send that: {e}.")),
    };
    // A plain form post can't keep its files; htmx (melissa.js) keeps them for a retry.
    if had_files && !is_htmx(headers) {
        if let Some(e) = error.as_mut() {
            e.push_str(" Attach the files again.");
        }
    }
    // Keep the text on failure so nothing typed is lost.
    let draft = if error.is_some() { text } else { "" };
    if is_htmx(headers) {
        let status = ui.status(who);
        let busy = status.as_ref().is_some_and(|s| s.turn.is_some());
        let backend = status
            .as_ref()
            .and_then(|s| s.backend.as_deref())
            .map(backend_label);
        let feed = Feed::new();
        let agents = ui.agents(who, None);
        return pages::composer_box(&ChatView {
            who,
            epoch: 0,
            feed: &feed,
            backend,
            agents: agents.as_ref(),
            busy,
            draft,
            error: error.as_deref(),
        })
        .into_response();
    }
    let back = match agent {
        Some(b) => pages::agent_href(&b.id),
        None => "/".to_string(),
    };
    match error {
        None => Redirect::to(&back).into_response(),
        Some(e) => match ui.chat_page(who, agent, draft, Some(&e)).await {
            Ok(page) => (StatusCode::UNPROCESSABLE_ENTITY, page).into_response(),
            Err(e) => error_page(e),
        },
    }
}

#[derive(Deserialize)]
struct BackendForm {
    choice: String,
}

async fn lead_backend(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Form(form): Form<BackendForm>,
) -> Response {
    let (backend, model) = agents::parse_choice(&form.choice);
    let result = ui.lead.use_backend(backend, model).map(|_| ());
    backend_reply(&ui, Who::Lead, None, &headers, backend, result).await
}

async fn agent_backend(
    State(ui): State<Ui>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    Form(form): Form<BackendForm>,
) -> Response {
    let Some(agent) = ui.agent(&agent) else {
        return (StatusCode::NOT_FOUND, pages::not_found()).into_response();
    };
    let (backend, model) = agents::parse_choice(&form.choice);
    let result = ui
        .lead
        .agents()
        .user_use_backend(&agent.id, backend, model)
        .map(|_| ());
    backend_reply(
        &ui,
        Who::Agent(&agent.id),
        Some(&agent),
        &headers,
        backend,
        result,
    )
    .await
}

/// The answer to a switch: the composer status with the agent menu (and why a switch was
/// refused) for htmx; the chat page otherwise. The feed line comes with the event stream.
async fn backend_reply(
    ui: &Ui,
    who: Who<'_>,
    agent: Option<&AgentView>,
    headers: &HeaderMap,
    backend: &str,
    result: Result<(), LeadError>,
) -> Response {
    let error = match result {
        Ok(()) => None,
        Err(LeadError::Unavailable(_)) => Some(format!(
            "{} isn't ready on this device yet.",
            backend_label(backend)
        )),
        Err(LeadError::Failed(e)) => Some(format!("Couldn't switch: {e}.")),
        Err(_) => Some("Couldn't switch: that agent or model isn't available here.".into()),
    };
    if is_htmx(headers) {
        let status = ui.status(who);
        let busy = status.as_ref().is_some_and(|s| s.turn.is_some());
        let label = status
            .as_ref()
            .and_then(|s| s.backend.as_deref())
            .map(backend_label);
        let agents = ui.agents(who, error);
        return composer_status(&who.cancel_url(), label, busy, agents.as_ref()).into_response();
    }
    let back = match agent {
        Some(b) => pages::agent_href(&b.id),
        None => "/".to_string(),
    };
    match error {
        None => Redirect::to(&back).into_response(),
        Some(e) => match ui.chat_page(who, agent, "", Some(&e)).await {
            Ok(page) => (StatusCode::UNPROCESSABLE_ENTITY, page).into_response(),
            Err(e) => error_page(e),
        },
    }
}

async fn lead_cancel(State(ui): State<Ui>, headers: HeaderMap) -> Response {
    ui.lead.cancel().await;
    cancelled(&headers, "/")
}

async fn agent_cancel(
    State(ui): State<Ui>,
    Path(agent): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(agent) = ui.agent(&agent) else {
        return (StatusCode::NOT_FOUND, pages::not_found()).into_response();
    };
    // Nothing running is fine: the button may be stale.
    let _ = ui.lead.agents().user_cancel(&agent.id).await;
    cancelled(&headers, &pages::agent_href(&agent.id))
}

fn cancelled(headers: &HeaderMap, back: &str) -> Response {
    if is_htmx(headers) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        Redirect::to(back).into_response()
    }
}

async fn resolve(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Path((id, verdict)): Path<(u64, String)>,
) -> Response {
    let verdict = match verdict.as_str() {
        "approve" => Verdict::Approved,
        "deny" => Verdict::Denied,
        _ => return (StatusCode::NOT_FOUND, pages::not_found()).into_response(),
    };
    let resolved = ui.lead.resolve_confirmation(id, verdict);
    if !is_htmx(&headers) {
        return Redirect::to("/approvals").into_response();
    }
    // htmx swaps only 2xx answers, so "already gone" is a 200 with a note.
    match resolved {
        Some(c) => {
            let mut a = approval_words(c.id, &c.tool, &c.reason, Some(&c.args), &ui);
            // The card keeps the words of the page it is on (a helper's chat has no label).
            if !from_helper_chat(&headers) {
                ui.label_asker(&mut a, c.task);
            }
            a.state = match verdict {
                Verdict::Approved => ApprovalState::Approved,
                Verdict::Denied => ApprovalState::Denied,
            };
            let target = pages::entry_id(&format!("c{id}"));
            pages::approval_content(&a, &target).into_response()
        }
        None => pages::approval_gone().into_response(),
    }
}

/// The request came from a helper's chat page (htmx sends the page's URL).
fn from_helper_chat(headers: &HeaderMap) -> bool {
    let Some(url) = headers.get("hx-current-url").and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let path = rest.find('/').map_or("", |i| &rest[i..]);
    let path = path.split(['?', '#']).next().unwrap_or("");
    path.starts_with("/agents/")
}

async fn new_chat(State(ui): State<Ui>) -> Response {
    match ui.lead.new_chat().await {
        Ok(_) => Redirect::to("/").into_response(),
        Err(e) => (
            StatusCode::CONFLICT,
            pages::error_page("Couldn't start a new chat", &e.to_string()),
        )
            .into_response(),
    }
}

async fn past_chat_page(State(ui): State<Ui>, Path(id): Path<u64>) -> Response {
    let (Some(chat), Some(events)) = (
        ui.lead.past_chats().into_iter().find(|c| c.id == id),
        ui.lead.past_chat(id),
    ) else {
        return (StatusCode::NOT_FOUND, pages::not_found()).into_response();
    };
    let mut feed = Feed::archived()
        .with_steps(ui.settings.prefs().show_steps)
        .fold(&events, &ui);
    feed.settle();
    ui.framed(|f| pages::past_chat(f, &chat, &feed))
        .await
        .into_response()
}
