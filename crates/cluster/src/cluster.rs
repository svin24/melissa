//! Kubernetes access for the lead agent.
//!
//! The lead agent never uses k3s's admin kubeconfig. It reads a kubeconfig for the `melissa-server`
//! ServiceAccount that a root helper writes into [`RUNTIME_DIR`] once k3s is up (or one from a
//! systemd credential, or a path in its config). The lead agent starts without waiting for k3s:
//! [`LazyCluster`] looks for the kubeconfig on every call and picks up a new or rotated one
//! without a restart. Without one, the cluster is "not configured" (or "still starting" while
//! [`RUNTIME_DIR`] exists but is empty) and cluster tools say so.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use k8s_openapi::api::core::v1::{Namespace, Node, Pod};
use kube::{
    api::{Api, ListParams},
    config::{KubeConfigOptions, Kubeconfig},
    Client, Config,
};
use melissa_agent::BoxFuture;

/// Name of the systemd credential (and of its file in `/run/credstore`) holding the kubeconfig.
pub const CREDENTIAL_NAME: &str = "melissa.kubeconfig";

/// Directory where `melissa-kubeconfig` (root) puts the lead agent's kubeconfig on devices that run k3s.
/// Created at boot (tmpfiles.d, headless image), group `melissa-kube`, so "directory present but
/// no file" means k3s is enabled and still starting.
pub const RUNTIME_DIR: &str = "/run/melissa-kube";

/// File name of the kubeconfig inside [`RUNTIME_DIR`].
pub const RUNTIME_FILE: &str = "server.kubeconfig";

/// File name of the write identity's kubeconfig (ServiceAccount `melissa/melissa-apps`) inside
/// [`RUNTIME_DIR`]. Only the app tools use it; read-only tools stay on [`RUNTIME_FILE`].
pub const APPS_RUNTIME_FILE: &str = "apps.kubeconfig";

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ClusterError {
    /// No kubeconfig: k3s is not enabled on this device (desktop) or its credentials are missing.
    #[error("k3s is not set up on this device (no cluster credentials)")]
    NotConfigured,
    /// k3s is enabled here but the lead agent's credentials aren't issued yet (k3s still starting).
    #[error("k3s is starting: the lead's cluster credentials are not issued yet")]
    Starting,
    /// The API server did not answer (k3s stopped, starting, or crashed).
    #[error("k3s API unreachable: {0}")]
    Unreachable(String),
    /// The API answered but refused (401/403): RBAC or token problem.
    #[error("k3s API refused the request: {0}")]
    Forbidden(String),
    #[error("k3s API error: {0}")]
    Api(String),
    #[error("bad kubeconfig {path}: {message}")]
    Config { path: PathBuf, message: String },
}

/// `app_platform` when k3s isn't on this device at all (from source, or a desktop without it).
/// Words the lead agent can pass on: why, where the full install is, what still works here.
pub const NOT_INSTALLED: &str = "not installed on this device: this copy of Melissa runs without \
    the app platform (k3s), so it can't install or run apps here. Apps need the full install, the \
    container or NixOS (see docs/QUICKSTART.md). Chat, helpers, schedules, files and describing \
    this device still work.";

/// `app_platform` when k3s is installed here but its API doesn't answer (starting, restarting).
pub const NOT_ANSWERING: &str = "installed but not answering right now (starting or restarting). \
    Melissa keeps trying; try again in a minute.";

impl ClusterError {
    /// What the error means for apps on this device, in words for the lead agent to pass on
    /// ([`NOT_INSTALLED`] or [`NOT_ANSWERING`]). `None`: some other failure; the error says what.
    pub fn app_platform(&self) -> Option<&'static str> {
        match self {
            Self::NotConfigured => Some(NOT_INSTALLED),
            Self::Starting | Self::Unreachable(_) => Some(NOT_ANSWERING),
            _ => None,
        }
    }

    /// The error as a tool's failure text, with [`Self::app_platform`] when it applies.
    pub fn for_tool(&self) -> String {
        match self.app_platform() {
            Some(words) => format!("The app platform is {words} ({self})"),
            None => self.to_string(),
        }
    }
}

/// A read tool's answer when k3s can't be used: a result, not an error, so the lead agent and
/// the UI can tell "not installed" (`k3s_set_up: false`) from "not answering".
pub(crate) fn unavailable(e: &ClusterError) -> serde_json::Value {
    let mut out = serde_json::json!({"k3s_reachable": false, "reason": e.to_string()});
    if let Some(words) = e.app_platform() {
        out["app_platform"] = words.into();
    }
    // No cluster credentials at all: k3s isn't set up here (not merely down or starting).
    if matches!(e, ClusterError::NotConfigured) {
        out["k3s_set_up"] = false.into();
    }
    out
}

/// Read-only view of the cluster. Kept narrow on purpose: one method per object kind a tool
/// needs. Writes get a separate trait so read-only tools can't reach them.
pub trait ClusterRead: Send + Sync {
    /// The API server's `gitVersion`, e.g. `v1.37.2+k3s1`.
    fn server_version(&self) -> BoxFuture<'_, Result<String, ClusterError>>;
    fn nodes(&self) -> BoxFuture<'_, Result<Vec<Node>, ClusterError>>;
    fn namespaces(&self) -> BoxFuture<'_, Result<Vec<Namespace>, ClusterError>>;
    /// Pods in all namespaces.
    fn pods(&self) -> BoxFuture<'_, Result<Vec<Pod>, ClusterError>>;
}

/// [`ClusterRead`] over a real API server.
#[derive(Clone)]
pub struct KubeCluster {
    client: Client,
}

impl KubeCluster {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    /// Build a client from a kubeconfig file. Does not contact the server.
    pub async fn from_kubeconfig(path: &Path) -> Result<Self, ClusterError> {
        let bad = |message: String| ClusterError::Config {
            path: path.to_path_buf(),
            message,
        };
        let kubeconfig = Kubeconfig::read_from(path).map_err(|e| bad(e.to_string()))?;
        let config = Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default())
            .await
            .map_err(|e| bad(e.to_string()))?;
        let client = Client::try_from(config).map_err(|e| bad(e.to_string()))?;
        Ok(Self::new(client))
    }

    /// The client, for future write-capable wrappers.
    pub fn client(&self) -> &Client {
        &self.client
    }
}

/// A [`ClusterRead`] that finds its kubeconfig on every call, so the lead agent can start before k3s
/// and attach to the cluster whenever the credentials appear (or change after a rotation).
///
/// The first existing file in `candidates` wins. The client is rebuilt only when the file's
/// contents change; building a client does not contact the server.
pub struct LazyCluster {
    candidates: Vec<PathBuf>,
    /// If this directory exists while no candidate does, report [`ClusterError::Starting`].
    pending_dir: Option<PathBuf>,
    current: tokio::sync::Mutex<Option<Loaded>>,
}

struct Loaded {
    path: PathBuf,
    contents: Vec<u8>,
    cluster: Arc<KubeCluster>,
}

impl LazyCluster {
    pub fn new(candidates: Vec<PathBuf>, pending_dir: Option<PathBuf>) -> Self {
        Self {
            candidates,
            pending_dir,
            current: tokio::sync::Mutex::new(None),
        }
    }

    /// The lead agent's sources in order: systemd credential, configured path, [`RUNTIME_DIR`].
    pub fn for_lead(credentials_dir: Option<&Path>, configured: Option<&Path>) -> Self {
        let runtime = PathBuf::from(RUNTIME_DIR);
        let candidates = credentials_dir
            .map(|d| d.join(CREDENTIAL_NAME))
            .into_iter()
            .chain(configured.map(Path::to_path_buf))
            .chain([runtime.join(RUNTIME_FILE)])
            .collect();
        Self::new(candidates, Some(runtime))
    }

    /// The app tools' sources: configured path, then `RUNTIME_DIR/apps.kubeconfig`.
    pub fn for_apps(configured: Option<&Path>) -> Self {
        let runtime = PathBuf::from(RUNTIME_DIR);
        let candidates = configured
            .map(Path::to_path_buf)
            .into_iter()
            .chain([runtime.join(APPS_RUNTIME_FILE)])
            .collect();
        Self::new(candidates, Some(runtime))
    }

    /// The client for the current kubeconfig, (re)built if the file appeared or changed.
    pub(crate) async fn cluster(&self) -> Result<Arc<KubeCluster>, ClusterError> {
        let found = self.candidates.iter().find(|p| p.is_file());
        let mut current = self.current.lock().await;
        let Some(path) = found else {
            if current.take().is_some() {
                tracing::warn!("k3s kubeconfig is gone; cluster tools are offline");
            }
            return Err(match &self.pending_dir {
                Some(d) if d.is_dir() => ClusterError::Starting,
                _ => ClusterError::NotConfigured,
            });
        };
        let contents = tokio::fs::read(path)
            .await
            .map_err(|e| ClusterError::Config {
                path: path.clone(),
                message: e.to_string(),
            })?;
        if let Some(loaded) = current.as_ref() {
            if loaded.path == *path && loaded.contents == contents {
                return Ok(loaded.cluster.clone());
            }
        }
        let cluster = Arc::new(KubeCluster::from_kubeconfig(path).await?);
        tracing::info!(path = %path.display(), "k3s client configured");
        *current = Some(Loaded {
            path: path.clone(),
            contents,
            cluster: cluster.clone(),
        });
        Ok(cluster)
    }
}

impl ClusterRead for LazyCluster {
    fn server_version(&self) -> BoxFuture<'_, Result<String, ClusterError>> {
        Box::pin(async move { self.cluster().await?.server_version().await })
    }

    fn nodes(&self) -> BoxFuture<'_, Result<Vec<Node>, ClusterError>> {
        Box::pin(async move { self.cluster().await?.nodes().await })
    }

    fn namespaces(&self) -> BoxFuture<'_, Result<Vec<Namespace>, ClusterError>> {
        Box::pin(async move { self.cluster().await?.namespaces().await })
    }

    fn pods(&self) -> BoxFuture<'_, Result<Vec<Pod>, ClusterError>> {
        Box::pin(async move { self.cluster().await?.pods().await })
    }
}

pub(crate) fn map_err(e: kube::Error) -> ClusterError {
    match e {
        kube::Error::Api(status) if status.code == 401 || status.code == 403 => {
            ClusterError::Forbidden(status.message.clone())
        }
        kube::Error::Api(status) => {
            ClusterError::Api(format!("{} ({})", status.message, status.code))
        }
        // Everything below the HTTP layer (connection refused, TLS, timeouts) means "can't reach".
        kube::Error::HyperError(_) | kube::Error::Service(_) | kube::Error::HttpError(_) => {
            ClusterError::Unreachable(e.to_string())
        }
        other => ClusterError::Api(other.to_string()),
    }
}

impl ClusterRead for KubeCluster {
    fn server_version(&self) -> BoxFuture<'_, Result<String, ClusterError>> {
        Box::pin(async move {
            let info = self.client.apiserver_version().await.map_err(map_err)?;
            Ok(info.git_version)
        })
    }

    fn nodes(&self) -> BoxFuture<'_, Result<Vec<Node>, ClusterError>> {
        Box::pin(async move {
            let api: Api<Node> = Api::all(self.client.clone());
            Ok(api
                .list(&ListParams::default())
                .await
                .map_err(map_err)?
                .items)
        })
    }

    fn namespaces(&self) -> BoxFuture<'_, Result<Vec<Namespace>, ClusterError>> {
        Box::pin(async move {
            let api: Api<Namespace> = Api::all(self.client.clone());
            Ok(api
                .list(&ListParams::default())
                .await
                .map_err(map_err)?
                .items)
        })
    }

    fn pods(&self) -> BoxFuture<'_, Result<Vec<Pod>, ClusterError>> {
        Box::pin(async move {
            let api: Api<Pod> = Api::all(self.client.clone());
            Ok(api
                .list(&ListParams::default())
                .await
                .map_err(map_err)?
                .items)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kubeconfig(server: &str) -> String {
        format!(
            r#"apiVersion: v1
kind: Config
clusters:
- name: local
  cluster: {{server: "{server}", insecure-skip-tls-verify: true}}
users:
- name: lead
  user: {{token: placeholder}}
contexts:
- name: local
  context: {{cluster: local, user: lead}}
current-context: local
"#
        )
    }

    #[tokio::test]
    async fn lazy_cluster_attaches_when_the_kubeconfig_appears_and_follows_changes() {
        let dir = std::env::temp_dir().join(format!("melissa-cluster-lazy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join(RUNTIME_FILE);
        let lazy = LazyCluster::new(vec![file.clone()], Some(dir.clone()));

        // No directory: this device has no k3s.
        assert_eq!(
            lazy.server_version().await,
            Err(ClusterError::NotConfigured)
        );
        // Directory but no file: k3s is enabled and still starting.
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(lazy.server_version().await, Err(ClusterError::Starting));
        // The kubeconfig appears: the same object now talks to the (dead) server.
        std::fs::write(&file, kubeconfig("https://127.0.0.1:1")).unwrap();
        assert!(matches!(
            lazy.nodes().await,
            Err(ClusterError::Unreachable(_))
        ));
        let first = lazy.cluster().await.unwrap();
        // Unchanged file: the client is reused.
        assert!(Arc::ptr_eq(&first, &lazy.cluster().await.unwrap()));
        // Rotated file: a new client.
        std::fs::write(&file, kubeconfig("https://127.0.0.1:2")).unwrap();
        assert!(!Arc::ptr_eq(&first, &lazy.cluster().await.unwrap()));
        // A broken file is a config error, not a panic.
        std::fs::write(&file, "not: [a kubeconfig").unwrap();
        assert!(matches!(
            lazy.server_version().await,
            Err(ClusterError::Config { .. })
        ));
        // Gone again (k3s disabled, reboot): back to "starting".
        std::fs::remove_file(&file).unwrap();
        assert_eq!(lazy.server_version().await, Err(ClusterError::Starting));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lead_sources_are_ordered_credential_config_runtime() {
        let lazy = LazyCluster::for_lead(Some(Path::new("/creds")), Some(Path::new("/etc/x.yaml")));
        assert_eq!(
            lazy.candidates,
            vec![
                PathBuf::from("/creds").join(CREDENTIAL_NAME),
                PathBuf::from("/etc/x.yaml"),
                Path::new(RUNTIME_DIR).join(RUNTIME_FILE),
            ]
        );
        assert_eq!(lazy.pending_dir.as_deref(), Some(Path::new(RUNTIME_DIR)));
    }

    #[tokio::test]
    async fn loads_a_token_kubeconfig_without_contacting_the_server() {
        let path =
            std::env::temp_dir().join(format!("melissa-cluster-kubeconfig-{}", std::process::id()));
        std::fs::write(
            &path,
            r#"apiVersion: v1
kind: Config
clusters:
- name: local
  cluster: {server: "https://127.0.0.1:1", insecure-skip-tls-verify: true}
users:
- name: lead
  user: {token: placeholder}
contexts:
- name: local
  context: {cluster: local, user: lead}
current-context: local
"#,
        )
        .unwrap();
        let cluster = KubeCluster::from_kubeconfig(&path).await.unwrap();
        // Port 1 on loopback: nothing listens, so this must be "unreachable", not a panic.
        assert!(matches!(
            cluster.server_version().await,
            Err(ClusterError::Unreachable(_))
        ));
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(
            KubeCluster::from_kubeconfig(&path).await,
            Err(ClusterError::Config { .. })
        ));
    }
}
