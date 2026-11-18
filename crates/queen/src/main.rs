#![forbid(unsafe_code)]
//! Queen bee: coordinates the hive and hosts the agent layer and the web UI.
//!
//! Usage:
//!   `melissa-queen [--config <path>]`                         serve the web UI and agents
//!   `melissa-queen [--config <path>] tool <name> [json-args]` run one tool and print its JSON
//!
//! Config: `$XDG_CONFIG_HOME/melissa/queen.toml` (optional). Audit log and agent working
//! directory: `$XDG_STATE_HOME/melissa/`. Cluster credentials: the `melissa.kubeconfig`
//! systemd credential, else `kubeconfig` in the config, else `/run/melissa-kube/queen.kubeconfig`
//! (written by root once k3s is up). They are looked up on every cluster call, so the queen
//! never waits for k3s and attaches to it without a restart.

mod config;

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{bail, Context};
use melissa_agent::{
    mcp::McpServer,
    tools::{BeeInfo, TaskContext, Tool, ToolRegistry},
    AuditLog, Backend, CliBackend, EchoBackend, McpLink, TaskBoard,
};
use melissa_hive::{ClusterHealth, LazyCluster};
use serde_json::{json, Value};
use tracing_subscriber::EnvFilter;

use config::{Dirs, QueenConfig};

const USAGE: &str = "usage: melissa-queen [--config <path>] [tool <name> [json-args]]";

enum Command {
    Serve,
    /// Run one tool through the MCP layer with an unattended policy context, print the result.
    Tool {
        name: String,
        args: Value,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logs go to stderr so `tool` output on stdout stays clean JSON.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let dirs = Dirs::from_env()?;
    let (config_path, command) = parse_args(std::env::args().skip(1).collect())?;
    let config_path = config_path.unwrap_or_else(|| dirs.config.join("queen.toml"));
    let config = QueenConfig::load(&config_path)?;
    config.validate()?;

    let audit = AuditLog::new(dirs.state.join("audit.jsonl"));
    let creds = std::env::var_os("CREDENTIALS_DIRECTORY").map(PathBuf::from);
    let cluster = LazyCluster::for_queen(creds.as_deref(), config.kubeconfig.as_deref());
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(BeeInfo::new()),
        Arc::new(ClusterHealth::new(Some(Arc::new(cluster)))),
    ];

    match command {
        Command::Serve => serve(config, dirs, audit, tools).await,
        Command::Tool { name, args } => run_tool(audit, tools, &name, args).await,
    }
}

async fn serve(
    config: QueenConfig,
    dirs: Dirs,
    audit: AuditLog,
    tools: Vec<Arc<dyn Tool>>,
) -> anyhow::Result<()> {
    let workdir = dirs.state.join("work");
    let mut backends: Vec<Arc<dyn Backend>> = vec![Arc::new(EchoBackend::new())];
    for spec in &config.backends {
        let backend = CliBackend::new(spec.clone(), workdir.clone());
        let info = backend.info();
        tracing::info!(name = %info.name, available = info.available, "agent backend");
        backends.push(Arc::new(backend));
    }
    tracing::info!(path = %audit.path().display(), "audit log");
    let mcp = McpServer::new(ToolRegistry::new(tools, Some(audit.clone())));
    let link = McpLink {
        server: mcp.clone(),
        base_url: format!("http://{}/mcp/", config.listen),
        workroot: workdir,
    };
    let board = TaskBoard::with_tools(backends, config.task_timeout(), Some(audit), link);

    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("binding {}", config.listen))?;
    tracing::info!("melissa queen listening on http://{}", config.listen);
    melissa_web::serve(listener, melissa_web::router_with_mcp(board, mcp)).await?;
    Ok(())
}

/// One tool call through the same MCP session path agents use: unattended, untainted, and
/// with no time to wait for a human, so anything that needs a confirmation is refused.
async fn run_tool(
    audit: AuditLog,
    tools: Vec<Arc<dyn Tool>>,
    name: &str,
    args: Value,
) -> anyhow::Result<()> {
    // The audit log may not be writable (e.g. a transient unit); the registry logs that and
    // still runs the call, which is acceptable for read-only operator queries.
    let registry = ToolRegistry::new(tools, Some(audit)).with_confirm_timeout(Duration::ZERO);
    let mcp = McpServer::new(registry);
    let token = mcp
        .open_session(0, TaskContext::default(), None)
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
        [tool, name] if tool == "tool" => Command::Tool {
            name: name.clone(),
            args: json!({}),
        },
        [tool, name, args] if tool == "tool" => Command::Tool {
            name: name.clone(),
            args: serde_json::from_str(args).context("tool arguments must be a JSON object")?,
        },
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
        assert!(parse(&["bogus"]).is_err());
    }

    #[tokio::test]
    async fn tool_command_refuses_unknown_tools() {
        let audit = AuditLog::new(std::env::temp_dir().join("melissa-queen-tool-test.jsonl"));
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(ClusterHealth::new(None))];
        assert!(run_tool(audit.clone(), tools.clone(), "nope", json!({}))
            .await
            .is_err());
        run_tool(audit, tools, "cluster_health", json!({}))
            .await
            .unwrap();
    }
}
