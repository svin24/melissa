//! Command-line client for a running queen: `chat`, `ask` (with the Queen or, with `--bee`,
//! any bee), `confirm`, `bee list|rename`, `cell list|reassign`.
//!
//! Talks to the daemon's JSON API (`/api/...`) on its configured listen address (loopback)
//! with plain HTTP/1.1, one connection per request. Sends no `Origin` header, like any
//! non-browser client, so the same-origin check lets it through.

use std::{io::Write, net::SocketAddr, time::Duration};

use anyhow::{bail, Context};
use http_body_util::{BodyExt, Full};
use hyper::{body::Bytes, Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use melissa_agent::queen::events::{EventPage, QueenEvent, TurnId};
use serde_json::{json, Value};
use tokio::io::AsyncBufReadExt;

/// Seconds one events request waits for news.
const POLL_SECS: u64 = 20;

pub struct Client {
    addr: SocketAddr,
    /// The conversation `chat`/`ask` talk to: `/api/queen` or `/api/bees/<bee>`.
    base: String,
    /// Who that is, for messages ("the Queen", "bee Ada").
    who: String,
}

impl Client {
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            base: "/api/queen".into(),
            who: "the Queen".into(),
        }
    }

    /// Talk to bee `bee` (name or id) instead of the Queen.
    pub fn with_bee(mut self, bee: &str) -> Self {
        self.base = format!("/api/bees/{}", path_segment(bee));
        self.who = format!("bee {bee}");
        self
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> anyhow::Result<(StatusCode, Value)> {
        let stream = tokio::net::TcpStream::connect(self.addr)
            .await
            .with_context(|| {
                format!(
                    "cannot reach the queen at http://{} (is `melissa-queen` running?)",
                    self.addr
                )
            })?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .context("HTTP handshake with the queen")?;
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::debug!("queen connection: {e}");
            }
        });
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header(hyper::header::HOST, self.addr.to_string());
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

    /// Send a message; returns the turn and the event cursor to follow it from.
    async fn send(&self, text: &str) -> anyhow::Result<(TurnId, u64)> {
        let (status, body) = self
            .post(&format!("{}/messages", self.base), json!({"text": text}))
            .await?;
        if status == StatusCode::CONFLICT {
            bail!(
                "{} is still working on another message (from this or another client, or a \
                 job from the Queen); wait for it or cancel it",
                self.who
            );
        }
        let body = ok(status, body)?;
        let turn = body["turn"].as_u64().context("no turn id in the reply")?;
        let since = body["since"]
            .as_u64()
            .context("no event cursor in the reply")?;
        Ok((turn, since))
    }
}

/// `name` as one URL path segment (bee names may contain spaces).
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
    bail!("queen answered {status}: {msg}")
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

    fn event(&mut self, event: &QueenEvent) {
        match event {
            QueenEvent::Text { text, .. } => {
                let mut out = std::io::stdout().lock();
                let _ = write!(out, "{text}");
                let _ = out.flush();
                self.mid_line = !text.ends_with('\n');
            }
            // Shown as typed (chat) or given on the command line (ask); the whole reply was
            // already streamed.
            QueenEvent::UserMessage { .. } | QueenEvent::AgentMessage { .. } => {}
            QueenEvent::ToolCall { title, status, .. } => {
                self.line(&format!("[tool] {} → {status}", one_line(title)));
            }
            QueenEvent::Permission {
                title, decision, ..
            } => {
                self.line(&format!("[permission] {} → {decision}", one_line(title)));
            }
            QueenEvent::ConfirmationNeeded {
                id,
                task,
                tool,
                reason,
            } => {
                self.line(&format!(
                    "[confirm #{id}] task {task} wants `{tool}`: {reason}\n    approve: melissa-queen confirm approve {id}    deny: melissa-queen confirm deny {id}"
                ));
            }
            QueenEvent::ConfirmationResolved { id, outcome } => {
                self.line(&format!("[confirm #{id}] {outcome}"));
            }
            QueenEvent::BeeCreated { bee, name, backend } => {
                self.line(&format!("[bee {name}] created ({bee}, {backend})"));
            }
            QueenEvent::BeeStarted { name, goal, .. } => {
                self.line(&format!("[bee {name}] job: {}", one_line(goal)));
            }
            QueenEvent::BeeFinished {
                name,
                status,
                error,
                ..
            } => match error {
                Some(e) => self.line(&format!("[bee {name}] {status}: {}", one_line(e))),
                None => self.line(&format!("[bee {name}] {status}")),
            },
            QueenEvent::CellReassigned { cell, from, to, by } => {
                self.line(&format!(
                    "[cell {cell}] now looked after by {to} (was: {}; by {by})",
                    from.as_deref().unwrap_or("no bee")
                ));
            }
            QueenEvent::SessionOpened { backend, resumed } => {
                let how = if *resumed {
                    "resumed the previous session"
                } else {
                    "new session"
                };
                self.line(&format!("[session] {backend}: {how}"));
            }
            QueenEvent::BackendSwitched {
                backend,
                model,
                model_label,
            } => {
                let model = match (model_label, model) {
                    (Some(l), _) => format!(" ({l})"),
                    (None, Some(m)) => format!(" ({m})"),
                    (None, None) => String::new(),
                };
                self.line(&format!(
                    "[session] switched to {backend}{model}; it takes over from the next message"
                ));
            }
            QueenEvent::BackendFailed { backend, error } => {
                self.line(&format!(
                    "[session] {backend} could not start: {}",
                    one_line(error)
                ));
            }
            QueenEvent::SessionLost { reason } => {
                self.line(&format!(
                    "[session] lost: {}. The agent's context may be lost; the next message starts \
                     a new session.",
                    one_line(reason)
                ));
            }
            QueenEvent::Error { message, .. } => self.line(&format!("[error] {message}")),
            QueenEvent::Tainted { tool, subject } => {
                let from = match (tool, subject) {
                    (Some(t), Some(s)) => format!(" ({t}: {s})"),
                    (Some(t), None) => format!(" ({t})"),
                    _ => String::new(),
                };
                self.line(&format!(
                    "[taint] read untrusted content{from}; changes now need your OK until a new chat"
                ));
            }
            QueenEvent::NewChat => self.line("[new chat] the earlier chat was archived"),
            QueenEvent::EarlierStart | QueenEvent::EarlierEnd => {}
            QueenEvent::TurnFinished { stop_reason, .. } => {
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
                    if let QueenEvent::TurnFinished { turn: t, stop_reason } = &record.event {
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

/// `new-chat` (and `chat --new`): archive the Queen's chat and start a fresh one.
pub async fn new_chat(client: &Client) -> anyhow::Result<()> {
    let (status, body) = client.post("/api/queen/new-chat", json!({})).await?;
    let body = ok(status, body)?;
    match body["archived"].as_u64() {
        Some(id) => println!(
            "Started a new chat. The earlier one is kept read-only (past chat {id}, in the web UI)."
        ),
        None => println!("Started a new chat (the earlier one was empty)."),
    }
    Ok(())
}

/// `chat`: a line-based conversation. Events that arrive between turns (a bee finishing,
/// a confirmation) are shown while waiting for input.
pub async fn chat(client: &Client) -> anyhow::Result<()> {
    let status = client.get(&format!("{}/status", client.base)).await?;
    let mut since = status["last_seq"].as_u64().unwrap_or(0);
    println!(
        "Talking to {} at http://{}. Enter sends; Ctrl-C cancels a running turn; \
         Ctrl-D (or Ctrl-C at the prompt) quits.",
        client.who, client.addr
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
        _ => bail!("usage: melissa-queen confirm list | approve <id> | deny <id>"),
    }
}

/// `bee list` (also `bees`): this comb's bees.
pub async fn bees(client: &Client) -> anyhow::Result<()> {
    let body = client.get("/api/bees").await?;
    let list = body["bees"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("No bees yet (the Queen creates them).");
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

/// `backend [list]`: the backends the Queen (or a bee) can use.
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
    let (status, body) = client
        .post("/api/queen/backends/refresh", json!({}))
        .await?;
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

/// `bee rename <bee> <new name>`.
pub async fn rename_bee(client: &Client, bee: &str, name: &str) -> anyhow::Result<()> {
    let (status, body) = client
        .post(
            &format!("/api/bees/{}/rename", path_segment(bee)),
            json!({"name": name}),
        )
        .await?;
    let body = ok(status, body)?;
    println!(
        "{} is now called {}",
        body["id"].as_str().unwrap_or(bee),
        body["name"].as_str().unwrap_or(name)
    );
    Ok(())
}

fn print_cell(c: &Value) {
    let bee = match (c["bee_name"].as_str(), c["bee"].as_str()) {
        (Some(name), Some(id)) => format!("{name} ({id})"),
        (None, Some(id)) => id.to_string(),
        _ => "no bee (only you or the Queen with your approval)".into(),
    };
    println!(
        "{:<14}  {}{}",
        c["cell"].as_str().unwrap_or("?"),
        bee,
        c["legacy_owner"]
            .as_str()
            .map(|o| format!("  [old owner label: {o}]"))
            .unwrap_or_default()
    );
}

/// `cell list`: every cell and the bee that looks after it.
pub async fn cells(client: &Client) -> anyhow::Result<()> {
    let body = client.get("/api/cells").await?;
    let list = body["cells"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("No cells (no apps installed).");
    }
    for c in &list {
        print_cell(c);
    }
    Ok(())
}

/// `cell reassign <cell> --to <bee>`: the user's own reassignment (no confirmation).
pub async fn reassign(client: &Client, cell: &str, to: &str) -> anyhow::Result<()> {
    let (status, body) = client
        .post(
            &format!("/api/cells/{}/reassign", path_segment(cell)),
            json!({"to": to}),
        )
        .await?;
    let body = ok(status, body)?;
    print_cell(&body);
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
                             `melissa-queen confirm approve {id}`, or deny",
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
        if let Some(setup) = p["setup"].as_str() {
            println!("    set up: {setup}");
        }
    }
    println!("(- = disabled)");
    if body["restart_needed"] == true {
        println!("Saved changes apply after Melissa restarts.");
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
        println!("Saved. Melissa uses it after a restart.");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_status_shows_digests_and_last_message() {
        let state = json!({
            "status": {
                "running": {"image": "registry.example.com/melissa:dev", "version": "abc1234",
                           "digest": "sha256:96e1d42a824a821abe9579b17235c180"},
                "previous": {"image": "registry.example.com/melissa:dev",
                             "digest": "sha256:fee4874b18675799b133faaac4b47a28"}
            },
            "last": {"action": "check", "ok": false, "at": 1, "message": "no network"}
        });
        assert_eq!(
            image_lines(&state),
            [
                "  running    registry.example.com/melissa:dev abc1234 sha256:96e1d42a824a",
                "  previous   registry.example.com/melissa:dev sha256:fee4874b1867",
                "  last check: failed (no network)",
            ]
        );
        assert!(image_lines(&json!({})).is_empty());
    }
}
