#![forbid(unsafe_code)]
//! Melissa's app spec: what an agent writes to deploy an app (`deploy_app`, `update_app`).
//!
//! There is no catalog. An agent picks images and settings for whatever the user asks for
//! (a Minecraft server, Jellyfin, a custom container) and writes an [`AppSpec`] as JSON. The
//! server validates it ([`AppSpec::validate`], which also normalizes image references),
//! shows the user a summary to confirm ([`AppSpec::summary`]), renders it to Kubernetes
//! objects and stores it on the app's namespace so `update_app` can change it later.
//!
//! What may run is limited by the server, not by the prompt: the renderer only produces a
//! single-replica Deployment, Services, PVCs, ConfigMaps, generated Secrets and an Ingress in
//! the app's own namespace, never privileged and never on the host; admission policies check
//! the same again (docs/agent-skills/app-deploy.md).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

mod validate;

pub use validate::{normalize_image, ImageRef};

/// Default domain for app ingress hosts (`<app>.<domain>`); `home.arpa` is reserved for home
/// networks (RFC 8375).
pub const DEFAULT_DOMAIN: &str = "melissa.home.arpa";

/// Default length of generated secrets (characters from a 62-letter alphabet, ~190 bits).
pub const DEFAULT_SECRET_LENGTH: usize = 32;

/// Limits that keep specs small and the confirmation readable.
pub const MAX_CONTAINERS: usize = 6;
pub const MAX_VOLUMES: usize = 8;
pub const MAX_PORTS: usize = 16;
/// Ports on the device per app (the whole range has 80).
pub const MAX_PUBLIC_PORTS: usize = 6;
pub const MAX_ENV: usize = 100;
pub const MAX_ENV_VALUE_BYTES: usize = 8 * 1024;
pub const MAX_ARGS: usize = 64;
pub const MAX_ARG_BYTES: usize = 4096;
/// Largest [`File::content`] in bytes, and most files per container.
pub const MAX_FILE_BYTES: usize = 16 * 1024;
pub const MAX_FILES: usize = 8;
pub const MAX_SECRETS: usize = 16;
/// Largest spec as stored (JSON bytes); it lives in a namespace annotation.
pub const MAX_SPEC_BYTES: usize = 128 * 1024;
/// Largest limits one container may ask for.
pub const MAX_CPU_MILLIS: u64 = 16_000;
pub const MAX_MEMORY_BYTES: u64 = 64 << 30;
pub const MAX_VOLUME_BYTES: u64 = 4 << 40;

/// Defaults when a container has no `resources` (or leaves parts out).
pub const DEFAULT_CPU_LIMIT: &str = "1";
pub const DEFAULT_MEMORY_LIMIT: &str = "512Mi";

/// Pod volume name the rendered files use; specs can't name a volume this.
pub const FILES_VOLUME: &str = "melissa-files";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SpecError {
    #[error("the spec is not valid JSON for an app spec: {0}")]
    Parse(String),
    /// A field is present but wrong; the path names it (`containers[0].image`).
    #[error("{path}: {message}")]
    Invalid { path: String, message: String },
}

impl SpecError {
    pub(crate) fn invalid(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Invalid {
            path: path.into(),
            message: message.into(),
        }
    }
}

/// One app, as an agent writes it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppSpec {
    /// One line for people (`Minecraft Java server`).
    pub description: String,
    /// The app's own version, for people (the image tag or digest is what runs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Containers of the app's one pod (the main one first; sidecars such as a database
    /// after it). They share the network: a database container is at `localhost`.
    pub containers: Vec<Container>,
    /// Persistent volumes (data that survives restarts and updates).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<Volume>,
    /// Secrets the server generates at the first deploy (passwords, keys). Containers read
    /// them with `{"secret": "<name>"}` env values; agents never see the values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<SecretField>,
    /// The port name of the app's web UI: it gets a link (`http://<device>:<port>`) and the
    /// `<app>.<domain>` ingress host. Must be a TCP port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<String>,
    /// The owner's login, shown by `melissa-server app-credentials <app>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials: Option<Credentials>,
    /// What kind of app it is: the web UI groups the app list by it (2026-11-22; specs
    /// without it are [`AppKind::Other`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<AppKind>,
    /// What the app may reach on the network (2026-11-24). Absent: nothing but its own pod,
    /// DNS, and people reaching its web and public ports ([`Network::default`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<Network>,
    /// How the app's pod is kept apart from the device's kernel (2026-11-23). Absent: the
    /// default, [`Isolation::DEFAULT`]; the deployer stores the level it used, so a later
    /// change of the default never moves a deployed app ([`AppSpec::isolation`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<Isolation>,
}

/// Most links one app may declare.
pub const MAX_LINKS: usize = 16;

/// An app's network permissions, enforced by Kubernetes NetworkPolicies in its namespace.
/// Every part is off by default: an app reaches only DNS and its own pod's containers, and
/// people reach its web and public ports. The owner sees and approves each change.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Network {
    /// Public internet addresses (never the home network, the tailnet, the cluster or this
    /// device).
    #[serde(default, skip_serializing_if = "is_false")]
    pub internet: bool,
    /// Devices on the home network (private address ranges), never the cluster, this device
    /// or the tailnet.
    #[serde(default, skip_serializing_if = "is_false")]
    pub lan: bool,
    /// Other apps on this device this app connects to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
}

/// A connection to another app: its pods, on one of its ports (or all of them).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    /// The other app's name.
    pub app: String,
    /// The other app's port name (`db`); absent: every port of that app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
}

impl Link {
    /// `postgres (port db)` / `postgres (all ports)`, for people.
    pub fn words(&self) -> String {
        match &self.port {
            Some(p) => format!("{} (port {p})", self.app),
            None => format!("{} (all ports)", self.app),
        }
    }
}

impl Network {
    /// Nothing granted (the default).
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// One line for people: `internet, home network, links to postgres (port db)` or
    /// `none (only its own ports)`.
    pub fn words(&self) -> String {
        let mut parts = Vec::new();
        if self.internet {
            parts.push("internet".to_string());
        }
        if self.lan {
            parts.push("home network".to_string());
        }
        if !self.links.is_empty() {
            let links: Vec<String> = self.links.iter().map(Link::words).collect();
            parts.push(format!("apps {}", links.join(", ")));
        }
        if parts.is_empty() {
            "nothing (no internet, no home network, no other apps)".into()
        } else {
            parts.join("; ")
        }
    }

    /// Check the links of app `app`'s network: names, no self-link, no duplicates. (Whether
    /// the other apps and ports exist is the deployer's check.)
    pub fn validate_for(&self, app: &str) -> Result<(), SpecError> {
        if self.links.len() > MAX_LINKS {
            return Err(SpecError::invalid(
                "network.links",
                format!("at most {MAX_LINKS} links"),
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for (i, l) in self.links.iter().enumerate() {
            let path = format!("network.links[{i}]");
            if !valid_app_name(&l.app) {
                return Err(SpecError::invalid(
                    format!("{path}.app"),
                    format!("`{}` is not an app name", l.app),
                ));
            }
            if l.app == app {
                return Err(SpecError::invalid(
                    format!("{path}.app"),
                    "an app reaches its own containers anyway; link other apps only",
                ));
            }
            if let Some(p) = &l.port {
                validate::dns_label(p, 15)
                    .map_err(|m| SpecError::invalid(format!("{path}.port"), m))?;
            }
            if !seen.insert(l) {
                return Err(SpecError::invalid(
                    &path,
                    format!("duplicate link to {}", l.words()),
                ));
            }
        }
        Ok(())
    }
}

/// How an app's pod is kept apart from the device's kernel: the k3s RuntimeClass it runs
/// under (docs/design/ISOLATION-RESEARCH-2026-11-23.md). Room for a `kata` level (a VM per
/// pod) later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Isolation {
    /// An ordinary container (runc): the app's system calls reach the device's kernel,
    /// filtered by seccomp `RuntimeDefault`. For apps that need host devices or kernel
    /// features gVisor lacks (io_uring, raw sockets).
    Standard,
    /// gVisor (`runsc`, RuntimeClass `gvisor`): the app's system calls go to its own kernel
    /// in user space (the Sentry), which uses only a small, filtered set of the device's.
    Gvisor,
}

impl Isolation {
    /// What a spec without `isolation` gets. Agents pick any image from any registry, and there
    /// is no list of trusted images, so every agent-written spec runs under gVisor unless it
    /// asks for `standard` (shown to the user in the confirmation). There is no per-app or
    /// per-image exception.
    pub const DEFAULT: Isolation = Isolation::Gvisor;

    pub const ALL: [Isolation; 2] = [Self::Standard, Self::Gvisor];

    /// The name in specs, labels and tool answers (`gvisor`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Gvisor => "gvisor",
        }
    }

    /// The level named `name` (as in specs); None for anything else.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|i| i.as_str() == name)
    }

    /// For people, in the confirmation: what the level means.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Standard => "standard (shares the device's kernel)",
            Self::Gvisor => "gVisor sandbox (its own kernel)",
        }
    }
}

/// The groups of the app list, in the order the UI shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AppKind {
    /// Game servers (Minecraft, Luanti).
    Games,
    /// Films, music, photos (Jellyfin, Navidrome, Immich).
    Media,
    /// The home itself (Home Assistant, Zigbee bridges).
    Home,
    /// Files, sync, documents, office (Nextcloud, Syncthing, Paperless).
    Files,
    /// The network (Pi-hole, a VPN, a reverse proxy).
    Network,
    /// For developers (Forgejo, CI runners, databases to try things).
    Developer,
    /// Anything else, and specs without a kind.
    Other,
}

impl AppKind {
    /// Every kind, in the UI's order.
    pub const ALL: [AppKind; 7] = [
        Self::Games,
        Self::Media,
        Self::Home,
        Self::Files,
        Self::Network,
        Self::Developer,
        Self::Other,
    ];

    /// The name in specs and tool answers (`games`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Games => "games",
            Self::Media => "media",
            Self::Home => "home",
            Self::Files => "files",
            Self::Network => "network",
            Self::Developer => "developer",
            Self::Other => "other",
        }
    }

    /// The kind named `name` (as in specs); None for anything else.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Container {
    /// DNS label (`server`, `db`).
    pub name: String,
    /// `registry/repository:tag` or `...@sha256:<digest>`; Docker Hub short names
    /// (`nginx:1.27`, `itzg/minecraft-server:java21`) are expanded. `latest` or no tag is
    /// refused unless a digest pins it.
    pub image: String,
    /// Replaces the image's entrypoint.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// Replaces the image's arguments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Environment variables: a plain value (string, number or boolean), or
    /// `{"secret": "<name>"}`, or `{"builtin": "device_hosts" | "ingress_host" | "ingress_url"}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, EnvValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<Port>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<Mount>,
    /// Small config files, mounted read-only (a ConfigMap).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<File>,
    /// Limits are always set; missing parts get defaults ([`Resources::resolved`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<Resources>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<Probe>,
    /// Run as this user/group id instead of the image's default (which may be root inside
    /// the container; never privileged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_as_user: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_as_group: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Tcp,
    Udp,
}

impl Protocol {
    /// `TCP`/`UDP`, as Kubernetes and people write it.
    pub fn upper(self) -> &'static str {
        match self {
            Self::Tcp => "TCP",
            Self::Udp => "UDP",
        }
    }

    pub fn lower(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_tcp(p: &Protocol) -> bool {
    *p == Protocol::Tcp
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Port {
    /// IANA-style name, at most 15 characters (`http`, `game`).
    pub name: String,
    /// The port inside the container.
    pub port: u16,
    #[serde(default, skip_serializing_if = "is_tcp")]
    pub protocol: Protocol,
    /// Reachable from other devices at `<device address>:<its own port>` (a port from
    /// Melissa's app range, given by the server). The `web` port is public anyway.
    #[serde(default, skip_serializing_if = "is_false")]
    pub public: bool,
}

/// An environment value: plain text, or a reference.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum EnvValue {
    Text(String),
    Number(serde_json::Number),
    Bool(bool),
    Ref(EnvRef),
}

/// Exactly one field must be set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnvRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// A generated secret (`secrets`), from the app's Secret, never in the pod spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin: Option<Builtin>,
}

/// The source of an [`EnvValue`], after validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvSource<'a> {
    Value(std::borrow::Cow<'a, str>),
    Secret(&'a str),
    Builtin(Builtin),
}

impl EnvValue {
    /// The single source; `None` if a reference sets zero or several fields (refused by
    /// validation).
    pub fn source(&self) -> Option<EnvSource<'_>> {
        use std::borrow::Cow;
        match self {
            Self::Text(s) => Some(EnvSource::Value(Cow::Borrowed(s))),
            Self::Number(n) => Some(EnvSource::Value(Cow::Owned(n.to_string()))),
            Self::Bool(b) => Some(EnvSource::Value(Cow::Owned(b.to_string()))),
            Self::Ref(r) => {
                let sources = [
                    r.value
                        .as_deref()
                        .map(|v| EnvSource::Value(Cow::Borrowed(v))),
                    r.secret.as_deref().map(EnvSource::Secret),
                    r.builtin.map(EnvSource::Builtin),
                ];
                let mut set = sources.into_iter().flatten();
                let first = set.next()?;
                set.next().is_none().then_some(first)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Builtin {
    /// `<app>.<domain>`.
    IngressHost,
    /// `http://<app>.<domain>`.
    IngressUrl,
    /// Space-separated host names the app is reached at: the ingress host, then the device's
    /// addresses and host name (for apps that check the `Host` header, like Nextcloud's
    /// `trusted_domains`).
    DeviceHosts,
}

/// A file placed into a container (a ConfigMap, mounted read-only with `subPath`, so it
/// never lands in a data volume).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct File {
    /// Absolute path in the container; not inside one of its volume mounts.
    pub path: String,
    /// The file's text (at most [`MAX_FILE_BYTES`]).
    pub content: String,
    /// Mode 0555 instead of 0444 (scripts).
    #[serde(default, skip_serializing_if = "is_false")]
    pub executable: bool,
}

/// A persistent volume (a PVC on the `local-path` storage class).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Volume {
    pub name: String,
    /// `512Mi`, `10Gi`. local-path does not enforce it; it says what the app expects.
    pub size: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Mount {
    pub volume: String,
    /// Absolute path in the container.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecretField {
    /// Lower snake case (`admin_password`).
    pub name: String,
    /// Generated length, 16-128 (default [`DEFAULT_SECRET_LENGTH`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<usize>,
}

/// CPU and memory. `cpu`/`memory` are the limits; requests default to a small share.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Resources {
    /// CPU limit (`500m`, `2`). Default [`DEFAULT_CPU_LIMIT`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<String>,
    /// Memory limit (`512Mi`, `2Gi`). Default [`DEFAULT_MEMORY_LIMIT`]. The container is
    /// killed above it, so leave headroom (Java heap + ~25%).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    /// What the scheduler reserves; default the smaller of 100m and the limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_request: Option<String>,
    /// Default a quarter of the memory limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_request: Option<String>,
}

/// Requests and limits with the defaults filled in (Kubernetes quantities).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedResources {
    pub cpu_request: String,
    pub cpu_limit: String,
    pub memory_request: String,
    pub memory_limit: String,
}

impl Resources {
    /// Defaults filled in. Call on validated specs (malformed values fall back to defaults).
    pub fn resolved(r: Option<&Self>) -> ResolvedResources {
        let r = r.cloned().unwrap_or_default();
        let cpu_limit = r.cpu.unwrap_or_else(|| DEFAULT_CPU_LIMIT.into());
        let memory_limit = r.memory.unwrap_or_else(|| DEFAULT_MEMORY_LIMIT.into());
        let cpu_request = r
            .cpu_request
            .unwrap_or_else(|| match validate::cpu_millis(&cpu_limit) {
                Some(m) if m < 100 => cpu_limit.clone(),
                _ => "100m".into(),
            });
        let memory_request = r.memory_request.unwrap_or_else(|| {
            let bytes = validate::binary_quantity(&memory_limit).unwrap_or(512 << 20);
            format!("{}Mi", (bytes >> 20).div_ceil(4).max(1))
        });
        ResolvedResources {
            cpu_request,
            cpu_limit,
            memory_request,
            memory_limit,
        }
    }
}

/// Health check: an HTTP GET (`path` set) or a TCP connect to a container port; used for
/// startup, readiness and liveness.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    /// Name of one of the container's TCP ports.
    pub port: String,
    /// HTTP path (`/status.php`); without it the probe only connects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Send the ingress host as `Host` (apps that check trusted domains); needs `web`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub host_header: bool,
    /// How long the first start (download, install, migrations) may take.
    #[serde(default = "default_startup_secs")]
    pub startup_timeout_secs: u32,
    #[serde(default = "default_period_secs")]
    pub period_secs: u32,
}

fn default_startup_secs() -> u32 {
    300
}

fn default_period_secs() -> u32 {
    10
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Credentials {
    /// The login name (literal), if the app has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// A generated secret (`secrets`): the password.
    pub password: String,
}

impl AppSpec {
    /// Parse JSON and validate (normalizing image references).
    pub fn from_json(value: &serde_json::Value) -> Result<Self, SpecError> {
        let mut spec: Self =
            serde_json::from_value(value.clone()).map_err(|e| SpecError::Parse(e.to_string()))?;
        spec.validate()?;
        Ok(spec)
    }

    /// Parse a stored spec (JSON text) and validate it.
    pub fn from_text(text: &str) -> Result<Self, SpecError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| SpecError::Parse(e.to_string()))?;
        Self::from_json(&value)
    }

    /// Compact JSON, as stored on the app's namespace.
    pub fn to_text(&self) -> String {
        // Plain data (strings, numbers, maps with string keys): serializing can't fail.
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Every port, with its container.
    pub fn ports(&self) -> impl Iterator<Item = &Port> {
        self.containers.iter().flat_map(|c| &c.ports)
    }

    pub fn port(&self, name: &str) -> Option<&Port> {
        self.ports().find(|p| p.name == name)
    }

    /// Ports that get a port on the device (the web port first, then `public` ones).
    pub fn public_ports(&self) -> Vec<&Port> {
        let web = self.web.as_deref().and_then(|w| self.port(w));
        web.into_iter()
            .chain(
                self.ports()
                    .filter(|p| p.public && Some(p.name.as_str()) != self.web.as_deref()),
            )
            .collect()
    }

    /// The isolation level the app runs at: the spec's own, else [`Isolation::DEFAULT`].
    pub fn isolation(&self) -> Isolation {
        self.isolation.unwrap_or(Isolation::DEFAULT)
    }

    pub fn secret(&self, name: &str) -> Option<&SecretField> {
        self.secrets.iter().find(|s| s.name == name)
    }

    /// Secret name → generated length.
    pub fn secret_lengths(&self) -> BTreeMap<String, usize> {
        self.secrets
            .iter()
            .map(|s| (s.name.clone(), s.length.unwrap_or(DEFAULT_SECRET_LENGTH)))
            .collect()
    }

    /// What will run, in a few short lines, for the person who confirms a deploy.
    pub fn summary(&self) -> Vec<String> {
        let mut out = Vec::new();
        for c in &self.containers {
            let r = Resources::resolved(c.resources.as_ref());
            out.push(format!(
                "{}: {} (up to {} CPU, {} memory)",
                c.name, c.image, r.cpu_limit, r.memory_limit
            ));
        }
        let public: Vec<String> = self
            .public_ports()
            .iter()
            .map(|p| {
                let web = if Some(p.name.as_str()) == self.web.as_deref() {
                    ", web"
                } else {
                    ""
                };
                format!("{} {}/{}{web}", p.name, p.port, p.protocol.lower())
            })
            .collect();
        if !public.is_empty() {
            out.push(format!("reachable from the network: {}", public.join(", ")));
        }
        if !self.volumes.is_empty() {
            let v: Vec<String> = self
                .volumes
                .iter()
                .map(|v| format!("{} {}", v.name, v.size))
                .collect();
            out.push(format!("storage: {}", v.join(", ")));
        }
        out.push(format!("may reach: {}", self.network_or_default().words()));
        out.push(format!("isolation: {}", self.isolation().describe()));
        out
    }

    /// The network permissions, defaults filled in.
    pub fn network_or_default(&self) -> Network {
        self.network.clone().unwrap_or_default()
    }

    /// Set the network permissions; the default is stored as no block at all.
    pub fn set_network(&mut self, network: Network) {
        self.network = (!network.is_default()).then_some(network);
    }
}

/// A person's account id as an app's `melissa.io/installed-for` annotation may hold it: 1-64 ASCII letters, digits, `-`, `_`
/// or `.` (anything else is ignored, so an edited annotation can't smuggle text into the UI).
pub fn valid_person_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Is `name` a well-formed app name?
pub fn valid_app_name(name: &str) -> bool {
    validate::dns_label(name, 40).is_ok()
}

/// `app-<name>`: the namespace of an app.
pub fn namespace_of(app: &str) -> String {
    format!("app-{app}")
}

#[cfg(test)]
mod tests;
