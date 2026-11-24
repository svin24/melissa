//! App spec → Kubernetes objects. Pure: no I/O, no randomness (secret values are generated
//! by the deployer and only placed into [`secret_object`]).
//!
//! Per app: Namespace `app-<name>` (labelled `melissa.io/app=<name>`, Pod Security
//! `baseline` enforced, the spec stored in [`SPEC_ANNOTATION`]), a RoleBinding that gives the
//! `melissa-apps` ServiceAccount the `melissa-app-workload` role in that namespace only, one
//! Secret per generated secret, one PVC per volume (`local-path`), a single-replica Deployment
//! with the `Recreate` strategy (never privileged, seccomp `RuntimeDefault`, no privilege
//! escalation, limits always set), a ClusterIP Service and, for an app with a `web` port, a
//! Traefik Ingress for `<name>.<domain>`. Ports on the device (the web port and `public`
//! ports, TCP or UDP) are one NodePort Service `<name>-port` with a stable port each from
//! [`APP_PORTS`], recorded on the namespace ([`PORT_ANNOTATION`] for the web port,
//! [`PORTS_ANNOTATION`] for the others), so the app is reachable at `<device address>:<port>`
//! without any DNS. NetworkPolicies isolate the app and grant what its spec's `network`
//! allows ([`network`]).

use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
};

use k8s_openapi::{
    api::{
        apps::v1::{Deployment, DeploymentSpec, DeploymentStrategy},
        core::v1::{
            ConfigMap, ConfigMapVolumeSource, Container, ContainerPort, EnvVar, EnvVarSource,
            HTTPGetAction, HTTPHeader, KeyToPath, Namespace, PersistentVolumeClaim,
            PersistentVolumeClaimSpec, PersistentVolumeClaimVolumeSource, PodSecurityContext,
            PodSpec, PodTemplateSpec, Probe, ResourceRequirements, SeccompProfile, Secret,
            SecretKeySelector, SecurityContext, Service, ServicePort, ServiceSpec, TCPSocketAction,
            Volume, VolumeMount, VolumeResourceRequirements,
        },
        networking::v1::{
            HTTPIngressPath, HTTPIngressRuleValue, Ingress, IngressBackend, IngressRule,
            IngressServiceBackend, IngressSpec, NetworkPolicy, ServiceBackendPort,
        },
        rbac::v1::{RoleBinding, RoleRef, Subject},
    },
    apimachinery::pkg::{
        api::resource::Quantity, apis::meta::v1::LabelSelector, util::intstr::IntOrString,
    },
};
use kube::api::ObjectMeta;
use melissa_agent::{migrate, tools::Owner};
use melissa_appspec::{
    namespace_of, AppSpec, Builtin, EnvSource, Protocol, Resources, FILES_VOLUME,
};
use sha2::{Digest, Sha256};

pub mod network;

use network::LinkPorts;

/// Label keys from before the rename to generic terms (docs/DECISIONS.md, 2026-11-19), with
/// old ids (`c-<hex>`, `c-<hex>.b<n>`). Read as aliases of the new keys until [`relabel_patch`]
/// has moved a namespace over (the server does that once the cluster is up); never written.
pub const OLD_WORKSPACE_LABEL: &str = "melissa.io/cell";
pub const OLD_DEVICE_LABEL: &str = "melissa.io/comb";
pub const OLD_OWNER_LABEL: &str = "melissa.io/owner-bee";

/// Label on every object of an app (and the namespace): the app's name.
pub const APP_LABEL: &str = "melissa.io/app";
/// Field manager for server-side apply.
pub const FIELD_MANAGER: &str = "melissa";
/// The write identity (see `packaging/common/usr/lib/melissa/k3s/manifests/apps-rbac.yaml`).
pub const APPS_SERVICE_ACCOUNT: &str = "melissa-apps";
pub const MELISSA_NAMESPACE: &str = "melissa";
/// ClusterRole bound per app namespace for [`APPS_SERVICE_ACCOUNT`].
pub const WORKLOAD_ROLE: &str = "melissa-app-workload";
/// The setup password reader (`credentials-rbac.yaml`): only `get` on Secrets, bound per app
/// namespace by [`APPS_SERVICE_ACCOUNT`] as [`CREDENTIALS_BINDING`] ([`credentials_binding`]).
pub const CREDENTIALS_SERVICE_ACCOUNT: &str = "melissa-credentials";
pub const CREDENTIALS_ROLE: &str = "melissa-app-credentials";
pub const CREDENTIALS_BINDING: &str = "melissa-app-credentials";
/// Annotation on the namespace: when the owner dismissed the app's setup password (RFC 3339,
/// UTC). Never removed or changed, and while it is there the admission policy refuses
/// [`CREDENTIALS_BINDING`], so Melissa can't read that password again.
pub const CREDENTIALS_DISMISSED_ANNOTATION: &str = "melissa.io/credentials-dismissed";
/// Workspace labels on the app's namespace (docs/AGENT-MODEL.md): the workspace's name, the device it is
/// on and the agent that owns it. `melissa.io/workspace` and `melissa.io/device` are set at the first
/// install; `melissa.io/owner-agent` is set at the first install (absent: unassigned) and
/// changed only by the reassign path together with fresh `assigned-*` annotations (admission
/// policy `melissa-apps-namespaces`).
pub const WORKSPACE_LABEL: &str = "melissa.io/workspace";
pub const DEVICE_LABEL: &str = "melissa.io/device";
pub const OWNER_AGENT_LABEL: &str = "melissa.io/owner-agent";
/// Who gave the workspace to its agent (`the lead agent, approved by the user`, `the user`, `agent Ada
/// (…) (task 7)` for a workspace an agent created) and when (RFC 3339, sub-second).
pub const ASSIGNED_BY_ANNOTATION: &str = "melissa.io/assigned-by";
pub const ASSIGNED_AT_ANNOTATION: &str = "melissa.io/assigned-at";
/// The owner label from before agents (`owner` or `job-<key>`). Read for migration only: such
/// a workspace counts as unassigned and loses the label at its next install or reassignment.
pub const LEGACY_OWNER_LABEL: &str = "melissa.io/owner";
/// Annotations on the app's namespace, set at the first install and kept: who installed it
/// (`the user`, `the lead agent (task 3)`, `agent Ada (d-….a1) (task 7)`), when (RFC 3339), and
/// from where (`agent spec`; `catalog` for apps from before the catalog was dropped).
pub const INSTALLED_BY_ANNOTATION: &str = "melissa.io/installed-by";
pub const INSTALLED_AT_ANNOTATION: &str = "melissa.io/installed-at";
/// Annotation on the app's namespace, set at the first install and kept: the signed-in person
/// (account id) the app was installed for, i.e. whose message started the turn that installed
/// it, or who called the tool in the UI (2026-11-22). Absent on apps from before, and when
/// nobody's message started the turn (a scheduled job): the UI then falls back to the person
/// the looking-after agent was made for, else "unknown".
pub const INSTALLED_FOR_ANNOTATION: &str = "melissa.io/installed-for";
pub const ORIGIN_ANNOTATION: &str = "melissa.io/origin";
/// [`ORIGIN_ANNOTATION`] of apps deployed from an agent's spec, and of old catalog installs.
pub const ORIGIN_SPEC: &str = "spec";
pub const ORIGIN_CATALOG: &str = "catalog";
/// Annotation on the namespace: the app's spec (compact JSON), as last deployed. Kept when
/// the app is uninstalled with its data, so `deploy_app` can bring it back.
pub const SPEC_ANNOTATION: &str = "melissa.io/spec";
/// Annotation on the namespace: the web port's NodePort (from [`APP_PORTS`]). Set when the
/// app first gets a web port and never changed while the namespace exists (admission policy
/// `melissa-apps-namespaces`), so links stay valid.
pub const PORT_ANNOTATION: &str = "melissa.io/port";
/// Annotation on the namespace: the other public ports, `name:nodeport/proto` separated by
/// spaces (`game:30001/tcp voice:30002/udp`). A port keeps its NodePort across updates.
pub const PORTS_ANNOTATION: &str = "melissa.io/ports";
/// Annotation on the namespace: secret fields kept in the old shared Secret `<app>-secrets`
/// (key = field name), space separated. Catalog installs only; everything else has one
/// Secret per field ([`secret_object_name`]).
pub const SHARED_SECRETS_ANNOTATION: &str = "melissa.io/shared-secrets";
/// Ports Melissa gives apps. Inside k3s's NodePort range (30000-32767) and in the lower
/// band (30000-30085) that Kubernetes keeps for static allocations, so automatically
/// allocated NodePorts (e.g. Traefik's) avoid it. The admission policy
/// `melissa-apps-services` allows NodePorts only here.
pub const APP_PORTS: std::ops::RangeInclusive<u16> = 30000..=30079;
/// Annotation on a stopped app's Deployment: the replica count `start_app` restores.
pub const REPLICAS_BEFORE_STOP_ANNOTATION: &str = "melissa.io/replicas-before-stop";
/// Annotation on Deployments from the catalog days: the resolved, non-secret config as JSON.
/// Read once, to rebuild such an app's spec ([`melissa_appspec::legacy`]); never written.
pub const CONFIG_ANNOTATION: &str = "melissa.io/config";
/// Pod Security Standard enforced in app namespaces. `baseline` forbids privileged pods,
/// host namespaces, hostPath volumes, host ports and added capabilities beyond a small
/// default set, so the write identity can't reach the node.
pub const POD_SECURITY_LEVEL: &str = "baseline";
pub const STORAGE_CLASS: &str = "local-path";
pub const INGRESS_CLASS: &str = "traefik";

/// Everything one deploy applies, in apply order.
#[derive(Debug, Clone, PartialEq)]
pub struct AppObjects {
    pub namespace: Namespace,
    pub role_binding: RoleBinding,
    /// Generated secrets. Each is created once and never overwritten.
    pub secrets: Vec<SecretPlan>,
    pub volumes: Vec<PersistentVolumeClaim>,
    pub deployment: Deployment,
    pub service: Option<Service>,
    /// The spec's files (`<name>-files`), if any container has some.
    pub files: Option<ConfigMap>,
    /// NodePort Service for the app's ports on the device (web and public ports that have a
    /// NodePort in [`Placement::node_ports`]).
    pub port_service: Option<Service>,
    pub ingress: Option<Ingress>,
    /// The app's own NetworkPolicies ([`network::app_policies`]), applied before its pods.
    pub network_policies: Vec<NetworkPolicy>,
}

/// Where and how an app runs, besides its spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement<'a> {
    /// Ingress domain (`<name>.<domain>`).
    pub domain: &'a str,
    /// Port name → its NodePort ([`APP_PORTS`]). Public ports without one get none.
    pub node_ports: &'a BTreeMap<String, u16>,
    /// The web NodePort recorded on the namespace, kept there even if the app has no web
    /// port any more (the record never changes while the namespace exists).
    pub recorded_web_port: Option<u16>,
    /// The app is stopped: render 0 replicas and remember this count for `start_app`.
    pub stopped_from: Option<i32>,
    /// The device's addresses and host name (builtin `device_hosts`, after the ingress host).
    pub hosts: &'a [String],
    /// Secret fields kept in the shared `<app>-secrets` Secret (catalog installs).
    pub shared_secrets: &'a BTreeSet<String>,
    /// The spec's links, resolved against the other apps' specs (see [`network`]).
    pub links: &'a [LinkPorts],
    /// This device's own addresses: no app's egress reaches them.
    pub device_addresses: &'a [IpAddr],
}

static NO_PORTS: BTreeMap<String, u16> = BTreeMap::new();
static NO_SECRETS: BTreeSet<String> = BTreeSet::new();

impl<'a> Placement<'a> {
    /// Running, without device ports.
    pub fn domain(domain: &'a str) -> Self {
        Self {
            domain,
            node_ports: &NO_PORTS,
            recorded_web_port: None,
            stopped_from: None,
            hosts: &[],
            shared_secrets: &NO_SECRETS,
            links: &[],
            device_addresses: &[],
        }
    }
}

/// Name of the ConfigMap holding app `app`'s files.
pub fn files_name(app: &str) -> String {
    format!("{app}-files")
}

/// Annotation on the pod template: hash of the spec's files, so a changed file restarts the
/// pod (`subPath` mounts never see ConfigMap updates).
pub const FILES_HASH_ANNOTATION: &str = "melissa.io/files-sha256";

/// Name of the NodePort Service of app `app`.
pub fn port_service_name(app: &str) -> String {
    format!("{app}-port")
}

/// The Secret object that holds secret field `field` of app `app`: its own
/// `<app>-secret-<field>` (`_` → `-`), or the shared `<app>-secrets` of a catalog install.
pub fn secret_object_name(app: &str, field: &str, shared: &BTreeSet<String>) -> String {
    if shared.contains(field) {
        melissa_appspec::secret_name(app)
    } else {
        format!("{app}-secret-{}", field.replace('_', "-"))
    }
}

/// One generated Secret: what [`secret_object`] needs besides the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretPlan {
    pub namespace: String,
    /// Object name ([`secret_object_name`]).
    pub name: String,
    /// The key (the field name).
    pub key: String,
    pub length: usize,
    pub labels: BTreeMap<String, String>,
}

/// `<nodeport>/<proto>` records: parse [`PORTS_ANNOTATION`] (`name:30001/tcp ...`). Odd
/// entries are skipped.
pub fn parse_ports_annotation(value: &str) -> BTreeMap<String, (u16, Protocol)> {
    value
        .split_whitespace()
        .filter_map(|entry| {
            let (name, rest) = entry.split_once(':')?;
            let (port, proto) = rest.split_once('/')?;
            let port: u16 = port.parse().ok().filter(|p| APP_PORTS.contains(p))?;
            let proto = match proto {
                "tcp" => Protocol::Tcp,
                "udp" => Protocol::Udp,
                _ => return None,
            };
            Some((name.to_string(), (port, proto)))
        })
        .collect()
}

fn labels(app: &str, spec: &AppSpec) -> BTreeMap<String, String> {
    let mut l = BTreeMap::from([
        (APP_LABEL.into(), app.to_string()),
        ("app.kubernetes.io/name".into(), app.to_string()),
        ("app.kubernetes.io/managed-by".into(), "melissa".into()),
    ]);
    if let Some(v) = spec
        .version
        .as_deref()
        .map(label_value)
        .filter(|v| !v.is_empty())
    {
        l.insert("app.kubernetes.io/version".into(), v);
    }
    l
}

/// Label values: at most 63 of `[A-Za-z0-9._-]`, alphanumeric at both ends.
fn label_value(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(63)
        .collect();
    cleaned
        .trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_string()
}

fn meta(app: &str, spec: &AppSpec, name: &str) -> ObjectMeta {
    ObjectMeta {
        name: Some(name.into()),
        namespace: Some(namespace_of(app)),
        labels: Some(labels(app, spec)),
        ..ObjectMeta::default()
    }
}

/// The RoleBinding that lets the setup password reader ([`CREDENTIALS_SERVICE_ACCOUNT`]) get
/// app `app`'s Secrets. Applied by the write identity right before a reveal, deleted when the
/// owner dismisses the password; never part of a deploy.
pub fn credentials_binding(app: &str) -> RoleBinding {
    RoleBinding {
        metadata: ObjectMeta {
            name: Some(CREDENTIALS_BINDING.into()),
            namespace: Some(namespace_of(app)),
            labels: Some(BTreeMap::from([
                (APP_LABEL.into(), app.to_string()),
                ("app.kubernetes.io/managed-by".into(), "melissa".into()),
            ])),
            ..ObjectMeta::default()
        },
        role_ref: RoleRef {
            api_group: Some("rbac.authorization.k8s.io".into()),
            kind: "ClusterRole".into(),
            name: CREDENTIALS_ROLE.into(),
        },
        subjects: Some(vec![Subject {
            kind: "ServiceAccount".into(),
            name: CREDENTIALS_SERVICE_ACCOUNT.into(),
            namespace: Some(MELISSA_NAMESPACE.into()),
            api_group: None,
        }]),
    }
}

/// A workspace's record on its namespace: which device and agent, and how it got here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ownership {
    /// The device id (None on a namespace from before devices had ids; set at the next install).
    pub device: Option<String>,
    /// The owning agent (`Unassigned`: no `melissa.io/owner-agent` label).
    pub owner: Owner,
    pub assigned_by: Option<String>,
    pub assigned_at: Option<String>,
    pub installed_by: String,
    pub installed_at: String,
    /// [`INSTALLED_FOR_ANNOTATION`], if set and well formed.
    pub installed_for: Option<String>,
    pub origin: String,
    /// `melissa.io/owner` from before agents, if still there.
    pub legacy_owner: Option<String>,
}

impl Ownership {
    /// The record on an existing namespace. Namespaces from before agents have no owner-agent
    /// label: they are unassigned (only the lead agent with the user's approval, or the user,
    /// may change them); unknown annotations stay `unknown`.
    pub fn of(ns: &Namespace) -> Self {
        let label = |k: &str| ns.metadata.labels.as_ref().and_then(|l| l.get(k)).cloned();
        let ann = |k: &str| {
            ns.metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get(k))
                .cloned()
        };
        let unknown = |v: Option<String>| v.unwrap_or_else(|| "unknown".into());
        let device = label(DEVICE_LABEL)
            .or_else(|| label(OLD_DEVICE_LABEL).and_then(|v| migrate::new_device_id(&v)));
        let owner = label(OWNER_AGENT_LABEL)
            .or_else(|| label(OLD_OWNER_LABEL).and_then(|v| migrate::new_agent_id(&v)));
        Self {
            device,
            owner: Owner::from_label(owner.as_deref()),
            assigned_by: ann(ASSIGNED_BY_ANNOTATION),
            assigned_at: ann(ASSIGNED_AT_ANNOTATION),
            installed_by: unknown(ann(INSTALLED_BY_ANNOTATION)),
            installed_at: unknown(ann(INSTALLED_AT_ANNOTATION)),
            installed_for: ann(INSTALLED_FOR_ANNOTATION)
                .filter(|p| melissa_appspec::valid_person_id(p)),
            origin: unknown(ann(ORIGIN_ANNOTATION)),
            legacy_owner: label(LEGACY_OWNER_LABEL),
        }
    }

    /// Put the record on a rendered namespace for workspace `workspace`. The legacy owner label is not
    /// written again, so a reinstall migrates the namespace to the workspace labels.
    pub fn apply_to(&self, workspace: &str, ns: &mut Namespace) {
        let labels = ns.metadata.labels.get_or_insert_with(BTreeMap::new);
        labels.insert(WORKSPACE_LABEL.into(), workspace.into());
        if let Some(device) = &self.device {
            labels.insert(DEVICE_LABEL.into(), device.clone());
        }
        if let Some(agent) = self.owner.label_value() {
            labels.insert(OWNER_AGENT_LABEL.into(), agent.into());
        }
        let annotations = ns.metadata.annotations.get_or_insert_with(BTreeMap::new);
        if let (Some(by), Some(at)) = (&self.assigned_by, &self.assigned_at) {
            annotations.insert(ASSIGNED_BY_ANNOTATION.into(), by.clone());
            annotations.insert(ASSIGNED_AT_ANNOTATION.into(), at.clone());
        }
        annotations.insert(INSTALLED_BY_ANNOTATION.into(), self.installed_by.clone());
        annotations.insert(INSTALLED_AT_ANNOTATION.into(), self.installed_at.clone());
        if let Some(person) = self
            .installed_for
            .as_ref()
            .filter(|p| melissa_appspec::valid_person_id(p))
        {
            annotations.insert(INSTALLED_FOR_ANNOTATION.into(), person.clone());
        }
        annotations.insert(ORIGIN_ANNOTATION.into(), self.origin.clone());
    }
}

/// The merge patch that gives workspace `workspace` on device `device` to agent `agent` (the reassign path):
/// new owner label, fresh `assigned-*` annotations (the admission policy requires them for
/// an owner change), and the legacy owner label removed.
pub fn assign_patch(
    workspace: &str,
    device: &str,
    agent: &str,
    by: &str,
    at: &str,
) -> serde_json::Value {
    serde_json::json!({
        "metadata": {
            "labels": {
                WORKSPACE_LABEL: workspace,
                DEVICE_LABEL: device,
                OWNER_AGENT_LABEL: agent,
                LEGACY_OWNER_LABEL: null,
                OLD_WORKSPACE_LABEL: null,
                OLD_DEVICE_LABEL: null,
                OLD_OWNER_LABEL: null,
            },
            "annotations": {
                ASSIGNED_BY_ANNOTATION: by,
                ASSIGNED_AT_ANNOTATION: at,
            },
        }
    })
}

/// Maps an old label value to the new one (None: not a valid old value).
type IdMap = fn(&str) -> Option<String>;

/// The merge patch that moves a namespace from the old label keys to the new ones (same
/// values, ids in the new shape), or None if it has no old keys. An old key whose value is not
/// a valid old id is kept (the admission policy would refuse dropping it without its
/// replacement); a new key that is already there wins and the old one is just dropped.
pub fn relabel_patch(ns: &Namespace) -> Option<serde_json::Value> {
    let labels = ns.metadata.labels.as_ref()?;
    let mut patch = serde_json::Map::new();
    let keys: [(&str, &str, IdMap); 3] = [
        (OLD_WORKSPACE_LABEL, WORKSPACE_LABEL, |v| {
            Some(v.to_string())
        }),
        (OLD_DEVICE_LABEL, DEVICE_LABEL, migrate::new_device_id),
        (OLD_OWNER_LABEL, OWNER_AGENT_LABEL, migrate::new_agent_id),
    ];
    for (old, new, map) in keys {
        let Some(value) = labels.get(old) else {
            continue;
        };
        if !labels.contains_key(new) {
            let Some(mapped) = map(value) else {
                continue;
            };
            patch.insert(new.into(), mapped.into());
        }
        patch.insert(old.into(), serde_json::Value::Null);
    }
    (!patch.is_empty()).then(|| serde_json::json!({ "metadata": { "labels": patch } }))
}

/// Render app `app` from `spec` for `placement`. `spec` must be validated.
pub fn render(app: &str, spec: &AppSpec, placement: &Placement<'_>) -> AppObjects {
    let ns = namespace_of(app);
    let host = format!("{app}.{}", placement.domain);
    let selector = BTreeMap::from([(APP_LABEL.to_string(), app.to_string())]);

    let ns_labels = BTreeMap::from([
        (APP_LABEL.to_string(), app.to_string()),
        ("melissa.io/managed".into(), "true".into()),
        (
            "pod-security.kubernetes.io/enforce".into(),
            POD_SECURITY_LEVEL.into(),
        ),
        (
            "pod-security.kubernetes.io/enforce-version".into(),
            "latest".into(),
        ),
        ("app.kubernetes.io/managed-by".into(), "melissa".into()),
    ]);
    let mut ns_annotations = BTreeMap::from([(SPEC_ANNOTATION.to_string(), spec.to_text())]);
    let web_port = spec
        .web
        .as_ref()
        .and_then(|w| placement.node_ports.get(w).copied())
        .or(placement.recorded_web_port);
    if let Some(p) = web_port {
        ns_annotations.insert(PORT_ANNOTATION.into(), p.to_string());
    }
    let others: Vec<String> = spec
        .public_ports()
        .into_iter()
        .filter(|p| Some(p.name.as_str()) != spec.web.as_deref())
        .filter_map(|p| {
            let np = placement.node_ports.get(&p.name)?;
            Some(format!("{}:{np}/{}", p.name, p.protocol.lower()))
        })
        .collect();
    if !others.is_empty() {
        ns_annotations.insert(PORTS_ANNOTATION.into(), others.join(" "));
    }
    if !placement.shared_secrets.is_empty() {
        ns_annotations.insert(
            SHARED_SECRETS_ANNOTATION.into(),
            placement
                .shared_secrets
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    let namespace = Namespace {
        metadata: ObjectMeta {
            name: Some(ns.clone()),
            labels: Some(ns_labels),
            annotations: Some(ns_annotations),
            ..ObjectMeta::default()
        },
        ..Namespace::default()
    };

    let role_binding = RoleBinding {
        metadata: meta(app, spec, APPS_SERVICE_ACCOUNT),
        role_ref: RoleRef {
            api_group: Some("rbac.authorization.k8s.io".into()),
            kind: "ClusterRole".into(),
            name: WORKLOAD_ROLE.into(),
        },
        subjects: Some(vec![Subject {
            kind: "ServiceAccount".into(),
            name: APPS_SERVICE_ACCOUNT.into(),
            namespace: Some(MELISSA_NAMESPACE.into()),
            api_group: None,
        }]),
    };

    let secrets = spec
        .secret_lengths()
        .into_iter()
        .map(|(key, length)| SecretPlan {
            namespace: ns.clone(),
            name: secret_object_name(app, &key, placement.shared_secrets),
            key,
            length,
            labels: labels(app, spec),
        })
        .collect();

    let volumes = spec
        .volumes
        .iter()
        .map(|v| PersistentVolumeClaim {
            metadata: meta(app, spec, &pvc_name(app, &v.name)),
            spec: Some(PersistentVolumeClaimSpec {
                access_modes: Some(vec!["ReadWriteOnce".into()]),
                storage_class_name: Some(STORAGE_CLASS.into()),
                resources: Some(VolumeResourceRequirements {
                    requests: Some(BTreeMap::from([(
                        "storage".into(),
                        Quantity(v.size.clone()),
                    )])),
                    ..VolumeResourceRequirements::default()
                }),
                ..PersistentVolumeClaimSpec::default()
            }),
            ..PersistentVolumeClaim::default()
        })
        .collect();

    let containers = spec
        .containers
        .iter()
        .map(|c| container(app, c, &host, placement))
        .collect();
    let mut pod_volumes: Vec<Volume> = spec
        .volumes
        .iter()
        .map(|v| Volume {
            name: v.name.clone(),
            persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                claim_name: pvc_name(app, &v.name),
                read_only: None,
            }),
            ..Volume::default()
        })
        .collect();
    // Files: one ConfigMap, one volume, a read-only subPath mount per file.
    let file_entries: Vec<(String, &melissa_appspec::File)> = spec
        .containers
        .iter()
        .flat_map(|c| {
            c.files
                .iter()
                .enumerate()
                .map(move |(i, f)| (file_key(&c.name, i), f))
        })
        .collect();
    let files = (!file_entries.is_empty()).then(|| ConfigMap {
        metadata: meta(app, spec, &files_name(app)),
        data: Some(
            file_entries
                .iter()
                .map(|(k, f)| (k.clone(), f.content.clone()))
                .collect(),
        ),
        ..ConfigMap::default()
    });
    let files_hash = (!file_entries.is_empty()).then(|| {
        let mut h = Sha256::new();
        for (k, f) in &file_entries {
            h.update(k.as_bytes());
            h.update([0]);
            h.update(f.content.as_bytes());
            h.update([0, u8::from(f.executable)]);
        }
        h.finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    });
    if !file_entries.is_empty() {
        pod_volumes.push(Volume {
            name: FILES_VOLUME.into(),
            config_map: Some(ConfigMapVolumeSource {
                name: files_name(app),
                default_mode: Some(0o444),
                items: Some(
                    file_entries
                        .iter()
                        .map(|(k, f)| KeyToPath {
                            key: k.clone(),
                            path: k.clone(),
                            mode: Some(if f.executable { 0o555 } else { 0o444 }),
                            ..KeyToPath::default()
                        })
                        .collect(),
                ),
                optional: Some(false),
                ..ConfigMapVolumeSource::default()
            }),
            ..Volume::default()
        });
    }

    let mut deploy_meta = meta(app, spec, app);
    if let Some(n) = placement.stopped_from {
        deploy_meta.annotations = Some(BTreeMap::from([(
            REPLICAS_BEFORE_STOP_ANNOTATION.to_string(),
            n.to_string(),
        )]));
    }
    let deployment = Deployment {
        metadata: deploy_meta,
        spec: DeploymentSpec {
            replicas: Some(if placement.stopped_from.is_some() {
                0
            } else {
                1
            }),
            revision_history_limit: Some(3),
            // One pod owns the ReadWriteOnce volumes; never run old and new at once.
            strategy: Some(DeploymentStrategy {
                type_: Some("Recreate".into()),
                rolling_update: None,
            }),
            selector: LabelSelector {
                match_labels: Some(selector.clone()),
                match_expressions: None,
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels(app, spec)),
                    annotations: files_hash
                        .map(|h| BTreeMap::from([(FILES_HASH_ANNOTATION.to_string(), h)])),
                    ..ObjectMeta::default()
                }),
                spec: Some(PodSpec {
                    containers,
                    volumes: (!pod_volumes.is_empty()).then_some(pod_volumes),
                    // Apps never talk to the Kubernetes API.
                    automount_service_account_token: Some(false),
                    // No `<SERVICE>_PORT=tcp://...` variables colliding with app settings.
                    enable_service_links: Some(false),
                    security_context: Some(PodSecurityContext {
                        seccomp_profile: Some(SeccompProfile {
                            type_: "RuntimeDefault".into(),
                            localhost_profile: None,
                        }),
                        ..PodSecurityContext::default()
                    }),
                    ..PodSpec::default()
                }),
            },
            ..DeploymentSpec::default()
        },
        ..Deployment::default()
    };

    let service_port = |p: &melissa_appspec::Port, node_port: Option<u16>| ServicePort {
        name: Some(p.name.clone()),
        port: i32::from(p.port),
        target_port: Some(IntOrString::String(p.name.clone())),
        node_port: node_port.map(i32::from),
        protocol: Some(p.protocol.upper().into()),
        ..ServicePort::default()
    };
    let ports: Vec<ServicePort> = spec.ports().map(|p| service_port(p, None)).collect();
    let service = (!ports.is_empty()).then(|| Service {
        metadata: meta(app, spec, app),
        spec: Some(ServiceSpec {
            type_: Some("ClusterIP".into()),
            selector: Some(selector.clone()),
            ports: Some(ports),
            ..ServiceSpec::default()
        }),
        ..Service::default()
    });

    // Ports on the device: the web port and public ports, each on its own NodePort.
    let node_ports: Vec<ServicePort> = spec
        .public_ports()
        .into_iter()
        .filter_map(|p| {
            let np = placement.node_ports.get(&p.name)?;
            Some(service_port(p, Some(*np)))
        })
        .collect();
    let port_service = (!node_ports.is_empty()).then(|| Service {
        metadata: meta(app, spec, &port_service_name(app)),
        spec: Some(ServiceSpec {
            type_: Some("NodePort".into()),
            selector: Some(selector.clone()),
            ports: Some(node_ports),
            ..ServiceSpec::default()
        }),
        ..Service::default()
    });

    let ingress = spec.web.as_ref().map(|web| Ingress {
        metadata: meta(app, spec, app),
        spec: Some(IngressSpec {
            ingress_class_name: Some(INGRESS_CLASS.into()),
            rules: Some(vec![IngressRule {
                host: Some(host.clone()),
                http: Some(HTTPIngressRuleValue {
                    paths: vec![HTTPIngressPath {
                        path: Some("/".into()),
                        path_type: "Prefix".into(),
                        backend: IngressBackend {
                            service: Some(IngressServiceBackend {
                                name: app.to_string(),
                                port: Some(ServiceBackendPort {
                                    name: Some(web.clone()),
                                    number: None,
                                }),
                            }),
                            resource: None,
                        },
                    }],
                }),
            }]),
            ..IngressSpec::default()
        }),
        ..Ingress::default()
    });

    AppObjects {
        namespace,
        role_binding,
        secrets,
        volumes,
        deployment,
        service,
        files,
        port_service,
        ingress,
        network_policies: network::app_policies(
            app,
            spec,
            placement.links,
            placement.device_addresses,
        ),
    }
}

/// ConfigMap key (and file name in the volume) of file `i` of container `container`.
fn file_key(container: &str, i: usize) -> String {
    format!("{container}.{i}")
}

/// PVC name for volume `volume` of app `app`.
pub fn pvc_name(app: &str, volume: &str) -> String {
    format!("{app}-{volume}")
}

fn container(
    app: &str,
    c: &melissa_appspec::Container,
    host: &str,
    placement: &Placement<'_>,
) -> Container {
    let env: Vec<EnvVar> = c
        .env
        .iter()
        .filter_map(|(name, value)| {
            // Validation guarantees exactly one source; skip defensively otherwise.
            let source = value.source()?;
            let (value, value_from) = match source {
                EnvSource::Value(v) => (Some(v.into_owned()), None),
                EnvSource::Builtin(Builtin::IngressHost) => (Some(host.to_string()), None),
                EnvSource::Builtin(Builtin::IngressUrl) => (Some(format!("http://{host}")), None),
                EnvSource::Builtin(Builtin::DeviceHosts) => (
                    Some(
                        std::iter::once(host)
                            .chain(
                                placement
                                    .hosts
                                    .iter()
                                    .map(String::as_str)
                                    .filter(|h| *h != host),
                            )
                            .collect::<Vec<_>>()
                            .join(" "),
                    ),
                    None,
                ),
                EnvSource::Secret(f) => (
                    None,
                    Some(EnvVarSource {
                        secret_key_ref: Some(SecretKeySelector {
                            name: secret_object_name(app, f, placement.shared_secrets),
                            key: f.to_string(),
                            optional: Some(false),
                        }),
                        ..EnvVarSource::default()
                    }),
                ),
            };
            Some(EnvVar {
                name: name.clone(),
                value,
                value_from,
            })
        })
        .collect();
    let r = Resources::resolved(c.resources.as_ref());
    let quantities = |cpu: &str, mem: &str| {
        BTreeMap::from([
            ("cpu".to_string(), Quantity(cpu.into())),
            ("memory".to_string(), Quantity(mem.into())),
        ])
    };
    let probe = c.probe.as_ref().map(|p| {
        let port = IntOrString::String(p.port.clone());
        let (http_get, tcp_socket) = match &p.path {
            Some(path) => (
                Some(HTTPGetAction {
                    path: Some(path.clone()),
                    port,
                    http_headers: p.host_header.then(|| {
                        vec![HTTPHeader {
                            name: "Host".into(),
                            value: host.to_string(),
                        }]
                    }),
                    ..HTTPGetAction::default()
                }),
                None,
            ),
            None => (None, Some(TCPSocketAction { port, host: None })),
        };
        let period = i32::try_from(p.period_secs).unwrap_or(10);
        let startup_failures =
            i32::try_from(p.startup_timeout_secs.div_ceil(p.period_secs.max(1))).unwrap_or(30);
        let base = Probe {
            http_get,
            tcp_socket,
            timeout_seconds: Some(5),
            ..Probe::default()
        };
        (
            // Startup: give the first start (installers, migrations) its full time.
            Probe {
                period_seconds: Some(period),
                failure_threshold: Some(startup_failures.max(1)),
                ..base.clone()
            },
            Probe {
                period_seconds: Some(period),
                failure_threshold: Some(3),
                ..base.clone()
            },
            // Liveness: restart only after a sustained failure (about two minutes).
            Probe {
                period_seconds: Some(30),
                timeout_seconds: Some(10),
                failure_threshold: Some(4),
                ..base
            },
        )
    });
    let (startup_probe, readiness_probe, liveness_probe) = match probe {
        Some((s, r, l)) => (Some(s), Some(r), Some(l)),
        None => (None, None, None),
    };
    let list = |v: &[String]| (!v.is_empty()).then(|| v.to_vec());
    Container {
        name: c.name.clone(),
        image: Some(c.image.clone()),
        image_pull_policy: Some("IfNotPresent".into()),
        command: list(&c.command),
        args: list(&c.args),
        ports: (!c.ports.is_empty()).then(|| {
            c.ports
                .iter()
                .map(|p| ContainerPort {
                    name: Some(p.name.clone()),
                    container_port: i32::from(p.port),
                    protocol: Some(p.protocol.upper().into()),
                    ..ContainerPort::default()
                })
                .collect()
        }),
        env: (!env.is_empty()).then_some(env),
        volume_mounts: (!c.mounts.is_empty() || !c.files.is_empty()).then(|| {
            c.mounts
                .iter()
                .map(|mnt| VolumeMount {
                    name: mnt.volume.clone(),
                    mount_path: mnt.path.clone(),
                    ..VolumeMount::default()
                })
                .chain(c.files.iter().enumerate().map(|(i, f)| VolumeMount {
                    name: FILES_VOLUME.into(),
                    mount_path: f.path.clone(),
                    sub_path: Some(file_key(&c.name, i)),
                    read_only: Some(true),
                    ..VolumeMount::default()
                }))
                .collect()
        }),
        resources: Some(ResourceRequirements {
            requests: Some(quantities(&r.cpu_request, &r.memory_request)),
            limits: Some(quantities(&r.cpu_limit, &r.memory_limit)),
            ..ResourceRequirements::default()
        }),
        // Root inside the container is allowed (many images need it to start); privileged,
        // extra capabilities and gaining privileges (setuid) are not.
        security_context: Some(SecurityContext {
            privileged: Some(false),
            allow_privilege_escalation: Some(false),
            run_as_user: c.run_as_user,
            run_as_group: c.run_as_group,
            ..SecurityContext::default()
        }),
        startup_probe,
        readiness_probe,
        liveness_probe,
        ..Container::default()
    }
}

/// The Secret for `plan` with its generated `value`.
pub fn secret_object(plan: &SecretPlan, value: String) -> Secret {
    Secret {
        metadata: ObjectMeta {
            name: Some(plan.name.clone()),
            namespace: Some(plan.namespace.clone()),
            labels: Some(plan.labels.clone()),
            ..ObjectMeta::default()
        },
        type_: Some("Opaque".into()),
        string_data: Some(BTreeMap::from([(plan.key.clone(), value)])),
        ..Secret::default()
    }
}

#[cfg(test)]
mod network_tests;
#[cfg(test)]
mod tests;
