#![forbid(unsafe_code)]
//! Hive layer for Melissa: the queen's view of the local k3s cluster.
//!
//! [`cluster`] holds the Kubernetes client (kube-rs over rustls) behind the small
//! [`ClusterRead`] trait, so tools can be tested against a fake. [`ClusterHealth`] is the first
//! cluster tool. The queen's credentials come from a kubeconfig for the `melissa-queen`
//! ServiceAccount (see `image/rootfs/usr/libexec/melissa/melissa-kubeconfig`); RBAC grants
//! read access only for now. Write operations (installing apps) will get their own trait.

pub mod cluster;
mod health;

pub use cluster::{find_kubeconfig, ClusterError, ClusterRead, KubeCluster, CREDENTIAL_NAME};
pub use health::ClusterHealth;
