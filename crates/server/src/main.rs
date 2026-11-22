#![forbid(unsafe_code)]
//! The Melissa server: runs the lead agent, coordinates the cluster and hosts the agent layer and the web UI.
//!
//! Usage:
//!   `melissa-server [--config <path>]`                         serve the web UI and agents
//!   `melissa-server [--config <path>] demo`                    the same, for a first run from
//!                                                            source (summary, quieter log)
//!   `melissa-server [--config <path>] tool <name> [json-args]` run one tool and print its JSON
//!   `melissa-server [--config <path>] tool --wait <name> [json]` same, in the running daemon,
//!                                                            waiting for confirmations
//!   `sudo melissa-server app-credentials <app>`                the owner's view of an app's
//!                                                            generated credentials
//!   `melissa-server setup-code`                              the one-time code that claims a
//!                                                            new box (prints a fresh one)
//!   `melissa-server reset-owner`                             a one-hour link that sets a new
//!                                                            owner password
//!   `melissa-server factory-reset --confirm melissa`         erase Melissa's data, start again
//!                                                            unclaimed (container install)
//!   `melissa-server factory-reset run [--unit|--confirm melissa]` the root side of it
//!   `melissa-server [--config <path>] chat [--agent <agent>]`      talk to the lead agent or an agent
//!   `melissa-server [--config <path>] fresh-start [--agent <agent>]` clear an agent's memory
//!   `melissa-server [--config <path>] ask [--agent <agent>] <message>`  one message, stream it
//!   `melissa-server [--config <path>] confirm list|approve <id>|deny <id>`
//!   `melissa-server [--config <path>] agent list|rename <agent> <name>`  this device's agents
//!   `melissa-server [--config <path>] workspace list|reassign <workspace> --to <agent>`  workspaces and owners
//!   `melissa-server [--config <path>] app details|logs <app>`  an app's status, its last log lines
//!   `melissa-server [--config <path>] app force-delete <app> --yes`  remove it and its data now;
//!                                                            its agent and the lead agent are told
//!
//! The client commands talk to the running daemon over its host-local socket
//! (`$XDG_RUNTIME_DIR/melissa/api.sock`, the service's `/run/melissa/api.sock`): only root
//! and Melissa's own user may connect, and they act as the owner (the web UI needs a login).
//!
//! Config: `$XDG_CONFIG_HOME/melissa/server.toml` (optional). Audit log, agent working
//! directory, device id (`device.json`), agent registry (`agents/`), "Always allow" rules
//! (`permissions.json`) and scheduled jobs (`schedules.json`): `$XDG_STATE_HOME/melissa/`. Cluster credentials: the `melissa.kubeconfig`
//! systemd credential, else `kubeconfig` in the config, else `/run/melissa-kube/server.kubeconfig`
//! (written by root once k3s is up). They are looked up on every cluster call, so the lead agent
//! never waits for k3s and attaches to it without a restart. Downloaded data (the speech model
//! for voice input): `$XDG_DATA_HOME/melissa/`.

mod client;
mod config;
mod credentials;
mod preflight;
mod reset;

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{bail, Context};
use melissa_agent::{
    device::DeviceIdentity,
    files::{FileBoxes, MyFilesTool},
    lead::{
        agents::{AgentLimits, AgentPool},
        events::EventLog,
        registry::AgentRegistry,
        schedules::{ScheduleJobTool, Schedules, TimeZone},
        transcript::Transcript,
        Lead, LeadSettings,
    },
    mcp::McpServer,
    session::SessionBackend,
    tools::{
        AgentNames, DeviceInfo, Permissions, ResourceOwners, Scope, SetupNotices, TaskContext,
        Tool, ToolRegistry, WorkspaceStore,
    },
    AcpBackend, AuditLog, Backend, ClaudeBackend, CliBackend, EchoBackend, EchoSessionBackend,
    EgressProxy, McpLink, TaskBoard,
};
use melissa_cluster::{
    app_tools, parse_publish_bind, AppCredentials, AppSettings, ClusterHealth, KubeApps,
    KubeSecretReader, LazyCluster, ReachCheck, Workspaces,
};
use melissa_settings::{
    app_access::{tool::SetAppAccess, AppAccessControl},
    tailscale::{
        tool::{EnableTailscale, TailscaleStatus},
        Management, TailscaleControl,
    },
};
use serde_json::{json, Value};
use tracing_subscriber::EnvFilter;

use config::{BackendSpec, Dirs, ServerConfig};
use melissa_settings::{
    providers::{BackendFacts, ProviderFile, ProviderStore},
    updates::{Release, UpdateEnv},
};

/// The lead agent's system prompt and every agent's (reviewed text files).
const LEAD_PROMPT: &str = include_str!("../prompts/lead.md");
const AGENT_PROMPT: &str = include_str!("../prompts/agent.md");

const USAGE: &str = "usage: melissa-server [--config <path>] [COMMAND]
  (none)                      serve the web UI, the lead agent and agents
  demo                        the same for a first run from source: a short summary
                              and only warnings in the log
  tool <name> [json-args]     run one tool here and print its JSON result (unattended:
                              anything that needs a confirmation is refused)
  tool --wait <name> [json]   run one tool in the running daemon; waits for a confirmation
                              (answer it with `confirm approve <id>` from another shell)
  app-credentials <app>       show an app's generated credentials (owner only, needs root)
  setup-code                  the one-time code to claim a new Melissa in the web UI
                              (a fresh one, valid 1 hour)
  reset-owner                 a link, valid 1 hour, that sets a new password for the owner
  factory-reset --confirm melissa
                              erase Melissa's data on this device (people, helpers, chats,
                              files, apps and their data, provider sign-ins and keys, settings,
                              logs) and start her again unclaimed; keeps her version, the
                              install settings and the voice model (container install)
  factory-reset run [--confirm melissa]
                              (root, container) the same without the server; the unit runs
                              `run --unit`. By hand: podman exec melissa
                              /usr/libexec/melissa/factory-reset --confirm melissa
  chat [--agent <agent>]          talk to the lead agent, or directly to an agent (Ctrl-C cancels a turn)
  fresh-start [--agent <agent>]  clear the lead agent's (or an agent's) memory; the chat goes on
                              (clears its taint; old names: new-chat, chat --new)
  ask [--agent <agent>] <message> send one message and stream the answer
  confirm list|approve <id>|deny <id>
  agent list                    this device's agents (also: agents)
  agent rename <agent> <name>     give an agent a new name
  backend [list] [--agent <agent>]
                              the agents the lead agent (or an agent) can run on, which one it uses
                              and the models each offers
  backend use <name> [--model <id>] [--agent <agent>]
                              switch the lead agent (or an agent) to that agent from its next message
  backend refresh [--agent <agent>]
                              ask every usable agent for its models now (no prompt), then list
  workspace list                   every workspace (installed app) and the agent that looks after it
  workspace reassign <workspace> --to <agent>
                              give a workspace to another agent (your decision, no confirmation)
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
                              Melissa and need --yes
  app-access apply [<file>]   (root, container: melissa-app-access.service) put the app port
                              filter in place for the setting in <file> (default
                              /etc/melissa/app-access.toml), verify it, write its status
  app-access status           what the filter lets in now
  tailscale enabled [<file>]  (root, container: melissa-tailscaled.service) exit 0 when
                              Melissa's own Tailscale should run (setting in <file>, default
                              /etc/melissa/tailscale.toml; never when the host runs Tailscale)";

#[derive(Debug, PartialEq, Eq)]
enum AppCommand {
    Details,
    Logs,
    ForceDelete,
}

enum Command {
    /// `demo` = true: first run from a source checkout (quieter log).
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
    /// `setup-code`: the code that claims a box without an owner.
    SetupCode,
    /// `reset-owner`: a reset link for the owner's password.
    ResetOwner,
    /// `factory-reset --confirm <word>`: ask the daemon for a factory reset.
    FactoryReset(String),
    /// `app details|logs|force-delete <app>`: the same as the app dialog.
    App(AppCommand, String),
    /// Talk to the lead agent (None) or an agent.
    Chat(Option<String>),
    /// Clear the memory of the lead agent (None) or an agent: "Fresh start" (`true`: then
    /// talk). `new-chat` and `chat --new` are old names for it (one release).
    FreshStart(Option<String>, bool),
    Ask(Option<String>, String),
    Confirm(String, Option<u64>),
    Agents,
    RenameAgent(String, String),
    Workspaces,
    /// `backend list|use`: the lead agent's (None) or an agent's backend.
    Backend {
        agent: Option<String>,
        /// `use <name> [--model <id>]`; None: list.
        switch: Option<(String, Option<String>)>,
    },
    /// `backend refresh`: probe every usable backend's models (forced), then list.
    RefreshModels(Option<String>),
    Reassign {
        workspace: String,
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
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // Root's helper for the app port filter: no config, no state, nothing else.
    if argv.first().is_some_and(|c| c == "app-access") {
        return app_access_command(&argv[1..]);
    }
    // Root's side of the factory reset (melissa-factory-reset.service, or by hand).
    if argv.first().is_some_and(|c| c == "factory-reset") && argv.get(1).is_some_and(|c| c == "run")
    {
        return reset::command(&argv[2..]);
    }
    // Root's check before tailscaled starts (the unit's ExecCondition).
    if argv.first().is_some_and(|c| c == "tailscale") {
        return tailscale_command(&argv[1..]);
    }
    let (config_path, command) = parse_args(argv)?;
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
    // Only the daemon (and an offline tool run, which opens the same state) migrates: client
    // commands may run as another user and must not move or create the daemon's files.
    if matches!(command, Command::Serve { .. } | Command::Tool { .. }) {
        migrate_old_names(&dirs);
    }
    let config_path = config_path.unwrap_or_else(|| {
        let new = dirs.config.join("server.toml");
        let old = dirs.config.join("queen.toml");
        // Not migrated yet (a client command before the daemon ran): read the old file.
        if !new.exists() && old.exists() {
            old
        } else {
            new
        }
    });
    let mut config = ServerConfig::load(&config_path)?;
    if matches!(command, Command::Serve { .. }) {
        let text = std::fs::read_to_string(&config_path).unwrap_or_default();
        for key in config::legacy_keys(&text) {
            tracing::warn!(file = %config_path.display(), key, "old config key; it still works for now, please rename it");
        }
    }
    config.validate()?;
    // The owner's provider settings (web UI, `provider set`) win over server.toml.
    let base = config.clone();
    let providers_path = dirs.config.join(melissa_settings::providers::FILE_NAME);
    let provider_file = load_providers(&providers_path);
    let facts = config.apply_providers(&provider_file);
    config.validate()?;
    if config.apps.catalog.is_some() && matches!(command, Command::Serve { .. }) {
        tracing::warn!(
            "[apps] catalog is ignored: the app catalog was dropped (agents deploy any app); \
             remove the setting"
        );
    }

    let socket = client::socket_path(&dirs.scratch);
    let api = client::Client::new(&socket);
    let to = |agent: &Option<String>| match agent {
        Some(b) => client::Client::new(&socket).with_agent(b),
        None => client::Client::new(&socket),
    };
    match &command {
        Command::Chat(agent) => return client::chat(&to(agent)).await,
        Command::FreshStart(agent, talk) => {
            client::fresh_start(&api, agent.as_deref()).await?;
            if *talk {
                return client::chat(&to(agent)).await;
            }
            return Ok(());
        }
        Command::Ask(agent, message) => return client::ask(&to(agent), message).await,
        Command::Confirm(action, id) => return client::confirm(&api, action, *id).await,
        Command::Agents => return client::agents(&api).await,
        Command::RenameAgent(agent, name) => return client::rename_agent(&api, agent, name).await,
        Command::Workspaces => return client::workspaces(&api).await,
        Command::Backend { agent, switch } => {
            let to = to(agent);
            return match switch {
                None => client::backends(&to).await,
                Some((name, model)) => client::use_backend(&to, name, model.as_deref()).await,
            };
        }
        Command::RefreshModels(agent) => {
            client::refresh_models(&api).await?;
            return client::backends(&to(agent)).await;
        }
        Command::Reassign { workspace, to } => return client::reassign(&api, workspace, to).await,
        Command::App(AppCommand::Details, app) => return client::app_details(&api, app).await,
        Command::App(AppCommand::Logs, app) => return client::app_logs(&api, app).await,
        Command::App(AppCommand::ForceDelete, app) => {
            return client::app_force_delete(&api, app).await
        }
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
        Command::SetupCode => return client::setup_code(&api).await,
        Command::ResetOwner => return client::reset_owner(&api, config.listen).await,
        Command::FactoryReset(word) => return client::factory_reset(&api, word).await,
        Command::AppCredentials(app) => {
            return credentials::show(
                app,
                &config.apps.domain,
                std::path::Path::new(credentials::ADMIN_KUBECONFIG),
            )
            .await
        }
        Command::Serve { .. } | Command::Tool { .. } => {}
    }

    let audit = AuditLog::new(dirs.state.join("audit.jsonl"));
    // This device's device id (made once) and its agents.
    let device = DeviceIdentity::load_or_create(&dirs.state.join("device.json"))?;
    let registry = AgentRegistry::open(&dirs.state.join("agents"), &device.id)?;
    let names: Arc<dyn AgentNames> = Arc::new(registry.clone());
    let creds = std::env::var_os("CREDENTIALS_DIRECTORY").map(PathBuf::from);
    let kubeconfigs = kubeconfig_candidates(creds.as_deref(), config.kubeconfig.as_deref());
    let cluster = LazyCluster::for_lead(creds.as_deref(), config.kubeconfig.as_deref());
    let mut tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(DeviceInfo::new()),
        Arc::new(ClusterHealth::new(Some(Arc::new(cluster)))),
    ];
    // App tools use the separate write identity (melissa-apps); read-only tools above
    // never hold it.
    let apps = Arc::new(KubeApps::new(LazyCluster::for_apps(
        config.apps.kubeconfig.as_deref(),
    )));
    // The owner's setup password card (web UI only; no tool reaches it): the write identity
    // binds the get-only reader per app right before a reveal (docs/DECISIONS.md, 2026-11-21).
    let setup_passwords: Arc<dyn melissa_web::SetupPasswords> =
        Arc::new(credentials::SetupPasswords(AppCredentials::new(
            apps.clone(),
            apps.clone(),
            Arc::new(KubeSecretReader::for_runtime(
                config.apps.credentials_kubeconfig.as_deref(),
            )),
        )));
    // New setup passwords go to the chat where the install ran (app and login name only).
    let setup_notices = SetupNotices::new();
    // Who may open apps: Settings → Apps and `set_app_access` (container install, where the
    // app ports are published on every host address and Melissa's filter decides; elsewhere
    // the host's firewall does).
    let app_access = melissa_settings::app_access::available()
        .then(|| AppAccessControl::system(&dirs.config, Some(audit.clone())));
    // Melissa's own Tailscale (container image; off by default), or "not managed by Melissa"
    // when the host runs Tailscale or this install has none (docs/DECISIONS.md, 2026-11-21).
    let tailscale = TailscaleControl::system(&dirs.config, Some(audit.clone()));
    tracing::info!(
        managed_by = tailscale.management().as_str(),
        node = tailscale.node_name(),
        "tailscale"
    );
    let app_settings = AppSettings {
        domain: config.apps.domain.clone(),
        addresses: config.apps.addresses.clone(),
        port_offset: config.apps.port_offset,
        device: device.id.clone(),
        agents: Some(names.clone()),
        // Links name only addresses that really reach the app (incident: a container install
        // published app ports on loopback while links named the Tailscale address).
        reach: Some(ReachCheck::tcp(publish_bind())),
        access: app_access.clone(),
        setup_notices: Some(setup_notices.clone()),
        tailscale: Some(tailscale.clone()),
    };
    tools.extend(app_tools(app_settings.clone(), Some(apps.clone())));
    if let Some(control) = &app_access {
        tools.push(Arc::new(SetAppAccess(control.clone())));
    }
    // Status and a proposal to enable; never the sign-in URL (web UI only).
    if tailscale.management() != Management::Unavailable {
        tools.push(Arc::new(TailscaleStatus(tailscale.clone())));
        tools.push(Arc::new(EnableTailscale(tailscale.clone())));
    }
    // Who owns which workspace, for the scope check before every workspace-changing tool call; also
    // the list of workspaces and the reassign path.
    let workspaces = Arc::new(Workspaces::new(app_settings, Some(apps)));
    if matches!(command, Command::Serve { .. }) {
        tokio::spawn(relabel_old_namespaces(workspaces.clone()));
    }
    let wiring = Wiring {
        owners: workspaces.clone(),
        workspaces,
        names,
        registry,
        device: device.id,
        setup_notices,
    };

    match command {
        Command::Tool { name, args } => run_tool(audit, tools, &wiring, &name, args).await,
        _ => {
            let start = Startup {
                config_path,
                app_access,
                tailscale,
                setup_passwords,
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
    /// Who may open apps (container install only).
    app_access: Option<AppAccessControl>,
    /// Melissa's own Tailscale (Settings → Tailscale).
    tailscale: TailscaleControl,
    /// The setup password card's server side.
    setup_passwords: Arc<dyn melissa_web::SetupPasswords>,
    /// Where the lead agent looks for cluster credentials (any present = k3s is there).
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
    base: ServerConfig,
}

/// Move config and state from the old role names (`queen.toml`, `comb.json`, `bees/`, …; see
/// `melissa_agent::migrate`). Idempotent. A failure is logged and startup goes on: the new
/// code then starts with fresh state rather than not at all, and the old files stay for a
/// later run.
fn migrate_old_names(dirs: &Dirs) {
    use melissa_agent::migrate::{migrate_config, migrate_state};
    for (what, result) in [
        ("config", migrate_config(&dirs.config)),
        ("state", migrate_state(&dirs.state)),
    ] {
        match result {
            Ok(report) => {
                for step in report.steps {
                    tracing::info!(what, "migrated old names: {step}");
                }
            }
            Err(e) => tracing::error!(what, "migrating old names failed: {e}"),
        }
    }
}

/// Move app namespaces from the old label keys to the new ones once the cluster answers
/// (`Workspaces::relabel_old`); retried every 30 s until one pass goes through.
async fn relabel_old_namespaces(workspaces: Arc<Workspaces>) {
    loop {
        match workspaces.relabel_old().await {
            Ok(0) => break,
            Ok(n) => {
                tracing::info!(namespaces = n, "moved app namespaces to the new label keys");
                break;
            }
            Err(e) => tracing::debug!("app namespace labels not migrated yet: {e}"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
    // Apps from the old catalog get their spec stored, so update_app works on them.
    loop {
        match workspaces.migrate_catalog_apps().await {
            Ok(0) => return,
            Ok(n) => {
                tracing::info!(apps = n, "stored specs for apps from the old catalog");
                return;
            }
            Err(e) => tracing::debug!("catalog apps not converted yet: {e}"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
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

/// `app-access apply [<file>]` and `app-access status` (see USAGE and
/// `melissa_settings::app_access`). `apply` exits non-zero when the setting is not in force
/// (the unit then fails; the filter is "this device" or closed, and the status says why).
fn app_access_command(args: &[String]) -> anyhow::Result<()> {
    use melissa_settings::app_access::{self as access, NftCommand};
    let status_path = PathBuf::from(access::STATUS_PATH);
    match args {
        [c, rest @ ..] if c == "apply" && rest.len() <= 1 => {
            let setting = rest.first().map_or_else(
                || PathBuf::from("/etc/melissa").join(access::FILE_NAME),
                PathBuf::from,
            );
            let route = std::fs::read_to_string("/proc/net/route").unwrap_or_default();
            let status = access::apply(
                &NftCommand::default(),
                access::read_setting(&setting),
                access::default_gateway(&route),
            );
            access::write_status(&status_path, &status)
                .with_context(|| format!("cannot write {}", status_path.display()))?;
            for e in &status.errors {
                eprintln!("melissa-app-access: {e}");
            }
            let open = status
                .open_to
                .map_or("nothing from outside (closed)", |a| a.label());
            println!(
                "melissa-app-access: wanted {}, open to {open}",
                status.wanted.label()
            );
            if status.ok() {
                Ok(())
            } else if status.verified {
                bail!("the setting is not in force; the filter fails closed")
            } else {
                bail!("no filter could be put in place or verified")
            }
        }
        [c] if c == "status" => {
            let text = std::fs::read_to_string(&status_path)
                .with_context(|| format!("no status yet ({})", status_path.display()))?;
            println!("{text}");
            Ok(())
        }
        _ => bail!("usage: melissa-server app-access apply [<setting file>] | app-access status"),
    }
}

/// `tailscale enabled [<setting file>]`: exit 0 when Melissa's own tailscaled should run (the
/// setting is on and the host doesn't run Tailscale), 1 when not. `melissa-tailscaled.service`
/// uses it as its `ExecCondition`, so the daemon comes back after a restart only while enabled.
fn tailscale_command(args: &[String]) -> anyhow::Result<()> {
    use melissa_settings::tailscale::{self as ts};
    match args {
        [c, rest @ ..] if c == "enabled" && rest.len() <= 1 => {
            let setting = rest.first().map_or_else(
                || PathBuf::from("/etc/melissa").join(ts::FILE_NAME),
                PathBuf::from,
            );
            let host = std::env::var(ts::HOST_ENV).ok();
            match ts::should_run(&setting, host.as_deref()) {
                Ok(true) => Ok(()),
                Ok(false) => {
                    println!("melissa-tailscale: off (or the host runs Tailscale)");
                    std::process::exit(1)
                }
                Err(e) => {
                    eprintln!("melissa-tailscale: {e}; treated as off");
                    std::process::exit(1)
                }
            }
        }
        _ => bail!("usage: melissa-server tailscale enabled [<setting file>]"),
    }
}

/// Where the host publishes the app ports: `MELISSA_PUBLISH_BIND`, set by the container install
/// (its `BIND`; melissa.container passes it on to this service). Unset (Nix/host install, or a
/// container installed before it was passed): `None`, app addresses are probed instead.
fn publish_bind() -> Option<std::net::IpAddr> {
    let value = std::env::var("MELISSA_PUBLISH_BIND").ok()?;
    let bind = parse_publish_bind(&value);
    match bind {
        Some(ip) => {
            tracing::info!(publish_bind = %ip, "app ports are published on this host address")
        }
        None if !value.trim().is_empty() => tracing::warn!(
            value,
            "MELISSA_PUBLISH_BIND is not an IP address; app addresses are probed instead"
        ),
        None => {}
    }
    bind
}

/// The same candidates `LazyCluster::for_lead` checks, for the startup summary.
fn kubeconfig_candidates(
    creds: Option<&std::path::Path>,
    configured: Option<&std::path::Path>,
) -> Vec<PathBuf> {
    creds
        .map(|d| d.join(melissa_cluster::CREDENTIAL_NAME))
        .into_iter()
        .chain(configured.map(std::path::Path::to_path_buf))
        .chain([PathBuf::from(melissa_cluster::RUNTIME_DIR).join(melissa_cluster::RUNTIME_FILE)])
        .collect()
}

/// Who owns which workspace and what the agents are called, shared by the daemon and `tool`.
struct Wiring {
    owners: Arc<dyn ResourceOwners>,
    workspaces: Arc<dyn WorkspaceStore>,
    names: Arc<dyn AgentNames>,
    registry: AgentRegistry,
    device: String,
    /// The install pipeline's setup password notices, for the chats.
    setup_notices: SetupNotices,
}

async fn serve(
    config: ServerConfig,
    dirs: Dirs,
    audit: AuditLog,
    mut tools: Vec<Arc<dyn Tool>>,
    wiring: Wiring,
    start: Startup,
) -> anyhow::Result<()> {
    let workdir = dirs.state.join("work");
    let lead_state = dirs.state.join("lead");
    let q = &config.lead;
    // The earlier chat comes back after a restart (the agent's session resumes too).
    let events = EventLog::with_history(Transcript::new(
        lead_state.join("transcript.jsonl"),
        q.transcript_max_kib.saturating_mul(1024),
    ));
    let pool = AgentPool::new(
        AgentLimits {
            max_concurrent: q.max_agents,
            default_timeout: Duration::from_secs(q.agent_timeout_secs),
            max_timeout: Duration::from_secs(q.agent_timeout_max_secs),
            transcript_max_bytes: q.transcript_max_kib.saturating_mul(1024),
            compact: melissa_agent::lead::CompactPolicy {
                percent: q.compact_at_percent.min(100),
                idle: Duration::from_secs(q.compact_idle_secs),
            },
        },
        events.clone(),
        wiring.registry,
        Some(wiring.workspaces),
    );
    tools.extend(pool.tools());
    // Scheduled jobs, kept in the state directory and evaluated in the box's time zone. A
    // broken file runs nothing and saves nothing (it is left for the owner to look at).
    let schedules_path = dirs.state.join("schedules.json");
    let schedules = Schedules::open(&schedules_path, TimeZone::system())
        .unwrap_or_else(|e| {
            tracing::error!("{e}; no schedules run (changes are not saved)");
            Schedules::in_memory(TimeZone::system())
        })
        .with_audit(Some(audit.clone()));
    tools.push(Arc::new(ScheduleJobTool::new(
        schedules.clone(),
        Some(wiring.names.clone()),
    )));
    // Every agent's file box, in the state directory (each bound only into its own agent's
    // sandbox); old chat attachments move in once (`Lead::new`).
    let files = FileBoxes::open(dirs.state.join("files")).with_audit(Some(audit.clone()));
    tools.push(Arc::new(MyFilesTool::new(files.clone())));
    // The owner's "Always allow" rules and No permission prompts, kept in the state directory
    // (outside every agent's sandbox). A broken file grants nothing: every request asks again.
    let permissions_path = dirs.state.join("permissions.json");
    let permissions = Permissions::open(&permissions_path).unwrap_or_else(|e| {
        tracing::error!("{e}; starting with no saved permissions (changes are not saved)");
        Permissions::in_memory()
    });
    // Confirmations wait less long than the agents' MCP clients do (see the config field).
    let mcp = McpServer::new(
        ToolRegistry::new(tools, Some(audit.clone()))
            .with_setup_notices(wiring.setup_notices)
            .with_owners(wiring.owners)
            .with_agents(wiring.names)
            .with_permissions(permissions)
            .with_confirm_timeout(config.confirm_timeout()),
    );
    // Sandboxed agents reach the network only through this (allowlist, audited; public web
    // pages read-only unless `[egress] web_read = false`) and Melissa's MCP endpoint at the
    // same address as outside (config.listen).
    let web_read = if config.egress.web_read {
        melissa_agent::egress::WebRead::on()
    } else {
        melissa_agent::egress::WebRead::default()
    };
    let egress = EgressProxy::new(dirs.egress.clone(), Some(audit.clone()))
        .with_context(|| format!("setting up egress sockets in {}", dirs.egress.display()))?
        .with_mcp(mcp.clone(), config.listen)
        .with_web_read(web_read);
    match egress.netbridge() {
        Some(path) => {
            tracing::info!(netbridge = %path.display(), sockets = %dirs.egress.display(), "egress proxy ready")
        }
        None => tracing::warn!(
            "melissa-netbridge not found (next to melissa-server, /usr/bin or PATH): backends \
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
    let audit_for_auth = audit.clone();
    let board = TaskBoard::with_tools(backends, config.task_timeout(), Some(audit), link);
    let settings = LeadSettings {
        backends: q.backends.clone(),
        system_prompt: LEAD_PROMPT.into(),
        agent_prompt: AGENT_PROMPT.into(),
        agent_backend: q.agent_backend.clone(),
        state_dir: lead_state,
        workdir: workdir.join("lead"),
        tools: q.tools.clone(),
        device: wiring.device,
        files,
    };
    let lead = Lead::new(settings, sessions, board.clone(), events, &pool)
        .context("setting up the lead agent")?;
    lead.start_schedules(schedules);
    // Expired files go (Settings → Files), checked every hour.
    lead.start_file_prune(Duration::from_secs(3600));
    tracing::info!(backends = ?q.backends, "lead agent ready (opens on the first message)");

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
    tracing::info!("melissa lead listening on http://{}", config.listen);
    let report = preflight::gather(preflight::Inputs {
        listen: config.listen,
        state: &dirs.state,
        config_path: &start.config_path,
        specs: &config.backends,
        infos: &infos,
        lead_order: &q.backends,
        kubeconfigs: &start.kubeconfigs,
    })
    .await;
    // The summary is for the person who started the daemon; stdout keeps it apart from the log.
    print!("{}", report.render());
    // Logins (PLAN 6c): accounts, invites and sessions in <state>/auth (0700/0600), audited.
    let auth_audit = audit_for_auth.clone();
    let auth = melissa_web::Auth::open(&dirs.state.join("auth"))
        .context("opening the login store")?
        .with_events(Arc::new(move |ev: &melissa_auth::AuthEvent| {
            let mut record = melissa_agent::AuditRecord::now(
                ev.action,
                0,
                "",
                format!("{} (from {})", ev.detail, ev.source),
            )
            .by("ui");
            record.person = Some(ev.person.clone());
            if let Err(e) = auth_audit.append(&record) {
                tracing::error!("audit log write failed: {e}");
            }
        }));
    // A box nobody has claimed: the code goes to the console and the log only.
    // It is kept until used or expired, so a restart shows the same code.
    if let Some((code, expires)) = auth.setup_code().context("making the setup code")? {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let minutes = expires.saturating_sub(now).div_ceil(60);
        println!(
            "  Claim:    open http://{}/claim and enter the setup code {code}\n            \
             (valid {minutes} more minutes; a new one: melissa-server setup-code)",
            config.listen
        );
        melissa_web::log_setup_code(&code);
    }
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
    let prefs = melissa_settings::prefs::PrefsStore::open(
        path.with_file_name(melissa_settings::prefs::FILE_NAME),
    );
    // Voice input: whisper.cpp from the packaging (if any); the model under the data dir.
    let voice = melissa_voice::Voice::new(melissa_voice::VoiceConfig {
        data: dirs.data.clone(),
        scratch: dirs.scratch.clone(),
        program: melissa_voice::find_program(),
    });
    let settings = melissa_web::Settings::new(melissa_web::SettingsParts {
        store: ProviderStore::new(path, file),
        prefs,
        backends: facts,
        agent_default: q
            .agent_backend
            .clone()
            .or_else(|| q.backends.first().cloned()),
        check: Some(check),
        audit: Some(settings_audit),
        installer: melissa_settings::install::Installer::system(updates.kind),
        updates,
        release: Release::read(&melissa_settings::updates::release_file()),
        home: std::env::var_os("HOME").map(PathBuf::from),
        voice,
        // The ACP registry cache sits with Melissa's state; fetched only on the owner's request.
        catalog: melissa_web::Catalog::new(&dirs.state),
        app_access: start.app_access,
        setup_passwords: Some(start.setup_passwords),
        tailscale: Some(start.tailscale),
        // The container's root unit erases and restarts (Settings → Reset); elsewhere none.
        factory_reset: melissa_settings::factory_reset::Requester::system(Some(
            audit_for_auth.clone(),
        )),
        // First-run setup after the claim (device name, provider, model, extras).
        onboarding: true,
    });
    let app = melissa_web::router_with_settings(board, mcp, lead.clone(), settings, auth);
    // The host's CLI (`chat`, `ask`, `confirm`, ...) talks over a local socket that only root
    // and Melissa's user can open: no password on the box itself, the owner's authority.
    let socket = dirs.scratch.join(client::SOCKET_NAME);
    match melissa_web::bind_host(&socket) {
        Ok(host) => {
            let app = app.clone();
            tracing::info!(path = %socket.display(), "host CLI socket");
            tokio::spawn(async move {
                if let Err(e) = melissa_web::serve_host(host, &socket, app).await {
                    tracing::error!("host CLI socket stopped: {e}");
                }
            });
        }
        Err(e) => tracing::error!(
            path = %socket.display(),
            "no host CLI socket (melissa-server chat, ask, confirm won't reach this daemon): {e}"
        ),
    }
    melissa_web::serve(listener, app).await?;
    lead.close().await;
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
        .with_agents(wiring.names.clone());
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
        [c, a, app] if c == "app" && a == "details" => {
            Command::App(AppCommand::Details, app.clone())
        }
        [c, a, app] if c == "app" && a == "logs" => Command::App(AppCommand::Logs, app.clone()),
        [c, a, app, yes] if c == "app" && a == "force-delete" && yes == "--yes" => {
            Command::App(AppCommand::ForceDelete, app.clone())
        }
        [c, a, ..] if c == "app" && a == "force-delete" => bail!(
            "force-delete removes the app and all its data for good: \
             `melissa-server app force-delete <app> --yes`"
        ),
        [c, ..] if c == "app" => {
            bail!("usage: melissa-server app details|logs <app>, app force-delete <app> --yes")
        }
        [c, ..] if c == "app-adopt" => bail!(
            "app-adopt is gone: workspaces now belong to agents. Use `melissa-server workspace list` and \
             `melissa-server workspace reassign <workspace> --to <agent>`"
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
        [c, flag] if c == "chat" && flag == "--new" => Command::FreshStart(None, true),
        [c] if c == "new-chat" || c == "fresh-start" => Command::FreshStart(None, false),
        [c, flag, agent] if c == "fresh-start" && flag == "--agent" => {
            Command::FreshStart(Some(agent.clone()), false)
        }
        [c, flag, agent] if c == "chat" && flag == "--agent" => Command::Chat(Some(agent.clone())),
        [c, flag, agent, words @ ..] if c == "ask" && flag == "--agent" && !words.is_empty() => {
            Command::Ask(Some(agent.clone()), words.join(" "))
        }
        [c, words @ ..] if c == "ask" && !words.is_empty() && words[0] != "--agent" => {
            Command::Ask(None, words.join(" "))
        }
        // `workers`: hidden alias from before the cluster model (one release).
        [c] if c == "agents" || c == "workers" => Command::Agents,
        [c, a] if c == "agent" && a == "list" => Command::Agents,
        [c, a, agent, words @ ..] if c == "agent" && a == "rename" && !words.is_empty() => {
            Command::RenameAgent(agent.clone(), words.join(" "))
        }
        [c, tail @ ..] if c == "backend" || c == "backends" => parse_backend(tail)?,
        [c, tail @ ..] if c == "provider" || c == "providers" => parse_provider(tail)?,
        [c] if c == "setup-code" => Command::SetupCode,
        [c] if c == "reset-owner" => Command::ResetOwner,
        [c, flag, word] if c == "factory-reset" && flag == "--confirm" => {
            Command::FactoryReset(word.clone())
        }
        [c, ..] if c == "factory-reset" => bail!(
            "factory-reset erases Melissa's data on this device ({}) and starts her again \
             unclaimed; it keeps {}. To go ahead: melissa-server factory-reset --confirm {}",
            melissa_settings::factory_reset::ERASED,
            melissa_settings::factory_reset::KEPT,
            melissa_settings::factory_reset::CONFIRM_WORD
        ),
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
        [c] if c == "workspace" => Command::Workspaces,
        [c, a] if c == "workspace" && a == "list" => Command::Workspaces,
        [c, a, workspace, flag, to] if c == "workspace" && a == "reassign" && flag == "--to" => {
            Command::Reassign {
                workspace: workspace.clone(),
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

/// `backend [list] [--agent <agent>]`, `backend use <name> [--model <id>] [--agent <agent>]`,
/// `backend refresh [--agent <agent>]`.
fn parse_backend(args: &[String]) -> anyhow::Result<Command> {
    let mut words = Vec::new();
    let (mut agent, mut model) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--agent" => agent = Some(it.next().context("--agent needs an agent's name")?.clone()),
            "--model" => model = Some(it.next().context("--model needs a model id")?.clone()),
            _ => words.push(a.clone()),
        }
    }
    let switch = match words.as_slice() {
        [] => None,
        [l] if l == "list" => None,
        [u, name] if u == "use" => Some((name.clone(), model.take())),
        [r] if r == "refresh" && model.is_none() => return Ok(Command::RefreshModels(agent)),
        _ => bail!("{USAGE}"),
    };
    if model.is_some() {
        bail!("--model goes with `backend use <name>`");
    }
    Ok(Command::Backend { agent, switch })
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
                agent: None,
                switch: None
            }
        ));
        assert!(matches!(
            parse(&["backend", "list", "--agent", "Ada"]).unwrap().1,
            Command::Backend { agent: Some(b), switch: None } if b == "Ada"
        ));
        assert!(matches!(
            parse(&["backend", "use", "claude-session"]).unwrap().1,
            Command::Backend { agent: None, switch: Some((n, None)) } if n == "claude-session"
        ));
        assert!(matches!(
            parse(&["backend", "use", "claude-session", "--model", "sonnet", "--agent", "Ada"]).unwrap().1,
            Command::Backend { agent: Some(b), switch: Some((n, Some(m))) }
                if b == "Ada" && n == "claude-session" && m == "sonnet"
        ));
        assert!(matches!(
            parse(&["backend", "refresh", "--agent", "Ada"]).unwrap().1,
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
        for prompt in [LEAD_PROMPT, AGENT_PROMPT] {
            assert!(prompt.contains("`stop_app`"));
            assert!(prompt.contains("`start_app`"));
            assert!(prompt.contains("turn off"));
            assert!(prompt.contains("only when"));
        }
    }

    /// Without k3s the lead passes the tools' `app_platform` words on, never "no helpers".
    #[test]
    fn lead_prompt_passes_on_the_app_platform_words() {
        assert!(LEAD_PROMPT.contains("`app_platform`"));
        assert!(LEAD_PROMPT.contains("Never give helpers or your own checks as the reason"));
    }

    #[test]
    fn factory_reset_needs_the_word() {
        assert!(
            matches!(parse(&["factory-reset", "--confirm", "melissa"]).unwrap().1, Command::FactoryReset(w) if w == "melissa")
        );
        for bad in [
            &["factory-reset"][..],
            &["factory-reset", "--yes"],
            &["factory-reset", "--confirm"],
        ] {
            let e = parse(bad).err().unwrap().to_string();
            assert!(e.contains("--confirm melissa"), "{e}");
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
        let (_, cmd) = parse(&["tool", "device_info", r#"{"a":1}"#]).unwrap();
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
            matches!(parse(&["app", "details", "nextcloud"]).unwrap().1, Command::App(AppCommand::Details, a) if a == "nextcloud")
        );
        assert!(matches!(
            parse(&["app", "logs", "nextcloud"]).unwrap().1,
            Command::App(AppCommand::Logs, _)
        ));
        assert!(matches!(
            parse(&["app", "force-delete", "nextcloud", "--yes"])
                .unwrap()
                .1,
            Command::App(AppCommand::ForceDelete, _)
        ));
        // Never without --yes.
        assert!(parse(&["app", "force-delete", "nextcloud"]).is_err());
        assert!(parse(&["app", "force-delete", "nextcloud", "-y"]).is_err());
        assert!(parse(&["app", "remove", "nextcloud"]).is_err());
        // Replaced by `workspace reassign`.
        assert!(parse(&["app-adopt", "blog"]).is_err());
        assert!(parse(&["bogus"]).is_err());
        assert!(matches!(parse(&["chat"]).unwrap().1, Command::Chat(None)));
        assert!(matches!(
            parse(&["chat", "--new"]).unwrap().1,
            Command::FreshStart(None, true)
        ));
        assert!(matches!(
            parse(&["new-chat"]).unwrap().1,
            Command::FreshStart(None, false)
        ));
        assert!(matches!(
            parse(&["fresh-start"]).unwrap().1,
            Command::FreshStart(None, false)
        ));
        assert!(
            matches!(parse(&["fresh-start", "--agent", "Ada"]).unwrap().1, Command::FreshStart(Some(a), false) if a == "Ada")
        );
        assert!(
            matches!(parse(&["chat", "--agent", "Ada"]).unwrap().1, Command::Chat(Some(b)) if b == "Ada")
        );
        assert!(
            matches!(parse(&["ask", "how", "are you?"]).unwrap().1, Command::Ask(None, m) if m == "how are you?")
        );
        assert!(
            matches!(parse(&["ask", "--agent", "Ada", "hi", "there"]).unwrap().1, Command::Ask(Some(b), m) if b == "Ada" && m == "hi there")
        );
        assert!(parse(&["ask"]).is_err());
        assert!(parse(&["ask", "--agent", "Ada"]).is_err());
        assert!(matches!(
            parse(&["agent", "list"]).unwrap().1,
            Command::Agents
        ));
        assert!(
            matches!(parse(&["agent", "rename", "Ada", "Ada", "Lovelace"]).unwrap().1, Command::RenameAgent(b, n) if b == "Ada" && n == "Ada Lovelace")
        );
        assert!(parse(&["agent", "rename", "Ada"]).is_err());
        assert!(matches!(
            parse(&["workspace", "list"]).unwrap().1,
            Command::Workspaces
        ));
        assert!(
            matches!(parse(&["workspace", "reassign", "nextcloud", "--to", "Bo"]).unwrap().1, Command::Reassign { workspace, to } if workspace == "nextcloud" && to == "Bo")
        );
        assert!(parse(&["workspace", "reassign", "nextcloud", "Bo"]).is_err());
        assert!(matches!(parse(&["agents"]).unwrap().1, Command::Agents));
        assert!(matches!(parse(&["workers"]).unwrap().1, Command::Agents));
        assert!(matches!(parse(&["confirm"]).unwrap().1, Command::Confirm(a, None) if a == "list"));
        assert!(
            matches!(parse(&["confirm", "approve", "#3"]).unwrap().1, Command::Confirm(a, Some(3)) if a == "approve")
        );
        assert!(parse(&["confirm", "deny", "x"]).is_err());
        assert!(parse(&["confirm", "maybe", "1"]).is_err());
    }

    #[tokio::test]
    async fn tool_command_refuses_unknown_tools() {
        let dir = std::env::temp_dir().join(format!("melissa-server-tool-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let audit = AuditLog::new(dir.join("audit.jsonl"));
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(ClusterHealth::new(None))];
        let registry = AgentRegistry::open(&dir.join("agents"), "d-0a1b2c3d").unwrap();
        let settings = AppSettings {
            domain: "example.com".into(),
            addresses: None,
            port_offset: 0,
            device: "d-0a1b2c3d".into(),
            agents: None,
            reach: None,
            access: None,
            setup_notices: None,
            tailscale: None,
        };
        let workspaces = Arc::new(Workspaces::new::<KubeApps>(settings, None));
        let wiring = Wiring {
            owners: workspaces.clone(),
            workspaces,
            names: Arc::new(registry.clone()),
            registry,
            device: "d-0a1b2c3d".into(),
            setup_notices: SetupNotices::new(),
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
