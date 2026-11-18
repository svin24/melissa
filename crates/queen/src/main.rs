#![forbid(unsafe_code)]
//! Queen bee: coordinates the hive and hosts the agent layer and the web UI.
//!
//! Usage: `melissa-queen [--config <path>]`
//! Config: `$XDG_CONFIG_HOME/melissa/queen.toml` (optional). Audit log and agent working
//! directory: `$XDG_STATE_HOME/melissa/`.

mod config;

use std::{path::PathBuf, sync::Arc};

use anyhow::{bail, Context};
use melissa_agent::{AuditLog, Backend, CliBackend, EchoBackend, TaskBoard};
use tracing_subscriber::EnvFilter;

use config::{Dirs, QueenConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let dirs = Dirs::from_env()?;
    let config_path = config_path_from_args()?.unwrap_or_else(|| dirs.config.join("queen.toml"));
    let config = QueenConfig::load(&config_path)?;
    config.validate()?;

    let workdir = dirs.state.join("work");
    let mut backends: Vec<Arc<dyn Backend>> = vec![Arc::new(EchoBackend::new())];
    for spec in &config.backends {
        let backend = CliBackend::new(spec.clone(), workdir.clone());
        let info = backend.info();
        tracing::info!(name = %info.name, available = info.available, "agent backend");
        backends.push(Arc::new(backend));
    }
    let audit = AuditLog::new(dirs.state.join("audit.jsonl"));
    tracing::info!(path = %audit.path().display(), "audit log");
    let board = TaskBoard::new(backends, config.task_timeout(), Some(audit));

    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("binding {}", config.listen))?;
    tracing::info!("melissa queen listening on http://{}", config.listen);
    melissa_web::serve(listener, melissa_web::router(board)).await?;
    Ok(())
}

fn config_path_from_args() -> anyhow::Result<Option<PathBuf>> {
    let mut args = std::env::args().skip(1);
    match (args.next().as_deref(), args.next(), args.next()) {
        (None, _, _) => Ok(None),
        (Some("--config"), Some(path), None) => Ok(Some(PathBuf::from(path))),
        (Some("-h" | "--help"), _, _) => {
            println!("usage: melissa-queen [--config <path>]");
            std::process::exit(0);
        }
        _ => bail!("usage: melissa-queen [--config <path>]"),
    }
}
