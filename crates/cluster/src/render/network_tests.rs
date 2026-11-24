//! The NetworkPolicies of each `network` combination, and a link's target side.

use std::net::IpAddr;

use serde_json::{json, Value};

use super::{network::LinkPorts, *};

fn policies_of(spec: &AppSpec, links: &[LinkPorts], device: &[IpAddr]) -> Value {
    let o = render(
        "web",
        spec,
        &Placement {
            links,
            device_addresses: device,
            ..Placement::domain("melissa.home.arpa")
        },
    );
    serde_json::to_value(&o.network_policies).unwrap()
}

fn with_network(network: Value) -> AppSpec {
    let mut v = json!({
        "description": "A web app",
        "containers": [{
            "name": "web",
            "image": "nginx:1.27",
            "ports": [
                {"name": "http", "port": 8080},
                {"name": "admin", "port": 9000},
                {"name": "dns", "port": 5353, "protocol": "udp", "public": true},
            ],
        }],
        "web": "http",
    });
    if !network.is_null() {
        v["network"] = network;
    }
    AppSpec::from_json(&v).unwrap()
}

fn egress_of(policies: &Value) -> &Value {
    &policies[2]["spec"]["egress"]
}

fn ips(list: &[&str]) -> Vec<IpAddr> {
    list.iter().map(|s| s.parse().unwrap()).collect()
}

#[test]
fn every_app_is_isolated_with_dns_and_its_public_ports_open() {
    let p = policies_of(&with_network(Value::Null), &[], &[]);
    let names: Vec<&str> = p
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["metadata"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, network::OWN_POLICIES);
    for x in p.as_array().unwrap() {
        assert_eq!(x["metadata"]["namespace"], "app-web");
        assert_eq!(x["spec"]["podSelector"], json!({}));
    }
    // Default deny both ways; the namespace's own pods and DNS only.
    assert_eq!(p[0]["spec"]["policyTypes"], json!(["Ingress", "Egress"]));
    assert_eq!(
        p[0]["spec"]["ingress"],
        json!([{"from": [{"podSelector": {}}]}])
    );
    assert_eq!(
        p[0]["spec"]["egress"],
        json!([
            {"to": [{"podSelector": {}}]},
            {
                "to": [{
                    "namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "kube-system"}},
                    "podSelector": {"matchLabels": {"k8s-app": "kube-dns"}},
                }],
                "ports": [{"port": 53, "protocol": "UDP"}, {"port": 53, "protocol": "TCP"}],
            },
        ])
    );
    // The web port and public ports from anywhere (NodePort traffic comes from the node);
    // `admin` (not public) never.
    assert_eq!(
        p[1]["spec"]["ingress"],
        json!([{"ports": [{"port": 8080, "protocol": "TCP"}, {"port": 5353, "protocol": "UDP"}]}])
    );
    // Nothing else out.
    assert_eq!(p[2]["spec"]["policyTypes"], json!(["Egress"]));
    assert_eq!(egress_of(&p), &json!([]));
}

#[test]
fn an_app_without_public_ports_takes_no_connections_from_outside() {
    let spec = AppSpec::from_json(&json!({
        "description": "A database",
        "containers": [{"name": "db", "image": "postgres:17.2", "ports": [{"name": "db", "port": 5432}]}],
    }))
    .unwrap();
    let p = policies_of(&spec, &[], &[]);
    assert_eq!(p[1]["spec"]["ingress"], json!([]));
}

#[test]
fn internet_is_everything_but_private_cluster_and_device_addresses() {
    let device = ips(&["192.168.1.10", "198.51.100.7", "fd00::5", "2001:db8:1::5"]);
    let p = policies_of(&with_network(json!({"internet": true})), &[], &device);
    let e = egress_of(&p);
    assert_eq!(e.as_array().unwrap().len(), 1);
    let v4 = &e[0]["to"][0]["ipBlock"];
    assert_eq!(v4["cidr"], "0.0.0.0/0");
    let except: Vec<&str> = v4["except"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    for range in [
        "10.0.0.0/8",
        "172.16.0.0/12",
        "192.168.0.0/16",
        "100.64.0.0/10",
        "127.0.0.0/8",
        "169.254.0.0/16",
        // The device's own public address: the Kubernetes API and Melissa listen there.
        "198.51.100.7/32",
    ] {
        assert!(except.contains(&range), "{range} missing from {except:?}");
    }
    // Covered by 10/8 and 192.168/16 already: not repeated.
    assert!(!except.contains(&"10.42.0.0/16"));
    assert!(!except.contains(&"192.168.1.10/32"));
    let v6 = &e[0]["to"][1]["ipBlock"];
    assert_eq!(v6["cidr"], "::/0");
    let except6 = v6["except"].as_array().unwrap();
    for range in ["fc00::/7", "fe80::/10", "64:ff9b::/96"] {
        assert!(except6.contains(&json!(range)), "{range}");
    }
    // Inside excepted ranges already.
    assert!(!except6.contains(&json!("2001:db8:1::5/128")));
    assert!(!except6.contains(&json!("fd00::5/128")));
    // No ports: any port.
    assert!(e[0].get("ports").is_none());
}

#[test]
fn lan_is_the_private_ranges_without_the_cluster_and_this_device() {
    let device = ips(&["192.168.1.10", "fd00::5"]);
    let p = policies_of(&with_network(json!({"lan": true})), &[], &device);
    assert_eq!(
        egress_of(&p),
        &json!([{"to": [
            {"ipBlock": {"cidr": "10.0.0.0/8", "except": ["10.42.0.0/16", "10.43.0.0/16"]}},
            {"ipBlock": {"cidr": "172.16.0.0/12"}},
            {"ipBlock": {"cidr": "192.168.0.0/16", "except": ["192.168.1.10/32"]}},
            {"ipBlock": {"cidr": "fc00::/7", "except": ["fd00::5/128"]}},
        ]}])
    );
    // Never the tailnet.
    assert!(!p.to_string().contains("100.64.0.0/10"));
}

#[test]
fn internet_lan_and_links_add_up() {
    let spec = with_network(json!({
        "internet": true, "lan": true,
        "links": [{"app": "postgres", "port": "db"}, {"app": "mqtt"}],
    }));
    let links = [
        LinkPorts {
            app: "mqtt".into(),
            ports: vec![],
        },
        LinkPorts {
            app: "postgres".into(),
            ports: vec![(5432, Protocol::Tcp)],
        },
    ];
    let p = policies_of(&spec, &links, &[]);
    let e = egress_of(&p).as_array().unwrap();
    assert_eq!(e.len(), 4);
    assert_eq!(e[0]["to"][0]["ipBlock"]["cidr"], "0.0.0.0/0");
    assert_eq!(e[1]["to"][0]["ipBlock"]["cidr"], "10.0.0.0/8");
    assert_eq!(
        e[2],
        json!({"to": [{"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "app-mqtt"}}}]})
    );
    assert_eq!(
        e[3],
        json!({
            "to": [{"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "app-postgres"}}}],
            "ports": [{"port": 5432, "protocol": "TCP"}],
        })
    );
    // Links are rendered only as resolved by the deployer: none given, none rendered.
    let p = policies_of(&spec, &[], &[]);
    assert_eq!(egress_of(&p).as_array().unwrap().len(), 2);
}

#[test]
fn a_links_target_side_lets_only_the_source_in_on_the_linked_ports() {
    let p = network::link_policy(
        "vuls-web",
        &LinkPorts {
            app: "postgres".into(),
            ports: vec![(5432, Protocol::Tcp)],
        },
    );
    assert_eq!(
        serde_json::to_value(&p).unwrap(),
        json!({
            "apiVersion": "networking.k8s.io/v1",
            "kind": "NetworkPolicy",
            "metadata": {
                "name": "melissa-link-from-vuls-web",
                "namespace": "app-postgres",
                "labels": {
                    "app.kubernetes.io/managed-by": "melissa",
                    "melissa.io/app": "postgres",
                    "melissa.io/link-from": "vuls-web",
                },
            },
            "spec": {
                "podSelector": {},
                "policyTypes": ["Ingress"],
                "ingress": [{
                    "from": [{"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "app-vuls-web"}}}],
                    "ports": [{"port": 5432, "protocol": "TCP"}],
                }],
            },
        })
    );
    // All ports: no `ports`.
    let p = network::link_policy(
        "vuls-web",
        &LinkPorts {
            app: "mqtt".into(),
            ports: vec![],
        },
    );
    let v = serde_json::to_value(&p).unwrap();
    assert!(v["spec"]["ingress"][0].get("ports").is_none());
}
