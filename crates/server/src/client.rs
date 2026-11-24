//! Command-line client for a running lead agent: `chat`, `ask` (with the lead agent or, with `--agent`,
//! any agent), `confirm`, `agent list|rename`, `workspace list|reassign`.
//!
//! Talks to the daemon's JSON API (`/api/...`) over its host-local Unix socket with plain
//! HTTP/1.1, one connection per request. The web UI needs a login; this socket instead
//! trusts the host: only root and Melissa's own user may connect (peer credentials), and
//! they act as the owner (audited as `host`). Sends no `Origin` header, like any
//! non-browser client.

use std::{
    io::Write,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{bail, Context};
use http_body_util::{BodyExt, Full};
use hyper::{body::Bytes, Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use melissa_agent::lead::events::{EventPage, LeadEvent, TurnId};
use serde_json::{json, Value};
use tokio::io::AsyncBufReadExt;

/// Seconds one events request waits for news.
const POLL_SECS: u64 = 20;

/// The socket's file name in Melissa's runtime directory.
pub const SOCKET_NAME: &str = "api.sock";
/// The service's runtime directory (`RuntimeDirectory=melissa`), for root's shell, which has
/// no `XDG_RUNTIME_DIR` of Melissa's (e.g. `podman exec`).
const SERVICE_SOCKET: &str = "/run/melissa/api.sock";

/// Where the daemon's socket is: `$MELISSA_SOCKET`, else `<runtime>/api.sock` (the same user's
/// daemon), else the service's.
pub fn socket_path(runtime: &Path) -> PathBuf {
    if let Some(p) = std::env::var_os("MELISSA_SOCKET").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    let own = runtime.join(SOCKET_NAME);
    if own.exists() || !Path::new(SERVICE_SOCKET).exists() {
        own
    } else {
        PathBuf::from(SERVICE_SOCKET)
    }
}

pub struct Client {
    socket: PathBuf,
    /// The conversation `chat`/`ask` talk to: `/api/lead` or `/api/agents/<agent>`.
    base: String,
    /// Who that is, for messages ("the lead agent", "agent Ada").
    who: String,
}

impl Client {
    pub fn new(socket: &Path) -> Self {
        Self {
            socket: socket.to_path_buf(),
            base: "/api/lead".into(),
            who: "the lead agent".into(),
        }
    }

    /// Talk to agent `agent` (name or id) instead of the lead agent.
    pub fn with_agent(mut self, agent: &str) -> Self {
        self.base = format!("/api/agents/{}", path_segment(agent));
        self.who = format!("agent {agent}");
        self
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> anyhow::Result<(StatusCode, Value)> {
        let stream = tokio::net::UnixStream::connect(&self.socket)
            .await
            .with_context(|| {
                format!(
                    "cannot reach Melissa at {} (is `melissa-server` running? Run this as root \
                     or as Melissa's user; in the container: sudo podman exec -it melissa \
                     melissa-server ...)",
                    self.socket.display()
                )
            })?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .context("HTTP handshake with the lead")?;
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::debug!("lead connection: {e}");
            }
        });
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header(hyper::header::HOST, "localhost");
        let body = match body {
            Some(v) => {
                req = req.header(hyper::header::CONTENT_TYPE, "application/json");
                Full::new(Bytes::from(v.to_string()))
            }
            None => Full::new(Bytes::new()),
        };
        let res = sender.send_request(req.body(body)?).await?;
        let status = res.status();
        let bytes = res.into_body().collect().await?.to_bytes();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
        };
        Ok((status, value))
    }

    async fn get(&self, path: &str) -> anyhow::Result<Value> {
        let (status, body) = self.request(Method::GET, path, None).await?;
        ok(status, body)
    }

    async fn post(&self, path: &str, body: Value) -> anyhow::Result<(StatusCode, Value)> {
        self.request(Method::POST, path, Some(body)).await
    }

    async fn events(&self, since: u64, wait: u64) -> anyhow::Result<EventPage> {
        let body = self
            .get(&format!("{}/events?since={since}&wait={wait}", self.base))
            .await?;
        Ok(serde_json::from_value(body)?)
    }

    /// Send a message; returns the turn and the event cursor to follow it from. While a turn
    /// runs the message waits in the queue: this waits (printing a line) until its turn starts.
    async fn send(&self, text: &str) -> anyhow::Result<(TurnId, u64)> {
        let (status, body) = self
            .post(&format!("{}/messages", self.base), json!({"text": text}))
            .await?;
        let body = ok(status, body)?;
        let since = body["since"]
            .as_u64()
            .context("no event cursor in the reply")?;
        if let Some(turn) = body["turn"].as_u64() {
            return Ok((turn, since));
        }
        let id = body["id"]
            .as_u64()
            .context("no turn or queue id in the reply")?;
        eprintln!(
            "[queued #{id}: starts when {} is done with the current turn]",
            self.who
        );
        self.wait_queued(id, since).await
    }

    /// Follow the events until queued message `id` starts its turn.
    async fn wait_queued(&self, id: u64, mut since: u64) -> anyhow::Result<(TurnId, u64)> {
        let mut started = false;
        loop {
            let page = self.events(since, 30).await?;
            for record in &page.events {
                match &record.event {
                    LeadEvent::MessageUnqueued { id: i, reason } if *i == id => {
                        if reason != "started" && reason != "steered" {
                            bail!("the queued message was {reason}");
                        }
                        started = true;
                    }
                    LeadEvent::UserMessage { turn, .. } if started => {
                        return Ok((*turn, record.seq - 1));
                    }
                    _ => {}
                }
            }
            since = page.next;
        }
    }
}

/// `name` as one URL path segment (agent names may contain spaces).
fn path_segment(name: &str) -> String {
    name.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn ok(status: StatusCode, body: Value) -> anyhow::Result<Value> {
    if status.is_success() {
        return Ok(body);
    }
    let msg = body["error"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| body.to_string());
    bail!("lead answered {status}: {msg}")
}

/// Prints events as plain text: the reply as it streams, everything else on its own line.
#[derive(Default)]
struct Printer {
    mid_line: bool,
}

impl Printer {
    fn line(&mut self, text: &str) {
        let mut out = std::io::stdout().lock();
        if self.mid_line {
            let _ = writeln!(out);
            self.mid_line = false;
        }
        let _ = writeln!(out, "{text}");
        let _ = out.flush();
    }

    fn event(&mut self, event: &LeadEvent) {
        match event {
            LeadEvent::Text { text, .. } => {
                let mut out = std::io::stdout().lock();
                let _ = write!(out, "{text}");
                let _ = out.flush();
                self.mid_line = !text.ends_with('\n');
            }
            // Shown as typed (chat) or given on the command line (ask); the whole reply was
            // already streamed.
            LeadEvent::UserMessage { .. } | LeadEvent::AgentMessage { .. } => {}
            // The agent's thinking is shown only in the web UI (Settings → Developer).
            LeadEvent::Thought { .. } => {}
            LeadEvent::ToolCall { title, status, .. } => {
                self.line(&format!("[tool] {} → {status}", one_line(title)));
            }
            LeadEvent::Permission {
                title, decision, ..
            } => {
                self.line(&format!("[permission] {} → {decision}", one_line(title)));
            }
            LeadEvent::ConfirmationNeeded {
                id,
                task,
                tool,
                reason,
                agent,
            } => {
                let who = match agent {
                    Some(a) => format!("agent {a} (task {task})"),
                    None => format!("task {task}"),
                };
                self.line(&format!(
                    "[confirm #{id}] {who} wants `{tool}`: {reason}\n    approve: melissa-server confirm approve {id}    deny: melissa-server confirm deny {id}"
                ));
            }
            LeadEvent::ConfirmationResolved { id, outcome } => {
                self.line(&format!("[confirm #{id}] {outcome}"));
            }
            LeadEvent::AgentCreated {
                agent,
                name,
                backend,
            } => {
                self.line(&format!("[agent {name}] created ({agent}, {backend})"));
            }
            LeadEvent::AgentDeleted { agent, name } => {
                self.line(&format!("[agent {name}] deleted ({agent})"));
            }
            LeadEvent::AgentStarted { name, goal, .. } => {
                self.line(&format!("[agent {name}] job: {}", one_line(goal)));
            }
            LeadEvent::AgentMessaged { name, .. } => {
                self.line(&format!("[agent {name}] messaged"));
            }
            LeadEvent::AgentFinished {
                name,
                status,
                error,
                ..
            } => match error {
                Some(e) => self.line(&format!("[agent {name}] {status}: {}", one_line(e))),
                None => self.line(&format!("[agent {name}] {status}")),
            },
            LeadEvent::WorkspaceReassigned {
                workspace,
                from,
                to,
                by,
            } => {
                self.line(&format!(
                    "[workspace {workspace}] now looked after by {to} (was: {}; by {by})",
                    from.as_deref().unwrap_or("no agent")
                ));
            }
            LeadEvent::WorkspaceForceDeleted { workspace, agent } => {
                self.line(&format!(
                    "[workspace {workspace}] force-deleted by the user (told: {})",
                    agent.as_deref().unwrap_or("the lead agent only")
                ));
            }
            LeadEvent::SessionOpened { backend, resumed } => {
                let how = if *resumed {
                    "resumed the previous session"
                } else {
                    "new session"
                };
                self.line(&format!("[session] {backend}: {how}"));
            }
            LeadEvent::BackendSwitched {
                backend,
                model,
                model_label,
                effort_label,
                fast,
            } => {
                let mut parts: Vec<String> = Vec::new();
                parts.extend(model_label.clone().or(model.clone()));
                parts.extend(effort_label.as_ref().map(|e| format!("reasoning {e}")));
                if *fast {
                    parts.push("fast".into());
                }
                let model = if parts.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", parts.join(", "))
                };
                self.line(&format!(
                    "[session] switched to {backend}{model}; it takes over from the next message"
                ));
            }
            LeadEvent::BackendFailed { backend, error } => {
                self.line(&format!(
                    "[session] {backend} could not start: {}",
                    one_line(error)
                ));
            }
            LeadEvent::SessionLost { reason } => {
                self.line(&format!(
                    "[session] lost: {}. The agent's context may be lost; the next message starts \
                     a new session.",
                    one_line(reason)
                ));
            }
            LeadEvent::Error { message, .. } => self.line(&format!("[error] {message}")),
            LeadEvent::Tainted { tool, subject } => {
                let from = match (tool, subject) {
                    (Some(t), Some(s)) => format!(" ({t}: {s})"),
                    (Some(t), None) => format!(" ({t})"),
                    _ => String::new(),
                };
                self.line(&format!(
                    "[taint] read untrusted content{from}; changes now need your OK until a fresh start"
                ));
            }
            LeadEvent::HelperReport {
                name,
                message: Some(kind),
                ..
            } => {
                self.line(&format!(
                    "[agent {name}] message ({kind}) passed to the lead agent"
                ));
            }
            LeadEvent::HelperReport { name, status, .. } => {
                self.line(&format!(
                    "[agent {name}] report ({status}) passed to the lead agent"
                ));
            }
            LeadEvent::MessageQueued { id, text, from, .. } => {
                self.line(&format!("[queued #{id}] ({from}) {}", one_line(text)));
            }
            LeadEvent::MessageUnqueued { id, reason } => {
                self.line(&format!("[queued #{id}] {reason}"));
            }
            LeadEvent::NewChat => self.line("[new chat] the earlier chat was archived"),
            LeadEvent::FreshStart => self.line("[fresh start] memory cleared; the chat goes on"),
            LeadEvent::SetupPassword { app, .. } => self.line(&format!(
                "[setup password] {app}: shown in the web UI (Melissa's chat card)"
            )),
            LeadEvent::EarlierStart
            | LeadEvent::EarlierEnd
            | LeadEvent::FilesChanged
            | LeadEvent::ContextCompacted { .. } => {}
            LeadEvent::TurnSteered { .. } => {
                self.line("[steered] stopping the turn for your message")
            }
            LeadEvent::TurnFinished { stop_reason, .. } => {
                if stop_reason == "end_turn" {
                    if self.mid_line {
                        self.line("");
                    }
                } else {
                    self.line(&format!("[turn ended: {stop_reason}]"));
                }
            }
        }
    }
}

fn one_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    if line.chars().count() > 160 {
        format!("{}...", line.chars().take(157).collect::<String>())
    } else {
        line.to_string()
    }
}

/// Follow `turn` from cursor `since` until it ends. Ctrl-C cancels the turn (once; a second
/// Ctrl-C quits). Returns the next cursor and the stop reason.
async fn follow_turn(
    client: &Client,
    printer: &mut Printer,
    mut since: u64,
    turn: TurnId,
) -> anyhow::Result<(u64, String)> {
    let mut cancelled = false;
    loop {
        tokio::select! {
            page = client.events(since, POLL_SECS) => {
                let page = page?;
                if page.missed {
                    printer.line("[some events were missed]");
                }
                for record in page.events {
                    since = record.seq;
                    printer.event(&record.event);
                    if let LeadEvent::TurnFinished { turn: t, stop_reason } = &record.event {
                        if *t == turn {
                            return Ok((since, stop_reason.clone()));
                        }
                    }
                }
                since = since.max(page.next);
            }
            _ = tokio::signal::ctrl_c() => {
                if cancelled {
                    printer.line("[quit]");
                    std::process::exit(130);
                }
                cancelled = true;
                printer.line("[cancelling the turn; Ctrl-C again quits]");
                if let Err(e) = client.post(&format!("{}/cancel", client.base), json!({})).await {
                    printer.line(&format!("[error] cancel failed: {e:#}"));
                }
            }
        }
    }
}

/// `ask`: one message, stream the turn, exit (status 1 if the turn failed).
pub async fn ask(client: &Client, message: &str) -> anyhow::Result<()> {
    let (turn, since) = client.send(message).await?;
    let mut printer = Printer::default();
    let (_, stop) = follow_turn(client, &mut printer, since, turn).await?;
    if stop == "error" {
        std::process::exit(1);
    }
    Ok(())
}

/// `fresh-start [--agent <agent>]` (old names: `new-chat`, `chat --new`): clear the memory
/// of the lead agent or an agent; the chat goes on.
pub async fn fresh_start(client: &Client, agent: Option<&str>) -> anyhow::Result<()> {
    let path = match agent {
        None => "/api/lead/fresh-start".to_string(),
        Some(a) => format!("/api/agents/{}/fresh-start", path_segment(a)),
    };
    let (status, body) = client.post(&path, json!({})).await?;
    ok(status, body)?;
    println!("Fresh start: the next message begins without the earlier conversation.");
    Ok(())
}

/// `chat`: a line-based conversation. Events that arrive between turns (an agent finishing,
/// a confirmation) are shown while waiting for input.
pub async fn chat(client: &Client) -> anyhow::Result<()> {
    let status = client.get(&format!("{}/status", client.base)).await?;
    let mut since = status["last_seq"].as_u64().unwrap_or(0);
    println!(
        "Talking to {}. Enter sends; Ctrl-C cancels a running turn; \
         Ctrl-D (or Ctrl-C at the prompt) quits.",
        client.who
    );
    let mut printer = Printer::default();
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    loop {
        prompt();
        let line = loop {
            tokio::select! {
                line = lines.next_line() => break line?,
                page = client.events(since, POLL_SECS) => {
                    let page = page?;
                    if !page.events.is_empty() {
                        for record in &page.events {
                            printer.event(&record.event);
                        }
                        since = page.next;
                        prompt();
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    println!();
                    // The stdin reader thread would keep the runtime alive.
                    std::process::exit(0);
                }
            }
        };
        let Some(line) = line else {
            println!();
            return Ok(());
        };
        if line.trim().is_empty() {
            continue;
        }
        match client.send(&line).await {
            Ok((turn, from)) => {
                let (next, _) = follow_turn(client, &mut printer, from, turn).await?;
                since = next;
            }
            Err(e) => printer.line(&format!("[error] {e:#}")),
        }
    }
}

fn prompt() {
    let mut out = std::io::stdout().lock();
    let _ = write!(out, "you> ");
    let _ = out.flush();
}

/// `confirm list|approve <id>|deny <id>`.
pub async fn confirm(client: &Client, action: &str, id: Option<u64>) -> anyhow::Result<()> {
    match (action, id) {
        ("list", None) => {
            let body = client.get("/api/confirmations").await?;
            let list = body["confirmations"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if list.is_empty() {
                println!("No pending confirmations.");
            }
            for c in list {
                println!(
                    "#{}  task {}  {}  ({})\n    args: {}",
                    c["id"],
                    c["task"],
                    c["tool"].as_str().unwrap_or("?"),
                    c["reason"].as_str().unwrap_or(""),
                    one_line(&c["args"].to_string())
                );
            }
            Ok(())
        }
        ("approve" | "deny", Some(id)) => {
            let (status, body) = client
                .post(&format!("/api/confirmations/{id}/{action}"), json!({}))
                .await?;
            let body = ok(status, body)?;
            println!(
                "#{id} {}: {}",
                body["verdict"].as_str().unwrap_or(action),
                body["tool"].as_str().unwrap_or("?")
            );
            Ok(())
        }
        _ => bail!("usage: melissa-server confirm list | approve <id> | deny <id>"),
    }
}

/// `agent list` (also `agents`): this device's agents.
pub async fn agents(client: &Client) -> anyhow::Result<()> {
    let body = client.get("/api/agents").await?;
    let list = body["agents"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("No agents yet (the lead agent creates them).");
    }
    for b in list {
        println!(
            "{:<12}  {:<16}  {:<8}  {}{}",
            b["name"].as_str().unwrap_or("?"),
            b["id"].as_str().unwrap_or("?"),
            b["status"].as_str().unwrap_or("?"),
            b["backend"].as_str().unwrap_or("?"),
            b["job"]
                .as_str()
                .map(|j| format!("  job: {}", one_line(j)))
                .unwrap_or_default()
        );
    }
    Ok(())
}

/// `backend [list]`: the backends the lead agent (or an agent) can use.
pub async fn backends(client: &Client) -> anyhow::Result<()> {
    let body = client.get(&format!("{}/backends", client.base)).await?;
    let list = body["backends"].as_array().cloned().unwrap_or_default();
    for b in &list {
        let name = b["name"].as_str().unwrap_or("?");
        let mark = if b["current"] == true { "*" } else { " " };
        let state = match (b["available"].as_bool(), b["reason"].as_str()) {
            (Some(true), _) => "ready".to_string(),
            (_, Some("not_logged_in")) => "not logged in".to_string(),
            (_, Some("sandbox")) => "sandbox cannot start".to_string(),
            _ => "not installed".to_string(),
        };
        let model = b["model"]
            .as_str()
            .map(|m| format!("  model: {m}"))
            .unwrap_or_default();
        println!("{mark} {name:<16} {state}{model}");
        if let Some(models) = b["models"].as_array() {
            let ids: Vec<&str> = models.iter().filter_map(|m| m["id"].as_str()).collect();
            println!("    models: {}", ids.join(", "));
        }
    }
    println!(
        "(* = what {} runs on, or will from its next message)",
        client.who
    );
    Ok(())
}

/// `backend refresh`: ask every usable agent for its models now (no prompt; each probe
/// takes up to 15 s).
pub async fn refresh_models(client: &Client) -> anyhow::Result<()> {
    let (status, body) = client.post("/api/lead/backends/refresh", json!({})).await?;
    let body = ok(status, body)?;
    for p in body["probes"].as_array().cloned().unwrap_or_default() {
        let name = p["backend"].as_str().unwrap_or("?");
        let what = match p["result"].as_str() {
            Some("refreshed") => format!("{} models", p["models"]),
            Some("no_models") => "lists no models".into(),
            Some("skipped") => "a check is already running".into(),
            _ => format!(
                "could not ask it: {}",
                p["error"].as_str().unwrap_or("unknown error")
            ),
        };
        println!("  {name:<16} {what}");
    }
    Ok(())
}

/// `backend use <name> [--model <id>]`.
pub async fn use_backend(client: &Client, name: &str, model: Option<&str>) -> anyhow::Result<()> {
    let (status, body) = client
        .post(
            &format!("{}/backend", client.base),
            json!({"name": name, "model": model}),
        )
        .await?;
    let body = ok(status, body)?;
    let what = match body["model"].as_str() {
        Some(m) => format!("{name} ({m})"),
        None => name.to_string(),
    };
    if body["changed"] == false {
        println!("{} already uses {what}.", client.who);
    } else if body["after_turn"] == true {
        println!(
            "{} switches to {what} once the running turn has ended.",
            client.who
        );
    } else {
        println!(
            "{} uses {what} from the next message, picking up where you left off.",
            client.who
        );
    }
    Ok(())
}

/// `agent rename <agent> <new name>`.
pub async fn rename_agent(client: &Client, agent: &str, name: &str) -> anyhow::Result<()> {
    let (status, body) = client
        .post(
            &format!("/api/agents/{}/rename", path_segment(agent)),
            json!({"name": name}),
        )
        .await?;
    let body = ok(status, body)?;
    println!(
        "{} is now called {}",
        body["id"].as_str().unwrap_or(agent),
        body["name"].as_str().unwrap_or(name)
    );
    Ok(())
}

fn print_workspace(c: &Value) {
    let agent = match (c["agent_name"].as_str(), c["agent"].as_str()) {
        (Some(name), Some(id)) => format!("{name} ({id})"),
        (None, Some(id)) => id.to_string(),
        _ => "no agent (only you or the lead agent with your approval)".into(),
    };
    println!(
        "{:<14}  {}{}",
        c["workspace"].as_str().unwrap_or("?"),
        agent,
        c["legacy_owner"]
            .as_str()
            .map(|o| format!("  [old owner label: {o}]"))
            .unwrap_or_default()
    );
}

/// `workspace list`: every workspace and the agent that looks after it.
pub async fn workspaces(client: &Client) -> anyhow::Result<()> {
    let body = client.get("/api/workspaces").await?;
    let list = body["workspaces"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("No workspaces (no apps installed).");
    }
    for c in &list {
        print_workspace(c);
    }
    Ok(())
}

/// `workspace reassign <workspace> --to <agent>`: the user's own reassignment (no confirmation).
pub async fn reassign(client: &Client, workspace: &str, to: &str) -> anyhow::Result<()> {
    let (status, body) = client
        .post(
            &format!("/api/workspaces/{}/reassign", path_segment(workspace)),
            json!({"to": to}),
        )
        .await?;
    let body = ok(status, body)?;
    print_workspace(&body);
    Ok(())
}

/// `app details <app>`: the app's status, as the app dialog shows it.
pub async fn app_details(client: &Client, app: &str) -> anyhow::Result<()> {
    let s = client
        .get(&format!("/api/apps/{}", path_segment(app)))
        .await?;
    let field = |k: &str| s[k].as_str().map(str::to_string);
    let pods = s["pods"].as_array().cloned().unwrap_or_default();
    let restarts: u64 = pods.iter().filter_map(|p| p["restarts"].as_u64()).sum();
    println!("{app}: {}", s["phase"].as_str().unwrap_or("unknown"));
    let agent = field("agent_name").or_else(|| field("agent"));
    println!(
        "  looked after by  {}",
        agent.as_deref().unwrap_or("no agent")
    );
    for (label, key) in [
        ("device", "device"),
        ("link", "url"),
        ("version", "version"),
        ("image", "image"),
    ] {
        if let Some(v) = field(key) {
            println!("  {label:<16} {v}");
        }
    }
    if s["installed"] == true {
        println!(
            "  pods             {}/{} ready, {restarts} restart(s)",
            s["ready_replicas"].as_u64().unwrap_or(0),
            s["replicas"].as_u64().unwrap_or(0)
        );
    }
    Ok(())
}

/// `app logs <app>`: the last log lines (untrusted app output, printed as plain text).
pub async fn app_logs(client: &Client, app: &str) -> anyhow::Result<()> {
    let v = client
        .get(&format!("/api/apps/{}/logs?lines=200", path_segment(app)))
        .await?;
    eprintln!(
        "{} / {} (last {} lines)",
        v["pod"].as_str().unwrap_or("?"),
        v["container"].as_str().unwrap_or("?"),
        v["lines"]
    );
    // Already stripped of control characters by the tool.
    println!("{}", v["log"].as_str().unwrap_or_default());
    Ok(())
}

/// `app network <app> [changes]`: what the app may reach; with changes (`change` not empty),
/// the owner's own change first (in force at once, audited).
pub async fn app_network(client: &Client, app: &str, change: &Value) -> anyhow::Result<()> {
    let path = format!("/api/apps/{}/network", path_segment(app));
    if change.as_object().is_some_and(|c| !c.is_empty()) {
        let (status, body) = client.post(&path, change.clone()).await?;
        let body = ok(status, body)?;
        for c in body["changed"].as_array().into_iter().flatten() {
            println!("{}", c.as_str().unwrap_or("?"));
        }
        for w in body["warnings"].as_array().into_iter().flatten() {
            eprintln!("warning: {}", w.as_str().unwrap_or("?"));
        }
    }
    let v = client.get(&path).await?;
    let on = |k: &str| if v[k] == true { "yes" } else { "no" };
    println!("{app} may reach:");
    println!("  internet         {}", on("internet"));
    println!("  home network     {}", on("lan"));
    let port_words = |l: &Value| {
        let ports: Vec<String> = l["ports"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|p| {
                format!(
                    "{} {}/{}",
                    p["name"].as_str().unwrap_or("?"),
                    p["port"],
                    p["protocol"].as_str().unwrap_or("?")
                )
            })
            .collect();
        let inactive = l["problem"]
            .as_str()
            .map(|p| format!(" (inactive: {p})"))
            .unwrap_or_default();
        format!(
            "{}: {}{inactive}",
            l["app"].as_str().unwrap_or("?"),
            if l["port"].is_null() {
                "all ports".to_string()
            } else {
                ports.join(", ")
            }
        )
    };
    for (label, key) in [("links to", "links"), ("linked from", "incoming")] {
        let list = v[key].as_array().cloned().unwrap_or_default();
        if list.is_empty() {
            println!("  {label:<16} none");
        }
        for l in &list {
            println!("  {label:<16} {}", port_words(l));
        }
    }
    for p in v["pending"].as_array().into_iter().flatten() {
        println!(
            "  waiting for you  confirmation {}: {}",
            p["id"],
            p["reason"].as_str().unwrap_or("?")
        );
    }
    Ok(())
}

/// `app force-delete <app> --yes`: remove the app and its data; its agent and the lead agent are told.
pub async fn app_force_delete(client: &Client, app: &str) -> anyhow::Result<()> {
    eprintln!("Deleting {app} and all its data (this can take a minute)…");
    let (status, body) = client
        .post(
            &format!("/api/apps/{}/force-delete", path_segment(app)),
            json!({"confirm": app}),
        )
        .await?;
    let body = ok(status, body)?;
    println!("{app}: deleted");
    for r in body["removed"].as_array().into_iter().flatten() {
        println!("  removed  {}", r.as_str().unwrap_or("?"));
    }
    let told = match (body["agent_name"].as_str(), body["agent_notice"].as_str()) {
        (Some(name), Some(n)) => format!("agent {name} ({n}) and the lead agent"),
        _ => "the lead agent".into(),
    };
    println!("  told     {told}");
    Ok(())
}

/// `tool --wait <name> [args]`: run one tool in the daemon (operator call, unattended
/// policy). While it waits for a confirmation, says which one and how to answer it (on
/// stderr, so stdout stays the result JSON).
pub async fn tool(client: &Client, name: &str, args: Value) -> anyhow::Result<()> {
    let path = format!("/api/tools/{name}");
    let call = client.post(&path, args);
    tokio::pin!(call);
    let mut announced = std::collections::BTreeSet::new();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    let (status, body) = loop {
        tokio::select! {
            res = &mut call => break res?,
            _ = tick.tick() => {
                // Operator calls run as task 0; show only confirmations for this tool.
                let Ok(list) = client.get("/api/confirmations").await else { continue };
                for c in list["confirmations"].as_array().into_iter().flatten() {
                    let Some(id) = c["id"].as_u64() else { continue };
                    if c["task"] == 0 && c["tool"] == name && announced.insert(id) {
                        eprintln!(
                            "waiting for confirmation #{id} ({}): approve with \
                             `melissa-server confirm approve {id}`, or deny",
                            c["reason"].as_str().unwrap_or("")
                        );
                    }
                }
            }
        }
    };
    let body = ok(status, body)?;
    println!("{}", serde_json::to_string_pretty(&body["result"])?);
    Ok(())
}

// --- Settings: providers and updates ----------------------------------------------------------

/// `provider list`: every agent program, its state and the owner's settings (secret values
/// are never sent; they show as "set").
pub async fn providers(client: &Client) -> anyhow::Result<()> {
    let body = client.get("/api/settings/providers").await?;
    for p in body["providers"].as_array().cloned().unwrap_or_default() {
        let id = p["id"].as_str().unwrap_or("?");
        let version = p["version"]
            .as_str()
            .map(|v| format!(" v{v}"))
            .unwrap_or_default();
        let mark = if p["enabled"] == true { " " } else { "-" };
        println!(
            "{mark} {id:<10}{version:<12} {}",
            p["status"].as_str().unwrap_or("")
        );
        let used: Vec<&str> = p["used_by"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if !used.is_empty() {
            println!("    used by: {}", used.join(", "));
        }
        let s = &p["settings"];
        if let Some(c) = s["command"].as_str() {
            println!("    program: {c}");
        }
        if let Some(a) = s["args"].as_array().filter(|a| !a.is_empty()) {
            let a: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
            println!("    extra arguments: {}", a.join(" "));
        }
        for v in s["env"].as_array().cloned().unwrap_or_default() {
            let name = v["name"].as_str().unwrap_or("?");
            match v["value"].as_str() {
                Some(val) => println!("    {name}={val}"),
                None => println!("    {name} (set, hidden)"),
            }
        }
        let install = &p["install"];
        match install["way"].as_str() {
            Some("button") => println!(
                "    install: {} (Settings → Providers → Install)",
                install["state"].as_str().unwrap_or("idle")
            ),
            Some(_) => {
                if let Some(text) = install["text"].as_str() {
                    println!("    install: {text}");
                }
                if let Some(cmd) = install["command"].as_str() {
                    println!("      {cmd}");
                }
            }
            None => {}
        }
        match p["sign_in"]["step"].as_str() {
            Some("sign_in") => println!("    sign in: needed (Settings → Providers)"),
            Some("paste_code") => println!(
                "    sign in: open {} and paste the code in Settings → Providers",
                p["sign_in"]["url"].as_str().unwrap_or("the link")
            ),
            Some("device_code") => println!(
                "    sign in: open {} and confirm the code {}",
                p["sign_in"]["url"].as_str().unwrap_or("the link"),
                p["sign_in"]["code"].as_str().unwrap_or("?")
            ),
            Some("command") => println!(
                "    sign in: {}",
                p["sign_in"]["command"].as_str().unwrap_or("")
            ),
            Some(step) => println!("    sign in: {step}"),
            None => {}
        }
    }
    println!("(- = disabled)");
    if body["restart_needed"] == true {
        println!("Saved changes apply once Melissa is idle (she restarts herself where she can).");
    }
    Ok(())
}

/// `provider enable|disable|set <id> ...`: one change through the daemon (checked and
/// audited there).
pub async fn provider_change(client: &Client, id: &str, change: Value) -> anyhow::Result<()> {
    let (status, body) = client
        .post(
            &format!("/api/settings/providers/{}", path_segment(id)),
            change,
        )
        .await?;
    let body = ok(status, body)?;
    println!(
        "{}: {}",
        body["id"].as_str().unwrap_or(id),
        body["status"].as_str().unwrap_or("saved")
    );
    if body["restart_needed"] == true {
        println!("Saved. Melissa applies it once she is idle.");
    }
    Ok(())
}

/// `update status`.
pub async fn update_status(client: &Client) -> anyhow::Result<()> {
    let body = client.get("/api/settings/updates").await?;
    let r = &body["release"];
    println!(
        "Melissa {} ({})",
        r["version"].as_str().unwrap_or("?"),
        r["runtime"]
            .as_str()
            .unwrap_or(body["host"].as_str().unwrap_or("?"))
    );
    for line in image_lines(&body["state"]) {
        println!("{line}");
    }
    let actions: Vec<&str> = body["actions"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if actions.is_empty() {
        for c in body["commands"].as_array().cloned().unwrap_or_default() {
            println!(
                "{}\n  {}",
                c["label"].as_str().unwrap_or(""),
                c["command"].as_str().unwrap_or("")
            );
        }
    } else {
        println!("  can: {}", actions.join(", "));
    }
    for a in body["agents"].as_array().cloned().unwrap_or_default() {
        println!(
            "  agent {}: {}",
            a["name"].as_str().unwrap_or("?"),
            a["version"].as_str().unwrap_or("?")
        );
    }
    Ok(())
}

/// The container image part of `update status`: one line per image (with its short digest, which tells
/// two builds with the same version apart), then the helper's last action.
fn image_lines(state: &Value) -> Vec<String> {
    let st = &state["status"];
    let mut lines = Vec::new();
    for (label, key) in [
        ("running", "running"),
        ("staged", "staged"),
        ("available", "available"),
        ("previous", "previous"),
    ] {
        if let Some(img) = st[key]["image"].as_str() {
            let mut line = format!("  {label:<10} {img}");
            if let Some(ver) = st[key]["version"].as_str() {
                line.push_str(&format!(" {ver}"));
            }
            if let Some(d) = st[key]["digest"].as_str().or(st[key]["id"].as_str()) {
                line.push_str(&format!(" {}", d.chars().take(19).collect::<String>()));
            }
            lines.push(line);
        }
    }
    if let Some(last) = state["last"].as_object() {
        let ok = last.get("ok") == Some(&Value::Bool(true));
        let mut line = format!(
            "  last {}: {}",
            last.get("action").and_then(Value::as_str).unwrap_or("?"),
            if ok { "ok" } else { "failed" }
        );
        if let Some(m) = last
            .get("message")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
        {
            line.push_str(&format!(" ({m})"));
        }
        lines.push(line);
    }
    lines
}

/// `update check|apply|rollback|restart [--yes]`.
pub async fn update_action(client: &Client, action: &str, yes: bool) -> anyhow::Result<()> {
    let (status, body) = client
        .post(
            &format!("/api/settings/updates/{}", path_segment(action)),
            json!({"confirm": yes}),
        )
        .await?;
    if status == StatusCode::CONFLICT {
        bail!("`update {action}` restarts Melissa; add --yes to confirm");
    }
    let body = ok(status, body)?;
    println!("{}", body["message"].as_str().unwrap_or("started"));
    Ok(())
}

/// `setup-code`: the code that claims a box nobody owns yet (a fresh one when it expired).
pub async fn setup_code(client: &Client) -> anyhow::Result<()> {
    let body = client.get("/api/host/setup-code").await?;
    if body["claimed"].as_bool() == Some(true) {
        println!(
            "This Melissa already has an owner. Lost the password? melissa-server reset-owner"
        );
        return Ok(());
    }
    let code = body["code"].as_str().context("no code in the reply")?;
    println!("Setup code: {code}");
    let left = body["expires"]
        .as_u64()
        .map(|e| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            e.saturating_sub(now).div_ceil(60)
        })
        .unwrap_or(60);
    println!(
        "Open Melissa's web UI and enter it to become the owner. It works for {left} more minutes."
    );
    Ok(())
}

/// `reset-owner`: a one-hour link that sets a new password for the owner.
pub async fn reset_owner(client: &Client, listen: SocketAddr) -> anyhow::Result<()> {
    let (status, body) = client.post("/api/host/reset-owner", json!({})).await?;
    let body = ok(status, body)?;
    let path = body["path"].as_str().context("no link in the reply")?;
    let base = if listen.ip().is_unspecified() {
        format!("http://<this box>:{}", listen.port())
    } else {
        format!("http://{listen}")
    };
    println!("Open this link within 1 hour to set a new owner password:");
    println!("  {base}{path}");
    println!("It works once. Every browser signed in as the owner is signed out.");
    Ok(())
}

/// `factory-reset --confirm <word>`: the daemon checks the word, writes the request and tidies
/// up; the root unit erases and starts Melissa again.
pub async fn factory_reset(client: &Client, word: &str) -> anyhow::Result<()> {
    let (status, body) = client
        .post("/api/settings/reset", json!({"confirm": word}))
        .await?;
    ok(status, body)?;
    println!("Resetting Melissa. She starts again in a few minutes, unclaimed.");
    println!("Then the setup code: melissa-server setup-code");
    Ok(())
}
