#![forbid(unsafe_code)]
//! Queen bee: coordinates the hive and hosts the agent layer and the web UI.
//!
//! Usage:
//!   `melissa-queen [--config <path>]`                         serve the web UI and agents
//!   `melissa-queen [--config <path>] tool <name> [json-args]` run one tool and print its JSON
//!   `melissa-queen [--config <path>] tool --wait <name> [json]` same, in the running daemon,
//!                                                            waiting for confirmations
//!   `sudo melissa-queen app-credentials <app>`                the owner's view of an app's
//!                                                            generated credentials
//!   `melissa-queen [--config <path>] chat [--bee <bee>]`      talk to the Queen or a bee
//!   `melissa-queen [--config <path>] ask [--bee <bee>] <message>`  one message, stream it
//!   `melissa-queen [--config <path>] confirm list|approve <id>|deny <id>`
//!   `melissa-queen [--config <path>] bee list|rename <bee> <name>`  this comb's bees
//!   `melissa-queen [--config <path>] cell list|reassign <cell> --to <bee>`  cells and owners
//!
//! The client commands talk to the running daemon on its configured `listen` address.
//!
//! Config: `$XDG_CONFIG_HOME/melissa/queen.toml` (optional). Audit log, agent working
//! directory, comb id (`comb.json`) and bee registry (`bees/`): `$XDG_STATE_HOME/melissa/`. Cluster credentials: the `melissa.kubeconfig`
//! systemd credential, else `kubeconfig` in the config, else `/run/melissa-kube/queen.kubeconfig`
//! (written by root once k3s is up). They are looked up on every cluster call, so the queen
//! never waits for k3s and attaches to it without a restart.

mod client;
mod config;
mod credentials;

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{bail, Context};
use melissa_agent::{
    comb::CombIdentity,
    mcp::McpServer,
    queen::{
        bees::{BeeLimits, BeePool},
        events::EventLog,
        registry::BeeRegistry,
        transcript::Transcript,
        Queen, QueenSettings,
    },
    session::SessionBackend,
    tools::{BeeInfo, BeeNames, CellStore, ResourceOwners, Scope, TaskContext, Tool, ToolRegistry},
    AcpBackend, AuditLog, Backend, ClaudeBackend, CliBackend, EchoBackend, EgressProxy, McpLink,
    TaskBoard,
};
use melissa_hive::{app_tools, AppSettings, Cells, ClusterHealth, KubeApps, LazyCluster};
use serde_json::{json, Value};
use tracing_subscriber::EnvFilter;

use config::{BackendSpec, Dirs, QueenConfig};

/// The Queen's system prompt and every bee's (reviewed text files).
const QUEEN_PROMPT: &str = include_str!("../prompts/queen.md");
const BEE_PROMPT: &str = include_str!("../prompts/bee.md");

const USAGE: &str = "usage: melissa-queen [--config <path>] [COMMAND]
  (none)                      serve the web UI, the Queen and agents
  tool <name> [json-args]     run one tool here and print its JSON result (unattended:
                              anything that needs a confirmation is refused)
  tool --wait <name> [json]   run one tool in the running daemon; waits for a confirmation
                              (answer it with `confirm approve <id>` from another shell)
  app-credentials <app>       show an app's generated credentials (owner only, needs root)
  chat [--bee <bee>]          talk to the Queen, or directly to a bee (Ctrl-C cancels a turn)
  ask [--bee <bee>] <message> send one message and stream the answer
  confirm list|approve <id>|deny <id>
  bee list                    this comb's bees (also: bees)
  bee rename <bee> <name>     give a bee a new name
  cell list                   every cell (installed app) and the bee that looks after it
  cell reassign <cell> --to <bee>
                              give a cell to another bee (your decision, no confirmation)";

enum Command {
    Serve,
    /// Run one tool through the MCP layer with an unattended policy context, print the result.
    Tool {
        name: String,
        args: Value,
    },
    /// The same through the daemon's API, waiting for confirmations.
    ToolWait {
        name: String,
        args: Value,
    },
    AppCredentials(String),
    /// Talk to the Queen (None) or a bee.
    Chat(Option<String>),
    Ask(Option<String>, String),
    Confirm(String, Option<u64>),
    Bees,
    RenameBee(String, String),
    Cells,
    Reassign {
        cell: String,
        to: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (config_path, command) = parse_args(std::env::args().skip(1).collect())?;
    // Logs go to stderr so `tool` output on stdout stays clean JSON; client commands only
    // show warnings.
    let default_level = match command {
        Command::Serve | Command::Tool { .. } => "info",
        _ => "warn",
    };
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level)),
        )
        .init();

    let dirs = Dirs::from_env()?;
    let config_path = config_path.unwrap_or_else(|| dirs.config.join("queen.toml"));
    let config = QueenConfig::load(&config_path)?;
    config.validate()?;

    let api = client::Client::new(config.listen);
    let to = |bee: &Option<String>| match bee {
        Some(b) => client::Client::new(config.listen).with_bee(b),
        None => client::Client::new(config.listen),
    };
    match &command {
        Command::Chat(bee) => return client::chat(&to(bee)).await,
        Command::Ask(bee, message) => return client::ask(&to(bee), message).await,
        Command::Confirm(action, id) => return client::confirm(&api, action, *id).await,
        Command::Bees => return client::bees(&api).await,
        Command::RenameBee(bee, name) => return client::rename_bee(&api, bee, name).await,
        Command::Cells => return client::cells(&api).await,
        Command::Reassign { cell, to } => return client::reassign(&api, cell, to).await,
        Command::ToolWait { name, args } => return client::tool(&api, name, args.clone()).await,
        Command::AppCredentials(app) => {
            return credentials::show(
                app,
                &config.apps.catalog,
                &config.apps.domain,
                std::path::Path::new(credentials::ADMIN_KUBECONFIG),
            )
            .await
        }
        Command::Serve | Command::Tool { .. } => {}
    }

    let audit = AuditLog::new(dirs.state.join("audit.jsonl"));
    // This device's comb id (made once) and its bees.
    let comb = CombIdentity::load_or_create(&dirs.state.join("comb.json"))?;
    let registry = BeeRegistry::open(&dirs.state.join("bees"), &comb.id)?;
    let names: Arc<dyn BeeNames> = Arc::new(registry.clone());
    let creds = std::env::var_os("CREDENTIALS_DIRECTORY").map(PathBuf::from);
    let cluster = LazyCluster::for_queen(creds.as_deref(), config.kubeconfig.as_deref());
    let mut tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(BeeInfo::new()),
        Arc::new(ClusterHealth::new(Some(Arc::new(cluster)))),
    ];
    // App tools use the separate write identity (melissa-apps); read-only tools above
    // never hold it.
    let apps = Arc::new(KubeApps::new(LazyCluster::for_apps(
        config.apps.kubeconfig.as_deref(),
    )));
    let app_settings = AppSettings {
        catalog_dir: config.apps.catalog.clone(),
        domain: config.apps.domain.clone(),
        comb: comb.id.clone(),
        bees: Some(names.clone()),
    };
    tools.extend(app_tools(app_settings.clone(), Some(apps.clone())));
    // Who owns which cell, for the scope check before every cell-changing tool call; also
    // the list of cells and the reassign path.
    let cells = Arc::new(Cells::new(app_settings, Some(apps)));
    let wiring = Wiring {
        owners: cells.clone(),
        cells,
        names,
        registry,
        comb: comb.id,
    };

    match command {
        Command::Tool { name, args } => run_tool(audit, tools, &wiring, &name, args).await,
        _ => serve(config, dirs, audit, tools, wiring).await,
    }
}

/// Who owns which cell and what the bees are called, shared by the daemon and `tool`.
struct Wiring {
    owners: Arc<dyn ResourceOwners>,
    cells: Arc<dyn CellStore>,
    names: Arc<dyn BeeNames>,
    registry: BeeRegistry,
    comb: String,
}

async fn serve(
    config: QueenConfig,
    dirs: Dirs,
    audit: AuditLog,
    mut tools: Vec<Arc<dyn Tool>>,
    wiring: Wiring,
) -> anyhow::Result<()> {
    let workdir = dirs.state.join("work");
    let queen_state = dirs.state.join("queen");
    let q = &config.queen;
    let events = EventLog::new(Some(Transcript::new(
        queen_state.join("transcript.jsonl"),
        q.transcript_max_kib.saturating_mul(1024),
    )));
    let pool = BeePool::new(
        BeeLimits {
            max_concurrent: q.max_bees,
            default_timeout: Duration::from_secs(q.bee_timeout_secs),
            max_timeout: Duration::from_secs(q.bee_timeout_max_secs),
            transcript_max_bytes: q.transcript_max_kib.saturating_mul(1024),
        },
        events.clone(),
        wiring.registry,
        Some(wiring.cells),
    );
    tools.extend(pool.tools());
    // Confirmations wait less long than the agents' MCP clients do (see the config field).
    let mcp = McpServer::new(
        ToolRegistry::new(tools, Some(audit.clone()))
            .with_owners(wiring.owners)
            .with_bees(wiring.names)
            .with_confirm_timeout(config.confirm_timeout()),
    );
    // Sandboxed agents reach the network only through this (allowlist, audited) and
    // Melissa's MCP endpoint at the same address as outside (config.listen).
    let egress = EgressProxy::new(dirs.egress.clone(), Some(audit.clone()))
        .with_context(|| format!("setting up egress sockets in {}", dirs.egress.display()))?
        .with_mcp(mcp.clone(), config.listen);
    match egress.netbridge() {
        Some(path) => {
            tracing::info!(netbridge = %path.display(), sockets = %dirs.egress.display(), "egress proxy ready")
        }
        None => tracing::warn!(
            "melissa-netbridge not found (next to melissa-queen, /usr/bin or PATH): backends \
             with network = \"proxy\" are unavailable"
        ),
    }
    let mut backends: Vec<Arc<dyn Backend>> = vec![Arc::new(EchoBackend::new())];
    let mut sessions: Vec<Arc<dyn SessionBackend>> = Vec::new();
    for spec in &config.backends {
        let backend: Arc<dyn Backend> = match spec {
            BackendSpec::Cli(spec) => {
                Arc::new(CliBackend::new(spec.clone(), workdir.clone()).with_egress(egress.clone()))
            }
            BackendSpec::Acp(spec) => {
                let acp = Arc::new(
                    AcpBackend::new(spec.clone(), workdir.clone())
                        .with_mcp(mcp.clone())
                        .with_egress(egress.clone()),
                );
                sessions.push(acp.clone());
                acp
            }
            BackendSpec::Claude(spec) => {
                let claude = Arc::new(
                    ClaudeBackend::new(spec.clone(), workdir.clone())
                        .with_mcp(mcp.clone())
                        .with_egress(egress.clone()),
                );
                sessions.push(claude.clone());
                claude
            }
        };
        let info = backend.info();
        tracing::info!(name = %info.name, kind = info.kind, available = info.available, detail = %info.detail, "agent backend");
        backends.push(backend);
    }
    tracing::info!(path = %audit.path().display(), "audit log");
    let link = McpLink {
        server: mcp.clone(),
        base_url: format!("http://{}/mcp/", config.listen),
        workroot: workdir.clone(),
    };
    let board = TaskBoard::with_tools(backends, config.task_timeout(), Some(audit), link);
    let settings = QueenSettings {
        backends: q.backends.clone(),
        system_prompt: QUEEN_PROMPT.into(),
        bee_prompt: BEE_PROMPT.into(),
        bee_backend: q.bee_backend.clone(),
        state_dir: queen_state,
        workdir: workdir.join("queen"),
        tools: q.tools.clone(),
        comb: wiring.comb,
    };
    let queen = Queen::new(settings, sessions, board.clone(), events, &pool)
        .context("setting up the Queen")?;
    tracing::info!(backends = ?q.backends, "queen agent ready (opens on the first message)");

    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("binding {}", config.listen))?;
    tracing::info!("melissa queen listening on http://{}", config.listen);
    melissa_web::serve(
        listener,
        melissa_web::router_with_queen(board, mcp, queen.clone()),
    )
    .await?;
    queen.close().await;
    Ok(())
}

/// One tool call through the same MCP session path agents use: unattended, untainted, and
/// with no time to wait for a human, so anything that needs a confirmation is refused.
async fn run_tool(
    audit: AuditLog,
    tools: Vec<Arc<dyn Tool>>,
    wiring: &Wiring,
    name: &str,
    args: Value,
) -> anyhow::Result<()> {
    // The audit log may not be writable (e.g. a transient unit); the registry logs that and
    // still runs the call, which is acceptable for read-only operator queries.
    let registry = ToolRegistry::new(tools, Some(audit))
        .with_confirm_timeout(Duration::ZERO)
        .with_owners(wiring.owners.clone())
        .with_bees(wiring.names.clone());
    let mcp = McpServer::new(registry);
    let token = mcp
        .open_session(
            0,
            // The operator is the user: no ownership checks; the label policy still applies.
            TaskContext {
                scope: Scope::User,
                ..TaskContext::default()
            },
            None,
        )
        .context("opening a tool session")?;
    let request = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": name, "arguments": args},
    });
    let response = mcp.handle(&token, request).await;
    mcp.close_session(&token);
    let result = match response {
        Ok(Some(r)) => r,
        _ => bail!("no response from the tool session"),
    };
    let result = &result["result"];
    if result["isError"] == true {
        bail!(
            "{}",
            result["content"][0]["text"]
                .as_str()
                .unwrap_or("tool failed")
        );
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&result["structuredContent"])?
    );
    Ok(())
}

fn parse_args(args: Vec<String>) -> anyhow::Result<(Option<PathBuf>, Command)> {
    let mut rest = args.as_slice();
    let mut config = None;
    if let [flag, path, tail @ ..] = rest {
        if flag == "--config" {
            config = Some(PathBuf::from(path));
            rest = tail;
        }
    }
    let command = match rest {
        [] => Command::Serve,
        [h] if h == "-h" || h == "--help" => {
            println!("{USAGE}");
            std::process::exit(0);
        }
        [tool, wait, name] if tool == "tool" && wait == "--wait" => Command::ToolWait {
            name: name.clone(),
            args: json!({}),
        },
        [tool, wait, name, args] if tool == "tool" && wait == "--wait" => Command::ToolWait {
            name: name.clone(),
            args: serde_json::from_str(args).context("tool arguments must be a JSON object")?,
        },
        [c, app] if c == "app-credentials" => Command::AppCredentials(app.clone()),
        [c, ..] if c == "app-adopt" => bail!(
            "app-adopt is gone: cells now belong to bees. Use `melissa-queen cell list` and \
             `melissa-queen cell reassign <cell> --to <bee>`"
        ),
        [tool, name] if tool == "tool" => Command::Tool {
            name: name.clone(),
            args: json!({}),
        },
        [tool, name, args] if tool == "tool" => Command::Tool {
            name: name.clone(),
            args: serde_json::from_str(args).context("tool arguments must be a JSON object")?,
        },
        [c] if c == "chat" => Command::Chat(None),
        [c, flag, bee] if c == "chat" && flag == "--bee" => Command::Chat(Some(bee.clone())),
        [c, flag, bee, words @ ..] if c == "ask" && flag == "--bee" && !words.is_empty() => {
            Command::Ask(Some(bee.clone()), words.join(" "))
        }
        [c, words @ ..] if c == "ask" && !words.is_empty() && words[0] != "--bee" => {
            Command::Ask(None, words.join(" "))
        }
        // `workers`: hidden alias from before the hive model (one release).
        [c] if c == "bees" || c == "workers" => Command::Bees,
        [c, a] if c == "bee" && a == "list" => Command::Bees,
        [c, a, bee, words @ ..] if c == "bee" && a == "rename" && !words.is_empty() => {
            Command::RenameBee(bee.clone(), words.join(" "))
        }
        [c] if c == "cell" => Command::Cells,
        [c, a] if c == "cell" && a == "list" => Command::Cells,
        [c, a, cell, flag, to] if c == "cell" && a == "reassign" && flag == "--to" => {
            Command::Reassign {
                cell: cell.clone(),
                to: to.clone(),
            }
        }
        [c] if c == "confirm" => Command::Confirm("list".into(), None),
        [c, a] if c == "confirm" && a == "list" => Command::Confirm("list".into(), None),
        [c, a, id] if c == "confirm" && (a == "approve" || a == "deny") => Command::Confirm(
            a.clone(),
            Some(
                id.trim_start_matches('#')
                    .parse()
                    .context("confirmation id must be a number")?,
            ),
        ),
        _ => bail!("{USAGE}"),
    };
    Ok((config, command))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> anyhow::Result<(Option<PathBuf>, Command)> {
        parse_args(args.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn parses_serve_and_tool_commands() {
        assert!(matches!(parse(&[]).unwrap(), (None, Command::Serve)));
        let (cfg, cmd) = parse(&["--config", "/x.toml", "tool", "cluster_health"]).unwrap();
        assert_eq!(cfg, Some(PathBuf::from("/x.toml")));
        assert!(
            matches!(cmd, Command::Tool { name, args } if name == "cluster_health" && args == json!({}))
        );
        let (_, cmd) = parse(&["tool", "bee_info", r#"{"a":1}"#]).unwrap();
        assert!(matches!(cmd, Command::Tool { args, .. } if args["a"] == 1));
        assert!(parse(&["tool", "x", "not json"]).is_err());
        let (_, cmd) = parse(&["tool", "--wait", "install_app", r#"{"app":"nextcloud"}"#]).unwrap();
        assert!(
            matches!(cmd, Command::ToolWait { name, args } if name == "install_app" && args["app"] == "nextcloud")
        );
        assert!(matches!(
            parse(&["tool", "--wait", "app_status"]).unwrap().1,
            Command::ToolWait { .. }
        ));
        assert!(
            matches!(parse(&["app-credentials", "nextcloud"]).unwrap().1, Command::AppCredentials(a) if a == "nextcloud")
        );
        assert!(parse(&["app-credentials"]).is_err());
        // Replaced by `cell reassign`.
        assert!(parse(&["app-adopt", "blog"]).is_err());
        assert!(parse(&["bogus"]).is_err());
        assert!(matches!(parse(&["chat"]).unwrap().1, Command::Chat(None)));
        assert!(
            matches!(parse(&["chat", "--bee", "Ada"]).unwrap().1, Command::Chat(Some(b)) if b == "Ada")
        );
        assert!(
            matches!(parse(&["ask", "how", "are you?"]).unwrap().1, Command::Ask(None, m) if m == "how are you?")
        );
        assert!(
            matches!(parse(&["ask", "--bee", "Ada", "hi", "there"]).unwrap().1, Command::Ask(Some(b), m) if b == "Ada" && m == "hi there")
        );
        assert!(parse(&["ask"]).is_err());
        assert!(parse(&["ask", "--bee", "Ada"]).is_err());
        assert!(matches!(parse(&["bee", "list"]).unwrap().1, Command::Bees));
        assert!(
            matches!(parse(&["bee", "rename", "Ada", "Ada", "Lovelace"]).unwrap().1, Command::RenameBee(b, n) if b == "Ada" && n == "Ada Lovelace")
        );
        assert!(parse(&["bee", "rename", "Ada"]).is_err());
        assert!(matches!(
            parse(&["cell", "list"]).unwrap().1,
            Command::Cells
        ));
        assert!(
            matches!(parse(&["cell", "reassign", "nextcloud", "--to", "Bo"]).unwrap().1, Command::Reassign { cell, to } if cell == "nextcloud" && to == "Bo")
        );
        assert!(parse(&["cell", "reassign", "nextcloud", "Bo"]).is_err());
        assert!(matches!(parse(&["bees"]).unwrap().1, Command::Bees));
        assert!(matches!(parse(&["workers"]).unwrap().1, Command::Bees));
        assert!(matches!(parse(&["confirm"]).unwrap().1, Command::Confirm(a, None) if a == "list"));
        assert!(
            matches!(parse(&["confirm", "approve", "#3"]).unwrap().1, Command::Confirm(a, Some(3)) if a == "approve")
        );
        assert!(parse(&["confirm", "deny", "x"]).is_err());
        assert!(parse(&["confirm", "maybe", "1"]).is_err());
    }

    #[tokio::test]
    async fn tool_command_refuses_unknown_tools() {
        let dir = std::env::temp_dir().join(format!("melissa-queen-tool-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let audit = AuditLog::new(dir.join("audit.jsonl"));
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(ClusterHealth::new(None))];
        let registry = BeeRegistry::open(&dir.join("bees"), "c-0a1b2c3d").unwrap();
        let settings = AppSettings {
            catalog_dir: dir.join("catalog"),
            domain: "example.com".into(),
            comb: "c-0a1b2c3d".into(),
            bees: None,
        };
        let cells = Arc::new(Cells::new::<KubeApps>(settings, None));
        let wiring = Wiring {
            owners: cells.clone(),
            cells,
            names: Arc::new(registry.clone()),
            registry,
            comb: "c-0a1b2c3d".into(),
        };
        assert!(
            run_tool(audit.clone(), tools.clone(), &wiring, "nope", json!({}))
                .await
                .is_err()
        );
        run_tool(audit, tools, &wiring, "cluster_health", json!({}))
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }
}
