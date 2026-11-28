//! Provider settings: the owner's overrides per agent CLI ("provider"), written by the web UI
//! and `melissa-server provider set`, in `$XDG_CONFIG_HOME/melissa/providers.toml` (mode 0600).
//!
//! A provider is an agent program, named by its file name: `claude`, `grok`, `opencode`,
//! `gemini`, `codex-acp` (Codex's ACP adapter), `muse`, `echo` (Melissa's built-in test
//! partner), or the file name of any other `[[backend]]`
//! command. One provider can back several backends (`grok` runs `grok-acp` and the one-shot
//! `grok`); its overrides apply to all of them.
//!
//! Precedence, lowest first: built-in defaults < `server.toml` < `providers.toml`. The lead agent
//! merges them once at startup (`ServerConfig::apply_providers`); a change made while it runs
//! is saved at once and used after the next restart (the UI says so).
//!
//! ```toml
//! [provider.claude]
//! enabled = true
//! display_name = "Claude"
//! command = "/var/lib/melissa/home/.local/bin/claude"   # file name must be the provider's
//! args = ["--verbose"]                                  # put before Melissa's own arguments
//! [provider.claude.env]
//! ANTHROPIC_BASE_URL = "https://llm.example.com"
//! ANTHROPIC_API_KEY = "..."                             # a secret: write-only in the UI
//! [provider.grok]
//! removed = true                                        # the owner removed it (Add brings it back)
//! [added.kimi]                                          # an ACP agent the owner added
//! label = "Kimi"
//! command = "/var/lib/melissa/home/.local/share/melissa-agents/kimi/1.2.0/kimi"
//! args = ["acp"]
//! allow = ["api.example.com"]                           # hosts it may reach (egress proxy)
//! registry = "kimi"                                     # where it came from (None: entered by hand)
//! ```
//!
//! Secrets: variables whose name looks like one ([`is_secret_name`]) are never shown, sent to
//! a client, logged or audited; views say only that they are set. `Debug` redacts every
//! variable value.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};

use crate::{sha256_hex, SettingsError};

/// The settings file's name in Melissa's config directory.
pub const FILE_NAME: &str = "providers.toml";

/// Most variables per provider.
pub const MAX_VARS: usize = 32;
/// Most extra launch arguments per provider.
pub const MAX_ARGS: usize = 16;
const MAX_ARG_LEN: usize = 256;
const MAX_VALUE_LEN: usize = 4096;
const MAX_NAME_LEN: usize = 40;

/// Launch arguments the owner may not add: they would undo how Melissa runs agents (its
/// permission prompts, tool list, MCP config, sandbox paths, one-shot or session mode).
/// Melissa's own arguments come after the extra ones, so for flags where the last one wins
/// Melissa's still apply; these are the ones where that is not enough.
const DENIED_ARGS: &[&str] = &[
    "--dangerously-skip-permissions",
    "--allow-dangerously-skip-permissions",
    "--permission-mode",
    "--permission-prompt-tool",
    "--allowedtools",
    "--allowed-tools",
    "--disallowedtools",
    "--disallowed-tools",
    "--tools",
    "--mcp-config",
    "--strict-mcp-config",
    "--add-dir",
    "--settings",
    "--setting-sources",
    "--always-approve",
    "--yolo",
    "--input-format",
    "--output-format",
    "--resume",
    "--continue",
    "--session-id",
    "-p",
    "--print",
    // Gemini CLI, Codex and Muse Code: approval, sandbox and config overrides (`-c key=value`
    // could turn approval or Muse's telemetry setting back on; `-y` is Gemini's YOLO).
    "--approval-mode",
    "-y",
    "-c",
    "--config",
    "--sandbox",
    "--disable-sandbox",
    "--disable-approval",
    "--approval-judge",
    "--permission-profile",
    "--dangerously-bypass-approvals-and-sandbox",
];

/// The whole settings file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderFile {
    #[serde(
        default,
        rename = "provider",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub providers: BTreeMap<String, ProviderOverride>,
    /// ACP agents the owner added (Settings → Providers → Add provider): each becomes a
    /// session backend `<id>-acp` at startup. Their overrides (API key) stay in `providers`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub added: BTreeMap<String, AddedProvider>,
}

/// The providers built into Melissa's default config; an added provider can't take their names.
pub const BUILT_IN: [&str; 7] = [
    "claude",
    "grok",
    "opencode",
    "gemini",
    "codex-acp",
    "muse",
    "echo",
];
/// Most hosts an added provider may reach.
pub const MAX_HOSTS: usize = 32;

/// An ACP agent the owner added: entered by hand (a command already on the box) or installed
/// by Melissa from the ACP registry into the agents' home.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddedProvider {
    /// The name the UI shows.
    pub label: String,
    /// The program (absolute; its file name is the provider id).
    pub command: String,
    /// The arguments that start its ACP server on stdio (e.g. `acp`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Hosts it may reach through the egress proxy (`host`, `*.domain`, `host:port`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    /// The ACP registry entry it came from (None: entered by hand).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
    /// The registry version Melissa installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The directory Melissa installed it into (deleted again when it is removed). None:
    /// Melissa didn't install it, so removing it leaves the program alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_in: Option<String>,
}

impl AddedProvider {
    /// Check an entry stored under `id`.
    pub fn validate(&self, id: &str) -> Result<(), SettingsError> {
        if !valid_id(id) {
            return Err(invalid(format!(
                "`{}` can't be a provider name: use lowercase letters, digits, - _ or .",
                crate::clean_text(id, 40)
            )));
        }
        if BUILT_IN.contains(&id) {
            return Err(invalid(format!(
                "`{id}` is built in; add it from the list above"
            )));
        }
        check_display_name(&self.label)?;
        check_command(id, &self.command)?;
        check_args(&self.args)?;
        check_hosts(&self.allow)?;
        if let Some(r) = &self.registry {
            if !valid_id(r) {
                return Err(invalid("the registry id is not valid"));
            }
        }
        if let Some(v) = &self.version {
            if v.is_empty()
                || v.len() > 64
                || !v
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
            {
                return Err(invalid("the version is not valid"));
            }
        }
        if let Some(dir) = &self.installed_in {
            let p = Path::new(dir);
            if !p.is_absolute()
                || has_control(dir)
                || p.components().any(|c| c == std::path::Component::ParentDir)
            {
                return Err(invalid("the install directory must be an absolute path"));
            }
        }
        Ok(())
    }
}

/// Hosts an added provider may reach: each a valid egress rule, no catch-all.
pub fn check_hosts(hosts: &[String]) -> Result<(), SettingsError> {
    if hosts.len() > MAX_HOSTS {
        return Err(invalid(format!("at most {MAX_HOSTS} hosts")));
    }
    for h in hosts {
        let bare = h.trim_start_matches("*.");
        if h.len() > 253
            || !bare.contains('.')
            || melissa_agent::egress::HostRule::parse(h).is_err()
        {
            return Err(invalid(format!(
                "`{}` is not a host name like api.example.com or *.example.com",
                crate::clean_text(h, 60)
            )));
        }
    }
    Ok(())
}

/// One provider's overrides. Absent fields leave `server.toml` (or the default) as it is.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderOverride {
    /// False: its backends are left out at startup (the lead agent and agents can't use it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// The name the UI shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// The program to run instead of the configured command (absolute path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Extra launch arguments, put before Melissa's own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Variables set for the agent (inside its sandbox).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// The owner removed this built-in provider (Settings → Providers → Remove): it is left
    /// out like a disabled one and hidden until it is added again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removed: bool,
}

impl std::fmt::Debug for ProviderOverride {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let env: Vec<&str> = self.env.keys().map(String::as_str).collect();
        f.debug_struct("ProviderOverride")
            .field("enabled", &self.enabled)
            .field("display_name", &self.display_name)
            .field("command", &self.command)
            .field("args", &self.args.len())
            .field("env", &env)
            .field("removed", &self.removed)
            .finish()
    }
}

impl ProviderOverride {
    /// Its backends run (not turned off, not removed).
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true) && !self.removed
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The variables as (name, value) pairs for the sandbox.
    pub fn vars(&self) -> Vec<(String, String)> {
        self.env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// The variables as clients may see them: secret values left out.
    pub fn env_view(&self) -> Vec<EnvVarView> {
        self.env
            .iter()
            .map(|(name, value)| {
                let secret = is_secret_name(name);
                EnvVarView {
                    name: name.clone(),
                    value: (!secret).then(|| value.clone()),
                    secret,
                }
            })
            .collect()
    }

    pub fn validate(&self, id: &str) -> Result<(), SettingsError> {
        if !valid_id(id) {
            return Err(invalid(format!("`{id}` is not a provider name")));
        }
        if let Some(n) = &self.display_name {
            check_display_name(n)?;
        }
        if let Some(c) = &self.command {
            check_command(id, c)?;
        }
        check_args(&self.args)?;
        if self.env.len() > MAX_VARS {
            return Err(invalid(format!(
                "at most {MAX_VARS} variables per provider"
            )));
        }
        for (name, value) in &self.env {
            check_var(name, value)?;
        }
        Ok(())
    }
}

/// One configured backend as the Settings page lists it (from the merged config).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackendFacts {
    /// The backend's name (`claude-session`, `grok-acp`, ...).
    pub name: String,
    /// Its provider ([`provider_id`] of the command, or `echo`).
    pub provider: String,
    /// A session backend (the lead agent and agents); else one-shot (agent tasks).
    pub session: bool,
    /// The program it runs (None: built into Melissa).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Running in this daemon (false: its provider is disabled).
    pub enabled: bool,
}

/// A variable as the UI and the API show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvVarView {
    pub name: String,
    /// None for a secret: it is set, but never shown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub secret: bool,
}

/// Whether a variable's value is a secret by its name (API keys, tokens, passwords).
pub fn is_secret_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    [
        "KEY",
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "AUTH",
    ]
    .iter()
    .any(|w| upper.contains(w))
}

/// The provider of a backend command: its file name, lower-cased (`/x/bin/claude` → `claude`).
pub fn provider_id(command: &str) -> String {
    Path::new(command)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(command)
        .to_ascii_lowercase()
}

/// A provider name: 1-32 of `a-z 0-9 - _ .`, starting with a letter or digit.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_.".contains(&b))
}

/// The UI name of a known provider.
pub fn default_label(id: &str) -> &str {
    match id {
        "claude" => "Claude Code",
        "grok" => "Grok",
        "opencode" => "OpenCode",
        "gemini" => "Gemini CLI",
        "codex-acp" => "Codex",
        "muse" => "Muse Code",
        "echo" => "Echo",
        other => other,
    }
}

fn invalid(msg: impl Into<String>) -> SettingsError {
    SettingsError::Invalid(msg.into())
}

fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

/// A display name: 1-40 characters on one line.
pub fn check_display_name(n: &str) -> Result<(), SettingsError> {
    if n.trim().is_empty() || n.chars().count() > MAX_NAME_LEN || has_control(n) {
        return Err(invalid(format!(
            "a display name is 1-{MAX_NAME_LEN} characters on one line"
        )));
    }
    Ok(())
}

fn check_command(id: &str, c: &str) -> Result<(), SettingsError> {
    let path = Path::new(c);
    if !path.is_absolute() || c.len() > 4096 || has_control(c) {
        return Err(invalid(
            "the program path must be an absolute path, like /usr/bin/claude",
        ));
    }
    if path
        .components()
        .any(|p| p == std::path::Component::ParentDir)
    {
        return Err(invalid("the program path may not contain `..`"));
    }
    // The file name picks the sandbox preset, the egress allowlist and the login check, so
    // it must stay the provider's.
    if provider_id(c) != id || path.file_name().and_then(|n| n.to_str()) != Some(id) {
        return Err(invalid(format!(
            "the program must be called `{id}` (…/{id})"
        )));
    }
    Ok(())
}

/// Extra launch arguments: a few short, one-line arguments that don't undo Melissa's own.
pub fn check_args(args: &[String]) -> Result<(), SettingsError> {
    if args.len() > MAX_ARGS {
        return Err(invalid(format!("at most {MAX_ARGS} extra arguments")));
    }
    for a in args {
        if a.is_empty() || a.len() > MAX_ARG_LEN || has_control(a) {
            return Err(invalid(format!(
                "each argument is 1-{MAX_ARG_LEN} characters on one line"
            )));
        }
        if a.contains("{prompt}") || a.contains("{mcp_config}") {
            return Err(invalid("arguments may not contain Melissa's placeholders"));
        }
        let flag = a.split('=').next().unwrap_or(a).to_ascii_lowercase();
        if DENIED_ARGS.contains(&flag.as_str()) {
            return Err(invalid(format!(
                "`{flag}` would change how Melissa runs the agent (permissions, tools or mode), so it can't be added here"
            )));
        }
    }
    Ok(())
}

/// A variable name and value the sandbox accepts and that cannot redirect Melissa's own setup.
pub fn check_var(name: &str, value: &str) -> Result<(), SettingsError> {
    let reserved = name.starts_with("LD_")
        || name.starts_with("MELISSA_")
        || name.starts_with("BWRAP")
        || name.eq_ignore_ascii_case("ALL_PROXY")
        || name.eq_ignore_ascii_case("NO_PROXY")
        || name.eq_ignore_ascii_case("HTTP_PROXY")
        || name.eq_ignore_ascii_case("HTTPS_PROXY");
    if name.len() > 64
        || reserved
        || melissa_agent::sandbox::check_env_name(name).is_err()
        || name.bytes().any(|b| b.is_ascii_lowercase())
    {
        return Err(invalid(format!(
            "`{}` can't be used: names are UPPER_CASE letters, digits and _, and not one Melissa sets itself (PATH, HOME, proxies, LD_*)",
            crate::clean_text(name, 64)
        )));
    }
    if value.len() > MAX_VALUE_LEN || value.contains('\0') || value.contains('\n') {
        return Err(invalid(format!(
            "the value of {name} must be one line of at most {MAX_VALUE_LEN} bytes"
        )));
    }
    Ok(())
}

impl ProviderFile {
    /// Read `path`; a missing file means no overrides. Every entry is checked.
    pub fn load(path: &Path) -> Result<Self, SettingsError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(SettingsError::Read {
                    path: path.display().to_string(),
                    source,
                })
            }
        };
        // A newer Melissa's file (after a rollback): `SettingsError::Newer`, and the caller
        // must leave it where it is (never move it aside as broken).
        let mut file: Self =
            crate::parse_versioned(path, &crate::formats::PROVIDERS, &text, |text| {
                toml::from_str(text).map_err(|e| {
                    // toml's and serde's messages quote the offending value, which may be a
                    // secret: say only where.
                    let line = e
                        .span()
                        .map(|s| text[..s.start.min(text.len())].lines().count().max(1));
                    SettingsError::Parse {
                        path: path.display().to_string(),
                        message: match line {
                            Some(l) => format!("line {l}: wrong name or type of value"),
                            None => "wrong name or type of value".into(),
                        },
                    }
                })
            })?;
        for (id, o) in &file.providers {
            o.validate(id).map_err(|e| SettingsError::Parse {
                path: path.display().to_string(),
                message: format!("[provider.{id}]: {e}"),
            })?;
        }
        // An agent added by hand or from the registry before Melissa built it in (e.g. Muse
        // Code, Gemini CLI on 2026-11-20): the built-in takes over; the rest of the file stays.
        file.added.retain(|id, a| {
            let taken = BUILT_IN.contains(&id.as_str());
            if taken {
                tracing::warn!(
                    provider = %id,
                    command = %a.command,
                    "[added.{id}] is built in now; using the built-in provider instead"
                );
            }
            !taken
        });
        for (id, a) in &file.added {
            a.validate(id).map_err(|e| SettingsError::Parse {
                path: path.display().to_string(),
                message: format!("[added.{id}]: {e}"),
            })?;
        }
        Ok(file)
    }

    /// Save to `path`; refused for a newer Melissa's file ([`SettingsError::Newer`]).
    pub fn save(&self, path: &Path) -> Result<(), SettingsError> {
        crate::save_versioned(
            path,
            &crate::formats::PROVIDERS,
            "# Melissa provider settings, written by the web UI and `melissa-server provider`.\n\
             # Merged over server.toml when Melissa starts. Holds secrets: keep it mode 0600.\n\n",
            self,
        )
    }

    pub fn get(&self, id: &str) -> ProviderOverride {
        self.providers.get(id).cloned().unwrap_or_default()
    }
}

/// One change to a provider's settings.
#[derive(Clone, PartialEq, Eq)]
pub enum Change {
    Enable(bool),
    /// None: back to the built-in name.
    DisplayName(Option<String>),
    /// None: back to the configured command.
    Command(Option<String>),
    Args(Vec<String>),
    SetVar {
        name: String,
        value: String,
    },
    RemoveVar(String),
    /// Remove a built-in provider: its settings (API key included) go, it is hidden.
    Remove,
    /// Add a removed built-in provider again.
    Restore,
}

impl std::fmt::Debug for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe().0)
    }
}

impl Change {
    /// For the audit log: what changed, without values, and a hash of the new value.
    pub fn describe(&self) -> (String, Option<String>) {
        match self {
            Self::Enable(true) => ("enabled".into(), None),
            Self::Enable(false) => ("disabled".into(), None),
            Self::DisplayName(n) => (
                "display name changed".into(),
                n.as_ref().map(|n| sha256_hex(n.as_bytes())),
            ),
            Self::Command(c) => (
                match c {
                    Some(_) => "program path set".into(),
                    None => "program path reset".into(),
                },
                c.as_ref().map(|c| sha256_hex(c.as_bytes())),
            ),
            Self::Args(a) => (
                format!("extra arguments set ({})", a.len()),
                Some(sha256_hex(a.join("\0").as_bytes())),
            ),
            Self::SetVar { name, value } => (
                format!("variable {name} set"),
                Some(sha256_hex(value.as_bytes())),
            ),
            Self::RemoveVar(name) => (format!("variable {name} removed"), None),
            Self::Remove => ("removed".into(), None),
            Self::Restore => ("added again".into(), None),
        }
    }

    /// Apply to `o` (unchecked; see [`ProviderOverride::validate`]).
    pub fn apply_to(&self, o: &mut ProviderOverride) {
        match self {
            Self::Enable(on) => o.enabled = (!on).then_some(false),
            Self::DisplayName(n) => o.display_name = n.clone().filter(|n| !n.trim().is_empty()),
            Self::Command(c) => o.command = c.clone().filter(|c| !c.trim().is_empty()),
            Self::Args(a) => o.args = a.clone(),
            Self::SetVar { name, value } => {
                o.env.insert(name.clone(), value.clone());
            }
            Self::RemoveVar(name) => {
                o.env.remove(name);
            }
            Self::Remove => {
                *o = ProviderOverride {
                    removed: true,
                    ..ProviderOverride::default()
                }
            }
            Self::Restore => {
                o.removed = false;
                o.enabled = None;
            }
        }
    }
}

/// The settings file, shared by the UI and the API. `applied` is what this daemon started
/// with; a difference means a restart is needed. Cheap to clone.
#[derive(Clone)]
pub struct ProviderStore {
    inner: Arc<Mutex<StoreInner>>,
}

struct StoreInner {
    path: Option<PathBuf>,
    file: ProviderFile,
    applied: ProviderFile,
}

impl ProviderStore {
    /// Kept in `path`, starting from `applied` (what the daemon loaded at startup).
    pub fn new(path: PathBuf, applied: ProviderFile) -> Self {
        Self {
            inner: Arc::new(Mutex::new(StoreInner {
                path: Some(path),
                file: applied.clone(),
                applied,
            })),
        }
    }

    /// In memory only (tests, routers without a config directory).
    pub fn in_memory() -> Self {
        Self {
            inner: Arc::new(Mutex::new(StoreInner {
                path: None,
                file: ProviderFile::default(),
                applied: ProviderFile::default(),
            })),
        }
    }

    pub fn path(&self) -> Option<PathBuf> {
        lock(&self.inner).path.clone()
    }

    /// The saved settings of a provider.
    pub fn get(&self, id: &str) -> ProviderOverride {
        lock(&self.inner).file.get(id)
    }

    /// The settings the running daemon uses for a provider.
    pub fn applied(&self, id: &str) -> ProviderOverride {
        lock(&self.inner).applied.get(id)
    }

    /// Saved changes that apply only after a restart.
    pub fn restart_needed(&self) -> bool {
        let inner = lock(&self.inner);
        inner.file != inner.applied
    }

    /// Check and save one change. The file is written before memory changes, so a failed
    /// write leaves both as they were.
    pub fn change(&self, id: &str, change: &Change) -> Result<(), SettingsError> {
        let mut inner = lock(&self.inner);
        let mut next = inner.file.clone();
        let entry = next.providers.entry(id.to_string()).or_default();
        change.apply_to(entry);
        entry.validate(id)?;
        if entry.is_empty() {
            next.providers.remove(id);
        }
        if let Some(path) = &inner.path {
            next.save(path)?;
        }
        inner.file = next;
        Ok(())
    }

    /// The owner removed it: a removed built-in, or an added provider deleted since start.
    pub fn is_removed(&self, id: &str) -> bool {
        let inner = lock(&self.inner);
        inner.file.get(id).removed
            || (inner.applied.added.contains_key(id) && !inner.file.added.contains_key(id))
    }

    /// The added providers as saved.
    pub fn added(&self) -> BTreeMap<String, AddedProvider> {
        lock(&self.inner).file.added.clone()
    }

    /// Save a new added provider (refused if the name is taken).
    pub fn add(&self, id: &str, provider: AddedProvider) -> Result<(), SettingsError> {
        provider.validate(id)?;
        let mut inner = lock(&self.inner);
        if inner.file.added.contains_key(id) {
            return Err(invalid(format!(
                "a provider called `{id}` is already added"
            )));
        }
        let mut next = inner.file.clone();
        next.added.insert(id.to_string(), provider);
        if let Some(path) = &inner.path {
            next.save(path)?;
        }
        inner.file = next;
        Ok(())
    }

    /// Change the hosts an added provider may reach.
    pub fn set_hosts(&self, id: &str, hosts: Vec<String>) -> Result<(), SettingsError> {
        let mut inner = lock(&self.inner);
        let mut next = inner.file.clone();
        let Some(entry) = next.added.get_mut(id) else {
            return Err(invalid(format!("no added provider `{id}`")));
        };
        entry.allow = hosts;
        entry.validate(id)?;
        if let Some(path) = &inner.path {
            next.save(path)?;
        }
        inner.file = next;
        Ok(())
    }

    /// Delete an added provider and its settings (API key included); returns what it was.
    pub fn forget(&self, id: &str) -> Result<Option<AddedProvider>, SettingsError> {
        let mut inner = lock(&self.inner);
        let mut next = inner.file.clone();
        let gone = next.added.remove(id);
        next.providers.remove(id);
        if let Some(path) = &inner.path {
            next.save(path)?;
        }
        inner.file = next;
        Ok(gone)
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // Poisoning only means another thread panicked mid-update; the data is still usable.
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::scratch;

    #[test]
    fn secret_names_are_recognised() {
        for s in [
            "ANTHROPIC_API_KEY",
            "XAI_API_KEY",
            "GITHUB_TOKEN",
            "CLIENT_SECRET",
            "DB_PASSWORD",
            "ANTHROPIC_AUTH_TOKEN",
        ] {
            assert!(is_secret_name(s), "{s}");
        }
        for s in ["ANTHROPIC_BASE_URL", "OPENCODE_MODEL", "LANG"] {
            assert!(!is_secret_name(s), "{s}");
        }
    }

    #[test]
    fn views_and_debug_never_show_secret_values() {
        let mut o = ProviderOverride::default();
        o.env
            .insert("ANTHROPIC_API_KEY".into(), "sk-very-secret".into());
        o.env.insert(
            "ANTHROPIC_BASE_URL".into(),
            "https://llm.example.com".into(),
        );
        let view = o.env_view();
        assert_eq!(view[0].name, "ANTHROPIC_API_KEY");
        assert_eq!(view[0].value, None);
        assert!(view[0].secret);
        assert_eq!(view[1].value.as_deref(), Some("https://llm.example.com"));
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("sk-very-secret"));
        assert!(!format!("{o:?}").contains("sk-very-secret"));
        let change = Change::SetVar {
            name: "ANTHROPIC_API_KEY".into(),
            value: "sk-very-secret".into(),
        };
        let (detail, hash) = change.describe();
        assert!(!detail.contains("sk-very-secret") && !format!("{change:?}").contains("sk-"));
        assert_eq!(hash.unwrap(), sha256_hex(b"sk-very-secret"));
    }

    #[test]
    fn checks_reject_what_would_undo_melissas_setup() {
        let ok = |o: &ProviderOverride| o.validate("claude");
        let mut o = ProviderOverride {
            command: Some("/opt/claude/bin/claude".into()),
            args: vec!["--verbose".into(), "--model=sonnet".into()],
            display_name: Some("Claude".into()),
            ..Default::default()
        };
        ok(&o).unwrap();
        for bad in [
            "claude",
            "/usr/bin/other",
            "/opt/../claude/claude",
            "/a/claude\n",
        ] {
            o.command = Some(bad.into());
            assert!(ok(&o).is_err(), "{bad}");
        }
        o.command = None;
        for bad in [
            "--dangerously-skip-permissions",
            "--permission-mode=bypassPermissions",
            "--allowedTools",
            "--mcp-config",
            "-p",
            "{prompt}",
            "",
            "a\nb",
        ] {
            o.args = vec![bad.into()];
            assert!(ok(&o).is_err(), "{bad}");
        }
        o.args = vec![];
        for bad in [
            "PATH",
            "HOME",
            "LD_PRELOAD",
            "HTTPS_PROXY",
            "no_proxy",
            "lower",
            "1X",
            "A-B",
        ] {
            assert!(check_var(bad, "x").is_err(), "{bad}");
        }
        assert!(check_var("ANTHROPIC_BASE_URL", "a\nb").is_err());
        check_var("ANTHROPIC_BASE_URL", "https://llm.example.com").unwrap();
        o.display_name = Some(" ".into());
        assert!(ok(&o).is_err());
        assert!(ProviderOverride::default().validate("Bad Name").is_err());
    }

    #[test]
    fn store_saves_0600_and_tracks_restart() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("store");
        let path = dir.join(FILE_NAME);
        let store = ProviderStore::new(path.clone(), ProviderFile::default());
        assert!(!store.restart_needed());
        store.change("claude", &Change::Enable(false)).unwrap();
        store
            .change(
                "claude",
                &Change::SetVar {
                    name: "ANTHROPIC_API_KEY".into(),
                    value: "sk-1".into(),
                },
            )
            .unwrap();
        assert!(store.restart_needed());
        assert!(store.applied("claude").is_enabled());
        assert!(!store.get("claude").is_enabled());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let again = ProviderFile::load(&path).unwrap();
        assert_eq!(again.get("claude").env["ANTHROPIC_API_KEY"], "sk-1");
        // A bad change is refused and nothing is written.
        let before = std::fs::read_to_string(&path).unwrap();
        assert!(store
            .change("claude", &Change::Args(vec!["--yolo".into()]))
            .is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        // Back to defaults removes the entry.
        store.change("claude", &Change::Enable(true)).unwrap();
        store
            .change("claude", &Change::RemoveVar("ANTHROPIC_API_KEY".into()))
            .unwrap();
        assert!(ProviderFile::load(&path).unwrap().providers.is_empty());
        assert!(!store.restart_needed());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn bad_files_are_errors_without_their_text() {
        let dir = scratch("bad");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, "[provider.claude]\nenv = \"sk-leak\"\n").unwrap();
        let err = ProviderFile::load(&path).unwrap_err().to_string();
        assert!(!err.contains("sk-leak"), "{err}");
        std::fs::write(&path, "[provider.claude]\nargs = [\"--yolo\"]\n").unwrap();
        assert!(ProviderFile::load(&path).is_err());
        assert!(ProviderFile::load(&dir.join("missing.toml"))
            .unwrap()
            .providers
            .is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn added(command: &str) -> AddedProvider {
        AddedProvider {
            label: "Kimi".into(),
            command: command.into(),
            args: vec!["acp".into()],
            allow: vec!["api.example.com".into(), "*.example.net".into()],
            registry: Some("kimi".into()),
            version: Some("1.2.0".into()),
            installed_in: Some("/var/lib/melissa/home/.local/share/melissa-agents/kimi".into()),
        }
    }

    #[test]
    fn added_providers_are_checked() {
        assert!(valid_id("opencode") && !valid_id("") && !valid_id("-x") && !valid_id("A"));
        assert!(added("/opt/kimi/kimi").validate("kimi").is_ok());
        // The file name is the provider id.
        assert!(added("/opt/kimi/other").validate("kimi").is_err());
        // Built-in names are taken.
        assert!(added("/opt/x/claude").validate("claude").is_err());
        for id in ["gemini", "codex-acp", "muse"] {
            let err = added(&format!("/opt/x/{id}")).validate(id).unwrap_err();
            assert!(err.to_string().contains("is built in"), "{id}: {err}");
        }
        // `-y` (Gemini's YOLO) and config overrides can't be added as arguments either.
        for arg in ["-y", "--approval-mode=yolo", "-c", "--disable-sandbox"] {
            let mut a = added("/opt/kimi/kimi");
            a.args = vec![arg.into()];
            assert!(a.validate("kimi").is_err(), "{arg}");
        }
        let mut a = added("/opt/kimi/kimi");
        a.allow = vec!["*".into()];
        assert!(a.validate("kimi").is_err());
        a.allow = vec!["localhost".into()];
        assert!(a.validate("kimi").is_err());
        let mut a = added("/opt/kimi/kimi");
        a.args = vec!["--yolo".into()];
        assert!(a.validate("kimi").is_err());
        let mut a = added("/opt/kimi/kimi");
        a.installed_in = Some("../x".into());
        assert!(a.validate("kimi").is_err());
        a.installed_in = None;
        a.version = Some("1.0; rm".into());
        assert!(a.validate("kimi").is_err());
    }

    #[test]
    fn an_agent_added_before_it_was_built_in_gives_way_to_the_built_in() {
        let dir = scratch("added-now-built-in");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("providers.toml");
        std::fs::write(
            &path,
            "[provider.muse]\nenv = { META_API_KEY = \"k\" }\n\
             [added.muse]\nlabel = \"Muse Code\"\n\
             command = \"/var/lib/melissa/home/.local/share/melissa-agents/muse-code/1.4.24/muse\"\n\
             args = [\"acp\"]\n\
             [added.kimi]\nlabel = \"Kimi\"\ncommand = \"/opt/kimi/kimi\"\n",
        )
        .unwrap();
        let file = ProviderFile::load(&path).unwrap();
        assert!(!file.added.contains_key("muse"));
        assert!(file.added.contains_key("kimi"));
        // Its key stays: the built-in Muse Code uses it.
        assert_eq!(file.providers["muse"].env["META_API_KEY"], "k");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn remove_restore_add_and_forget_are_saved() {
        let dir = scratch("add-remove");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("providers.toml");
        let store = ProviderStore::new(path.clone(), ProviderFile::default());
        store
            .change(
                "grok",
                &Change::SetVar {
                    name: "XAI_API_KEY".into(),
                    value: "secret-value".into(),
                },
            )
            .unwrap();
        store.change("grok", &Change::Remove).unwrap();
        let o = store.get("grok");
        assert!(o.removed && !o.is_enabled() && o.env.is_empty());
        assert!(store.is_removed("grok"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("removed = true") && !text.contains("secret-value"));
        store.change("grok", &Change::Restore).unwrap();
        assert!(!store.is_removed("grok") && store.get("grok").is_enabled());

        store.add("kimi", added("/opt/kimi/kimi")).unwrap();
        assert!(store.add("kimi", added("/opt/kimi/kimi")).is_err());
        store
            .change(
                "kimi",
                &Change::SetVar {
                    name: "KIMI_API_KEY".into(),
                    value: "k".into(),
                },
            )
            .unwrap();
        let loaded = ProviderFile::load(&path).unwrap();
        assert_eq!(loaded.added["kimi"], added("/opt/kimi/kimi"));
        store
            .set_hosts("kimi", vec!["api.example.org".into()])
            .unwrap();
        assert!(store.set_hosts("kimi", vec!["nope".into()]).is_err());
        assert_eq!(store.added()["kimi"].allow, ["api.example.org"]);

        // Removing an added provider that the daemon started with hides it until restart.
        let started = ProviderStore::new(path.clone(), ProviderFile::load(&path).unwrap());
        let gone = started.forget("kimi").unwrap().unwrap();
        assert_eq!(gone.label, "Kimi");
        assert!(started.is_removed("kimi"));
        let after = ProviderFile::load(&path).unwrap();
        assert!(after.added.is_empty() && !after.providers.contains_key("kimi"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_added_entry_fails_the_load() {
        let dir = scratch("added-bad");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("providers.toml");
        std::fs::write(
            &path,
            "[added.kimi]\nlabel = \"Kimi\"\ncommand = \"kimi\"\n",
        )
        .unwrap();
        let e = ProviderFile::load(&path).unwrap_err().to_string();
        assert!(e.contains("[added.kimi]"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
