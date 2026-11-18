//! The Queen's web UI: the chat (home), apps and devices, and the approvals list, with
//! live updates over server-sent events.
//!
//! - `GET /` the chat (`?draft=` pre-fills the message box), `GET /hive`, `GET /approvals`.
//! - `GET /ui/stream?since=<seq>&view=chat|approvals` (SSE). Each `feed` event carries
//!   server-rendered HTML: new entries (the htmx SSE extension appends them to the list)
//!   and out-of-band replacements for entries that changed (a growing reply, a finished
//!   tool call, a decided approval), the composer status and the waiting count. The event
//!   id is the Queen event sequence number; on reconnect the browser sends it back as
//!   `Last-Event-ID` and the stream continues from there. The stream re-folds the log up
//!   to the cursor first, so its keys match the page's DOM.
//! - `GET /ui/strip`, `GET /ui/hive/apps`: fragments that poll (apps change slowly).
//! - `POST /ui/messages` (form `text`), `POST /ui/cancel`,
//!   `POST /ui/confirmations/{id}/approve|deny`: the same Queen calls as the JSON API.
//!   htmx gets a fragment; a plain form post gets a redirect.

use std::{convert::Infallible, time::Duration};

use axum::{
    extract::{Form, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Redirect, Response,
    },
    routing::{get, post},
    Router,
};
use futures::Stream;
use melissa_agent::{
    queen::{events::EventLog, Queen, QueenError},
    tools::Verdict,
};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    feed::{approval_words, backend_label, Feed},
    is_htmx, pages,
    pages::View,
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
        .route("/ui/stream", get(stream))
        .route("/ui/strip", get(strip))
        .route("/ui/hive/apps", get(hive_apps))
        .route("/ui/messages", post(send_message))
        .route("/ui/cancel", post(cancel))
        .route("/ui/confirmations/{id}/{verdict}", post(resolve))
        .with_state(Ui { queen, hive })
}

impl Ui {
    /// The arguments of a pending confirmation (agent-supplied; shown only as text).
    fn args_lookup(&self) -> impl Fn(u64) -> Option<Value> + '_ {
        |id| self.queen.confirmations().get(id).map(|c| c.args)
    }

    /// The chat folded from every event in the log.
    fn feed(&self) -> Feed {
        let page = self.queen.events().since(0, EventLog::CAPACITY);
        Feed::from_events(&page.events, &self.args_lookup())
    }

    fn backend(&self, feed: &Feed) -> Option<String> {
        self.queen
            .status()
            .backend
            .map(|b| backend_label(&b).to_string())
            .or_else(|| feed.backend().map(str::to_string))
    }

    fn waiting(&self) -> usize {
        self.queen.confirmations().pending().len()
    }

    async fn home_page(&self, draft: &str, error: Option<&str>) -> maud::Markup {
        let feed = self.feed();
        let snap = self.hive.get().await;
        let backend = self.backend(&feed);
        pages::home(&pages::HomeView {
            feed: &feed,
            snap: &snap,
            waiting: self.waiting(),
            backend: backend.as_deref(),
            busy: self.queen.status().turn.is_some(),
            draft,
            error,
        })
    }
}

#[derive(Deserialize)]
struct HomeQuery {
    #[serde(default)]
    draft: String,
}

async fn home(State(ui): State<Ui>, Query(q): Query<HomeQuery>) -> maud::Markup {
    // A draft is only a suggestion for the box; it is never sent by itself.
    let draft: String = q.draft.chars().take(200).collect();
    ui.home_page(&draft, None).await
}

async fn hive_page(State(ui): State<Ui>) -> maud::Markup {
    let snap = ui.hive.get().await;
    pages::hive(&snap, ui.waiting())
}

async fn hive_apps(State(ui): State<Ui>) -> maud::Markup {
    pages::hive_apps(&*ui.hive.get().await)
}

async fn strip(State(ui): State<Ui>) -> maud::Markup {
    pages::strip(&*ui.hive.get().await)
}

async fn approvals_page(State(ui): State<Ui>) -> maud::Markup {
    let since = ui.queen.events().last_seq();
    let list: Vec<_> = ui
        .queen
        .confirmations()
        .pending()
        .into_iter()
        .map(|c| approval_words(c.id, &c.tool, &c.reason, Some(&c.args)))
        .collect();
    let snap = ui.hive.get().await;
    pages::approvals(&list, since, &snap)
}

#[derive(Deserialize)]
struct StreamQuery {
    #[serde(default)]
    since: u64,
    #[serde(default)]
    view: Option<String>,
}

/// How long one wait for new events lasts before it is started again.
const STREAM_WAIT: Duration = Duration::from_secs(25);

struct StreamState {
    ui: Ui,
    feed: Feed,
    cursor: u64,
    view: View,
    reload: bool,
}

async fn stream(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Query(q): Query<StreamQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // A reconnecting browser says where it got to.
    let cursor = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(q.since);
    let view = match q.view.as_deref() {
        Some("approvals") => View::Approvals,
        _ => View::Chat,
    };
    // The page is newer than this log (the daemon restarted): its DOM can't be updated.
    let reload = cursor > ui.queen.events().last_seq();
    let feed = {
        let page = ui.queen.events().since(0, EventLog::CAPACITY);
        Feed::from_events(
            page.events.iter().filter(|r| r.seq <= cursor),
            &ui.args_lookup(),
        )
    };
    let state = StreamState {
        ui,
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
            let page = st
                .ui
                .queen
                .events()
                .wait_since(st.cursor, 200, STREAM_WAIT)
                .await;
            if page.events.is_empty() {
                continue;
            }
            let mut changes = Vec::new();
            {
                let lookup = st.ui.args_lookup();
                for rec in &page.events {
                    changes.extend(st.feed.apply(rec, &lookup));
                }
            }
            st.cursor = page.next;
            let backend = st.ui.backend(&st.feed);
            let html = pages::stream_update(
                &st.feed,
                &changes,
                st.view,
                backend.as_deref(),
                st.ui.waiting(),
            );
            if html.is_empty() {
                continue;
            }
            let event = Event::default()
                .event("feed")
                .id(st.cursor.to_string())
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

async fn send_message(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Form(form): Form<NewMessage>,
) -> Response {
    let result = ui.queen.send(&form.text);
    let error = match &result {
        Ok(_) => None,
        Err(QueenError::Busy) => {
            Some("I'm still working on your last message. Wait for it, or press Stop.".to_string())
        }
        Err(QueenError::Empty) => Some("Type a message first.".to_string()),
        Err(e) => Some(format!("Couldn't send that: {e}.")),
    };
    // Keep the text on failure so nothing typed is lost.
    let draft = if error.is_some() {
        form.text.as_str()
    } else {
        ""
    };
    if is_htmx(&headers) {
        let feed_backend = ui.queen.status().backend;
        let backend = feed_backend.as_deref().map(backend_label);
        let busy = ui.queen.status().turn.is_some();
        return pages::composer_box(draft, error.as_deref(), backend, busy).into_response();
    }
    match error {
        None => Redirect::to("/").into_response(),
        Some(e) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            ui.home_page(draft, Some(&e)).await,
        )
            .into_response(),
    }
}

async fn cancel(State(ui): State<Ui>, headers: HeaderMap) -> Response {
    ui.queen.cancel().await;
    if is_htmx(&headers) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        Redirect::to("/").into_response()
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
            let mut a = approval_words(c.id, &c.tool, &c.reason, Some(&c.args));
            a.state = match verdict {
                Verdict::Approved => crate::components::ApprovalState::Approved,
                Verdict::Denied => crate::components::ApprovalState::Denied,
            };
            let target = pages::entry_id(&format!("c{id}"));
            pages::approval_content(&a, &target).into_response()
        }
        None => pages::approval_gone().into_response(),
    }
}
