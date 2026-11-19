#![forbid(unsafe_code)]
//! Queen bee: coordinates the hive and hosts the agent layer and the web UI.
//!
//! Usage:
//!   `melissa-queen [--config <path>]`                         serve the web UI and agents
//!   `melissa-queen [--config <path>] demo`                    the same, for a first run from
//!                                                            source (summary, quieter log)
//!   `melissa-queen [--config <path>] tool <name> [json-args]` run one tool and print its JSON
//!   `melissa-queen [--config <path>] tool --wait <name> [json]` same, in the running daemon,
//!                                                            waiting for confirmations
//!   `sudo melissa-queen app-credentials <app>`                the owner's view of an app's
//!                                                            generated credentials
//!   `melissa-queen [--config <path>] chat [--bee <bee>]`      talk to the Queen or a bee
//!   `melissa-queen [--config <path>] new-chat` (`chat --new`) start a new chat with the Queen
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
mod preflight;

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
    tools::{
        BeeNames, CellStore, CombInfo, ResourceOwners, Scope, TaskContext, Tool, ToolRegistry,
    },
    AcpBackend, AuditLog, Backend, ClaudeBackend, CliBackend, EchoBackend, EchoSessionBackend,
    EgressProxy, McpLink, TaskBoard,
};
use melissa_hive::{app_tools, AppSettings, Cells, ClusterHealth, KubeApps, LazyCluster};
use serde_json::{json, Value};
use tracing_subscriber::EnvFilter;

use config::{BackendSpec, Dirs, QueenConfig};
use melissa_settings::{
    providers::{BackendFacts, ProviderFile, ProviderStore},
    updates::{Release, UpdateEnv},
};

/// The Queen's system prompt and every bee's (reviewed text files).
const QUEEN_PROMPT: &str = include_str!("../prompts/queen.md");
const BEE_PROMPT: &str = include_str!("../prompts/bee.md");

const USAGE: &str = "usage: melissa-queen [--config <path>] [COMMAND]
  (none)                      serve the web UI, the Queen and agents
  demo                        the same for a first run from source: a short summary
                              and only warnings in the log
  tool <name> [json-args]     run one tool here and print its JSON result (unattended:
                              anything that needs a confirmation is refused)
  tool --wait <name> [json]   run one tool in the running daemon; waits for a confirmation
                              (answer it with `confirm approve <id>` from another shell)
  app-credentials <app>       show an app's generated credentials (owner only, needs root)
  chat [--bee <bee>]          talk to the Queen, or directly to a bee (Ctrl-C cancels a turn)
  chat --new                  start a new chat with the Queen, then talk (also: new-chat)
  new-chat                    archive the Queen's chat and start a fresh one (clears its taint)
  ask [--bee <bee>] <message> send one message and stream the answer
  confirm list|approve <id>|deny <id>
  bee list                    this comb's bees (also: bees)
  bee rename <bee> <name>     give a bee a new name
  backend [list] [--bee <bee>]
                              the agents the Queen (or a bee) can run on, which one it uses
                              and the models each offers
  backend use <name> [--model <id>] [--bee <bee>]
                              switch the Queen (or a bee) to that agent from its next message
  backend refresh [--bee <bee>]
                              ask every usable agent for its models now (no prompt), then list
  cell list                   every cell (installed app) and the bee that looks after it
  cell reassign <cell> --to <bee>
                              give a cell to another bee (your decision, no confirmation)
  provider [list]             the agent programs (providers): installed, version, logged in,
                              enabled, who uses them, their settings (secret values are never shown)
  provider enable|disable <id>
  provider set <id> [--name <text>] [--command <path>] [--arg <a>]... [--no-args]
                    [--env NAME=VALUE] [--env NAME] [--unset NAME]
                              change a provider's settings (providers.toml, used after a
                              restart); `--env NAME` reads the value from stdin (secrets stay
                              out of shell history)
  update [status]             Melissa's version and the container image (running, staged,
                              previous)
  update check|apply|rollback|restart [--yes]
                              ask the host's update helper; apply and rollback restart
                              Melissa and need --yes";

enum Command {
    /// `demo` = true: first run from a source checkout (quieter log, repo catalog).
    Serve {
        demo: bool,
    },
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
    /// Start a new chat with the Queen (`true`: then talk).
    NewChat(bool),
    Ask(Option<String>, String),
    Confirm(String, Option<u64>),
    Bees,
    RenameBee(String, String),
    Cells,
    /// `backend list|use`: the Queen's (None) or a bee's backend.
    Backend {
        bee: Option<String>,
        /// `use <name> [--model <id>]`; None: list.
        switch: Option<(String, Option<String>)>,
    },
    /// `backend refresh`: probe every usable backend's models (forced), then list.
    RefreshModels(Option<String>),
    Reassign {
        cell: String,
        to: String,
    },
    Providers,
    /// `provider enable|disable|set`: the JSON change, and variables whose value comes
    /// from stdin.
    ProviderChange {
        id: String,
        change: Value,
        read_env: Vec<String>,
    },
    /// `update [status]` (None) or `update <action> [--yes]`.
    Update {
        action: Option<String>,
        yes: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (config_path, command) = parse_args(std::env::args().skip(1).collect())?;
    // Logs go to stderr so `tool` output on stdout stays clean JSON; client commands only
    // show warnings.
    let default_level = match command {
        Command::Serve { demo: false } | Command::Tool { .. } => "info",
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
    let mut config = QueenConfig::load(&config_path)?;
    config.validate()?;
    // The owner's provider settings (web UI, `provider set`) win over queen.toml.
    let base = config.clone();
    let providers_path = dirs.config.join(melissa_settings::providers::FILE_NAME);
    let provider_file = load_providers(&providers_path);
    let facts = config.apply_providers(&provider_file);
    config.validate()?;
    use_repo_catalog(&mut config.apps);

    let api = client::Client::new(config.listen);
    let to = |bee: &Option<String>| match bee {
        Some(b) => client::Client::new(config.listen).with_bee(b),
        None => client::Client::new(config.listen),
    };
    match &command {
        Command::Chat(bee) => return client::chat(&to(bee)).await,
        Command::NewChat(talk) => {
            client::new_chat(&api).await?;
            if *talk {
                return client::chat(&api).await;
            }
            return Ok(());
        }
        Command::Ask(bee, message) => return client::ask(&to(bee), message).await,
        Command::Confirm(action, id) => return client::confirm(&api, action, *id).await,
        Command::Bees => return client::bees(&api).await,
        Command::RenameBee(bee, name) => return client::rename_bee(&api, bee, name).await,
        Command::Cells => return client::cells(&api).await,
        Command::Backend { bee, switch } => {
            let to = to(bee);
            return match switch {
                None => client::backends(&to).await,
                Some((name, model)) => client::use_backend(&to, name, model.as_deref()).await,
            };
        }
        Command::RefreshModels(bee) => {
            client::refresh_models(&api).await?;
            return client::backends(&to(bee)).await;
        }
        Command::Reassign { cell, to } => return client::reassign(&api, cell, to).await,
        Command::Providers => return client::providers(&api).await,
        Command::ProviderChange {
            id,
            change,
            read_env,
        } => {
            let mut change = change.clone();
            for name in read_env {
                eprint!("Value for {name} (one line): ");
                let mut line = String::new();
                std::io::stdin()
                    .read_line(&mut line)
                    .context("reading the value from stdin")?;
                change["set_env"][name] = Value::String(line.trim_end_matches(['\n', '\r']).into());
            }
            return client::provider_change(&api, id, change).await;
        }
        Command::Update { action, yes } => {
            return match action {
                None => client::update_status(&api).await,
                Some(a) => client::update_action(&api, a, *yes).await,
            }
        }
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
        Command::Serve { .. } | Command::Tool { .. } => {}
    }

    let audit = AuditLog::new(dirs.state.join("audit.jsonl"));
    // This device's comb id (made once) and its bees.
    let comb = CombIdentity::load_or_create(&dirs.state.join("comb.json"))?;
    let registry = BeeRegistry::open(&dirs.state.join("bees"), &comb.id)?;
    let names: Arc<dyn BeeNames> = Arc::new(registry.clone());
    let creds = std::env::var_os("CREDENTIALS_DIRECTORY").map(PathBuf::from);
    let kubeconfigs = kubeconfig_candidates(creds.as_deref(), config.kubeconfig.as_deref());
    let cluster = LazyCluster::for_queen(creds.as_deref(), config.kubeconfig.as_deref());
    let mut tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(CombInfo::new()),
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
        addresses: config.apps.addresses.clone(),
        port_offset: config.apps.port_offset,
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
        _ => {
            let start = Startup {
                config_path,
                kubeconfigs,
                providers: ProviderSetup {
                    path: providers_path,
                    file: provider_file,
                    facts,
                    base,
                },
            };
            serve(config, dirs, audit, tools, wiring, start).await
        }
    }
}

/// What the startup summary needs beyond the config.
struct Startup {
    config_path: PathBuf,
    /// Where the queen looks for cluster credentials (any present = k3s is there).
    kubeconfigs: Vec<PathBuf>,
    providers: ProviderSetup,
}

/// The provider settings as loaded, for the Settings pages.
struct ProviderSetup {
    path: PathBuf,
    file: ProviderFile,
    /// Every backend, also those left out because their provider is disabled.
    facts: Vec<BackendFacts>,
    /// The config before the provider settings, to check a change against.
    base: QueenConfig,
}

/// Read `providers.toml`. A broken file must not keep the box from starting (the UI is how
/// the owner fixes things): it is moved aside, logged, and Melissa starts without it.
fn load_providers(path: &std::path::Path) -> ProviderFile {
    match ProviderFile::load(path) {
        Ok(f) => f,
        Err(e) => {
            let aside = path.with_extension("toml.broken");
            let moved = std::fs::rename(path, &aside).is_ok();
            tracing::error!(
                aside = %aside.display(),
                moved,
                "ignoring the provider settings: {e}"
            );
            ProviderFile::default()
        }
    }
}

/// The same candidates `LazyCluster::for_queen` checks, for the startup summary.
fn kubeconfig_candidates(
    creds: Option<&std::path::Path>,
    configured: Option<&std::path::Path>,
) -> Vec<PathBuf> {
    creds
        .map(|d| d.join(melissa_hive::CREDENTIAL_NAME))
        .into_iter()
        .chain(configured.map(std::path::Path::to_path_buf))
        .chain([PathBuf::from(melissa_hive::RUNTIME_DIR).join(melissa_hive::RUNTIME_FILE)])
        .collect()
}

/// Run from a source checkout, the installed catalog (`/usr/share/melissa/catalog`) is usually
/// missing; use the repo's `manifests/` instead, unless the config names a catalog.
fn use_repo_catalog(apps: &mut config::AppsSection) {
    let default = std::path::Path::new(melissa_catalog::DEFAULT_CATALOG_DIR);
    let repo = std::path::Path::new("manifests");
    if apps.catalog == default && !default.is_dir() && repo.is_dir() {
        if let Ok(abs) = repo.canonicalize() {
            apps.catalog = abs;
        }
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
    start: Startup,
) -> anyhow::Result<()> {
    let workdir = dirs.state.join("work");
    let queen_state = dirs.state.join("queen");
    let q = &config.queen;
    // The earlier chat comes back after a restart (the agent's session resumes too).
    let events = EventLog::with_history(Transcript::new(
        queen_state.join("transcript.jsonl"),
        q.transcript_max_kib.saturating_mul(1024),
    ));
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
    let mut infos = Vec::new();
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
            BackendSpec::Echo(spec) => {
                let echo = Arc::new(EchoSessionBackend::new(spec));
                sessions.push(echo.clone());
                echo
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
        infos.push(info);
        backends.push(backend);
    }
    tracing::info!(path = %audit.path().display(), "audit log");
    let link = McpLink {
        server: mcp.clone(),
        base_url: format!("http://{}/mcp/", config.listen),
        workroot: workdir.clone(),
    };
    let settings_audit = audit.clone();
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
        .with_context(|| {
            format!(
                "cannot listen on {}: is Melissa (or another program) already using it? \
                 Stop it, or set e.g. `listen = \"127.0.0.1:8090\"` in {}",
                config.listen,
                start.config_path.display()
            )
        })?;
    tracing::info!("melissa queen listening on http://{}", config.listen);
    let report = preflight::gather(preflight::Inputs {
        listen: config.listen,
        state: &dirs.state,
        config_path: &start.config_path,
        specs: &config.backends,
        infos: &infos,
        queen_order: &q.backends,
        kubeconfigs: &start.kubeconfigs,
        catalog: &config.apps.catalog,
    })
    .await;
    // The summary is for the person who started the daemon; stdout keeps it apart from the log.
    print!("{}", report.render());
    let ProviderSetup {
        path,
        file,
        mut facts,
        base,
    } = start.providers;
    // The built-in one-shot echo is always there (not a config entry).
    facts.push(BackendFacts {
        name: "echo".into(),
        provider: "echo".into(),
        session: false,
        command: None,
        enabled: true,
    });
    let check: melissa_web::CheckFn = Arc::new(move |id, o| base.check_provider(id, o));
    let updates = UpdateEnv::system();
    let settings = melissa_web::Settings::new(melissa_web::SettingsParts {
        store: ProviderStore::new(path, file),
        backends: facts,
        bee_default: q
            .bee_backend
            .clone()
            .or_else(|| q.backends.first().cloned()),
        check: Some(check),
        audit: Some(settings_audit),
        installer: melissa_settings::install::Installer::system(updates.kind),
        updates,
        release: Release::read(&melissa_settings::updates::release_file()),
        home: std::env::var_os("HOME").map(PathBuf::from),
    });
    melissa_web::serve(
        listener,
        melissa_web::router_with_settings(board, mcp, queen.clone(), settings),
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
        [] => Command::Serve { demo: false },
        [c] if c == "demo" => Command::Serve { demo: true },
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
        [c, flag] if c == "chat" && flag == "--new" => Command::NewChat(true),
        [c] if c == "new-chat" => Command::NewChat(false),
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
        [c, tail @ ..] if c == "backend" || c == "backends" => parse_backend(tail)?,
        [c, tail @ ..] if c == "provider" || c == "providers" => parse_provider(tail)?,
        [c] if c == "update" => Command::Update {
            action: None,
            yes: false,
        },
        [c, a] if c == "update" && a == "status" => Command::Update {
            action: None,
            yes: false,
        },
        [c, a, rest @ ..]
            if c == "update"
                && ["check", "apply", "rollback", "restart"].contains(&a.as_str())
                && rest.iter().all(|r| r == "--yes") =>
        {
            Command::Update {
                action: Some(a.clone()),
                yes: !rest.is_empty(),
            }
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

/// `provider [list]`, `provider enable|disable <id>`, `provider set <id> [options]`.
fn parse_provider(args: &[String]) -> anyhow::Result<Command> {
    match args {
        [] => Ok(Command::Providers),
        [l] if l == "list" => Ok(Command::Providers),
        [a, id] if a == "enable" || a == "disable" => Ok(Command::ProviderChange {
            id: id.clone(),
            change: json!({"enabled": a == "enable"}),
            read_env: Vec::new(),
        }),
        [a, id, rest @ ..] if a == "set" && !rest.is_empty() => {
            let mut change = json!({});
            let (mut args, mut set, mut unset, mut read) =
                (Vec::new(), serde_json::Map::new(), Vec::new(), Vec::new());
            let mut no_args = false;
            let mut it = rest.iter();
            while let Some(flag) = it.next() {
                let mut value = |what: &str| {
                    it.next()
                        .cloned()
                        .with_context(|| format!("{flag} needs {what}"))
                };
                match flag.as_str() {
                    "--name" => change["display_name"] = Value::String(value("a name")?),
                    "--command" => change["command"] = Value::String(value("a path")?),
                    "--arg" => args.push(value("an argument")?),
                    "--no-args" => no_args = true,
                    "--unset" => unset.push(value("a variable name")?),
                    "--env" => {
                        let v = value("NAME=VALUE or NAME")?;
                        match v.split_once('=') {
                            Some((k, val)) => {
                                set.insert(k.to_string(), Value::String(val.to_string()));
                            }
                            None => read.push(v),
                        }
                    }
                    _ => bail!("{USAGE}"),
                }
            }
            if no_args || !args.is_empty() {
                change["args"] = json!(args);
            }
            if !set.is_empty() || !read.is_empty() {
                change["set_env"] = Value::Object(set);
            }
            if !unset.is_empty() {
                change["remove_env"] = json!(unset);
            }
            Ok(Command::ProviderChange {
                id: id.clone(),
                change,
                read_env: read,
            })
        }
        _ => bail!("{USAGE}"),
    }
}

/// `backend [list] [--bee <bee>]`, `backend use <name> [--model <id>] [--bee <bee>]`,
/// `backend refresh [--bee <bee>]`.
fn parse_backend(args: &[String]) -> anyhow::Result<Command> {
    let mut words = Vec::new();
    let (mut bee, mut model) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--bee" => bee = Some(it.next().context("--bee needs a bee's name")?.clone()),
            "--model" => model = Some(it.next().context("--model needs a model id")?.clone()),
            _ => words.push(a.clone()),
        }
    }
    let switch = match words.as_slice() {
        [] => None,
        [l] if l == "list" => None,
        [u, name] if u == "use" => Some((name.clone(), model.take())),
        [r] if r == "refresh" && model.is_none() => return Ok(Command::RefreshModels(bee)),
        _ => bail!("{USAGE}"),
    };
    if model.is_some() {
        bail!("--model goes with `backend use <name>`");
    }
    Ok(Command::Backend { bee, switch })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_provider_and_update_commands() {
        assert!(matches!(
            parse(&["provider"]).unwrap().1,
            Command::Providers
        ));
        let Command::ProviderChange { id, change, .. } =
            parse(&["provider", "disable", "grok"]).unwrap().1
        else {
            panic!("not a change")
        };
        assert_eq!((id.as_str(), &change), ("grok", &json!({"enabled": false})));
        let Command::ProviderChange {
            change, read_env, ..
        } = parse(&[
            "provider",
            "set",
            "claude",
            "--name",
            "Mine",
            "--arg",
            "--effort",
            "--arg",
            "high",
            "--env",
            "ANTHROPIC_BASE_URL=https://llm.example.com",
            "--env",
            "ANTHROPIC_API_KEY",
            "--unset",
            "OLD",
        ])
        .unwrap()
        .1
        else {
            panic!("not a change")
        };
        assert_eq!(change["display_name"], "Mine");
        assert_eq!(change["args"], json!(["--effort", "high"]));
        assert_eq!(
            change["set_env"]["ANTHROPIC_BASE_URL"],
            "https://llm.example.com"
        );
        assert_eq!(change["remove_env"], json!(["OLD"]));
        // A value without `=` is read from stdin, not taken from the command line.
        assert_eq!(read_env, ["ANTHROPIC_API_KEY"]);
        assert!(parse(&["provider", "set", "claude"]).is_err());
        assert!(parse(&["provider", "set", "claude", "--bogus"]).is_err());
        assert!(matches!(
            parse(&["update"]).unwrap().1,
            Command::Update { action: None, .. }
        ));
        let Command::Update { action, yes } = parse(&["update", "apply", "--yes"]).unwrap().1
        else {
            panic!("not update")
        };
        assert_eq!((action.as_deref(), yes), (Some("apply"), true));
        assert!(parse(&["update", "reboot"]).is_err());
        assert!(parse(&["update", "check", "--force"]).is_err());
    }

    #[test]
    fn parses_backend_commands() {
        assert!(matches!(
            parse(&["backend"]).unwrap().1,
            Command::Backend {
                bee: None,
                switch: None
            }
        ));
        assert!(matches!(
            parse(&["backend", "list", "--bee", "Ada"]).unwrap().1,
            Command::Backend { bee: Some(b), switch: None } if b == "Ada"
        ));
        assert!(matches!(
            parse(&["backend", "use", "claude-session"]).unwrap().1,
            Command::Backend { bee: None, switch: Some((n, None)) } if n == "claude-session"
        ));
        assert!(matches!(
            parse(&["backend", "use", "claude-session", "--model", "sonnet", "--bee", "Ada"]).unwrap().1,
            Command::Backend { bee: Some(b), switch: Some((n, Some(m))) }
                if b == "Ada" && n == "claude-session" && m == "sonnet"
        ));
        assert!(matches!(
            parse(&["backend", "refresh", "--bee", "Ada"]).unwrap().1,
            Command::RefreshModels(Some(b)) if b == "Ada"
        ));
        assert!(parse(&["backend", "refresh", "--model", "x"]).is_err());
        assert!(parse(&["backend", "use"]).is_err());
        assert!(parse(&["backend", "list", "--model", "x"]).is_err());
        assert!(parse(&["backend", "use", "x", "--model"]).is_err());
    }

    fn parse(args: &[&str]) -> anyhow::Result<(Option<PathBuf>, Command)> {
        parse_args(args.iter().map(|s| s.to_string()).collect())
    }

    /// "Turn it off" must never become an uninstall (owner's test, 2026-11-19).
    #[test]
    fn prompts_map_turning_off_to_stop_app() {
        for prompt in [QUEEN_PROMPT, BEE_PROMPT] {
            assert!(prompt.contains("`stop_app`"));
            assert!(prompt.contains("`start_app`"));
            assert!(prompt.contains("turn off"));
            assert!(prompt.contains("only when"));
        }
    }

    #[test]
    fn parses_serve_and_tool_commands() {
        assert!(matches!(
            parse(&[]).unwrap(),
            (None, Command::Serve { demo: false })
        ));
        assert!(matches!(
            parse(&["demo"]).unwrap().1,
            Command::Serve { demo: true }
        ));
        let (cfg, cmd) = parse(&["--config", "/x.toml", "tool", "cluster_health"]).unwrap();
        assert_eq!(cfg, Some(PathBuf::from("/x.toml")));
        assert!(
            matches!(cmd, Command::Tool { name, args } if name == "cluster_health" && args == json!({}))
        );
        let (_, cmd) = parse(&["tool", "comb_info", r#"{"a":1}"#]).unwrap();
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
        assert!(matches!(
            parse(&["chat", "--new"]).unwrap().1,
            Command::NewChat(true)
        ));
        assert!(matches!(
            parse(&["new-chat"]).unwrap().1,
            Command::NewChat(false)
        ));
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
            addresses: None,
            port_offset: 0,
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
