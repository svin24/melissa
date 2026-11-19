use serde_json::json;

use super::*;

const NEXTCLOUD: &str = include_str!("../../../manifests/nextcloud.toml");

const MINIMAL: &str = r#"
schema = 1
name = "demo"
version = "1.0"
description = "demo app"
registries = ["docker.io"]

[[config]]
name = "greeting"
type = "string"
description = "text"
default = "hi"

[[config]]
name = "workers"
type = "integer"
description = "n"
default = 2
min = 1
max = 4

[[config]]
name = "token"
type = "secret"
description = "generated"

[[volumes]]
name = "data"
size = "1Gi"

[[containers]]
name = "web"
image = "docker.io/library/demo:1.0@sha256:0000000000000000000000000000000000000000000000000000000000000000"
ports = [{ name = "http", port = 8080 }]
mounts = [{ volume = "data", path = "/data" }]
env = { GREETING = { config = "greeting" }, TOKEN = { secret = "token" } }
resources = { cpu_request = "50m", cpu_limit = "500m", memory_request = "64Mi", memory_limit = "128Mi" }

[ingress]
port = "http"
"#;

/// The minimal manifest with `from` replaced by `to` (must match once).
fn variant(from: &str, to: &str) -> Result<Manifest, ManifestError> {
    assert_eq!(MINIMAL.matches(from).count(), 1, "pattern {from:?}");
    Manifest::parse(&MINIMAL.replacen(from, to, 1))
}

fn invalid_at(result: Result<Manifest, ManifestError>, path: &str) -> String {
    match result {
        Err(ManifestError::Invalid { path: p, message }) if p == path => message,
        other => panic!("expected an error at {path}, got {other:?}"),
    }
}

#[test]
fn the_shipped_nextcloud_manifest_is_valid() {
    let m = Manifest::parse(NEXTCLOUD).unwrap();
    assert_eq!(m.name, "nextcloud");
    assert_eq!(m.namespace(), "app-nextcloud");
    assert_eq!(m.host(DEFAULT_DOMAIN), "nextcloud.melissa.home.arpa");
    let image = ImageRef::parse(&m.containers[0].image).unwrap();
    assert_eq!(image.registry, "docker.io");
    assert_eq!(image.repository, "library/nextcloud");
    assert_eq!(image.tag, Some("34.0.6-apache"));
    let resolved = m.resolve_config(&serde_json::Map::new()).unwrap();
    assert_eq!(resolved.values["admin_user"], Scalar::Str("admin".into()));
    assert_eq!(resolved.secrets["admin_password"], 32);
}

#[test]
fn the_repo_catalog_loads_without_errors() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../manifests");
    let catalog = Catalog::load_dir(&dir);
    assert!(catalog.errors.is_empty(), "{:?}", catalog.errors);
    assert!(catalog.get("nextcloud").is_some());
}

#[test]
fn unknown_keys_are_rejected() {
    let err = Manifest::parse(
        &MINIMAL.replace("version = \"1.0\"", "version = \"1.0\"\nprivileged = true"),
    )
    .unwrap_err();
    assert!(
        matches!(&err, ManifestError::Parse(m) if m.contains("privileged")),
        "{err}"
    );
    let err = variant("mounts = [", "host_network = true\nmounts = [").unwrap_err();
    assert!(err.to_string().contains("host_network"), "{err}");
}

#[test]
fn images_must_be_pinned_by_digest_from_an_allowed_registry() {
    let image = "docker.io/library/demo:1.0@sha256:0000000000000000000000000000000000000000000000000000000000000000";
    for (bad, needle) in [
        ("docker.io/library/demo:1.0", "pinned by digest"),
        ("library/demo@sha256:0000000000000000000000000000000000000000000000000000000000000000", "registry"),
        ("docker.io/library/demo@sha256:abc", "64 lowercase hex"),
        ("ghcr.io/x/demo@sha256:0000000000000000000000000000000000000000000000000000000000000000", "not in `registries`"),
        ("docker.io/Library/demo@sha256:0000000000000000000000000000000000000000000000000000000000000000", "repository"),
    ] {
        let message = invalid_at(variant(image, bad), "containers[0].image");
        assert!(message.contains(needle), "{bad}: {message}");
    }
    // Registry with a port, no tag.
    let r = ImageRef::parse("localhost:5000/a/b@sha256:0000000000000000000000000000000000000000000000000000000000000000").unwrap();
    assert_eq!(
        (r.registry, r.repository, r.tag),
        ("localhost:5000", "a/b", None)
    );
}

#[test]
fn names_ports_and_references_are_checked() {
    invalid_at(variant("name = \"demo\"", "name = \"Demo\""), "name");
    invalid_at(
        variant(
            "name = \"demo\"",
            "name = \"a-very-long-application-name-that-is-too-long\"",
        ),
        "name",
    );
    invalid_at(variant("schema = 1", "schema = 2"), "schema");
    invalid_at(
        variant("port = 8080 }", "port = 0 }"),
        "containers[0].ports[0].port",
    );
    invalid_at(
        variant("name = \"http\"", "name = \"http-port-name-too-long\""),
        "containers[0].ports[0].name",
    );
    let m = invalid_at(
        variant("[ingress]\nport = \"http\"", "[ingress]\nport = \"web\""),
        "ingress.port",
    );
    assert!(m.contains("web"));
    let m = invalid_at(
        variant("volume = \"data\"", "volume = \"cache\""),
        "containers[0].mounts[0]",
    );
    assert!(m.contains("cache"));
    invalid_at(
        variant("path = \"/data\"", "path = \"data\""),
        "containers[0].mounts[0]",
    );
    // A config field used as a secret and vice versa.
    let m = invalid_at(
        variant("{ config = \"greeting\" }", "{ secret = \"greeting\" }"),
        "containers[0].env.GREETING",
    );
    assert!(m.contains("not a secret"));
    let m = invalid_at(
        variant("{ secret = \"token\" }", "{ config = \"token\" }"),
        "containers[0].env.TOKEN",
    );
    assert!(m.contains("is a secret"));
    let m = invalid_at(
        variant(
            "{ config = \"greeting\" }",
            "{ config = \"greeting\", value = \"x\" }",
        ),
        "containers[0].env.GREETING",
    );
    assert!(m.contains("exactly one"));
    invalid_at(
        variant("{ config = \"greeting\" }", "{ }"),
        "containers[0].env.GREETING",
    );
    invalid_at(variant("GREETING =", "1BAD ="), "containers[0].env.1BAD");
}

#[test]
fn volumes_and_resources_are_checked() {
    invalid_at(
        variant("size = \"1Gi\"", "size = \"1G\""),
        "volumes[0].size",
    );
    invalid_at(
        variant("size = \"1Gi\"", "size = \"10\""),
        "volumes[0].size",
    );
    let m = invalid_at(
        variant("cpu_limit = \"500m\"", "cpu_limit = \"10m\""),
        "containers[0].resources",
    );
    assert!(m.contains("above"));
    let m = invalid_at(
        variant("memory_limit = \"128Mi\"", "memory_limit = \"lots\""),
        "containers[0].resources",
    );
    assert!(m.contains("memory_limit"));
    invalid_at(
        variant("cpu_request = \"50m\"", "cpu_request = \"0\""),
        "containers[0].resources",
    );
    // A volume nobody mounts.
    let unused = MINIMAL.replace("[[volumes]]\nname = \"data\"\nsize = \"1Gi\"\n", "[[volumes]]\nname = \"data\"\nsize = \"1Gi\"\n\n[[volumes]]\nname = \"spare\"\nsize = \"1Gi\"\n");
    let m = invalid_at(Manifest::parse(&unused), "volumes");
    assert!(m.contains("spare"));
}

#[test]
fn config_fields_are_typed() {
    let m = invalid_at(variant("default = \"hi\"", "default = 3"), "config[0]");
    assert!(m.contains("string"), "{m}");
    let m = invalid_at(variant("default = 2", "default = 9"), "config[1]");
    assert!(m.contains("between 1 and 4"), "{m}");
    let m = invalid_at(
        variant(
            "description = \"generated\"",
            "description = \"generated\"\ndefault = \"x\"",
        ),
        "config[2]",
    );
    assert!(m.contains("generated"), "{m}");
    let m = invalid_at(
        variant(
            "description = \"generated\"",
            "description = \"generated\"\nlength = 8",
        ),
        "config[2]",
    );
    assert!(m.contains("16-128"), "{m}");
    let m = invalid_at(
        variant("default = \"hi\"", "default = \"hi\"\nmin = 1"),
        "config[0]",
    );
    assert!(m.contains("integer"), "{m}");
    invalid_at(
        variant("name = \"greeting\"", "name = \"Greeting\""),
        "config[0].name",
    );
    invalid_at(
        variant("name = \"workers\"", "name = \"greeting\""),
        "config[1].name",
    );
}

#[test]
fn overrides_are_limited_to_declared_non_secret_fields() {
    let m = Manifest::parse(MINIMAL).unwrap();
    let obj = |v: serde_json::Value| v.as_object().cloned().unwrap();
    let r = m.resolve_config(&obj(json!({"workers": 3}))).unwrap();
    assert_eq!(r.values["workers"], Scalar::Int(3));
    assert_eq!(r.values["greeting"], Scalar::Str("hi".into()));
    assert_eq!(r.secrets["token"], DEFAULT_SECRET_LENGTH);
    assert!(!r.values.contains_key("token"));

    assert_eq!(
        m.resolve_config(&obj(json!({"token": "chosen-by-agent"}))),
        Err(ConfigError::Secret("token".into()))
    );
    assert_eq!(
        m.resolve_config(&obj(json!({"image": "evil"}))),
        Err(ConfigError::Unknown("image".into()))
    );
    assert!(matches!(
        m.resolve_config(&obj(json!({"workers": 99}))),
        Err(ConfigError::Invalid { field, .. }) if field == "workers"
    ));
    assert!(matches!(
        m.resolve_config(&obj(json!({"workers": "3"}))),
        Err(ConfigError::Invalid { .. })
    ));
    assert!(matches!(
        m.resolve_config(&obj(json!({"greeting": "a\u{1b}[31mb"}))),
        Err(ConfigError::Invalid { .. })
    ));
    let nc = Manifest::parse(NEXTCLOUD).unwrap();
    assert!(nc
        .resolve_config(&obj(json!({"upload_limit": "2G"})))
        .is_ok());
    assert!(matches!(
        nc.resolve_config(&obj(json!({"upload_limit": "100G"}))),
        Err(ConfigError::Invalid { .. })
    ));
}

#[test]
fn catalog_reports_bad_files_and_name_mismatches() {
    let dir = std::env::temp_dir().join(format!("melissa-catalog-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("demo.toml"), MINIMAL).unwrap();
    std::fs::write(dir.join("other.toml"), MINIMAL).unwrap();
    std::fs::write(dir.join("broken.toml"), "schema = ").unwrap();
    std::fs::write(dir.join("README.md"), "not a manifest").unwrap();
    let catalog = Catalog::load_dir(&dir);
    assert_eq!(catalog.apps.keys().collect::<Vec<_>>(), ["demo"]);
    assert_eq!(catalog.errors.len(), 2);
    assert!(catalog
        .errors
        .iter()
        .any(|(p, e)| p.ends_with("other.toml") && e.contains("demo.toml")));
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(Catalog::load_dir(&dir).apps.is_empty());
}

#[test]
fn files_are_small_text_outside_the_data_volumes() {
    let with_file = |file: &str| {
        variant(
            "mounts = [{ volume = \"data\", path = \"/data\" }]",
            &format!("mounts = [{{ volume = \"data\", path = \"/data\" }}]\nfiles = [{file}]"),
        )
    };
    let ok = with_file(
        r##"{ path = "/etc/demo/hook.sh", content = "#!/bin/sh\necho hi\n", executable = true }"##,
    )
    .unwrap();
    assert!(ok.containers[0].files[0].executable);
    // Inside a volume mount: the file would land in the app's data.
    for bad in ["/data/hook.sh", "/data"] {
        let msg = invalid_at(
            with_file(&format!(r#"{{ path = "{bad}", content = "x" }}"#)),
            "containers[0].files[0].path",
        );
        assert!(!msg.is_empty());
    }
    for bad in ["relative", "/etc/../x", "/etc/dir/"] {
        invalid_at(
            with_file(&format!(r#"{{ path = "{bad}", content = "x" }}"#)),
            "containers[0].files[0].path",
        );
    }
    invalid_at(
        with_file(r#"{ path = "/x", content = "" }"#),
        "containers[0].files[0].content",
    );
    let big = "a".repeat(MAX_FILE_BYTES + 1);
    invalid_at(
        with_file(&format!(r#"{{ path = "/x", content = "{big}" }}"#)),
        "containers[0].files[0].content",
    );
    // Two files on one path.
    invalid_at(
        with_file(r#"{ path = "/x", content = "a" }, { path = "/x", content = "b" }"#),
        "containers[0].files[1].path",
    );
    // The files volume name is reserved.
    invalid_at(
        variant("name = \"data\"\nsize", "name = \"melissa-files\"\nsize"),
        "volumes[0].name",
    );
    assert!(with_file(r#"{ path = "/x", content = "a", mode = "0777" }"#).is_err());
}

#[test]
fn device_hosts_is_a_builtin() {
    let m = variant(
        "GREETING = { config = \"greeting\" }",
        "GREETING = { builtin = \"device_hosts\" }",
    )
    .unwrap();
    assert_eq!(
        m.containers[0].env["GREETING"].source(),
        Some(EnvSource::Builtin(Builtin::DeviceHosts))
    );
}
