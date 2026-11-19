#![forbid(unsafe_code)]
//! Hive layer for Melissa: the queen's view of the local k3s cluster.
//!
//! [`cluster`] holds the Kubernetes client (kube-rs over rustls) behind the small
//! [`ClusterRead`] trait, so tools can be tested against a fake. [`ClusterHealth`] is the first
//! cluster tool. The queen's credentials come from a kubeconfig for the `melissa-queen`
//! ServiceAccount (see `packaging/common/usr/libexec/melissa/melissa-kubeconfig`), loaded lazily by
//! [`LazyCluster`] so the queen never waits for k3s; read-only tools use it. App installs go through [`AppDeployer`] over a second, narrower
//! write identity (`melissa-apps`); see [`apps`], [`render`] and the app tools.

mod app_tools;
pub mod apps;
pub mod cluster;
mod device;
mod health;
pub mod render;

pub use app_tools::{app_tools, generate_secret, AppSettings, Cells};
pub use apps::{AppDeployer, AppRead, KubeApps};
pub use cluster::{
    ClusterError, ClusterRead, KubeCluster, LazyCluster, APPS_RUNTIME_FILE, CREDENTIAL_NAME,
    RUNTIME_DIR, RUNTIME_FILE,
};
pub use health::ClusterHealth;
