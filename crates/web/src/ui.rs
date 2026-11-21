//! The lead agent's web UI: the chat (home), apps and devices, the approvals list, and a chat
//! with each helper (agent), with live updates over server-sent events.
//!
//! - `GET /` the chat with Melissa (`?draft=` pre-fills the message box), `GET /cluster`,
//!   `GET /approvals`, `GET /agents/{agent}` (a helper's conversation; by id or name).
//! - `GET /ui/stream?since=<seq>&view=chat|approvals` and `GET /ui/agents/{agent}/stream?since=`
//!   (SSE). Each `feed` event carries server-rendered HTML: new entries (the htmx SSE
//!   extension appends them to the list) and out-of-band replacements for entries that
//!   changed (a growing reply, a finished tool call, a decided approval), the composer status
//!   and the waiting count, and the rail's conversations when helpers come, go, are renamed
//!   or change status (a helper's open chat whose helper is deleted gets a notice in place
//!   of its composer). The event id is the conversation's event sequence number; on
//!   reconnect the browser sends it back as `Last-Event-ID` and the stream continues from
//!   there. The stream re-folds the log up to the cursor first, so its keys match the page.
//! - `GET /ui/cluster/apps`: a fragment that polls (apps change slowly).
//! - `POST /ui/messages` / `POST /ui/agents/{agent}/messages` (multipart: `text` and any
//!   number of `files`; or a urlencoded `text`), `POST /ui/cancel` /
//!   `POST /ui/agents/{agent}/cancel`, `POST /ui/confirmations/{id}/approve|deny|always`: the
//!   same calls as the JSON API (`always` approves and saves the "Always allow" rule). htmx
//!   gets a fragment; a plain form post gets a redirect.
//! - `POST /ui/backend` / `POST /ui/agents/{agent}/backend` (form `choice`: `<backend>` or
//!   `<backend> <model>`, posted with the composer like Stop): switch the agent from the
//!   next turn. htmx gets the composer status (with the menu, or why it was refused);
//!   a plain form post gets a redirect.
//! - Each chat has a details panel about its agent (`crate::details`): who it is, its apps,
//!   schedules and file box; `/ui/files/...` serves, adds, sends and deletes files (see
//!   `crate::files`). `GET /agents/{agent}?draft=` pre-fills the message box like home.
//! - One chat per agent (2026-11-20): no new chat. `POST /ui/fresh-start` and
//!   `POST /ui/agents/{agent}/fresh-start` clear the agent's memory (the chat goes on);
//!   `POST /ui/agents/{agent}/pin` (`pinned` = `on`|`off`) pins it in the sidebar. Plain forms
//!   from the agent's menu: they redirect back to its chat.
//! - `GET /ui/older?before=<n>&epoch=` / `GET /ui/agents/{agent}/older?...`: the feed entries
//!   before entry `n` (a chat page renders its newest entries and loads older ones as the user
//!   scrolls up), with a marker for the ones before those.
//! - `GET /chats`: Melissa's earlier chats (archived by "New chat" before 2026-11-20);
//!   `GET /chats/{id}`: one of them, read-only.
//! - Stream cursors carry the log's epoch (`?epoch=`, SSE ids `<epoch>-<seq>`): a page from
//!   before a daemon restart gets `reload`.

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
    lead::{
        agents::AgentView, backends::BackendView, events::EventLog, Lead, LeadError, LeadStatus,
    },
    tools::{AlwaysAllowError, PermissionsError, Principal, RuleOffer, ToolRegistry, Verdict},
};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    agents,
    components::{
        model_chip, model_picker, oob_inner, status_pill, AgentMenu, ApprovalState, Avatar,
        ChatHead, HeadStatus, MenuWho, ModelPicker, CHAT_STATUS_ID, MODEL_CHIP_ID,
    },
    feed::{always_words, approval_words, asked_by, backend_label, Approval, Change, Feed, Lookup},
    is_htmx, pages,
    pages::{ChatView, EntryCtx, Frame, Speaker, View, Who},
    state::ClusterCache,
    workspace::{MenuUrls, SideData},
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
        .route(pages::MELISSA_CHAT_URL, get(melissa_chat))
        .route("/cluster", get(cluster_page))
        .route("/usage", get(crate::usage::page))
        .route("/approvals", get(approvals_page))
        .route("/agents/{agent}", get(agent_page))
        .route("/ui/stream", get(lead_stream))
        .route("/ui/agents/{agent}/stream", get(agent_stream))
        .route("/ui/cluster/apps", get(cluster_apps))
        .route(
            "/ui/messages",
            post(lead_message).layer(DefaultBodyLimit::max(MESSAGE_BODY_LIMIT)),
        )
        .route(
            "/ui/agents/{agent}/messages",
            post(agent_message).layer(DefaultBodyLimit::max(MESSAGE_BODY_LIMIT)),
        )
        .route("/ui/cancel", post(lead_cancel))
        .route("/ui/agents/{agent}/cancel", post(agent_cancel))
        .route("/ui/confirmations/{id}/{verdict}", post(resolve))
        .route("/ui/fresh-start", post(lead_fresh_start))
        .route("/ui/agents/{agent}/fresh-start", post(agent_fresh_start))
        .route("/ui/agents/{agent}/pin", post(agent_pin))
        .route("/ui/older", get(lead_older))
        .route("/ui/agents/{agent}/older", get(agent_older))
        .route("/chats", get(past_chats_page))
        .route("/ui/backend", post(lead_backend))
        .route("/ui/agents/{agent}/backend", post(agent_backend))
        .route("/chats/{id}", get(past_chat_page))
        .merge(crate::settings::ui_routes())
        .merge(crate::files::routes())
        .merge(crate::details::routes())
        .with_state(ui)
}

impl Lookup for Ui {
    fn confirmation_args(&self, id: u64) -> Option<Value> {
        self.lead.confirmations().get(id).map(|c| c.args)
    }

    fn agent_name(&self, agent: &str) -> Option<String> {
        self.lead.agents().get(agent).map(|b| b.name)
    }

    fn lead_name(&self) -> String {
        self.lead.agents().lead_name()
    }

    fn rule_offer(&self, id: u64) -> Option<RuleOffer> {
        self.lead
            .confirmations()
            .get(id)
            .and_then(|c| c.rule)
            .map(|r| *r)
    }
}

impl Ui {
    /// The tool registry: confirmations and the owner's standing permissions.
    pub(crate) fn registry(&self) -> &ToolRegistry {
        self.lead.server().registry()
    }

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

    /// The backends a conversation may switch to (None if they cannot be read).
    fn backend_views(&self, who: Who<'_>) -> Option<Vec<BackendView>> {
        match who {
            Who::Lead => Some(self.lead.backends()),
            Who::Agent(id) => self.lead.agents().backends_of(id).ok(),
        }
    }

    /// The model picker of a conversation (None if it cannot be read).
    fn picker(&self, who: Who<'_>, error: Option<String>) -> Option<ModelPicker> {
        let views = self.backend_views(who)?;
        let action = match who {
            Who::Lead => "/ui/backend".to_string(),
            Who::Agent(id) => format!("/ui/agents/{id}/backend"),
        };
        let permissions = self.registry().permissions();
        let access = permissions
            .full_access_allowed()
            .then(|| permissions.has_full_access(&principal(who)));
        (!views.is_empty()).then(|| agents::picker(&views, action, error, access))
    }

    /// Name the helper that asked for confirmation `task` in its card (views that show every
    /// helper's: Melissa's chat, Waiting for you).
    fn label_asker(&self, a: &mut Approval, task: melissa_agent::TaskId) {
        if let Some(agent) = self.lead.agents().agent_of_task(task) {
            asked_by(a, &agent, self);
        }
    }

    /// The agent for a path segment (id or name).
    pub(crate) fn agent(&self, agent: &str) -> Option<AgentView> {
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
        let side = self.side_data(&snap);
        let past = self.lead.past_chats();
        page(&Frame {
            bypass: self.registry().permissions().bypass(),
            full_access: self.registry().permissions().full_access_allowed(),
            snap: &snap,
            side: &side,
            past: &past,
            host,
            update: self.settings.update_hint(),
        })
    }

    /// A chat page (home or a helper's).
    async fn chat_page(
        &self,
        who: Who<'_>,
        agent: Option<&AgentView>,
        draft: &str,
        error: Option<&str>,
        all: bool,
    ) -> Result<Markup, LeadError> {
        let log = self.log(who)?;
        let feed = self.feed(who, &log);
        let backend = self.backend(who, &feed);
        // Learn the models of usable agents that never had a chat (rate-limited, no prompt);
        // they show in the menu from the next render.
        self.lead.refresh_models_soon();
        let picker = self.picker(who, None);
        let links = self.file_links(who);
        let lead_name = self.lead.agents().lead_name();
        let details = self.details_of(&crate::files::owner_of(who), agent).await?;
        let files = details.files.view();
        let panel = details.view(&files);
        let view = ChatView {
            who,
            epoch: log.epoch(),
            feed: &feed,
            speaker: speaker_of(agent, &lead_name),
            backend: backend.as_deref(),
            picker: picker.as_ref(),
            busy: self.status(who).is_some_and(|s| s.turn.is_some()),
            draft,
            error,
            voice: self.settings.voice_url(),
            links: &links,
            details: Some(&panel),
            all,
            caption: !self.registry().permissions().bypass(),
        };
        Ok(self
            .framed(|f| {
                let key = who.key();
                let entry = f.side.entry(&key);
                let urls = entry.map_or_else(
                    || MenuUrls::of(&key, false, !f.past.is_empty()),
                    |e| e.urls.clone(),
                );
                let head = ChatHead {
                    avatar: view.speaker.avatar,
                    device: f.side.device(),
                    name: view.speaker.name,
                    status: entry.and_then(|e| e.head),
                    menu: AgentMenu {
                        who: Some(MenuWho {
                            avatar: view.speaker.avatar,
                            device: f.side.device(),
                            status: entry.and_then(|e| e.status.as_deref()),
                        }),
                        ..urls.menu(view.speaker.name)
                    },
                };
                pages::chat(f, &view, &head)
            })
            .await)
    }

    /// The chat header's status of `who` now (from the sidebar's data).
    fn head_status(&self, who: Who<'_>, side: &SideData) -> Option<HeadStatus> {
        side.entry(&who.key()).and_then(|e| e.head)
    }
}

/// Who a chat is with, for its messages' heads (`lead`: the lead agent's name).
pub(crate) fn speaker_of<'a>(agent: Option<&'a AgentView>, lead: &'a str) -> Speaker<'a> {
    match agent {
        None => Speaker::lead(lead),
        Some(a) => Speaker {
            name: &a.name,
            avatar: Avatar::Agent {
                shape: &a.icon.shape,
                color: &a.icon.color,
            },
        },
    }
}

#[derive(Deserialize)]
struct HomeQuery {
    #[serde(default)]
    draft: String,
    /// Every entry at once (the older marker's link without script).
    #[serde(default)]
    all: Option<String>,
}

/// The home page: Melissa's chat; on phones the agent list (a draft or `all` asks for the
/// chat itself, so those links still land in it).
async fn home(state: State<Ui>, q: Query<HomeQuery>) -> Response {
    if q.draft.is_empty() && q.all.is_none() {
        pages::as_phone_home(melissa_chat(state, q)).await
    } else {
        melissa_chat(state, q).await
    }
}

/// Melissa's chat (`/chat`; also `/` on wide screens).
async fn melissa_chat(State(ui): State<Ui>, Query(q): Query<HomeQuery>) -> Response {
    // A draft is only a suggestion for the box; it is never sent by itself.
    let draft: String = q.draft.chars().take(300).collect();
    match ui
        .chat_page(Who::Lead, None, &draft, None, q.all.is_some())
        .await
    {
        Ok(page) => page.into_response(),
        Err(e) => error_page(e),
    }
}

async fn agent_page(
    State(ui): State<Ui>,
    Path(agent): Path<String>,
    Query(q): Query<HomeQuery>,
) -> Response {
    let Some(agent) = ui.agent(&agent) else {
        // Deleted (an old link, a stale rail) or a wrong address: say so in the app.
        return (StatusCode::NOT_FOUND, ui.framed(pages::helper_gone).await).into_response();
    };
    // A draft is only a suggestion for the box ("Reference in chat" without script).
    let draft: String = q.draft.chars().take(300).collect();
    match ui
        .chat_page(
            Who::Agent(&agent.id),
            Some(&agent),
            &draft,
            None,
            q.all.is_some(),
        )
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

async fn approvals_page(State(ui): State<Ui>) -> Markup {
    let since = ui.lead.events().last_seq();
    let list: Vec<_> = ui
        .lead
        .confirmations()
        .pending()
        .into_iter()
        .map(|c| {
            let mut a = approval_words(c.id, &c.tool, &c.reason, Some(&c.args), &ui);
            a.always = c.rule.as_ref().map(|o| always_words(o, &ui));
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
    /// Wakes when the helpers change (see [`AgentRegistry::changes`]).
    ///
    /// [`AgentRegistry::changes`]: melissa_agent::lead::registry::AgentRegistry::changes
    roster: tokio::sync::watch::Receiver<u64>,
    /// Melissa's log, in a helper's chat: approvals and helpers' jobs change the sidebar.
    lead_log: Option<EventLog>,
    lead_cursor: u64,
    /// The sidebar as the page shows it (as last sent).
    side: SideData,
    /// The header's status as last sent.
    head: Option<HeadStatus>,
    /// The helper's name (helper chats), to say it was deleted; None once said.
    name: Option<String>,
    /// Melissa has earlier chats (her menu's "Earlier chats").
    earlier: bool,
}

impl StreamState {
    fn who(&self) -> Who<'_> {
        match &self.agent {
            Some(id) => Who::Agent(id),
            None => Who::Lead,
        }
    }

    /// A live update at the current cursor.
    fn event(&self, html: String) -> Event {
        Event::default()
            .event("feed")
            .id(format!("{}-{}", self.log.epoch(), self.cursor))
            .data(html)
    }

    /// The sidebar's data now.
    async fn side_now(&self) -> SideData {
        let snap = self.ui.cluster.get().await;
        self.ui.side_data_with(&snap, self.earlier)
    }

    /// Out-of-band updates for agents that came, went, moved between groups or changed since
    /// the last call: the sidebar's groups and, when this chat's helper was deleted, a notice
    /// in place of the composer. `side`: the sidebar now. Empty if nothing changed.
    fn roster_update(&mut self, side: SideData) -> String {
        let mut out = String::new();
        if side != self.side {
            let current = match (&self.agent, self.view) {
                (Some(id), _) => Some(id.as_str()),
                (None, View::Chat) => Some("lead"),
                (None, View::Approvals) => None,
            };
            out.push_str(&pages::sidebar_update(&side, current).into_string());
            self.side = side;
        }
        if let Some(id) = &self.agent {
            match self.side.entry(id) {
                Some(e) => {
                    if let Some(name) = self.name.as_mut() {
                        name.clone_from(&e.name);
                    }
                }
                None => {
                    if let Some(name) = self.name.take() {
                        out.push_str(
                            &pages::helper_deleted(&name, self.side.lead_name()).into_string(),
                        );
                    }
                }
            }
        }
        out
    }

    /// The header's new status, if it changed.
    fn head_change(&mut self) -> Option<Option<HeadStatus>> {
        if self.view != View::Chat {
            return None;
        }
        let now = self.ui.head_status(self.who(), &self.side);
        (now != self.head).then(|| {
            self.head = now;
            now
        })
    }
}

/// Until `rx` sees a change; never, once its sender is gone.
async fn changed(rx: &mut tokio::sync::watch::Receiver<u64>) {
    if rx.changed().await.is_err() {
        std::future::pending::<()>().await;
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
    let side = side_at_start(&ui).await;
    stream(ui, log, None, view, at, side).into_response()
}

async fn agent_stream(
    State(ui): State<Ui>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    Query(q): Query<StreamQuery>,
) -> Response {
    let Some(agent) = ui.agent(&agent) else {
        // Gone (deleted while the page was closed or the connection was down): the page
        // reloads and says so.
        let reload = futures::stream::once(async {
            Ok::<_, Infallible>(Event::default().event("reload").data(""))
        });
        return Sse::new(reload).into_response();
    };
    let log = match ui.log(Who::Agent(&agent.id)) {
        Ok(log) => log,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let at = cursor(&headers, &q, log.epoch());
    let side = side_at_start(&ui).await;
    stream(ui, log, Some(agent.id), View::Chat, at, side).into_response()
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

/// The sidebar when a stream starts (what its page shows, near enough): later changes go out.
async fn side_at_start(ui: &Ui) -> SideData {
    let snap = ui.cluster.get().await;
    ui.side_data(&snap)
}

fn stream(
    ui: Ui,
    log: EventLog,
    agent: Option<String>,
    view: View,
    cursor: Option<u64>,
    side: SideData,
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
    let agents = ui.lead.agents();
    let roster = agents.registry().changes();
    let name = agent.as_ref().and_then(|id| agents.get(id).map(|a| a.name));
    let lead_log = helper.then(|| ui.lead.events().clone());
    let lead_cursor = lead_log.as_ref().map_or(0, EventLog::last_seq);
    let earlier = !ui.lead.past_chats().is_empty();
    let head = {
        let who = match &agent {
            Some(id) => Who::Agent(id),
            None => Who::Lead,
        };
        ui.head_status(who, &side)
    };
    let state = StreamState {
        roster,
        name,
        ui,
        log,
        agent,
        feed,
        cursor,
        view,
        reload,
        lead_log,
        lead_cursor,
        head,
        side,
        earlier,
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
            // The log first: chat events that came with a roster change go out with it.
            let lead_log = st.lead_log.clone();
            let lead_cursor = st.lead_cursor;
            let lead_wait = async move {
                match lead_log {
                    Some(l) => {
                        l.wait_since(lead_cursor, 1, STREAM_WAIT).await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            let page = tokio::select! {
                biased;
                page = st.log.wait_since(st.cursor, 200, STREAM_WAIT) => Some(page),
                () = changed(&mut st.roster) => None,
                () = lead_wait => None,
            };
            if let Some(l) = &st.lead_log {
                st.lead_cursor = l.last_seq();
            }
            let page = match page {
                Some(page) if !page.events.is_empty() => page,
                // The helpers changed, Melissa's chat moved on (approvals, jobs), or it was
                // quiet (a helper's status may still have changed by turns in its own chat).
                _ => {
                    let side = st.side_now().await;
                    let mut html = st.roster_update(side);
                    if let Some(h) = st.head_change() {
                        html.push_str(&oob_inner(CHAT_STATUS_ID, status_pill(h)).into_string());
                    }
                    if html.is_empty() {
                        continue;
                    }
                    return Some((Ok(st.event(html)), st));
                }
            };
            let mut changes = Vec::new();
            for rec in &page.events {
                changes.extend(st.feed.apply(rec, &st.ui));
            }
            st.cursor = page.next;
            if changes.contains(&Change::Reset) {
                // A new chat (only in logs from before one chat per agent): start over.
                st.reload = true;
                st.cursor = u64::MAX;
                return Some((Ok(Event::default().event("reload").data("")), st));
            }
            // Text pieces alone don't move anyone in the sidebar.
            let moved = changes
                .iter()
                .any(|c| matches!(c, Change::Status | Change::Waiting | Change::Added(_)));
            let roster = if moved {
                let side = st.side_now().await;
                st.roster_update(side)
            } else {
                String::new()
            };
            let head = if moved { st.head_change() } else { None };
            let backend = st.ui.backend(st.who(), &st.feed);
            let picker = changes
                .contains(&Change::Status)
                .then(|| st.ui.picker(st.who(), None))
                .flatten();
            let links = st.ui.file_links(st.who());
            let panel = if changes.contains(&Change::Files) && st.view == View::Chat {
                let ui = st.ui.clone();
                let owner = crate::files::owner_of(st.who());
                tokio::task::spawn_blocking(move || ui.panel_of_owner(&owner))
                    .await
                    .ok()
            } else {
                None
            };
            let files = panel.as_ref().map(|p| p.view());
            let agent_view = st.agent.as_ref().and_then(|id| st.ui.agent(id));
            let lead_name = st.ui.lead.agents().lead_name();
            let x = EntryCtx {
                links: &links,
                speaker: speaker_of(agent_view.as_ref(), &lead_name),
                clock: st.feed.clock(),
            };
            let mut html = pages::stream_update(
                &st.feed,
                &changes,
                st.view,
                st.who(),
                &pages::StreamExtras {
                    backend: backend.as_deref(),
                    picker: picker.as_ref(),
                    entry: &x,
                    files: files.as_ref(),
                    status: head,
                },
            );
            html.push_str(&roster);
            if html.is_empty() {
                continue;
            }
            return Some((Ok(st.event(html)), st));
        }
    });
    Sse::new(events).keep_alive(KeepAlive::default())
}

/// Largest message request body: the files' limit plus room for the text and the form's
/// framing. Bigger bodies are cut off while they stream in.
pub(crate) const MESSAGE_BODY_LIMIT: usize = MAX_MESSAGE_BYTES + 1024 * 1024;

#[derive(Deserialize)]
struct TextOnly {
    text: String,
}

/// A message from the composer: its text and files. The composer posts
/// `multipart/form-data` (fields `text` and `files`); a urlencoded form has text only.
pub(crate) struct NewMessage {
    pub(crate) text: String,
    pub(crate) files: Vec<Upload>,
    /// Why the files were refused while reading them (too large, too many, cut off).
    pub(crate) refused: Option<LeadError>,
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
        let picker = ui.picker(who, None);
        let links = pages::FileLinks::none();
        let lead_name = ui.lead.agents().lead_name();
        return pages::composer_box(&ChatView {
            who,
            epoch: 0,
            feed: &feed,
            speaker: speaker_of(agent, &lead_name),
            backend,
            picker: picker.as_ref(),
            busy,
            draft,
            error: error.as_deref(),
            voice: ui.settings.voice_url(),
            links: &links,
            details: None,
            all: false,
            caption: !ui.registry().permissions().bypass(),
        })
        .into_response();
    }
    let back = match agent {
        Some(b) => pages::agent_href(&b.id),
        // Back where it was sent from: `/chat` on a phone (`/` is the agent list there).
        None if back_to(headers, "/").starts_with(pages::MELISSA_CHAT_URL) => {
            pages::MELISSA_CHAT_URL.to_string()
        }
        None => "/".to_string(),
    };
    match error {
        None => Redirect::to(&back).into_response(),
        Some(e) => match ui.chat_page(who, agent, draft, Some(&e), false).await {
            Ok(page) => (StatusCode::UNPROCESSABLE_ENTITY, page).into_response(),
            Err(e) => error_page(e),
        },
    }
}

/// Who asks for a conversation's confirmations, for its access level.
fn principal(who: Who<'_>) -> Principal {
    match who {
        Who::Lead => Principal::Lead,
        Who::Agent(id) => Principal::Agent(id.to_string()),
    }
}

/// A pick in the model picker: a model (`choice`), or a reasoning level (`effort`, empty for
/// the default) or speed (`fast`: `on`/`off`) for the current model (`current`), or the
/// agent's access level (`access`: `ask`/`full`). Picking a model keeps the reasoning level
/// and Fast when the new model offers them too.
#[derive(Deserialize)]
struct BackendForm {
    #[serde(default)]
    choice: Option<String>,
    #[serde(default)]
    current: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    fast: Option<String>,
    #[serde(default)]
    access: Option<String>,
}

/// What a picker form asks for.
struct Pick {
    backend: String,
    model: Option<String>,
    effort: Option<String>,
    fast: bool,
}

impl BackendForm {
    /// Backend, model, reasoning level and speed to switch to.
    fn resolve(&self, views: &[BackendView]) -> Pick {
        let choice = self
            .choice
            .as_deref()
            .or(self.current.as_deref())
            .unwrap_or_default();
        let (backend, model) = agents::parse_choice(choice);
        let effort = match &self.effort {
            Some(e) => Some(e.trim().to_string()).filter(|e| !e.is_empty()),
            None => agents::kept_effort(views, backend, model),
        };
        let fast = match self.fast.as_deref().map(str::trim) {
            Some(f) => f == "on",
            None => agents::kept_fast(views, backend, model),
        };
        Pick {
            backend: backend.to_string(),
            model: model.map(str::to_string),
            effort,
            fast,
        }
    }

    /// The access level asked for: Some(true) Full access, Some(false) Ask.
    fn access(&self) -> Option<Result<bool, ()>> {
        self.access.as_deref().map(|a| match a.trim() {
            "full" => Ok(true),
            "ask" => Ok(false),
            _ => Err(()),
        })
    }
}

/// Put the conversation's agent on Full access or back on Ask (the owner's own change,
/// audited by the registry). Why it was refused, if it was.
fn set_access(ui: &Ui, who: Who<'_>, asked: Result<bool, ()>) -> Option<String> {
    let Ok(full) = asked else {
        return Some("That access level isn't available.".into());
    };
    match ui.registry().set_access(&principal(who), full) {
        Ok(()) => None,
        Err(PermissionsError::FullAccessOff) => {
            Some("Full access per agent is off. Turn it on in Settings → Developer.".into())
        }
        Err(e) => {
            tracing::warn!("cannot save the access level: {e}");
            Some("Couldn't save the access level.".into())
        }
    }
}

async fn lead_backend(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Form(form): Form<BackendForm>,
) -> Response {
    if let Some(asked) = form.access() {
        let error = set_access(&ui, Who::Lead, asked);
        return picker_reply(&ui, Who::Lead, None, &headers, error).await;
    }
    let pick = form.resolve(&ui.lead.backends());
    let result = ui
        .lead
        .use_backend_with(
            &pick.backend,
            pick.model.as_deref(),
            pick.effort.as_deref(),
            pick.fast,
        )
        .map(|_| ());
    backend_reply(&ui, Who::Lead, None, &headers, &pick.backend, result).await
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
    let who = Who::Agent(&agent.id);
    if let Some(asked) = form.access() {
        let error = set_access(&ui, who, asked);
        return picker_reply(&ui, who, Some(&agent), &headers, error).await;
    }
    let views = ui.backend_views(who).unwrap_or_default();
    let pick = form.resolve(&views);
    let result = ui
        .lead
        .agents()
        .user_use_backend_with(
            &agent.id,
            &pick.backend,
            pick.model.as_deref(),
            pick.effort.as_deref(),
            pick.fast,
        )
        .map(|_| ());
    backend_reply(&ui, who, Some(&agent), &headers, &pick.backend, result).await
}

/// The answer to a switch: the picker (closed, or reopened by melissa.js with why a switch
/// was refused) and the chip out of band for htmx; the chat page otherwise. The feed line
/// comes with the event stream.
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
        Err(_) => Some("Couldn't switch: that model or option isn't available here.".into()),
    };
    picker_reply(ui, who, agent, headers, error).await
}

/// The picker after a pick: closed, or reopened by melissa.js with why it was refused, and
/// the chip out of band for htmx; the chat page otherwise.
async fn picker_reply(
    ui: &Ui,
    who: Who<'_>,
    agent: Option<&AgentView>,
    headers: &HeaderMap,
    error: Option<String>,
) -> Response {
    if is_htmx(headers) {
        return match ui.picker(who, error) {
            Some(p) => maud::html! {
                (model_picker(&p))
                (oob_inner(MODEL_CHIP_ID, model_chip(&p)))
            }
            .into_response(),
            None => StatusCode::NO_CONTENT.into_response(),
        };
    }
    let back = match agent {
        Some(b) => pages::agent_href(&b.id),
        None => "/".to_string(),
    };
    match error {
        None => Redirect::to(&back).into_response(),
        Some(e) => match ui.chat_page(who, agent, "", Some(&e), false).await {
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
        "always" => return always_allow(&ui, &headers, id),
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

/// "Always allow": approve confirmation `id` and save its rule (with any other pending
/// confirmation the rule matches). The card shows the outcome.
fn always_allow(ui: &Ui, headers: &HeaderMap, id: u64) -> Response {
    let done = ui.registry().always_allow(id);
    if !is_htmx(headers) {
        return Redirect::to("/approvals").into_response();
    }
    match done {
        Ok(done) => match done.approved.into_iter().find(|c| c.id == id) {
            Some(c) => {
                let mut a = approval_words(c.id, &c.tool, &c.reason, Some(&c.args), ui);
                if !from_helper_chat(headers) {
                    ui.label_asker(&mut a, c.task);
                }
                a.state = ApprovalState::Approved;
                let target = pages::entry_id(&format!("c{id}"));
                pages::approval_content(&a, &target).into_response()
            }
            // Saved, but this one timed out meanwhile.
            None => pages::approval_gone().into_response(),
        },
        Err(AlwaysAllowError::Gone(_)) => pages::approval_gone().into_response(),
        Err(e) => pages::approval_error(&e.to_string()).into_response(),
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

/// Where a form from an agent's menu goes back to: the page it came from (same origin), else
/// the agent's chat.
fn back_to(headers: &HeaderMap, chat: &str) -> String {
    headers
        .get(header::REFERER)
        .and_then(|v| v.to_str().ok())
        .and_then(|url| {
            let rest = url.split_once("://").map_or(url, |(_, r)| r);
            let (host_part, path) = rest.split_at(rest.find('/')?);
            (Some(host_part) == host(headers)).then(|| path.to_string())
        })
        .filter(|p| p.starts_with('/') && !p.starts_with("//"))
        .unwrap_or_else(|| chat.to_string())
}

async fn lead_fresh_start(State(ui): State<Ui>, headers: HeaderMap) -> Response {
    match ui.lead.fresh_start().await {
        Ok(()) => Redirect::to(&back_to(&headers, "/")).into_response(),
        Err(e) => (
            StatusCode::CONFLICT,
            pages::error_page(
                &format!("Couldn't clear {}'s memory", ui.lead.agents().lead_name()),
                &e.to_string(),
            ),
        )
            .into_response(),
    }
}

async fn agent_fresh_start(
    State(ui): State<Ui>,
    Path(agent): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(agent) = ui.agent(&agent) else {
        return (StatusCode::NOT_FOUND, pages::not_found()).into_response();
    };
    match ui.lead.agents().fresh_start(&agent.id).await {
        Ok(_) => Redirect::to(&back_to(&headers, &pages::agent_href(&agent.id))).into_response(),
        Err(e) => (
            StatusCode::CONFLICT,
            pages::error_page(
                &format!("Couldn't clear {}'s memory", agent.name),
                &e.to_string(),
            ),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct PinForm {
    #[serde(default)]
    pinned: String,
}

async fn agent_pin(
    State(ui): State<Ui>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    Form(form): Form<PinForm>,
) -> Response {
    let Some(agent) = ui.agent(&agent) else {
        return (StatusCode::NOT_FOUND, pages::not_found()).into_response();
    };
    if let Err(e) = ui.lead.agents().set_pinned(&agent.id, form.pinned == "on") {
        tracing::warn!("cannot pin {}: {e}", agent.id);
    }
    Redirect::to(&back_to(&headers, &pages::agent_href(&agent.id))).into_response()
}

#[derive(Deserialize)]
struct OlderQuery {
    #[serde(default)]
    before: usize,
    #[serde(default)]
    epoch: Option<u64>,
}

async fn lead_older(State(ui): State<Ui>, Query(q): Query<OlderQuery>) -> Response {
    older(&ui, Who::Lead, None, &q)
}

async fn agent_older(
    State(ui): State<Ui>,
    Path(agent): Path<String>,
    Query(q): Query<OlderQuery>,
) -> Response {
    let Some(agent) = ui.agent(&agent) else {
        return (StatusCode::NOT_FOUND, pages::not_found()).into_response();
    };
    older(&ui, Who::Agent(&agent.id), Some(&agent), &q)
}

/// The feed entries before `before` (lazy loading), with a marker for the ones before those.
/// A page from before a restart (another epoch) gets nothing: its stream reloads it.
fn older(ui: &Ui, who: Who<'_>, agent: Option<&AgentView>, q: &OlderQuery) -> Response {
    let Ok(log) = ui.log(who) else {
        return (StatusCode::NOT_FOUND, pages::not_found()).into_response();
    };
    if q.epoch.is_some_and(|e| e != log.epoch()) {
        return maud::html! {}.into_response();
    }
    let feed = ui.feed(who, &log);
    let links = ui.file_links(who);
    let lead_name = ui.lead.agents().lead_name();
    let x = EntryCtx {
        links: &links,
        speaker: speaker_of(agent, &lead_name),
        clock: feed.clock(),
    };
    pages::feed_page(&feed, q.before, who, log.epoch(), false, &x).into_response()
}

async fn past_chats_page(State(ui): State<Ui>) -> Response {
    ui.framed(pages::past_chats).await.into_response()
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
    // Past chats are Melissa's: their files are in her box.
    let links = ui.file_links(Who::Lead);
    ui.framed(|f| pages::past_chat(f, &chat, &feed, &links))
        .await
        .into_response()
}
