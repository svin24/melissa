//! The Queen's web UI: the chat (home), apps and devices, the approvals list, and a chat
//! with each helper (bee), with live updates over server-sent events.
//!
//! - `GET /` the chat with Melissa (`?draft=` pre-fills the message box), `GET /hive`,
//!   `GET /approvals`, `GET /bees/{bee}` (a helper's conversation; by id or name).
//! - `GET /ui/stream?since=<seq>&view=chat|approvals` and `GET /ui/bees/{bee}/stream?since=`
//!   (SSE). Each `feed` event carries server-rendered HTML: new entries (the htmx SSE
//!   extension appends them to the list) and out-of-band replacements for entries that
//!   changed (a growing reply, a finished tool call, a decided approval), the composer status
//!   and the waiting count. The event id is the conversation's event sequence number; on
//!   reconnect the browser sends it back as `Last-Event-ID` and the stream continues from
//!   there. The stream re-folds the log up to the cursor first, so its keys match the page.
//! - `GET /ui/strip`, `GET /ui/hive/apps`: fragments that poll (apps change slowly).
//! - `POST /ui/messages` / `POST /ui/bees/{bee}/messages` (form `text`), `POST /ui/cancel` /
//!   `POST /ui/bees/{bee}/cancel`, `POST /ui/confirmations/{id}/approve|deny`: the same calls
//!   as the JSON API. htmx gets a fragment; a plain form post gets a redirect.
//! - `POST /ui/backend` / `POST /ui/bees/{bee}/backend` (form `choice`: `<backend>` or
//!   `<backend> <model>`, posted with the composer like Stop): switch the agent from the
//!   next turn. htmx gets the composer status (with the menu, or why it was refused);
//!   a plain form post gets a redirect.
//! - `POST /ui/new-chat`: a new chat with Melissa (plain form, redirects to `/`);
//!   `GET /chats/{id}`: an archived chat, read-only.
//! - Stream cursors carry the log's epoch (`?epoch=`, SSE ids `<epoch>-<seq>`): a page from
//!   before a daemon restart gets `reload`, and so does every page when a new chat starts.

use std::{convert::Infallible, time::Duration};

use axum::{
    extract::{Form, Path, Query, State},
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
    queen::{bees::BeeView, events::EventLog, Queen, QueenError, QueenStatus},
    tools::Verdict,
};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    agents,
    components::{composer_status, AgentMenu, ApprovalState},
    feed::{approval_words, backend_label, Change, Feed, Lookup},
    is_htmx, pages,
    pages::{ChatView, Frame, View, Who},
    state::HiveCache,
};

#[derive(Clone)]
pub(crate) struct Ui {
    queen: Queen,
    hive: HiveCache,
}

pub(crate) fn routes(queen: Queen, hive: HiveCache) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/hive", get(hive_page))
        .route("/approvals", get(approvals_page))
        .route("/bees/{bee}", get(bee_page))
        .route("/ui/stream", get(queen_stream))
        .route("/ui/bees/{bee}/stream", get(bee_stream))
        .route("/ui/strip", get(strip))
        .route("/ui/hive/apps", get(hive_apps))
        .route("/ui/messages", post(queen_message))
        .route("/ui/bees/{bee}/messages", post(bee_message))
        .route("/ui/cancel", post(queen_cancel))
        .route("/ui/bees/{bee}/cancel", post(bee_cancel))
        .route("/ui/confirmations/{id}/{verdict}", post(resolve))
        .route("/ui/new-chat", post(new_chat))
        .route("/ui/backend", post(queen_backend))
        .route("/ui/bees/{bee}/backend", post(bee_backend))
        .route("/chats/{id}", get(past_chat_page))
        .with_state(Ui { queen, hive })
}

impl Lookup for Ui {
    fn confirmation_args(&self, id: u64) -> Option<Value> {
        self.queen.confirmations().get(id).map(|c| c.args)
    }

    fn bee_name(&self, bee: &str) -> Option<String> {
        self.queen.bees().get(bee).map(|b| b.name)
    }
}

impl Ui {
    /// A conversation's event log.
    fn log(&self, who: Who<'_>) -> Result<EventLog, QueenError> {
        match who {
            Who::Queen => Ok(self.queen.events().clone()),
            Who::Bee(id) => self.queen.bees().events_of(id),
        }
    }

    fn status(&self, who: Who<'_>) -> Option<QueenStatus> {
        match who {
            Who::Queen => Some(self.queen.status()),
            Who::Bee(id) => self.queen.bees().conversation_status(id).ok(),
        }
    }

    /// A conversation folded from every event in its log.
    fn feed(&self, who: Who<'_>, log: &EventLog) -> Feed {
        let page = log.since(0, EventLog::CAPACITY);
        Feed::for_helper(matches!(who, Who::Bee(_))).fold(&page.events, self)
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
            Who::Queen => (self.queen.backends(), "/ui/backend".to_string()),
            Who::Bee(id) => (
                self.queen.bees().backends_of(id).ok()?,
                format!("/ui/bees/{id}/backend"),
            ),
        };
        (!views.is_empty()).then(|| agents::menu(&views, action, error))
    }

    fn waiting(&self) -> usize {
        self.queen.confirmations().pending().len()
    }

    /// The bee for a path segment (id or name).
    fn bee(&self, bee: &str) -> Option<BeeView> {
        self.queen.bees().get(bee)
    }

    /// Render a page with the rail.
    async fn framed(&self, page: impl FnOnce(&Frame<'_>) -> Markup) -> Markup {
        self.framed_at(None, page).await
    }

    /// Render a page with the rail for a visitor who came in at `host` (app links).
    async fn framed_at(
        &self,
        host: Option<&str>,
        page: impl FnOnce(&Frame<'_>) -> Markup,
    ) -> Markup {
        let snap = self.hive.get().await;
        let bees = self.queen.bees().list();
        let past = self.queen.past_chats();
        page(&Frame {
            waiting: self.waiting(),
            snap: &snap,
            bees: &bees,
            past: &past,
            host,
        })
    }

    /// A chat page (home or a helper's).
    async fn chat_page(
        &self,
        who: Who<'_>,
        bee: Option<&BeeView>,
        draft: &str,
        error: Option<&str>,
    ) -> Result<Markup, QueenError> {
        let log = self.log(who)?;
        let feed = self.feed(who, &log);
        let backend = self.backend(who, &feed);
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
            .framed(|f| match bee {
                Some(bee) => pages::bee_chat(f, bee, &view),
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
    match ui.chat_page(Who::Queen, None, &draft, None).await {
        Ok(page) => page.into_response(),
        Err(e) => error_page(e),
    }
}

async fn bee_page(State(ui): State<Ui>, Path(bee): Path<String>) -> Response {
    let Some(bee) = ui.bee(&bee) else {
        return (StatusCode::NOT_FOUND, pages::not_found()).into_response();
    };
    match ui.chat_page(Who::Bee(&bee.id), Some(&bee), "", None).await {
        Ok(page) => page.into_response(),
        Err(e) => error_page(e),
    }
}

fn error_page(e: QueenError) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        pages::error_page("Couldn't open the conversation", &e.to_string()),
    )
        .into_response()
}

/// The `Host` header, for app links built from the address the visitor used.
fn host(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::HOST).and_then(|h| h.to_str().ok())
}

async fn hive_page(State(ui): State<Ui>, headers: HeaderMap) -> Markup {
    ui.framed_at(host(&headers), pages::hive).await
}

async fn hive_apps(State(ui): State<Ui>, headers: HeaderMap) -> Markup {
    pages::hive_apps(&*ui.hive.get().await, host(&headers))
}

async fn strip(State(ui): State<Ui>) -> Markup {
    pages::strip(&*ui.hive.get().await)
}

async fn approvals_page(State(ui): State<Ui>) -> Markup {
    let since = ui.queen.events().last_seq();
    let list: Vec<_> = ui
        .queen
        .confirmations()
        .pending()
        .into_iter()
        .map(|c| approval_words(c.id, &c.tool, &c.reason, Some(&c.args), &ui))
        .collect();
    let epoch = ui.queen.events().epoch();
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
    /// The helper whose conversation this is (None: the Queen's).
    bee: Option<String>,
    feed: Feed,
    cursor: u64,
    view: View,
    reload: bool,
}

impl StreamState {
    fn who(&self) -> Who<'_> {
        match &self.bee {
            Some(id) => Who::Bee(id),
            None => Who::Queen,
        }
    }
}

async fn queen_stream(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Query(q): Query<StreamQuery>,
) -> Response {
    let view = match q.view.as_deref() {
        Some("approvals") => View::Approvals,
        _ => View::Chat,
    };
    let log = ui.queen.events().clone();
    let at = cursor(&headers, &q, log.epoch());
    stream(ui, log, None, view, at).into_response()
}

async fn bee_stream(
    State(ui): State<Ui>,
    Path(bee): Path<String>,
    headers: HeaderMap,
    Query(q): Query<StreamQuery>,
) -> Response {
    let Some(bee) = ui.bee(&bee) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let log = match ui.log(Who::Bee(&bee.id)) {
        Ok(log) => log,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let at = cursor(&headers, &q, log.epoch());
    stream(ui, log, Some(bee.id), View::Chat, at).into_response()
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
    bee: Option<String>,
    view: View,
    cursor: Option<u64>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // The page is from another log or newer than this one (the daemon restarted): its DOM
    // can't be updated.
    let reload = cursor.is_none_or(|c| c > log.last_seq());
    let cursor = cursor.unwrap_or(0);
    let helper = bee.is_some();
    let feed = {
        let page = log.since(0, EventLog::CAPACITY);
        Feed::for_helper(helper).fold(page.events.iter().filter(|r| r.seq <= cursor), &ui)
    };
    let state = StreamState {
        ui,
        log,
        bee,
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

#[derive(Deserialize)]
struct NewMessage {
    text: String,
}

async fn queen_message(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Form(form): Form<NewMessage>,
) -> Response {
    let result = ui.queen.send(&form.text).map(|_| ());
    message_reply(&ui, Who::Queen, None, &headers, &form.text, result).await
}

async fn bee_message(
    State(ui): State<Ui>,
    Path(bee): Path<String>,
    headers: HeaderMap,
    Form(form): Form<NewMessage>,
) -> Response {
    let Some(bee) = ui.bee(&bee) else {
        return (StatusCode::NOT_FOUND, pages::not_found()).into_response();
    };
    let result = ui.queen.bees().user_send(&bee.id, &form.text).map(|_| ());
    message_reply(
        &ui,
        Who::Bee(&bee.id),
        Some(&bee),
        &headers,
        &form.text,
        result,
    )
    .await
}

/// The answer to a sent message: the composer (cleared, or with the text and why it wasn't
/// sent) for htmx; a redirect or the page with the error otherwise.
async fn message_reply(
    ui: &Ui,
    who: Who<'_>,
    bee: Option<&BeeView>,
    headers: &HeaderMap,
    text: &str,
    result: Result<(), QueenError>,
) -> Response {
    let error = match &result {
        Ok(()) => None,
        Err(QueenError::Busy) => {
            Some("Still working on your last message. Wait for it, or press Stop.".to_string())
        }
        Err(QueenError::Empty) => Some("Type a message first.".to_string()),
        Err(e) => Some(format!("Couldn't send that: {e}.")),
    };
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
    let back = match bee {
        Some(b) => pages::bee_href(&b.id),
        None => "/".to_string(),
    };
    match error {
        None => Redirect::to(&back).into_response(),
        Some(e) => match ui.chat_page(who, bee, draft, Some(&e)).await {
            Ok(page) => (StatusCode::UNPROCESSABLE_ENTITY, page).into_response(),
            Err(e) => error_page(e),
        },
    }
}

#[derive(Deserialize)]
struct BackendForm {
    choice: String,
}

async fn queen_backend(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Form(form): Form<BackendForm>,
) -> Response {
    let (backend, model) = agents::parse_choice(&form.choice);
    let result = ui.queen.use_backend(backend, model).map(|_| ());
    backend_reply(&ui, Who::Queen, None, &headers, backend, result).await
}

async fn bee_backend(
    State(ui): State<Ui>,
    Path(bee): Path<String>,
    headers: HeaderMap,
    Form(form): Form<BackendForm>,
) -> Response {
    let Some(bee) = ui.bee(&bee) else {
        return (StatusCode::NOT_FOUND, pages::not_found()).into_response();
    };
    let (backend, model) = agents::parse_choice(&form.choice);
    let result = ui
        .queen
        .bees()
        .user_use_backend(&bee.id, backend, model)
        .map(|_| ());
    backend_reply(
        &ui,
        Who::Bee(&bee.id),
        Some(&bee),
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
    bee: Option<&BeeView>,
    headers: &HeaderMap,
    backend: &str,
    result: Result<(), QueenError>,
) -> Response {
    let error = match result {
        Ok(()) => None,
        Err(QueenError::Unavailable(_)) => Some(format!(
            "{} isn't ready on this device yet.",
            backend_label(backend)
        )),
        Err(QueenError::Failed(e)) => Some(format!("Couldn't switch: {e}.")),
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
    let back = match bee {
        Some(b) => pages::bee_href(&b.id),
        None => "/".to_string(),
    };
    match error {
        None => Redirect::to(&back).into_response(),
        Some(e) => match ui.chat_page(who, bee, "", Some(&e)).await {
            Ok(page) => (StatusCode::UNPROCESSABLE_ENTITY, page).into_response(),
            Err(e) => error_page(e),
        },
    }
}

async fn queen_cancel(State(ui): State<Ui>, headers: HeaderMap) -> Response {
    ui.queen.cancel().await;
    cancelled(&headers, "/")
}

async fn bee_cancel(State(ui): State<Ui>, Path(bee): Path<String>, headers: HeaderMap) -> Response {
    let Some(bee) = ui.bee(&bee) else {
        return (StatusCode::NOT_FOUND, pages::not_found()).into_response();
    };
    // Nothing running is fine: the button may be stale.
    let _ = ui.queen.bees().user_cancel(&bee.id).await;
    cancelled(&headers, &pages::bee_href(&bee.id))
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
    let resolved = ui.queen.resolve_confirmation(id, verdict);
    if !is_htmx(&headers) {
        return Redirect::to("/approvals").into_response();
    }
    // htmx swaps only 2xx answers, so "already gone" is a 200 with a note.
    match resolved {
        Some(c) => {
            let mut a = approval_words(c.id, &c.tool, &c.reason, Some(&c.args), &ui);
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

async fn new_chat(State(ui): State<Ui>) -> Response {
    match ui.queen.new_chat().await {
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
        ui.queen.past_chats().into_iter().find(|c| c.id == id),
        ui.queen.past_chat(id),
    ) else {
        return (StatusCode::NOT_FOUND, pages::not_found()).into_response();
    };
    let mut feed = Feed::archived().fold(&events, &ui);
    feed.settle();
    ui.framed(|f| pages::past_chat(f, &chat, &feed))
        .await
        .into_response()
}
