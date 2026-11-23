use std::path::Path;

use melissa_appspec::legacy;
use serde_json::{json, Map, Value};

use super::*;

fn nextcloud() -> AppSpec {
    legacy::nextcloud(&Map::new())
}

fn minecraft() -> AppSpec {
    AppSpec::from_json(&json!({
        "description": "Minecraft Java server",
        "containers": [{
            "name": "server",
            "image": "itzg/minecraft-server:java21",
            "env": {"EULA": "TRUE", "MEMORY": "1G"},
            "ports": [
                {"name": "game", "port": 25565, "public": true},
                {"name": "bedrock", "port": 19132, "protocol": "udp", "public": true},
                {"name": "rcon", "port": 25575},
            ],
            "mounts": [{"volume": "data", "path": "/data"}],
            "resources": {"cpu": "2", "memory": "1536Mi"},
            "probe": {"port": "game"},
            "run_as_user": 1000,
        }],
        "volumes": [{"name": "data", "size": "5Gi"}],
        "secrets": [{"name": "rcon_password"}],
    }))
    .unwrap()
}

/// All objects as one JSON document (what the golden file holds).
fn as_json(o: &AppObjects) -> Value {
    json!({
        "namespace": o.namespace,
        "role_binding": o.role_binding,
        "secrets": o.secrets.iter().map(|p| json!({
            "namespace": p.namespace, "name": p.name, "key": p.key, "length": p.length,
            "labels": p.labels,
        })).collect::<Vec<_>>(),
        "volumes": o.volumes,
        "deployment": o.deployment,
        "service": o.service,
        "files": o.files,
        "port_service": o.port_service,
        "ingress": o.ingress,
    })
}

/// Compare with `tests/golden/<name>.json`; `MELISSA_UPDATE_GOLDEN=1` rewrites it.
fn golden(name: &str, actual: &Value) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.json"));
    let text = serde_json::to_string_pretty(actual).unwrap() + "\n";
    if std::env::var_os("MELISSA_UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &text).unwrap();
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing {}; run with MELISSA_UPDATE_GOLDEN=1",
            path.display()
        )
    });
    assert_eq!(text, expected, "golden {name} differs; review and update");
}

fn shared(keys: &[&str]) -> BTreeSet<String> {
    keys.iter().map(|k| k.to_string()).collect()
}

#[test]
fn nextcloud_renders_to_the_golden_objects() {
    let ports = BTreeMap::from([("http".to_string(), 30000)]);
    let shared = shared(&["admin_password"]);
    golden(
        "nextcloud",
        &as_json(&render(
            "nextcloud",
            &nextcloud(),
            &Placement {
                node_ports: &ports,
                hosts: &["192.0.2.15".to_string(), "melissa".to_string()],
                shared_secrets: &shared,
                ..Placement::domain("melissa.home.arpa")
            },
        )),
    );
}

#[test]
fn minecraft_renders_to_the_golden_objects() {
    let ports = BTreeMap::from([("game".to_string(), 30001), ("bedrock".to_string(), 30002)]);
    golden(
        "minecraft",
        &as_json(&render(
            "minecraft",
            &minecraft(),
            &Placement {
                node_ports: &ports,
                ..Placement::domain("melissa.home.arpa")
            },
        )),
    );
}

/// A converted catalog install renders the objects the catalog did: same volume, Secret and
/// key, files, Service ports and device port, so an update keeps its data and password.
#[test]
fn a_converted_catalog_install_keeps_its_objects() {
    let ports = BTreeMap::from([("http".to_string(), 30000)]);
    let shared = shared(&["admin_password"]);
    let o = render(
        "nextcloud",
        &nextcloud(),
        &Placement {
            node_ports: &ports,
            shared_secrets: &shared,
            ..Placement::domain("x.test")
        },
    );
    assert_eq!(
        o.volumes[0].metadata.name.as_deref(),
        Some("nextcloud-data")
    );
    assert_eq!(o.secrets.len(), 1);
    assert_eq!(
        (o.secrets[0].name.as_str(), o.secrets[0].key.as_str()),
        ("nextcloud-secrets", "admin_password")
    );
    let pod = o.deployment.spec.template.spec.as_ref().unwrap();
    let env = pod.containers[0].env.as_ref().unwrap();
    let pw = env
        .iter()
        .find(|e| e.name == "NEXTCLOUD_ADMIN_PASSWORD")
        .unwrap();
    let key = pw
        .value_from
        .as_ref()
        .unwrap()
        .secret_key_ref
        .as_ref()
        .unwrap();
    assert_eq!(
        (key.name.as_str(), key.key.as_str()),
        ("nextcloud-secrets", "admin_password")
    );
    assert_eq!(
        o.files.unwrap().metadata.name.as_deref(),
        Some("nextcloud-files")
    );
    let ns = o.namespace.metadata.annotations.unwrap();
    assert_eq!(ns[PORT_ANNOTATION], "30000");
    assert_eq!(ns[SHARED_SECRETS_ANNOTATION], "admin_password");
    assert_eq!(
        AppSpec::from_text(&ns[SPEC_ANNOTATION]).unwrap(),
        nextcloud()
    );
    // The old golden file's files hash: the hook is byte for byte the catalog's.
    let hash = o
        .deployment
        .spec
        .template
        .metadata
        .unwrap()
        .annotations
        .unwrap()[FILES_HASH_ANNOTATION]
        .clone();
    assert_eq!(
        hash,
        "3cc528c204f35a34aec5a1f1bf808a942e074ac03bdac24367683f02d6945d28"
    );
}

#[test]
fn isolation_picks_the_runtime_class_and_labels_pod_and_namespace() {
    let gvisor = |o: &AppObjects| {
        let template = &o.deployment.spec.template;
        (
            template.spec.as_ref().unwrap().runtime_class_name.clone(),
            template.metadata.as_ref().unwrap().labels.as_ref().unwrap()[ISOLATION_LABEL].clone(),
            o.namespace.metadata.labels.as_ref().unwrap()[ISOLATION_LABEL].clone(),
        )
    };
    // No `isolation` in the spec: the default, gVisor.
    let mc = minecraft();
    assert_eq!(mc.isolation, None);
    let o = render("minecraft", &mc, &Placement::domain("x.test"));
    assert_eq!(
        gvisor(&o),
        (Some("gvisor".into()), "gvisor".into(), "gvisor".into())
    );
    // The selector stays the app label alone (a level change is an update, not a new pod set).
    assert_eq!(
        o.deployment
            .spec
            .selector
            .match_labels
            .as_ref()
            .unwrap()
            .len(),
        1
    );
    // Other objects don't carry the level (only the namespace and the pods).
    assert!(!o.volumes[0]
        .metadata
        .labels
        .as_ref()
        .unwrap()
        .contains_key(ISOLATION_LABEL));

    // `standard`: the node's default runtime, labelled so.
    let mut mc = minecraft();
    mc.isolation = Some(Isolation::Standard);
    let o = render("minecraft", &mc, &Placement::domain("x.test"));
    assert_eq!(gvisor(&o), (None, "standard".into(), "standard".into()));
    // The old catalog's Nextcloud stays an ordinary container.
    let o = render("nextcloud", &nextcloud(), &Placement::domain("x.test"));
    assert_eq!(gvisor(&o).0, None);

    assert_eq!(runtime_class(Isolation::Gvisor), Some(GVISOR_RUNTIME_CLASS));
    assert_eq!(runtime_class(Isolation::Standard), None);
}

#[test]
fn every_container_is_locked_down_and_sized() {
    let o = render("minecraft", &minecraft(), &Placement::domain("x.test"));
    let labels = o.namespace.metadata.labels.unwrap();
    assert_eq!(labels[APP_LABEL], "minecraft");
    assert_eq!(labels["pod-security.kubernetes.io/enforce"], "baseline");
    assert_eq!(o.role_binding.role_ref.name, WORKLOAD_ROLE);
    let pod = o.deployment.spec.template.spec.unwrap();
    assert_eq!(pod.automount_service_account_token, Some(false));
    assert!(pod.host_network.is_none() && pod.host_pid.is_none() && pod.host_ipc.is_none());
    assert_eq!(
        pod.security_context.unwrap().seccomp_profile.unwrap().type_,
        "RuntimeDefault"
    );
    for c in &pod.containers {
        let sc = c.security_context.as_ref().unwrap();
        assert_eq!(sc.privileged, Some(false));
        assert_eq!(sc.allow_privilege_escalation, Some(false));
        assert!(sc.capabilities.is_none());
        let limits = c.resources.as_ref().unwrap().limits.as_ref().unwrap();
        assert!(limits.contains_key("cpu") && limits.contains_key("memory"));
    }
    assert_eq!(
        pod.containers[0]
            .security_context
            .as_ref()
            .unwrap()
            .run_as_user,
        Some(1000)
    );
    // TCP probe without a path.
    let probe = pod.containers[0].readiness_probe.as_ref().unwrap();
    assert!(probe.tcp_socket.is_some() && probe.http_get.is_none());
    // No web port: no Ingress; no NodePorts given: no port Service.
    assert!(o.ingress.is_none() && o.port_service.is_none());
    // Its own Secret per field.
    assert_eq!(o.secrets[0].name, "minecraft-secret-rcon-password");
}

#[test]
fn public_tcp_and_udp_ports_get_node_ports_and_records() {
    let ports = BTreeMap::from([("game".to_string(), 30001), ("bedrock".to_string(), 30002)]);
    let o = render(
        "minecraft",
        &minecraft(),
        &Placement {
            node_ports: &ports,
            ..Placement::domain("x.test")
        },
    );
    let svc = o.port_service.unwrap();
    assert_eq!(svc.metadata.name.as_deref(), Some("minecraft-port"));
    let spec = svc.spec.unwrap();
    assert_eq!(spec.type_.as_deref(), Some("NodePort"));
    let got: Vec<(i32, String)> = spec
        .ports
        .unwrap()
        .iter()
        .map(|p| (p.node_port.unwrap(), p.protocol.clone().unwrap()))
        .collect();
    assert_eq!(
        got,
        [(30001, "TCP".to_string()), (30002, "UDP".to_string())]
    );
    let ann = o.namespace.metadata.annotations.unwrap();
    assert_eq!(ann[PORTS_ANNOTATION], "game:30001/tcp bedrock:30002/udp");
    assert!(!ann.contains_key(PORT_ANNOTATION));
    assert_eq!(
        parse_ports_annotation(&ann[PORTS_ANNOTATION]),
        BTreeMap::from([
            ("game".to_string(), (30001, Protocol::Tcp)),
            ("bedrock".to_string(), (30002, Protocol::Udp)),
        ])
    );
    // rcon is not public: only in the ClusterIP Service.
    let cluster_ports = o.service.unwrap().spec.unwrap().ports.unwrap();
    assert_eq!(cluster_ports.len(), 3);
}

#[test]
fn a_recorded_web_port_stays_on_the_namespace() {
    let mut spec = nextcloud();
    spec.web = None;
    spec.containers[0].probe.as_mut().unwrap().host_header = false;
    let o = render(
        "nextcloud",
        &spec,
        &Placement {
            recorded_web_port: Some(30000),
            ..Placement::domain("x.test")
        },
    );
    assert_eq!(
        o.namespace.metadata.annotations.unwrap()[PORT_ANNOTATION],
        "30000"
    );
    assert!(o.ingress.is_none() && o.port_service.is_none());
}

#[test]
fn a_stopped_app_renders_with_no_replicas_and_remembers_the_count() {
    let o = render(
        "nextcloud",
        &nextcloud(),
        &Placement {
            stopped_from: Some(2),
            ..Placement::domain("x.test")
        },
    );
    assert_eq!(o.deployment.spec.replicas, Some(0));
    assert_eq!(
        o.deployment.metadata.annotations.unwrap()[REPLICAS_BEFORE_STOP_ANNOTATION],
        "2"
    );
}

#[test]
fn secret_object_carries_only_its_value() {
    let o = render("minecraft", &minecraft(), &Placement::domain("x.test"));
    let s = secret_object(&o.secrets[0], "pw".into());
    assert_eq!(
        s.metadata.name.as_deref(),
        Some("minecraft-secret-rcon-password")
    );
    assert_eq!(s.string_data.unwrap()["rcon_password"], "pw");
}

#[test]
fn label_values_are_sanitized() {
    assert_eq!(label_value("34.0.6"), "34.0.6");
    assert_eq!(label_value("1.0 (beta)"), "1.0__beta");
    assert_eq!(label_value(&"x".repeat(80)).len(), 63);
}
