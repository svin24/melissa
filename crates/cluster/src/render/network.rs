//! An app's network permissions ([`melissa_appspec::Network`]) as NetworkPolicies, enforced by
//! k3s's built-in policy controller (kube-router). Pure, like the rest of `render`.
//!
//! In the app's own namespace, always the same three objects (so an update overwrites them):
//! - [`ISOLATION_POLICY`]: default deny both ways for every pod; allowed: traffic between the
//!   namespace's own pods, and DNS to kube-dns (kube-system, UDP/TCP 53).
//! - [`PORTS_POLICY`]: people reach the web and public ports, from anywhere (NodePort traffic
//!   arrives from the node, often SNATed; Traefik's pods for the web host). Other ports: never.
//! - [`EGRESS_POLICY`]: what the app may reach besides that: the public internet
//!   (`internet`), private home network ranges (`lan`), and linked apps' pods (`links`).
//!   Never the cluster's pod and service networks, this device's own addresses (the
//!   Kubernetes API, Melissa, other apps' device ports) or the tailnet (100.64.0.0/10).
//!
//! A link also needs the other side's consent: in the target app's namespace, one
//! [`link_policy`] per linking app (`melissa-link-from-<app>`, labelled [`LINK_FROM_LABEL`])
//! lets that app's namespace in on the linked ports. The deployer applies and deletes these
//! when the linking app changes or goes away, and re-applies them when the target is deployed
//! again. Namespaces are matched by `kubernetes.io/metadata.name`, which the API server sets
//! and nobody can change.

use std::{collections::BTreeMap, net::IpAddr};

use k8s_openapi::api::networking::v1::NetworkPolicy;
use melissa_appspec::{namespace_of, AppSpec, Protocol};
use serde_json::{json, Value};

/// Default deny, own namespace, DNS.
pub const ISOLATION_POLICY: &str = "melissa-isolation";
/// People reach the web and public ports.
pub const PORTS_POLICY: &str = "melissa-ports";
/// The app's own grants: internet, home network, links.
pub const EGRESS_POLICY: &str = "melissa-egress";
/// The policies Melissa renders in every app namespace.
pub const OWN_POLICIES: [&str; 3] = [ISOLATION_POLICY, PORTS_POLICY, EGRESS_POLICY];
/// On a target's [`link_policy`]: the app that links to it.
pub const LINK_FROM_LABEL: &str = "melissa.io/link-from";

/// k3s's default pod and service networks (Melissa does not change them).
pub const CLUSTER_CIDRS: [&str; 2] = ["10.42.0.0/16", "10.43.0.0/16"];

/// Not "the internet": private, shared (CGNAT, the tailnet), loopback, link-local,
/// documentation, benchmarking, multicast and reserved ranges.
const NOT_INTERNET_V4: [&str; 12] = [
    "0.0.0.0/8",
    "10.0.0.0/8",
    "100.64.0.0/10",
    "127.0.0.0/8",
    "169.254.0.0/16",
    "172.16.0.0/12",
    "192.0.0.0/24",
    "192.0.2.0/24",
    "192.168.0.0/16",
    "198.18.0.0/15",
    "224.0.0.0/4",
    "240.0.0.0/4",
];
/// The same for IPv6, including the IPv4-mapped and NAT64 ranges (they lead to IPv4
/// addresses, private ones too).
const NOT_INTERNET_V6: [&str; 7] = [
    "::/127",
    "::ffff:0:0/96",
    "64:ff9b::/96",
    "2001:db8::/32",
    "fc00::/7",
    "fe80::/10",
    "ff00::/8",
];
/// The home network: RFC 1918 and IPv6 unique local addresses.
const LAN: [&str; 4] = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "fc00::/7"];

/// A link resolved against the target app's spec: the ports it opens (empty: all).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LinkPorts {
    /// The target app.
    pub app: String,
    /// `(port, protocol)` of the target's container ports; empty: every port.
    pub ports: Vec<(u16, Protocol)>,
}

/// A CIDR as (address bits, prefix length, is IPv6).
fn parse_cidr(cidr: &str) -> Option<(u128, u8, bool)> {
    let (addr, len) = cidr.split_once('/')?;
    let len: u8 = len.parse().ok()?;
    match addr.parse::<IpAddr>().ok()? {
        IpAddr::V4(a) if len <= 32 => Some((u128::from(u32::from(a)), len, false)),
        IpAddr::V6(a) if len <= 128 => Some((u128::from(a), len, true)),
        _ => None,
    }
}

/// `inner` lies inside `outer` (same family, longer or equal prefix, same leading bits).
fn within(inner: &str, outer: &str) -> bool {
    let (Some((a, al, a6)), Some((b, bl, b6))) = (parse_cidr(inner), parse_cidr(outer)) else {
        return false;
    };
    if a6 != b6 || al < bl {
        return false;
    }
    let width = if a6 { 128 } else { 32 };
    let shift = width - u32::from(bl);
    shift >= width || (a >> shift) == (b >> shift)
}

/// `addr/32` or `addr/128`.
pub fn host_cidr(addr: IpAddr) -> String {
    match addr {
        IpAddr::V4(a) => format!("{a}/32"),
        IpAddr::V6(a) => format!("{a}/128"),
    }
}

/// An ipBlock peer for `cidr` without `except` entries outside it (the API refuses those),
/// and without ones another except entry already covers.
fn ip_block(cidr: &str, except: &[String]) -> Value {
    let mut inside: Vec<&String> = except
        .iter()
        .filter(|e| within(e, cidr) && e.as_str() != cidr)
        .collect();
    inside.sort();
    inside.dedup();
    let kept: Vec<&String> = inside
        .iter()
        .filter(|e| !inside.iter().any(|o| o != *e && within(e, o)))
        .copied()
        .collect();
    if kept.is_empty() {
        json!({"ipBlock": {"cidr": cidr}})
    } else {
        json!({"ipBlock": {"cidr": cidr, "except": kept}})
    }
}

fn ports_json(ports: &[(u16, Protocol)]) -> Option<Value> {
    (!ports.is_empty()).then(|| {
        json!(ports
            .iter()
            .map(|(p, proto)| json!({"port": p, "protocol": proto.upper()}))
            .collect::<Vec<_>>())
    })
}

fn rule(peer_key: &str, peers: Vec<Value>, ports: Option<Value>) -> Value {
    let mut r = serde_json::Map::new();
    if !peers.is_empty() {
        r.insert(peer_key.into(), Value::Array(peers));
    }
    if let Some(p) = ports {
        r.insert("ports".into(), p);
    }
    Value::Object(r)
}

fn namespace_peer(app: &str) -> Value {
    json!({"namespaceSelector": {"matchLabels": {
        "kubernetes.io/metadata.name": namespace_of(app),
    }}})
}

fn policy(
    namespace: &str,
    name: &str,
    labels: &BTreeMap<String, String>,
    spec: Value,
) -> NetworkPolicy {
    let v = json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "NetworkPolicy",
        "metadata": {"name": name, "namespace": namespace, "labels": labels},
        "spec": spec,
    });
    // A fixed shape of known fields and string/number values: it always deserializes (the
    // render tests check every variant).
    serde_json::from_value(v).unwrap_or_default()
}

fn base_labels(app: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        (super::APP_LABEL.to_string(), app.to_string()),
        ("app.kubernetes.io/managed-by".into(), "melissa".into()),
    ])
}

/// What an app's egress may never reach besides the fixed ranges: this device's own
/// addresses (`device`) and the cluster's networks.
fn blocked(device: &[IpAddr]) -> Vec<String> {
    CLUSTER_CIDRS
        .iter()
        .map(|c| c.to_string())
        .chain(device.iter().copied().map(host_cidr))
        .collect()
}

/// The egress peers for `internet` (v4 and v6).
pub fn internet_peers(device: &[IpAddr]) -> Vec<Value> {
    let block = blocked(device);
    let v4: Vec<String> = NOT_INTERNET_V4
        .iter()
        .map(|c| c.to_string())
        .chain(block.iter().cloned())
        .collect();
    let v6: Vec<String> = NOT_INTERNET_V6
        .iter()
        .map(|c| c.to_string())
        .chain(block.iter().cloned())
        .collect();
    vec![ip_block("0.0.0.0/0", &v4), ip_block("::/0", &v6)]
}

/// The egress peers for `lan`.
pub fn lan_peers(device: &[IpAddr]) -> Vec<Value> {
    let block = blocked(device);
    LAN.iter().map(|c| ip_block(c, &block)).collect()
}

/// The three policies of app `app`'s own namespace. `links`: its links resolved against the
/// targets' specs (links to apps that are gone are left out by the caller); `device`: this
/// device's own addresses, never reachable.
pub fn app_policies(
    app: &str,
    spec: &AppSpec,
    links: &[LinkPorts],
    device: &[IpAddr],
) -> Vec<NetworkPolicy> {
    let ns = namespace_of(app);
    let labels = base_labels(app);
    let all_pods = json!({});
    let isolation = policy(
        &ns,
        ISOLATION_POLICY,
        &labels,
        json!({
            "podSelector": all_pods,
            "policyTypes": ["Ingress", "Egress"],
            "ingress": [{"from": [{"podSelector": {}}]}],
            "egress": [
                {"to": [{"podSelector": {}}]},
                {
                    "to": [{
                        "namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "kube-system"}},
                        "podSelector": {"matchLabels": {"k8s-app": "kube-dns"}},
                    }],
                    "ports": [{"port": 53, "protocol": "UDP"}, {"port": 53, "protocol": "TCP"}],
                },
            ],
        }),
    );

    let public: Vec<(u16, Protocol)> = spec
        .public_ports()
        .iter()
        .map(|p| (p.port, p.protocol))
        .collect();
    let ingress: Vec<Value> = ports_json(&public)
        .map(|p| vec![rule("from", vec![], Some(p))])
        .unwrap_or_default();
    let ports = policy(
        &ns,
        PORTS_POLICY,
        &labels,
        json!({
            "podSelector": {},
            "policyTypes": ["Ingress"],
            "ingress": ingress,
        }),
    );

    let network = spec.network_or_default();
    let mut egress: Vec<Value> = Vec::new();
    if network.internet {
        egress.push(rule("to", internet_peers(device), None));
    }
    if network.lan {
        egress.push(rule("to", lan_peers(device), None));
    }
    for l in links {
        egress.push(rule(
            "to",
            vec![namespace_peer(&l.app)],
            ports_json(&l.ports),
        ));
    }
    let egress = policy(
        &ns,
        EGRESS_POLICY,
        &labels,
        json!({
            "podSelector": {},
            "policyTypes": ["Egress"],
            "egress": egress,
        }),
    );
    vec![isolation, ports, egress]
}

/// Name of the policy in a target app's namespace that lets app `source` in.
pub fn link_policy_name(source: &str) -> String {
    format!("melissa-link-from-{source}")
}

/// The target side of `source`'s link(s) to `link.app`: ingress from `source`'s namespace on
/// the linked ports (empty: all ports).
pub fn link_policy(source: &str, link: &LinkPorts) -> NetworkPolicy {
    let mut labels = base_labels(&link.app);
    labels.insert(LINK_FROM_LABEL.into(), source.to_string());
    policy(
        &namespace_of(&link.app),
        &link_policy_name(source),
        &labels,
        json!({
            "podSelector": {},
            "policyTypes": ["Ingress"],
            "ingress": [rule("from", vec![namespace_peer(source)], ports_json(&link.ports))],
        }),
    )
}

/// Merge links to the same app: the union of their ports, or all ports if any link has all.
pub fn merge_links(links: Vec<LinkPorts>) -> Vec<LinkPorts> {
    let mut by_app: BTreeMap<String, Option<Vec<(u16, Protocol)>>> = BTreeMap::new();
    for l in links {
        let entry = by_app.entry(l.app).or_insert_with(|| Some(Vec::new()));
        match (entry.as_mut(), l.ports.is_empty()) {
            (Some(_), true) => *entry = None,
            (Some(ports), false) => ports.extend(l.ports),
            (None, _) => {}
        }
    }
    by_app
        .into_iter()
        .map(|(app, ports)| {
            let mut ports = ports.unwrap_or_default();
            ports.sort();
            ports.dedup();
            LinkPorts { app, ports }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cidr_containment() {
        assert!(within("10.42.0.0/16", "10.0.0.0/8"));
        assert!(!within("10.0.0.0/8", "10.42.0.0/16"));
        assert!(within("192.168.1.10/32", "192.168.0.0/16"));
        assert!(!within("192.168.1.10/32", "10.0.0.0/8"));
        assert!(within("fd00::1/128", "fc00::/7"));
        assert!(!within("fd00::1/128", "10.0.0.0/8"));
        assert!(within("203.0.113.5/32", "0.0.0.0/0"));
        assert!(within("2001:db8::/32", "::/0"));
    }

    #[test]
    fn except_entries_stay_inside_their_block_and_are_not_repeated() {
        let except: Vec<String> = ["10.42.0.0/16", "10.0.0.0/8", "192.168.1.10/32"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            ip_block("10.0.0.0/8", &except),
            json!({"ipBlock": {"cidr": "10.0.0.0/8", "except": ["10.42.0.0/16"]}})
        );
        assert_eq!(
            ip_block("0.0.0.0/0", &except),
            json!({"ipBlock": {"cidr": "0.0.0.0/0", "except": ["10.0.0.0/8", "192.168.1.10/32"]}})
        );
        assert_eq!(
            ip_block("172.16.0.0/12", &except),
            json!({"ipBlock": {"cidr": "172.16.0.0/12"}})
        );
    }

    #[test]
    fn links_to_one_app_merge() {
        let l = |app: &str, ports: &[u16]| LinkPorts {
            app: app.into(),
            ports: ports.iter().map(|p| (*p, Protocol::Tcp)).collect(),
        };
        assert_eq!(
            merge_links(vec![
                l("pg", &[5432]),
                l("pg", &[9187]),
                l("redis", &[6379])
            ]),
            vec![l("pg", &[5432, 9187]), l("redis", &[6379])]
        );
        assert_eq!(
            merge_links(vec![l("pg", &[5432]), l("pg", &[])]),
            vec![l("pg", &[])]
        );
    }
}
