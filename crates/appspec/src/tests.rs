use serde_json::{json, Value};

use super::*;

fn minecraft() -> Value {
    json!({
        "description": "Minecraft Java server",
        "containers": [{
            "name": "server",
            "image": "itzg/minecraft-server:java21",
            "env": {"EULA": "TRUE", "MEMORY": "1G", "MAX_PLAYERS": 10, "ONLINE_MODE": true},
            "ports": [{"name": "game", "port": 25565, "public": true}],
            "mounts": [{"volume": "data", "path": "/data"}],
            "resources": {"cpu": "2", "memory": "1536Mi"},
            "probe": {"port": "game", "startup_timeout_secs": 600},
        }],
        "volumes": [{"name": "data", "size": "5Gi"}],
    })
}

fn err_of(v: Value) -> String {
    AppSpec::from_json(&v).unwrap_err().to_string()
}

#[test]
fn a_minecraft_server_validates_and_its_image_is_expanded() {
    let spec = AppSpec::from_json(&minecraft()).unwrap();
    assert_eq!(
        spec.containers[0].image,
        "docker.io/itzg/minecraft-server:java21"
    );
    assert_eq!(spec.public_ports().len(), 1);
    let env = &spec.containers[0].env;
    assert_eq!(
        env["MAX_PLAYERS"].source(),
        Some(EnvSource::Value("10".into()))
    );
    assert_eq!(
        env["ONLINE_MODE"].source(),
        Some(EnvSource::Value("true".into()))
    );
    let summary = spec.summary().join("\n");
    assert!(
        summary.contains("docker.io/itzg/minecraft-server:java21"),
        "{summary}"
    );
    assert!(summary.contains("game 25565/tcp"), "{summary}");
    assert!(summary.contains("data 5Gi"), "{summary}");
    // Stored text parses back to the same spec.
    assert_eq!(AppSpec::from_text(&spec.to_text()).unwrap(), spec);
}

#[test]
fn image_references_must_be_pinned() {
    assert_eq!(
        normalize_image("nginx:1.27").unwrap(),
        "docker.io/library/nginx:1.27"
    );
    assert_eq!(
        normalize_image("ghcr.io/home-assistant/home-assistant:2026.11").unwrap(),
        "ghcr.io/home-assistant/home-assistant:2026.11"
    );
    assert_eq!(
        normalize_image("localhost:5000/x/y:1").unwrap(),
        "localhost:5000/x/y:1"
    );
    let digest = format!("nginx@sha256:{}", "a".repeat(64));
    assert!(normalize_image(&digest).is_ok());
    assert!(normalize_image("nginx").unwrap_err().contains("not pinned"));
    assert!(normalize_image("nginx:latest")
        .unwrap_err()
        .contains("not pinned"));
    assert!(normalize_image("Nginx:1").is_err());
    assert!(normalize_image("nginx:1 --privileged").is_err());
    assert!(normalize_image("nginx@sha256:abc").is_err());
}

#[test]
fn defaults_fill_in_resources() {
    let r = Resources::resolved(None);
    assert_eq!(
        (r.cpu_request.as_str(), r.cpu_limit.as_str()),
        ("100m", "1")
    );
    assert_eq!(
        (r.memory_request.as_str(), r.memory_limit.as_str()),
        ("128Mi", "512Mi")
    );
    let small = Resources {
        cpu: Some("50m".into()),
        memory: Some("2Gi".into()),
        ..Resources::default()
    };
    let r = Resources::resolved(Some(&small));
    assert_eq!(r.cpu_request, "50m");
    assert_eq!(r.memory_request, "512Mi");
}

#[test]
fn bad_specs_name_the_field() {
    let mut v = minecraft();
    v["containers"][0]["resources"]["memory"] = json!("100Gi");
    assert!(err_of(v).starts_with("containers[0].resources"));

    let mut v = minecraft();
    v["containers"][0]["env"]["PW"] = json!({"secret": "nope"});
    assert!(err_of(v).contains("no secret `nope`"));

    let mut v = minecraft();
    v["containers"][0]["env"]["X"] = json!({"secret": "a", "builtin": "ingress_host"});
    assert!(err_of(v).contains("exactly one"));

    let mut v = minecraft();
    v["web"] = json!("game2");
    assert!(err_of(v).contains("no port `game2`"));

    let mut v = minecraft();
    v["containers"][0]["ports"][0]["protocol"] = json!("udp");
    v["containers"][0]["probe"] = Value::Null;
    v["web"] = json!("game");
    assert!(err_of(v).contains("must be TCP"));

    let mut v = minecraft();
    v["volumes"] = json!([{"name": "data", "size": "5Gi"}, {"name": "spare", "size": "1Gi"}]);
    assert!(err_of(v).contains("not mounted"));

    let mut v = minecraft();
    v["containers"][0]["mounts"][0]["path"] = json!("/data/../etc");
    assert!(err_of(v).contains("mounts[0]"));

    let mut v = minecraft();
    v["containers"][0]["files"] = json!([{"path": "/data/server.properties", "content": "x"}]);
    assert!(err_of(v).contains("inside a volume mount"));

    let mut v = minecraft();
    v["containers"][0]["probe"]["port"] = json!("other");
    assert!(err_of(v).contains("no TCP port"));

    // Unknown fields (e.g. Kubernetes keys) are refused, not ignored.
    let mut v = minecraft();
    v["containers"][0]["privileged"] = json!(true);
    assert!(err_of(v).contains("unknown field"));
    let mut v = minecraft();
    v["hostNetwork"] = json!(true);
    assert!(err_of(v).contains("unknown field"));

    let mut v = minecraft();
    v["containers"][0]["ports"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name": "game", "port": 1}));
    assert!(err_of(v).contains("duplicate port name"));

    let mut v = minecraft();
    let ports: Vec<Value> = (0..7)
        .map(|i| json!({"name": format!("p{i}"), "port": 1000 + i, "public": true}))
        .collect();
    v["containers"][0]["ports"] = json!(ports);
    v["containers"][0]["probe"] = Value::Null;
    assert!(err_of(v).contains("public ports"));
}

#[test]
fn tcp_and_udp_may_share_a_number() {
    let mut v = minecraft();
    v["containers"][0]["ports"] = json!([
        {"name": "game", "port": 25565, "public": true},
        {"name": "voice", "port": 25565, "protocol": "udp", "public": true},
    ]);
    let spec = AppSpec::from_json(&v).unwrap();
    assert_eq!(spec.public_ports().len(), 2);
    assert!(spec.summary().join(" ").contains("voice 25565/udp"));
}

#[test]
fn the_web_port_is_public_and_listed_first() {
    let spec = AppSpec::from_json(&json!({
        "description": "whoami",
        "web": "http",
        "containers": [{
            "name": "web",
            "image": "traefik/whoami:v1.10",
            "ports": [{"name": "metrics", "port": 9000, "public": true}, {"name": "http", "port": 80}],
        }],
    }))
    .unwrap();
    let names: Vec<&str> = spec
        .public_ports()
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(names, ["http", "metrics"]);
}

#[test]
fn app_names_and_namespaces() {
    assert!(valid_app_name("minecraft"));
    assert!(!valid_app_name("Minecraft"));
    assert!(!valid_app_name(&"a".repeat(41)));
    assert_eq!(namespace_of("minecraft"), "app-minecraft");
    assert_eq!(secret_name("nextcloud"), "nextcloud-secrets");
}
