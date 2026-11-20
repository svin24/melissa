//! Settings → Providers → Add provider (2026-11-20, owner-requested): a panel with three
//! steps, 1 Provider → 2 Identity → 3 Config.
//!
//! 1. Provider: the agents Melissa runs itself (Claude Code, Grok, OpenCode, Gemini CLI, Codex,
//!    Muse Code; one removed
//!    earlier comes back and continues to its usual Install and Sign in on the Providers page),
//!    or an agent from the public ACP registry, or a local ACP command entered by hand.
//!    - The registry is fetched only when the owner presses "Load the list" / "Refresh"
//!      (`POST /ui/settings/providers/registry`), never in the background or on a page view,
//!      and cached in Melissa's state directory (`melissa_settings::registry`). Search filters
//!      the cached copy.
//!    - "Add" on a registry agent with a release binary for this machine and a SHA-256:
//!      Melissa downloads it (https), checks the checksum, unpacks it into the agents' home
//!      (`~/.local/share/melissa-agents/<id>/<version>`), checks that it answers ACP
//!      `initialize`, and saves it as an added provider (`providers.toml` `[added.<id>]`).
//!      Anything else (npx, uvx, no checksum) offers "Enter manually".
//!    - "Enter manually": a name and the command line of an ACP agent already on this
//!      computer; saved only after it answers `initialize`.
//! 2. Identity: an API key (stored like the other provider secrets: `providers.toml`, 0600,
//!    write-only in the UI). Built-in providers use their own sign-in instead.
//! 3. Config: the hosts it may reach (deny by default, through the egress proxy). Finishing
//!    applies the change (Melissa restarts when idle); the agent then shows up in the model
//!    picker once it is installed and usable.
//!
//! Every add is audited (`settings.provider.add`, `settings.provider.registry`).

use std::{
    collections::BTreeMap,
    path::{Component, Path as FsPath, PathBuf},
    sync::{Arc, Mutex},
};

use axum::{
    extract::{Form, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Json,
};
use futures::future::BoxFuture;
use maud::{html, Markup};
use melissa_agent::acp::probe::{ProbeInfo, PROBE_TIMEOUT};
use melissa_settings::{
    providers::{
        check_display_name, check_hosts, check_var, default_label, is_secret_name, provider_id,
        valid_id, AddedProvider, Change, BUILT_IN,
    },
    registry::{self, BinaryInstall, Cache, Install, Registry, RegistryAgent},
    sha256_hex,
    updates::{ago, now_secs},
    version,
};
use melissa_voice::fetch::Fetch;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{apply, pages::Flash};
use crate::{
    components::{
        actions, add_panel, disclosure, group, lines_field, link, link_button, live_region, notice,
        provider_tiles, registry_list, search_box, settings_form, stepper, text_field,
        ProviderTile, RegistryAction, RegistryRow, Tone,
    },
    is_htmx,
    ui::Ui,
};

/// The page of the panel.
pub(super) const ADD_URL: &str = "/settings/providers/add";
/// DOM id of the panel (its forms swap it).
const PANEL_ID: &str = "add-provider";
/// DOM id of the registry list (search and installs swap it).
const REGISTRY_ID: &str = "acp-registry";
const REGISTRY_URL: &str = "/ui/settings/providers/registry";
const STEPS: [&str; 3] = ["Provider", "Identity", "Config"];
/// The built-in providers offered in the grid, with their makers.
const KNOWN: [(&str, &str); 6] = [
    ("claude", "Anthropic"),
    ("grok", "xAI"),
    ("opencode", "OpenCode"),
    ("gemini", "Google"),
    ("codex-acp", "OpenAI"),
    ("muse", "Meta"),
];

/// Checks a program for ACP (`initialize`), for the manual and registry adds. Replaced in tests.
pub type ProbeFn =
    Arc<dyn Fn(ProbeRequest) -> BoxFuture<'static, Result<ProbeInfo, String>> + Send + Sync>;

/// One probe: the program, its ACP arguments and the agents' home.
pub struct ProbeRequest {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub home: Option<PathBuf>,
}

/// One registry install, by registry id.
#[derive(Debug, Clone, PartialEq, Eq)]
enum JobState {
    Working(&'static str),
    /// Saved as this provider id.
    Done(String),
    Failed(String),
}

/// The registry, its downloads and the ACP check. Cheap to clone.
#[derive(Clone)]
pub struct Catalog {
    inner: Arc<CatalogInner>,
}

struct CatalogInner {
    /// None: no HTTPS client (no root certificates); the registry can't be loaded.
    fetch: Option<Arc<dyn Fetch>>,
    cache: Option<Cache>,
    platform: String,
    probe: ProbeFn,
    jobs: Mutex<BTreeMap<String, JobState>>,
}

/// The real ACP check: the program as Melissa's user, in the agents' home, with a small
/// clean environment.
fn real_probe() -> ProbeFn {
    Arc::new(|req: ProbeRequest| {
        Box::pin(async move {
            let home = req.home.unwrap_or_else(std::env::temp_dir);
            let cwd = if home.is_dir() {
                home.clone()
            } else {
                std::env::temp_dir()
            };
            let env = vec![
                ("HOME".to_string(), home.display().to_string()),
                (
                    "PATH".to_string(),
                    std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
                ),
                ("LANG".to_string(), "C.UTF-8".to_string()),
            ];
            melissa_agent::acp::probe::probe(&req.program, &req.args, &cwd, &env, PROBE_TIMEOUT)
                .await
                .map_err(|e| melissa_settings::clean_text(&e.to_string(), 300))
        })
    })
}

impl Catalog {
    /// The real one: HTTPS downloads, the cache in `state_dir`.
    pub fn new(state_dir: &FsPath) -> Self {
        let fetch = melissa_voice::fetch::Https::new()
            .map(|h| Arc::new(h) as Arc<dyn Fetch>)
            .map_err(|e| tracing::warn!("ACP registry unavailable: {e}"))
            .ok();
        Self::with_parts(
            fetch,
            Some(state_dir.join(registry::CACHE_FILE)),
            registry::platform(),
            None,
        )
    }

    /// No registry and no downloads (routers without a lead agent).
    pub fn unavailable() -> Self {
        Self::with_parts(None, None, registry::platform(), None)
    }

    /// From parts (tests: a fake fetcher, a cache path, a platform, a fake probe).
    pub fn with_parts(
        fetch: Option<Arc<dyn Fetch>>,
        cache: Option<PathBuf>,
        platform: String,
        probe: Option<ProbeFn>,
    ) -> Self {
        Self {
            inner: Arc::new(CatalogInner {
                fetch,
                cache: cache.map(Cache::new),
                platform,
                probe: probe.unwrap_or_else(real_probe),
                jobs: Mutex::default(),
            }),
        }
    }

    fn cached(&self) -> Option<(Registry, u64)> {
        self.inner.cache.as_ref()?.load(&self.inner.platform)
    }

    fn jobs(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, JobState>> {
        self.inner.jobs.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn set_job(&self, id: &str, state: JobState) {
        self.jobs().insert(id.to_string(), state);
    }

    fn working(&self) -> bool {
        self.jobs()
            .values()
            .any(|j| matches!(j, JobState::Working(_)))
    }

    async fn probe(&self, req: ProbeRequest) -> Result<ProbeInfo, String> {
        (self.inner.probe)(req).await
    }
}

/// Read a whole response body, at most `max` bytes.
async fn read_all(fetch: &dyn Fetch, url: &str, max: usize) -> Result<Vec<u8>, String> {
    let mut body = fetch.get(url).await.map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    while let Some(chunk) = body.next().await.map_err(|e| e.to_string())? {
        if out.len() + chunk.len() > max {
            return Err("the file is larger than expected".into());
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// Fetch the registry now and keep it. Only called from the owner's explicit action.
async fn fetch_registry(ui: &Ui, actor: &str) -> Result<Registry, String> {
    let c = &ui.settings.parts().catalog;
    let (Some(fetch), Some(cache)) = (&c.inner.fetch, &c.inner.cache) else {
        return Err("The registry can't be loaded on this computer.".into());
    };
    let result = async {
        let bytes = read_all(fetch.as_ref(), registry::REGISTRY_URL, registry::MAX_BYTES).await?;
        let text = String::from_utf8(bytes).map_err(|_| "not text".to_string())?;
        cache
            .store(&text, &c.inner.platform)
            .map_err(|e| e.to_string())
    }
    .await;
    let detail = match &result {
        Ok(r) => format!(
            "fetched {} ({} agents)",
            registry::REGISTRY_URL,
            r.agents.len()
        ),
        Err(e) => format!("fetch of {} failed: {e}", registry::REGISTRY_URL),
    };
    ui.settings
        .audit("settings.provider.registry", actor, &detail, None);
    result.map_err(|e| {
        tracing::warn!("ACP registry: {e}");
        "The registry couldn't be loaded. Check the connection and try again.".to_string()
    })
}

// --- Installing a registry agent ------------------------------------------------------------

/// Drop `.` components (`./bin/agent` → `bin/agent`).
fn tidy(p: &str) -> PathBuf {
    FsPath::new(p)
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect()
}

/// Download `b` to `to`; its SHA-256 must match the registry's.
async fn download(fetch: &dyn Fetch, b: &BinaryInstall, to: &FsPath) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    let mut body = fetch.get(&b.url).await.map_err(|e| e.to_string())?;
    if body
        .len_hint()
        .is_some_and(|n| n > registry::MAX_ARCHIVE_BYTES)
    {
        return Err("the download is too large".into());
    }
    let mut file = tokio::fs::File::create(to)
        .await
        .map_err(|e| e.to_string())?;
    let mut size: u64 = 0;
    while let Some(chunk) = body.next().await.map_err(|e| e.to_string())? {
        size += chunk.len() as u64;
        if size > registry::MAX_ARCHIVE_BYTES {
            return Err("the download is too large".into());
        }
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
    }
    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);
    let path = to.to_path_buf();
    let got = tokio::task::spawn_blocking(move || registry::file_sha256(&path))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    if got != b.sha256 {
        return Err("the download didn't match its published checksum".into());
    }
    Ok(())
}

async fn install(ui: &Ui, agent: &RegistryAgent, b: &BinaryInstall) -> Result<String, String> {
    let s = &ui.settings;
    let c = &s.parts().catalog;
    let fetch = c
        .inner
        .fetch
        .clone()
        .ok_or("Downloads aren't possible on this computer.")?;
    let home = s
        .parts()
        .home
        .clone()
        .ok_or("Melissa doesn't know the agents' home folder.")?;
    if s.parts().store.added().contains_key(&b.provider) {
        return Err(format!(
            "A provider called {} is already added.",
            b.provider
        ));
    }
    let version = if agent.version.is_empty() {
        "latest"
    } else {
        &agent.version
    };
    let dir = registry::install_dir(&home, &agent.id, version)
        .or_else(|| registry::install_dir(&home, &agent.id, "latest"))
        .ok_or("This agent's name can't be used.")?;
    let root = home.join(registry::AGENTS_DIR).join(&agent.id);
    let archive = root.join(format!("{version}.download"));
    let _ = tokio::fs::remove_dir_all(&dir).await;
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|e| format!("Can't make its folder: {e}"))?;
    c.set_job(&agent.id, JobState::Working("Downloading…"));
    let fetched = download(fetch.as_ref(), b, &archive).await;
    if let Err(e) = fetched {
        let _ = tokio::fs::remove_file(&archive).await;
        return Err(format!("The download failed: {e}."));
    }
    c.set_job(&agent.id, JobState::Working("Unpacking…"));
    let (from, spec, into) = (archive.clone(), b.clone(), dir.clone());
    let unpacked = tokio::task::spawn_blocking(move || registry::unpack(&from, &spec, &into))
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r.map_err(|e| e.to_string()));
    let _ = tokio::fs::remove_file(&archive).await;
    let program = match unpacked {
        Ok(_) => dir.join(tidy(&b.cmd)),
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&dir).await;
            return Err(format!("It couldn't be unpacked: {e}."));
        }
    };
    c.set_job(&agent.id, JobState::Working("Checking…"));
    let checked = c
        .probe(ProbeRequest {
            program: program.clone(),
            args: b.args.clone(),
            home: Some(home.clone()),
        })
        .await;
    if let Err(e) = checked.and_then(|info| tools_ok(&info)) {
        let _ = tokio::fs::remove_dir_all(&dir).await;
        return Err(format!("It didn't start as an ACP agent: {e}"));
    }
    let added = AddedProvider {
        label: agent.name.clone(),
        command: program.display().to_string(),
        args: b.args.clone(),
        allow: Vec::new(),
        registry: Some(agent.id.clone()),
        version: Some(version.to_string()),
        installed_in: Some(root.display().to_string()),
    };
    if let Err(e) = s.parts().store.add(&b.provider, added) {
        let _ = tokio::fs::remove_dir_all(&dir).await;
        return Err(e.to_string());
    }
    Ok(b.provider.clone())
}

/// Melissa's tools reach an agent as an MCP server over HTTP; one that can't take that is
/// no use as a provider.
fn tools_ok(info: &ProbeInfo) -> Result<(), String> {
    if info.mcp_http {
        Ok(())
    } else {
        Err("it can't use Melissa's tools (no MCP over HTTP)".into())
    }
}

fn start_install(ui: &Ui, rid: &str, actor: &str) -> Result<(), String> {
    let c = ui.settings.parts().catalog.clone();
    if matches!(c.jobs().get(rid), Some(JobState::Working(_))) {
        return Ok(());
    }
    let (reg, _) = c.cached().ok_or("Load the registry first.")?;
    let agent = reg
        .get(rid)
        .cloned()
        .ok_or("No such agent in the registry.")?;
    let Install::Binary(b) = agent.install.clone() else {
        return Err("Melissa can't install this one; enter it manually.".into());
    };
    c.set_job(rid, JobState::Working("Downloading…"));
    ui.settings.audit(
        "settings.provider.add",
        actor,
        &format!(
            "{}: install from the ACP registry asked ({} {})",
            b.provider, agent.id, agent.version
        ),
        Some(b.sha256.clone()),
    );
    let ui = ui.clone();
    let actor = actor.to_string();
    let rid = rid.to_string();
    tokio::spawn(async move {
        let result = install(&ui, &agent, &b).await;
        let c = &ui.settings.parts().catalog;
        let detail = match &result {
            Ok(id) => format!(
                "{id}: installed {} {} from the ACP registry (checksum matched)",
                agent.id, agent.version
            ),
            Err(e) => format!("{}: install from the ACP registry failed ({e})", b.provider),
        };
        ui.settings.audit(
            "settings.provider.add",
            &actor,
            &detail,
            Some(b.sha256.clone()),
        );
        c.set_job(
            &rid,
            match result {
                Ok(id) => JobState::Done(id),
                Err(e) => JobState::Failed(e),
            },
        );
    });
    Ok(())
}

// --- Entering an agent by hand --------------------------------------------------------------

/// Split a command line into words: spaces separate, '…' and "…" group (no escapes, no
/// variables, no globbing: this is not a shell).
fn split_command(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for ch in line.chars() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(ch);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            (None, c) => {
                cur.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err("A quote isn't closed.".into());
    }
    if started {
        words.push(cur);
    }
    if words.is_empty() {
        return Err("Enter the command that starts the agent.".into());
    }
    if line.chars().any(char::is_control) || words.iter().any(|w| w.len() > 4096) {
        return Err("That command can't be used.".into());
    }
    Ok(words)
}

/// What the manual form found wrong (shown in the form).
async fn add_manual(ui: &Ui, name: &str, line: &str, actor: &str) -> Result<String, String> {
    let s = &ui.settings;
    let words = split_command(line.trim())?;
    let (first, args) = words.split_first().ok_or("Enter a command.")?;
    let program = if first.contains('/') {
        let p = PathBuf::from(first);
        if !p.is_absolute() {
            return Err("Give the program's full path, like /usr/local/bin/my-agent.".into());
        }
        p
    } else {
        version::resolve(first)
            .ok_or_else(|| format!("{first} isn't installed here (not found on PATH)."))?
    };
    if !program.is_file() {
        return Err(format!("{} isn't a program.", program.display()));
    }
    let id = provider_id(&program.display().to_string());
    let file = program
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if !valid_id(&id) || file != id {
        return Err(
            "Melissa names a provider after its program: the file name may use only lowercase letters, digits, - _ and .".into(),
        );
    }
    if BUILT_IN.contains(&id.as_str()) {
        return Err(format!(
            "{} is built in: choose it above.",
            default_label(&id)
        ));
    }
    if s.parts().store.added().contains_key(&id) {
        return Err(format!("{id} is already added."));
    }
    let label = if name.trim().is_empty() {
        id.clone()
    } else {
        name.trim().to_string()
    };
    check_display_name(&label).map_err(|e| e.to_string())?;
    melissa_settings::providers::check_args(args).map_err(|e| e.to_string())?;
    let info = s
        .parts()
        .catalog
        .probe(ProbeRequest {
            program: program.clone(),
            args: args.to_vec(),
            home: s.parts().home.clone(),
        })
        .await
        .map_err(|e| format!("It didn't answer as an ACP agent: {e}"))?;
    tools_ok(&info).map_err(|e| format!("It answered, but {e}."))?;
    let added = AddedProvider {
        label,
        command: program.display().to_string(),
        args: args.to_vec(),
        allow: Vec::new(),
        registry: None,
        version: None,
        installed_in: None,
    };
    s.parts().store.add(&id, added).map_err(|e| e.to_string())?;
    let agent = info.agent.unwrap_or_default();
    s.audit(
        "settings.provider.add",
        actor,
        &format!(
            "{id}: added by hand ({})",
            melissa_settings::clean_text(&agent, 80)
        ),
        Some(sha256_hex(line.as_bytes())),
    );
    Ok(id)
}

// --- Views ----------------------------------------------------------------------------------

fn letter(label: &str) -> String {
    label
        .chars()
        .find(|c| c.is_alphanumeric())
        .map_or_else(|| "?".into(), |c| c.to_uppercase().collect())
}

fn manual_href(name: &str) -> String {
    let mut q = String::from("?step=manual");
    if !name.is_empty() {
        q.push_str("&name=");
        q.push_str(&encode(name));
    }
    format!("{ADD_URL}{q}")
}

/// Percent-encode a query value.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The registry part of step 1: load/refresh, search, the list.
fn registry_region(ui: &Ui, q: &str, error: Option<&str>) -> Markup {
    let c = &ui.settings.parts().catalog;
    let cached = c.cached();
    let jobs = c.jobs().clone();
    let added = ui.settings.parts().store.added();
    let working = c.working();
    let live = if q.is_empty() {
        REGISTRY_URL.to_string()
    } else {
        format!("{REGISTRY_URL}?q={}", encode(q))
    };
    let content = match &cached {
        None => html! {
            @if let Some(e) = error { (notice(e, Tone::Error)) }
            p.quiet { "Melissa asks the ACP registry only when you load it." }
            form method="post" action=(REGISTRY_URL) hx-post=(REGISTRY_URL)
                hx-target={ "#" (REGISTRY_ID) } hx-swap="outerHTML" {
                button.secondary type="submit" { "Load the list" }
            }
        },
        Some((reg, at)) => {
            let found = reg.search(q);
            let hrefs: Vec<(String, String, String)> = found
                .iter()
                .map(|a| {
                    (
                        format!("{REGISTRY_URL}/{}/add", a.id),
                        manual_href(&a.name),
                        match jobs.get(&a.id) {
                            Some(JobState::Done(id)) => format!("{ADD_URL}?step=identity&id={id}"),
                            _ => String::new(),
                        },
                    )
                })
                .collect();
            let letters: Vec<String> = found.iter().map(|a| letter(&a.name)).collect();
            let rows: Vec<RegistryRow<'_>> = found
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    let (add, manual, cont) = &hrefs[i];
                    let installed_here =
                        matches!(&a.install, Install::Binary(b) if added.contains_key(&b.provider));
                    let action = match (jobs.get(&a.id), &a.install) {
                        (Some(JobState::Working(w)), _) => RegistryAction::Working { what: w },
                        (Some(JobState::Done(_)), _) => RegistryAction::Added { href: cont },
                        (Some(JobState::Failed(e)), _) => RegistryAction::Failed {
                            why: e,
                            action: add,
                        },
                        (None, Install::Binary(_)) if installed_here => RegistryAction::Manual {
                            why: "Added",
                            href: manual,
                        },
                        (None, Install::Binary(_)) => RegistryAction::Add { action: add },
                        (None, Install::Manual(why)) => {
                            RegistryAction::Manual { why, href: manual }
                        }
                    };
                    RegistryRow {
                        id: &a.id,
                        name: &a.name,
                        letter: &letters[i],
                        version: &a.version,
                        description: &a.description,
                        link: a.link.as_deref(),
                        action,
                    }
                })
                .collect();
            html! {
                (search_box(ADD_URL, REGISTRY_URL, REGISTRY_ID, "Search the registry", q))
                div.reg-head {
                    span.quiet.small { "Updated " (ago(*at, now_secs())) }
                    form.inline method="post" action=(REGISTRY_URL) hx-post=(REGISTRY_URL)
                        hx-target={ "#" (REGISTRY_ID) } hx-swap="outerHTML" {
                        button.link type="submit" { "Refresh" }
                    }
                }
                @if let Some(e) = error { (notice(e, Tone::Error)) }
                (registry_list(&rows, REGISTRY_ID, "No agents match."))
            }
        }
    };
    live_region(REGISTRY_ID, &live, 2, working, content)
}

fn tiles_view(ui: &Ui) -> Markup {
    let store = &ui.settings.parts().store;
    let known: Vec<(&str, &str, String, String)> = KNOWN
        .iter()
        .filter(|(id, _)| {
            ui.settings
                .parts()
                .backends
                .iter()
                .any(|b| b.provider == *id)
        })
        .map(|(id, vendor)| {
            (
                *id,
                *vendor,
                letter(default_label(id)),
                format!("/ui/settings/providers/add/{id}"),
            )
        })
        .collect();
    let tiles: Vec<ProviderTile<'_>> = known
        .iter()
        .map(|(id, vendor, l, action)| ProviderTile {
            id,
            label: default_label(id),
            letter: l,
            vendor,
            action: store.is_removed(id).then_some(action.as_str()),
        })
        .collect();
    provider_tiles("Providers Melissa runs", &tiles, PANEL_ID)
}

fn panel(step: usize, content: Markup) -> Markup {
    add_panel(
        PANEL_ID,
        "Add provider",
        "/settings/providers",
        stepper(&STEPS, step),
        content,
    )
}

fn choose_view(ui: &Ui, q: &str, flash: Option<&Flash>) -> Markup {
    panel(
        0,
        html! {
            @if let Some(f) = flash { (notice(&f.text, f.tone)) }
            (tiles_view(ui))
            (group("Or choose from the ACP registry", None, registry_region(ui, q, None)))
            p.small { (link(&manual_href(""), "Or enter a local ACP command")) }
        },
    )
}

fn manual_view(name: &str, command: &str, error: Option<&str>) -> Markup {
    panel(
        0,
        html! {
            @if let Some(e) = error { (notice(e, Tone::Error)) }
            (settings_form("/ui/settings/providers/manual", PANEL_ID, "Check and add", html! {
                (text_field("name", "Name", name, "My agent", "text", None))
                (text_field("command", "Command", command, "/usr/local/bin/my-agent acp", "text",
                    Some("The command that starts it as an ACP agent. Melissa checks it answers.")))
            }))
            p.small { (link(ADD_URL, "Back")) }
        },
    )
}

/// The variable a known agent reads its API key from (a guess the owner can change).
fn key_guess(id: &str) -> String {
    let base = match id {
        "kimi" => "MOONSHOT",
        "vibe-acp" => "MISTRAL",
        "goose" => "OPENAI",
        other => other,
    };
    let mut v: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    v.push_str("_API_KEY");
    v
}

fn identity_view(id: &str, a: &AddedProvider, key_set: bool, error: Option<&str>) -> Markup {
    let action = format!("/ui/settings/providers/new/{id}/key");
    let next = format!("{ADD_URL}?step=config&id={id}");
    panel(
        1,
        html! {
            h2 { (a.label) }
            @if let Some(e) = error { (notice(e, Tone::Error)) }
            @if key_set { (notice("API key saved.", Tone::Ok)) }
            p.quiet { "Most agents sign in with an API key from their maker." }
            (settings_form(&action, PANEL_ID, "Save key", html! {
                (text_field("var", "Variable", &key_guess(id), "", "text",
                    Some("Where the agent looks for its key.")))
                (text_field("value", "API key", "", "", "password", None))
            }))
            (actions(link_button(&next, if key_set { "Next" } else { "Skip" }, key_set)))
        },
    )
}

fn config_view(id: &str, a: &AddedProvider, error: Option<&str>) -> Markup {
    let action = format!("/ui/settings/providers/new/{id}/finish");
    panel(
        2,
        html! {
            h2 { (a.label) }
            @if let Some(e) = error { (notice(e, Tone::Error)) }
            (settings_form(&action, PANEL_ID, "Finish", html! {
                (lines_field("hosts", "Hosts it may reach", &a.allow.join("\n"),
                    Some("Its AI service, one per line (api.example.com). Melissa blocks every other host.")))
            }))
            (disclosure("Details", false, html! {
                p.small { "Program: " (a.command) }
                @if !a.args.is_empty() { p.small { "Arguments: " (a.args.join(" ")) } }
                p.small.quiet { "Pick its model in the chat once it is ready." }
            }))
        },
    )
}

fn page_or_panel(ui: &Ui, headers: &HeaderMap, body: Markup) -> BoxFuture<'static, Response> {
    let htmx = is_htmx(headers);
    let ui = ui.clone();
    Box::pin(async move {
        if htmx {
            return body.into_response();
        }
        ui.framed(|f| super::pages::frame_at(f, "Providers", Some("Add provider"), body))
            .await
            .into_response()
    })
}

// --- Handlers -------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub(super) struct AddQuery {
    #[serde(default)]
    step: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

fn added(ui: &Ui, id: &str) -> Option<AddedProvider> {
    ui.settings.parts().store.added().get(id).cloned()
}

/// `GET /settings/providers/add`: the panel (never fetches anything).
pub(super) async fn page(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Query(q): Query<AddQuery>,
) -> Response {
    let id = q.id.as_deref().unwrap_or_default();
    let body = match (q.step.as_deref(), added(&ui, id)) {
        (Some("manual"), _) => manual_view(q.name.as_deref().unwrap_or_default(), "", None),
        (Some("identity"), Some(a)) => {
            let set = ui
                .settings
                .parts()
                .store
                .get(id)
                .env
                .keys()
                .any(|k| is_secret_name(k));
            identity_view(id, &a, set, None)
        }
        (Some("config"), Some(a)) => config_view(id, &a, None),
        _ => choose_view(&ui, q.q.as_deref().unwrap_or_default().trim(), None),
    };
    page_or_panel(&ui, &headers, body).await
}

/// `GET /ui/settings/providers/registry?q=`: the registry list, filtered (no fetch).
pub(super) async fn registry_list_region(
    State(ui): State<Ui>,
    Query(q): Query<AddQuery>,
) -> Markup {
    registry_region(&ui, q.q.as_deref().unwrap_or_default().trim(), None)
}

/// `POST /ui/settings/providers/registry`: fetch the registry now (the owner asked).
pub(super) async fn registry_refresh(State(ui): State<Ui>, headers: HeaderMap) -> Response {
    let result = fetch_registry(&ui, "user").await;
    if is_htmx(&headers) {
        return registry_region(&ui, "", result.as_ref().err().map(String::as_str)).into_response();
    }
    match result {
        Ok(_) => Redirect::to(ADD_URL).into_response(),
        Err(e) => {
            let body = panel(
                0,
                html! {
                    (tiles_view(&ui))
                    (group("Or choose from the ACP registry", None, registry_region(&ui, "", Some(&e))))
                },
            );
            (
                StatusCode::BAD_GATEWAY,
                page_or_panel(&ui, &headers, body).await,
            )
                .into_response()
        }
    }
}

/// `POST /ui/settings/providers/registry/{rid}/add`: install a registry agent.
pub(super) async fn registry_add(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Path(rid): Path<String>,
) -> Response {
    let result = start_install(&ui, &rid, "user");
    if is_htmx(&headers) {
        return registry_region(&ui, "", result.as_ref().err().map(String::as_str)).into_response();
    }
    Redirect::to(ADD_URL).into_response()
}

/// `POST /ui/settings/providers/add/{id}`: add a removed built-in provider again; its
/// install and sign-in continue on the Providers page.
pub(super) async fn add_known(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let s = &ui.settings;
    if !KNOWN.iter().any(|(k, _)| *k == id) || !s.parts().backends.iter().any(|b| b.provider == id)
    {
        return (StatusCode::NOT_FOUND, "No such provider.").into_response();
    }
    if s.parts().store.is_removed(&id) {
        if let Err(e) = s.parts().store.change(&id, &Change::Restore) {
            let flash = Flash {
                text: e.to_string(),
                tone: Tone::Error,
            };
            let body = choose_view(&ui, "", Some(&flash));
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                page_or_panel(&ui, &headers, body).await,
            )
                .into_response();
        }
        s.audit(
            "settings.provider.add",
            "user",
            &format!("{id}: added again"),
            None,
        );
        apply::schedule(&ui);
    }
    let to = format!("/settings/providers?p={id}");
    if is_htmx(&headers) {
        // Leave the panel: the Providers page continues with Install and Sign in.
        return ([("HX-Redirect", to)], "").into_response();
    }
    Redirect::to(&to).into_response()
}

#[derive(Deserialize)]
pub(super) struct ManualForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    command: String,
}

/// `POST /ui/settings/providers/manual`: check a local ACP command and save it.
pub(super) async fn manual(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Form(f): Form<ManualForm>,
) -> Response {
    match add_manual(&ui, &f.name, &f.command, "user").await {
        Ok(id) => {
            let a = added(&ui, &id);
            match (is_htmx(&headers), a) {
                (true, Some(a)) => identity_view(&id, &a, false, None).into_response(),
                _ => Redirect::to(&format!("{ADD_URL}?step=identity&id={id}")).into_response(),
            }
        }
        Err(e) => {
            let body = manual_view(&f.name, &f.command, Some(&e));
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                page_or_panel(&ui, &headers, body).await,
            )
                .into_response()
        }
    }
}

#[derive(Deserialize)]
pub(super) struct KeyForm {
    #[serde(default)]
    var: String,
    #[serde(default)]
    value: String,
}

fn save_key(ui: &Ui, id: &str, var: &str, value: &str, actor: &str) -> Result<(), String> {
    let var = var.trim();
    if value.trim().is_empty() {
        return Err("Paste the API key.".into());
    }
    if !is_secret_name(var) {
        return Err(
            "Use a variable name with KEY or TOKEN in it, so Melissa keeps it secret.".into(),
        );
    }
    check_var(var, value.trim()).map_err(|e| e.to_string())?;
    let change = Change::SetVar {
        name: var.to_string(),
        value: value.trim().to_string(),
    };
    ui.settings
        .parts()
        .store
        .change(id, &change)
        .map_err(|e| e.to_string())?;
    let (what, hash) = change.describe();
    ui.settings
        .audit("settings.provider", actor, &format!("{id}: {what}"), hash);
    Ok(())
}

/// `POST /ui/settings/providers/new/{id}/key`: step 2, the API key.
pub(super) async fn new_key(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(f): Form<KeyForm>,
) -> Response {
    let Some(a) = added(&ui, &id) else {
        return (StatusCode::NOT_FOUND, "No such provider.").into_response();
    };
    let body = match save_key(&ui, &id, &f.var, &f.value, "user") {
        Ok(()) if !is_htmx(&headers) => {
            return Redirect::to(&format!("{ADD_URL}?step=identity&id={id}")).into_response()
        }
        Ok(()) => identity_view(&id, &a, true, None),
        Err(e) => {
            let body = identity_view(&id, &a, false, Some(&e));
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                page_or_panel(&ui, &headers, body).await,
            )
                .into_response();
        }
    };
    body.into_response()
}

#[derive(Deserialize)]
pub(super) struct HostsForm {
    #[serde(default)]
    hosts: String,
}

fn lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// `POST /ui/settings/providers/new/{id}/finish`: step 3, the hosts; then apply.
pub(super) async fn finish(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(f): Form<HostsForm>,
) -> Response {
    let Some(a) = added(&ui, &id) else {
        return (StatusCode::NOT_FOUND, "No such provider.").into_response();
    };
    let hosts = lines(&f.hosts);
    let saved = check_hosts(&hosts)
        .map_err(|e| e.to_string())
        .and_then(|()| {
            ui.settings
                .parts()
                .store
                .set_hosts(&id, hosts.clone())
                .map_err(|e| e.to_string())
        });
    if let Err(e) = saved {
        let body = config_view(&id, &AddedProvider { allow: hosts, ..a }, Some(&e));
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            page_or_panel(&ui, &headers, body).await,
        )
            .into_response();
    }
    ui.settings.audit(
        "settings.provider",
        "user",
        &format!("{id}: hosts set ({})", hosts.len()),
        Some(sha256_hex(hosts.join("\n").as_bytes())),
    );
    apply::schedule(&ui);
    let to = format!("/settings/providers?p={id}");
    if is_htmx(&headers) {
        return ([("HX-Redirect", to)], "").into_response();
    }
    Redirect::to(&to).into_response()
}

// --- JSON API -------------------------------------------------------------------------------

fn agent_json(a: &RegistryAgent, job: Option<&JobState>) -> Value {
    let install = match &a.install {
        Install::Binary(b) => json!({"kind": "binary", "provider": b.provider}),
        Install::Manual(why) => json!({"kind": "manual", "why": why}),
    };
    let job = job.map(|j| match j {
        JobState::Working(w) => json!({"state": "working", "step": w}),
        JobState::Done(id) => json!({"state": "done", "provider": id}),
        JobState::Failed(e) => json!({"state": "failed", "error": e}),
    });
    json!({
        "id": a.id, "name": a.name, "version": a.version, "description": a.description,
        "link": a.link, "license": a.license, "install": install, "job": job,
    })
}

fn registry_json(ui: &Ui) -> Value {
    let c = &ui.settings.parts().catalog;
    let jobs = c.jobs().clone();
    match c.cached() {
        None => json!({"url": registry::REGISTRY_URL, "fetched_at": null, "agents": []}),
        Some((r, at)) => json!({
            "url": registry::REGISTRY_URL,
            "fetched_at": at,
            "agents": r.agents.iter().map(|a| agent_json(a, jobs.get(&a.id))).collect::<Vec<_>>(),
        }),
    }
}

/// `GET /api/settings/providers/registry`: the cached registry (no fetch).
pub(super) async fn api_registry(State(ui): State<Ui>) -> Json<Value> {
    Json(registry_json(&ui))
}

/// `POST /api/settings/providers/registry/refresh`: fetch it now.
pub(super) async fn api_registry_refresh(State(ui): State<Ui>) -> Response {
    match fetch_registry(&ui, "api").await {
        Ok(_) => Json(registry_json(&ui)).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({"error": e}))).into_response(),
    }
}

/// `POST /api/settings/providers/add`: `{"provider": "grok"}` (a removed built-in),
/// `{"registry": "<id>"}` (install; 202, then poll the registry) or `{"name", "command"}`.
#[derive(Deserialize)]
pub(super) struct AddBody {
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    registry: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    command: Option<String>,
}

pub(super) async fn api_add(State(ui): State<Ui>, Json(b): Json<AddBody>) -> Response {
    let err = |status: StatusCode, e: String| (status, Json(json!({"error": e}))).into_response();
    if let Some(id) = b.provider {
        let s = &ui.settings;
        if !KNOWN.iter().any(|(k, _)| *k == id)
            || !s.parts().backends.iter().any(|f| f.provider == id)
        {
            return err(StatusCode::NOT_FOUND, "No such provider.".into());
        }
        if s.parts().store.is_removed(&id) {
            if let Err(e) = s.parts().store.change(&id, &Change::Restore) {
                return err(StatusCode::UNPROCESSABLE_ENTITY, e.to_string());
            }
            s.audit(
                "settings.provider.add",
                "api",
                &format!("{id}: added again"),
                None,
            );
            apply::schedule(&ui);
        }
        return Json(json!({"provider": id, "added": true})).into_response();
    }
    if let Some(rid) = b.registry {
        return match start_install(&ui, &rid, "api") {
            Ok(()) => (
                StatusCode::ACCEPTED,
                Json(json!({"registry": rid, "started": true})),
            )
                .into_response(),
            Err(e) => err(StatusCode::UNPROCESSABLE_ENTITY, e),
        };
    }
    match b.command {
        Some(cmd) => {
            match add_manual(&ui, b.name.as_deref().unwrap_or_default(), &cmd, "api").await {
                Ok(id) => Json(json!({"provider": id, "added": true})).into_response(),
                Err(e) => err(StatusCode::UNPROCESSABLE_ENTITY, e),
            }
        }
        None => err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Give `provider`, `registry` or `command`.".into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_provider_has_a_tile_with_its_maker() {
        let tiles: Vec<&str> = KNOWN.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            tiles,
            ["claude", "grok", "opencode", "gemini", "codex-acp", "muse"]
        );
        // Every built-in except Melissa's test partner, each with a name of its own.
        for id in BUILT_IN.iter().filter(|id| **id != "echo") {
            assert!(tiles.contains(id), "{id}");
            assert_ne!(default_label(id), *id, "{id}");
        }
        let makers: Vec<&str> = KNOWN.iter().map(|(_, m)| *m).collect();
        assert_eq!(
            makers,
            ["Anthropic", "xAI", "OpenCode", "Google", "OpenAI", "Meta"]
        );
        // Letter marks (their brands' rules), from the shown names.
        assert_eq!(letter(default_label("gemini")), "G");
        assert_eq!(letter(default_label("codex-acp")), "C");
        assert_eq!(letter(default_label("muse")), "M");
    }

    #[test]
    fn command_lines_split_like_a_simple_shell() {
        assert_eq!(
            split_command("/opt/a/agent acp --flag 'two words' \"x y\"").unwrap(),
            ["/opt/a/agent", "acp", "--flag", "two words", "x y"]
        );
        assert_eq!(split_command("  agent  ").unwrap(), ["agent"]);
        assert_eq!(split_command("a ''").unwrap(), ["a", ""]);
        assert!(split_command("a 'open").is_err());
        assert!(split_command("   ").is_err());
        // No shell: these are plain words.
        assert_eq!(split_command("a $(x) ;").unwrap(), ["a", "$(x)", ";"]);
    }

    #[test]
    fn key_names_and_encoding() {
        assert_eq!(key_guess("kimi"), "MOONSHOT_API_KEY");
        assert_eq!(key_guess("my-agent"), "MY_AGENT_API_KEY");
        assert_eq!(encode("Fake Agent/1"), "Fake%20Agent%2F1");
        assert_eq!(tidy("./bin/agent"), PathBuf::from("bin/agent"));
    }
}
