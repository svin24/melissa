#![forbid(unsafe_code)]
//! Cluster layer for Melissa: the lead agent's view of the local k3s cluster.
//!
//! [`cluster`] holds the Kubernetes client (kube-rs over rustls) behind the small
//! [`ClusterRead`] trait, so tools can be tested against a fake. [`ClusterHealth`] is the first
//! cluster tool. The lead agent's credentials come from a kubeconfig for the `melissa-server`
//! ServiceAccount (see `packaging/common/usr/libexec/melissa/melissa-kubeconfig`), loaded lazily by
//! [`LazyCluster`] so the lead agent never waits for k3s; read-only tools use it. App installs go through [`AppDeployer`] over a second, narrower
//! write identity (`melissa-apps`); see [`apps`], [`render`] and the app tools.

mod app_tools;
pub mod apps;
pub mod cluster;
mod device;
mod health;
mod reach;
pub mod render;

pub use app_tools::{app_tools, generate_secret, AppSettings, ForceTiming, Workspaces};
pub use apps::{AppDeployer, AppExec, AppRead, ExecOutput, ExecRequest, KubeApps};
pub use cluster::{
    ClusterError, ClusterRead, KubeCluster, LazyCluster, APPS_RUNTIME_FILE, CREDENTIAL_NAME,
    RUNTIME_DIR, RUNTIME_FILE,
};
pub use health::ClusterHealth;
pub use reach::{PortProbe, Probe, ReachCheck, TcpProbe};
