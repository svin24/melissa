//! `cluster_health`: is k3s up, are the nodes ready, which pods are unhappy.

use std::{collections::BTreeMap, sync::Arc};

use k8s_openapi::api::core::v1::{Node, Pod};
use melissa_agent::{
    tools::{Labels, Tool, ToolError, ToolSpec},
    BoxFuture,
};
use serde_json::{json, Map, Value};

use crate::{ClusterError, ClusterRead};

/// Most problem pods listed in one answer; the rest are only counted.
const MAX_PROBLEM_PODS: usize = 50;

/// `cluster_health` tool. Read-only; works (and says so) when there is no cluster.
#[derive(Clone)]
pub struct ClusterHealth {
    cluster: Option<Arc<dyn ClusterRead>>,
}

impl ClusterHealth {
    /// `None` = no kubeconfig on this device (desktop, or k3s credentials missing).
    pub fn new(cluster: Option<Arc<dyn ClusterRead>>) -> Self {
        Self { cluster }
    }

    async fn report(&self) -> Result<Value, ToolError> {
        let Some(cluster) = &self.cluster else {
            return Ok(crate::cluster::unavailable(&ClusterError::NotConfigured));
        };
        // The version endpoint is public to any authenticated client; use it as the probe.
        let version = match cluster.server_version().await {
            Ok(v) => v,
            Err(
                e @ (ClusterError::Unreachable(_)
                | ClusterError::NotConfigured
                | ClusterError::Starting),
            ) => return Ok(crate::cluster::unavailable(&e)),
            Err(e) => return Err(ToolError::Failed(e.to_string())),
        };
        // k3s restarts itself now and then (e.g. when the node's IPs change at boot), so the
        // API can vanish between the probe and the lists: that is "unreachable", not a failure.
        let lists = tokio::try_join!(cluster.nodes(), cluster.namespaces(), cluster.pods());
        let (nodes, namespaces, pods) = match lists {
            Ok(l) => l,
            Err(e @ ClusterError::Unreachable(_)) => return Ok(crate::cluster::unavailable(&e)),
            Err(e) => return Err(ToolError::Failed(e.to_string())),
        };

        // Every namespace appears, even empty ones (a freshly created app namespace).
        let mut by_ns: BTreeMap<String, BTreeMap<String, u64>> = namespaces
            .iter()
            .filter_map(|n| n.metadata.name.clone())
            .map(|n| (n, BTreeMap::new()))
            .collect();
        let mut problems = Vec::new();
        let mut problem_count = 0usize;
        for pod in &pods {
            let ns = pod.metadata.namespace.clone().unwrap_or_default();
            let phase = pod_phase(pod);
            *by_ns
                .entry(ns)
                .or_default()
                .entry(phase.clone())
                .or_default() += 1;
            if let Some(p) = problem(pod, &phase) {
                problem_count += 1;
                if problems.len() < MAX_PROBLEM_PODS {
                    problems.push(p);
                }
            }
        }

        Ok(json!({
            "k3s_reachable": true,
            "server_version": version,
            "nodes": nodes.iter().map(node_summary).collect::<Vec<_>>(),
            "pods": {
                "total": pods.len(),
                "by_namespace": by_ns,
            },
            "problem_pods": problems,
            "problem_pods_total": problem_count,
        }))
    }
}

impl Tool for ClusterHealth {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "cluster_health",
            description:
                "Health of this cluster's k3s cluster: whether the API is reachable, server \
                version, nodes (ready, roles, kubelet version, allocatable CPU/memory), pod counts \
                by phase per namespace, and pods that are not Running/Succeeded with the reason. \
                When k3s can't be used, `app_platform` says why in words for the user.",
            // Only names, counts and kubelet-set reason codes are returned (sanitized below);
            // no free-text messages an app could write, so this stays untainted.
            labels: Labels::READ_ONLY,
            params: json!({"type": "object", "properties": {}, "additionalProperties": false}),
        }
    }

    fn call<'a>(&'a self, _args: Value) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(self.report())
    }
}

fn pod_phase(pod: &Pod) -> String {
    pod.status
        .as_ref()
        .and_then(|s| s.phase.clone())
        .unwrap_or_else(|| "Unknown".into())
}

fn node_summary(node: &Node) -> Value {
    let status = node.status.as_ref();
    let ready = status
        .and_then(|s| s.conditions.as_ref())
        .and_then(|c| c.iter().find(|c| c.type_ == "Ready"))
        .is_some_and(|c| c.status == "True");
    let roles: Vec<&str> = node
        .metadata
        .labels
        .iter()
        .flatten()
        .filter_map(|(k, _)| k.strip_prefix("node-role.kubernetes.io/"))
        .collect();
    let allocatable = status.and_then(|s| s.allocatable.as_ref());
    let quantity = |key: &str| allocatable.and_then(|a| a.get(key)).map(|q| q.0.clone());
    json!({
        "name": node.metadata.name,
        "ready": ready,
        "roles": roles,
        "kubelet_version": status.and_then(|s| s.node_info.as_ref()).map(|i| &i.kubelet_version),
        "allocatable": {"cpu": quantity("cpu"), "memory": quantity("memory")},
    })
}

/// A pod worth reporting: not Running/Succeeded, or Running with a container stuck waiting
/// (e.g. CrashLoopBackOff). Returns `None` for healthy pods.
fn problem(pod: &Pod, phase: &str) -> Option<Value> {
    let status = pod.status.as_ref();
    let containers = status
        .into_iter()
        .flat_map(|s| {
            s.init_container_statuses
                .iter()
                .chain(&s.container_statuses)
        })
        .flatten();
    let mut waiting = None;
    let mut terminated = None;
    for c in containers {
        if let Some(state) = &c.state {
            if let Some(w) = state.waiting.as_ref().and_then(|w| w.reason.as_deref()) {
                waiting.get_or_insert(w);
            }
            if let Some(t) = state.terminated.as_ref().and_then(|t| t.reason.as_deref()) {
                terminated.get_or_insert(t);
            }
        }
    }
    let healthy_phase = phase == "Running" || phase == "Succeeded";
    if healthy_phase && waiting.is_none() {
        return None;
    }
    let unschedulable = status
        .and_then(|s| s.conditions.as_ref())
        .and_then(|c| {
            c.iter()
                .find(|c| c.type_ == "PodScheduled" && c.status == "False")
        })
        .and_then(|c| c.reason.as_deref());
    let reason = status
        .and_then(|s| s.reason.as_deref())
        .or(waiting)
        .or(terminated)
        .or(unschedulable)
        .map(reason_code);
    let mut out = Map::new();
    out.insert("namespace".into(), json!(pod.metadata.namespace));
    out.insert("name".into(), json!(pod.metadata.name));
    out.insert("phase".into(), json!(phase));
    out.insert("reason".into(), json!(reason));
    Some(Value::Object(out))
}

/// Reason fields are short CamelCase codes set by the kubelet/scheduler (`CrashLoopBackOff`,
/// `ErrImagePull`, `Evicted`). Keep only that shape so no free text reaches the agent.
pub(crate) fn reason_code(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(64)
        .collect()
}

#[cfg(test)]
mod tests {
    use k8s_openapi::{
        api::core::v1::{
            ContainerState, ContainerStateWaiting, ContainerStatus, Namespace, NodeCondition,
            NodeStatus, NodeSystemInfo, PodCondition, PodStatus,
        },
        apimachinery::pkg::api::resource::Quantity,
    };
    use kube::api::ObjectMeta;

    use super::*;

    #[derive(Default)]
    struct Fake {
        version: Option<Result<String, ClusterError>>,
        nodes: Vec<Node>,
        namespaces: Vec<Namespace>,
        pods: Vec<Pod>,
        pods_error: Option<ClusterError>,
    }

    impl ClusterRead for Fake {
        fn server_version(&self) -> BoxFuture<'_, Result<String, ClusterError>> {
            let v = self.version.clone().unwrap_or(Ok("v1.37.2+k3s1".into()));
            Box::pin(async move { v })
        }
        fn nodes(&self) -> BoxFuture<'_, Result<Vec<Node>, ClusterError>> {
            Box::pin(async move { Ok(self.nodes.clone()) })
        }
        fn namespaces(&self) -> BoxFuture<'_, Result<Vec<Namespace>, ClusterError>> {
            Box::pin(async move { Ok(self.namespaces.clone()) })
        }
        fn pods(&self) -> BoxFuture<'_, Result<Vec<Pod>, ClusterError>> {
            Box::pin(async move {
                match &self.pods_error {
                    Some(e) => Err(e.clone()),
                    None => Ok(self.pods.clone()),
                }
            })
        }
    }

    fn meta(ns: Option<&str>, name: &str) -> ObjectMeta {
        ObjectMeta {
            name: Some(name.into()),
            namespace: ns.map(Into::into),
            ..ObjectMeta::default()
        }
    }

    fn node(name: &str, ready: &str) -> Node {
        let mut m = meta(None, name);
        m.labels = Some(
            [
                (
                    "node-role.kubernetes.io/control-plane".to_string(),
                    "true".to_string(),
                ),
                ("kubernetes.io/hostname".to_string(), name.to_string()),
            ]
            .into(),
        );
        Node {
            metadata: m,
            spec: None,
            status: Some(NodeStatus {
                conditions: Some(vec![NodeCondition {
                    type_: "Ready".into(),
                    status: ready.into(),
                    ..NodeCondition::default()
                }]),
                node_info: Some(NodeSystemInfo {
                    kubelet_version: "v1.37.2+k3s1".into(),
                    ..NodeSystemInfo::default()
                }),
                allocatable: Some(
                    [
                        ("cpu".to_string(), Quantity("2".into())),
                        ("memory".to_string(), Quantity("2867000Ki".into())),
                    ]
                    .into(),
                ),
                ..NodeStatus::default()
            }),
        }
    }

    fn pod(ns: &str, name: &str, phase: &str, waiting: Option<&str>) -> Pod {
        Pod {
            metadata: meta(Some(ns), name),
            spec: None,
            status: Some(PodStatus {
                phase: Some(phase.into()),
                container_statuses: waiting.map(|r| {
                    vec![ContainerStatus {
                        name: "main".into(),
                        state: Some(ContainerState {
                            waiting: Some(ContainerStateWaiting {
                                reason: Some(r.into()),
                                message: Some("free text an app could influence".into()),
                            }),
                            ..ContainerState::default()
                        }),
                        ..ContainerStatus::default()
                    }]
                }),
                ..PodStatus::default()
            }),
        }
    }

    fn namespace(name: &str) -> Namespace {
        Namespace {
            metadata: meta(None, name),
            ..Namespace::default()
        }
    }

    async fn call(fake: Fake) -> Result<Value, ToolError> {
        ClusterHealth::new(Some(Arc::new(fake)))
            .call(json!({}))
            .await
    }

    #[tokio::test]
    async fn no_cluster_says_k3s_is_not_running() {
        let out = ClusterHealth::new(None).call(Value::Null).await.unwrap();
        assert_eq!(out["k3s_reachable"], false);
        assert!(out["reason"].as_str().unwrap().contains("not set up"));
        assert_eq!(out["k3s_set_up"], false);
        assert_eq!(out["app_platform"], crate::cluster::NOT_INSTALLED);
    }

    #[tokio::test]
    async fn missing_or_pending_credentials_are_results() {
        for (err, needle) in [
            (ClusterError::NotConfigured, "not set up"),
            (ClusterError::Starting, "starting"),
        ] {
            let out = call(Fake {
                version: Some(Err(err)),
                ..Fake::default()
            })
            .await
            .unwrap();
            assert_eq!(out["k3s_reachable"], false);
            assert!(out["reason"].as_str().unwrap().contains(needle), "{out}");
            // Only missing credentials mean "not set up"; starting is not.
            assert_eq!(
                out.get("k3s_set_up").is_some(),
                needle == "not set up",
                "{out}"
            );
            let words = if needle == "not set up" {
                crate::cluster::NOT_INSTALLED
            } else {
                crate::cluster::NOT_ANSWERING
            };
            assert_eq!(out["app_platform"], words, "{out}");
        }
    }

    #[tokio::test]
    async fn unreachable_api_is_a_result_not_an_error() {
        let out = call(Fake {
            version: Some(Err(ClusterError::Unreachable("connection refused".into()))),
            ..Fake::default()
        })
        .await
        .unwrap();
        assert_eq!(out["k3s_reachable"], false);
        assert!(out["reason"]
            .as_str()
            .unwrap()
            .contains("connection refused"));
        assert_eq!(out["app_platform"], crate::cluster::NOT_ANSWERING);
        assert!(out.get("k3s_set_up").is_none(), "{out}");
    }

    #[tokio::test]
    async fn rbac_failures_are_tool_errors_and_a_vanished_api_is_unreachable() {
        let with = |e| Fake {
            pods_error: Some(e),
            ..Fake::default()
        };
        let err = call(with(ClusterError::Forbidden("pods is forbidden".into())))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Failed(m) if m.contains("forbidden")));
        let out = call(with(ClusterError::Unreachable("connection reset".into())))
            .await
            .unwrap();
        assert_eq!(out["k3s_reachable"], false);
    }

    #[tokio::test]
    async fn summarizes_nodes_and_pods() {
        let out = call(Fake {
            nodes: vec![node("agent-1", "True"), node("agent-2", "Unknown")],
            namespaces: vec![namespace("kube-system"), namespace("empty-app")],
            pods: vec![
                pod("kube-system", "coredns", "Running", None),
                pod("kube-system", "helm-install-traefik", "Succeeded", None),
                pod("kube-system", "traefik", "Running", None),
                pod("app", "crashy", "Running", Some("CrashLoopBackOff")),
                pod("app", "pending", "Pending", Some("ErrImagePull <script>")),
            ],
            ..Fake::default()
        })
        .await
        .unwrap();
        assert_eq!(out["k3s_reachable"], true);
        assert_eq!(out["server_version"], "v1.37.2+k3s1");

        let n = &out["nodes"][0];
        assert_eq!(n["name"], "agent-1");
        assert_eq!(n["ready"], true);
        assert_eq!(n["roles"], json!(["control-plane"]));
        assert_eq!(n["kubelet_version"], "v1.37.2+k3s1");
        assert_eq!(n["allocatable"], json!({"cpu": "2", "memory": "2867000Ki"}));
        assert_eq!(out["nodes"][1]["ready"], false);

        assert_eq!(out["pods"]["total"], 5);
        assert_eq!(
            out["pods"]["by_namespace"]["kube-system"],
            json!({"Running": 2, "Succeeded": 1})
        );
        assert_eq!(
            out["pods"]["by_namespace"]["app"],
            json!({"Running": 1, "Pending": 1})
        );
        assert_eq!(out["pods"]["by_namespace"]["empty-app"], json!({}));

        assert_eq!(out["problem_pods_total"], 2);
        let problems = out["problem_pods"].as_array().unwrap();
        assert_eq!(
            problems[0],
            json!({"namespace": "app", "name": "crashy", "phase": "Running", "reason": "CrashLoopBackOff"})
        );
        // Sanitized to a reason code; container messages never appear.
        assert_eq!(problems[1]["reason"], "ErrImagePullscript");
        assert!(!out.to_string().contains("free text"));
    }

    #[tokio::test]
    async fn unschedulable_pods_report_the_scheduler_reason() {
        let mut p = pod("app", "big", "Pending", None);
        if let Some(s) = p.status.as_mut() {
            s.conditions = Some(vec![PodCondition {
                type_: "PodScheduled".into(),
                status: "False".into(),
                reason: Some("Unschedulable".into()),
                ..PodCondition::default()
            }]);
        }
        let out = call(Fake {
            pods: vec![p],
            ..Fake::default()
        })
        .await
        .unwrap();
        assert_eq!(out["problem_pods"][0]["reason"], "Unschedulable");
    }

    #[tokio::test]
    async fn problem_list_is_capped() {
        let pods = (0..MAX_PROBLEM_PODS + 5)
            .map(|i| pod("app", &format!("p{i}"), "Failed", None))
            .collect();
        let out = call(Fake {
            pods,
            ..Fake::default()
        })
        .await
        .unwrap();
        assert_eq!(
            out["problem_pods"].as_array().unwrap().len(),
            MAX_PROBLEM_PODS
        );
        assert_eq!(out["problem_pods_total"], MAX_PROBLEM_PODS + 5);
    }

    #[test]
    fn spec_is_read_only() {
        assert_eq!(ClusterHealth::new(None).spec().labels, Labels::READ_ONLY);
    }
}
