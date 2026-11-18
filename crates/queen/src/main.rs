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
//!   `melissa-queen [--config <path>] chat`                    talk to the Queen (line based)
//!   `melissa-queen [--config <path>] ask <message>`           one message, stream the answer
//!   `melissa-queen [--config <path>] confirm list|approve <id>|deny <id>`
//!   `melissa-queen [--config <path>] bees`                 the Queen's bee agents
//!
//! The client commands talk to the running daemon on its configured `listen` address.
//!
//! Config: `$XDG_CONFIG_HOME/melissa/queen.toml` (optional). Audit log and agent working
//! directory: `$XDG_STATE_HOME/melissa/`. Cluster credentials: the `melissa.kubeconfig`
//! systemd credential, else `kubeconfig` in the config, else `/run/melissa-kube/queen.kubeconfig`
//! (written by root once k3s is up). They are looked up on every cluster call, so the queen
//! never waits for k3s and attaches to it without a restart.

mod adopt;
mod client;
mod config;
mod credentials;

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{bail, Context};
use melissa_agent::{
    mcp::McpServer,
    queen::{
        bees::{BeeLimits, BeePool},
        events::EventLog,
        transcript::Transcript,
        Queen, QueenSettings,
    },
    session::SessionBackend,
    tools::{BeeInfo, ResourceOwners, Scope, TaskContext, Tool, ToolRegistry},
    AcpBackend, AuditLog, Backend, ClaudeBackend, CliBackend, EchoBackend, EgressProxy, McpLink,
    TaskBoard,
};
use melissa_hive::{app_tools, AppOwners, AppSettings, ClusterHealth, KubeApps, LazyCluster};
use serde_json::{json, Value};
use tracing_subscriber::EnvFilter;

use config::{BackendSpec, Dirs, QueenConfig};

/// The Queen's system prompt and the instructions every bee gets (reviewed text files).
const QUEEN_PROMPT: &str = include_str!("../prompts/queen.md");
const BEE_PROMPT: &str = include_str!("../prompts/bee.md");

const USAGE: &str = "usage: melissa-queen [--config <path>] [COMMAND]
  (none)                      serve the web UI, the Queen and agents
  tool <name> [json-args]     run one tool here and print its JSON result (unattended:
                              anything that needs a confirmation is refused)
  tool --wait <name> [json]   run one tool in the running daemon; waits for a confirmation
                              (answer it with `confirm approve <id>` from another shell)
  app-credentials <app>       show an app's generated credentials (owner only, needs root)
  app-adopt <app>             make an app a bee job created yours (owner only, needs root)
  chat                        talk to the Queen (Ctrl-C cancels a turn)
  ask <message>               send one message and stream the answer
  confirm list|approve <id>|deny <id>
  bees                     list the Queen's bee agents";

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
    AppAdopt(String),
    Chat,
    Ask(String),
    Confirm(String, Option<u64>),
    Bees,
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
    match &command {
        Command::Chat => return client::chat(&api).await,
        Command::Ask(message) => return client::ask(&api, message).await,
        Command::Confirm(action, id) => return client::confirm(&api, action, *id).await,
        Command::Bees => return client::bees(&api).await,
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
        Command::AppAdopt(app) => {
            return adopt::adopt(app, std::path::Path::new(credentials::ADMIN_KUBECONFIG)).await
        }
        Command::Serve | Command::Tool { .. } => {}
    }

    let audit = AuditLog::new(dirs.state.join("audit.jsonl"));
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
    tools.extend(app_tools(
        AppSettings {
            catalog_dir: config.apps.catalog.clone(),
            domain: config.apps.domain.clone(),
        },
        Some(apps.clone()),
    ));
    // Who owns which app, for the scope check before every app-changing tool call.
    let owners: Arc<dyn ResourceOwners> = Arc::new(AppOwners::new(Some(apps)));

    match command {
        Command::Tool { name, args } => run_tool(audit, tools, owners, &name, args).await,
        _ => serve(config, dirs, audit, tools, owners).await,
    }
}

async fn serve(
    config: QueenConfig,
    dirs: Dirs,
    audit: AuditLog,
    mut tools: Vec<Arc<dyn Tool>>,
    owners: Arc<dyn ResourceOwners>,
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
        },
        events.clone(),
    );
    tools.extend(pool.tools());
    // Confirmations wait less long than the agents' MCP clients do (see the config field).
    let mcp = McpServer::new(
        ToolRegistry::new(tools, Some(audit.clone()))
            .with_owners(owners)
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
    owners: Arc<dyn ResourceOwners>,
    name: &str,
    args: Value,
) -> anyhow::Result<()> {
    // The audit log may not be writable (e.g. a transient unit); the registry logs that and
    // still runs the call, which is acceptable for read-only operator queries.
    let registry = ToolRegistry::new(tools, Some(audit))
        .with_confirm_timeout(Duration::ZERO)
        .with_owners(owners);
    let mcp = McpServer::new(registry);
    let token = mcp
        .open_session(
            0,
            // The operator is the owner: no ownership checks; the label policy still applies.
            TaskContext {
                scope: Scope::Owner,
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
        [c, app] if c == "app-adopt" => Command::AppAdopt(app.clone()),
        [tool, name] if tool == "tool" => Command::Tool {
            name: name.clone(),
            args: json!({}),
        },
        [tool, name, args] if tool == "tool" => Command::Tool {
            name: name.clone(),
            args: serde_json::from_str(args).context("tool arguments must be a JSON object")?,
        },
        [c] if c == "chat" => Command::Chat,
        [c, words @ ..] if c == "ask" && !words.is_empty() => Command::Ask(words.join(" ")),
        // `workers`: hidden alias from before the hive model (one release).
        [c] if c == "bees" || c == "workers" => Command::Bees,
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
        assert!(
            matches!(parse(&["app-adopt", "blog"]).unwrap().1, Command::AppAdopt(a) if a == "blog")
        );
        assert!(parse(&["app-adopt"]).is_err());
        assert!(parse(&["bogus"]).is_err());
        assert!(matches!(parse(&["chat"]).unwrap().1, Command::Chat));
        assert!(
            matches!(parse(&["ask", "how", "are you?"]).unwrap().1, Command::Ask(m) if m == "how are you?")
        );
        assert!(parse(&["ask"]).is_err());
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
        let audit = AuditLog::new(std::env::temp_dir().join("melissa-queen-tool-test.jsonl"));
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(ClusterHealth::new(None))];
        let owners: Arc<dyn ResourceOwners> = Arc::new(AppOwners::new::<KubeApps>(None));
        assert!(run_tool(
            audit.clone(),
            tools.clone(),
            owners.clone(),
            "nope",
            json!({})
        )
        .await
        .is_err());
        run_tool(audit, tools, owners, "cluster_health", json!({}))
            .await
            .unwrap();
    }
}
