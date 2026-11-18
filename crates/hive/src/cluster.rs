//! Kubernetes access for the queen.
//!
//! The queen never uses k3s's admin kubeconfig. It gets a kubeconfig for the `melissa-queen`
//! ServiceAccount as a systemd credential (`ImportCredential=melissa.kubeconfig`), or from a
//! path in its config. Without one, the cluster is "not configured" and cluster tools say so.

use std::path::{Path, PathBuf};

use k8s_openapi::api::core::v1::{Namespace, Node, Pod};
use kube::{
    api::{Api, ListParams},
    config::{KubeConfigOptions, Kubeconfig},
    Client, Config,
};
use melissa_agent::BoxFuture;

/// Name of the systemd credential (and of its file in `/run/credstore`) holding the kubeconfig.
pub const CREDENTIAL_NAME: &str = "melissa.kubeconfig";

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ClusterError {
    /// No kubeconfig: k3s is not enabled on this bee (desktop) or its credentials are missing.
    #[error("k3s is not running on this bee (no cluster credentials)")]
    NotConfigured,
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

/// Where the queen finds its kubeconfig: the systemd credential if present, else the
/// configured path if it exists, else nowhere.
pub fn find_kubeconfig(
    credentials_dir: Option<&Path>,
    configured: Option<&Path>,
) -> Option<PathBuf> {
    credentials_dir
        .map(|d| d.join(CREDENTIAL_NAME))
        .filter(|p| p.is_file())
        .or_else(|| configured.filter(|p| p.is_file()).map(Path::to_path_buf))
}

fn map_err(e: kube::Error) -> ClusterError {
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

    #[test]
    fn credential_wins_over_configured_path() {
        let dir = std::env::temp_dir().join(format!("melissa-hive-cred-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let configured = dir.join("configured.yaml");
        std::fs::write(&configured, "x").unwrap();

        assert_eq!(
            find_kubeconfig(Some(&dir), Some(&configured)),
            Some(configured.clone())
        );
        std::fs::write(dir.join(CREDENTIAL_NAME), "x").unwrap();
        assert_eq!(
            find_kubeconfig(Some(&dir), Some(&configured)),
            Some(dir.join(CREDENTIAL_NAME))
        );
        assert_eq!(find_kubeconfig(None, Some(&dir.join("missing"))), None);
        assert_eq!(find_kubeconfig(None, None), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn loads_a_token_kubeconfig_without_contacting_the_server() {
        let path =
            std::env::temp_dir().join(format!("melissa-hive-kubeconfig-{}", std::process::id()));
        std::fs::write(
            &path,
            r#"apiVersion: v1
kind: Config
clusters:
- name: local
  cluster: {server: "https://127.0.0.1:1", insecure-skip-tls-verify: true}
users:
- name: queen
  user: {token: placeholder}
contexts:
- name: local
  context: {cluster: local, user: queen}
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
