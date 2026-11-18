//! Command-line client for a running queen: `chat`, `ask`, `confirm`, `workers`.
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
}

impl Client {
    pub fn new(addr: SocketAddr) -> Self {
        Self { addr }
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
            .get(&format!("/api/queen/events?since={since}&wait={wait}"))
            .await?;
        Ok(serde_json::from_value(body)?)
    }

    /// Send a message; returns the turn and the event cursor to follow it from.
    async fn send(&self, text: &str) -> anyhow::Result<(TurnId, u64)> {
        let (status, body) = self
            .post("/api/queen/messages", json!({"text": text}))
            .await?;
        if status == StatusCode::CONFLICT {
            bail!(
                "the Queen is still working on another message (from this or another client); \
                 wait for it or cancel it"
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
            QueenEvent::UserMessage { .. } | QueenEvent::QueenMessage { .. } => {}
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
            QueenEvent::WorkerStarted {
                worker,
                backend,
                goal,
            } => {
                self.line(&format!(
                    "[worker {worker}] started on {backend}: {}",
                    one_line(goal)
                ));
            }
            QueenEvent::WorkerFinished {
                worker,
                status,
                error,
            } => match error {
                Some(e) => self.line(&format!("[worker {worker}] {status}: {}", one_line(e))),
                None => self.line(&format!("[worker {worker}] {status}")),
            },
            QueenEvent::SessionOpened { backend, resumed } => {
                let how = if *resumed {
                    "resumed the previous session"
                } else {
                    "new session"
                };
                self.line(&format!("[session] {backend}: {how}"));
            }
            QueenEvent::BackendFailed { backend, error } => {
                self.line(&format!(
                    "[session] {backend} could not start: {}",
                    one_line(error)
                ));
            }
            QueenEvent::SessionLost { reason } => {
                self.line(&format!(
                    "[session] lost: {}. The Queen's context may be lost; the next message starts \
                     a new session.",
                    one_line(reason)
                ));
            }
            QueenEvent::Error { message, .. } => self.line(&format!("[error] {message}")),
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
                if let Err(e) = client.post("/api/queen/cancel", json!({})).await {
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

/// `chat`: a line-based conversation. Events that arrive between turns (a worker finishing,
/// a confirmation) are shown while waiting for input.
pub async fn chat(client: &Client) -> anyhow::Result<()> {
    let status = client.get("/api/queen/status").await?;
    let mut since = status["last_seq"].as_u64().unwrap_or(0);
    println!(
        "Talking to the Queen at http://{}. Enter sends; Ctrl-C cancels a running turn; \
         Ctrl-D (or Ctrl-C at the prompt) quits.",
        client.addr
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

/// `workers`: the Queen's workers.
pub async fn workers(client: &Client) -> anyhow::Result<()> {
    let body = client.get("/api/workers").await?;
    let list = body["workers"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("No workers.");
    }
    for w in list {
        let elapsed = Duration::from_secs(w["elapsed_secs"].as_u64().unwrap_or(0));
        println!(
            "#{}  {:<9}  {}  {}s  {}{}",
            w["worker"],
            w["status"].as_str().unwrap_or("?"),
            w["backend"].as_str().unwrap_or("?"),
            elapsed.as_secs(),
            one_line(w["goal"].as_str().unwrap_or("")),
            w["error"]
                .as_str()
                .map(|e| format!("  error: {}", one_line(e)))
                .unwrap_or_default()
        );
    }
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
