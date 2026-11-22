//! Apps installed from the old catalog (before 2026-11-19), as app specs.
//!
//! The catalog is gone, but an install from it keeps running and must stay manageable. Its
//! namespace has no stored spec; [`from_catalog`] rebuilds one from the app's name and the
//! config it was installed with (the `melissa.io/config` annotation on its Deployment). The
//! result renders to the same objects as before (same volume, Secret, files and port), so an
//! `update_app` keeps the data.
//!
//! The catalog only ever shipped Nextcloud. Its spec is also a good example of a real app:
//! a generated admin password, a host-checking app (`device_hosts`) and an entrypoint hook.

use serde_json::{json, Map, Value};

use crate::AppSpec;

/// Nextcloud's entrypoint hook from the old catalog: makes `trusted_domains` exactly the hosts
/// in `NEXTCLOUD_TRUSTED_DOMAINS` on every start (the image reads it only at the first
/// install). Kept byte for byte so a converted install doesn't restart for nothing.
pub const NEXTCLOUD_TRUSTED_DOMAINS_HOOK: &str = r#"#!/bin/sh
# Melissa: make Nextcloud's trusted_domains exactly the hosts in NEXTCLOUD_TRUSTED_DOMAINS
# (the image itself reads it only at the first install). Never fails the start: a problem
# here only means the app answers at fewer addresses.
set -u
[ -n "${NEXTCLOUD_TRUSTED_DOMAINS:-}" ] || exit 0
cd /var/www/html
php occ status --output=json 2>/dev/null | grep -q '"installed":true' || exit 0
set -f
i=0
for host in $NEXTCLOUD_TRUSTED_DOMAINS; do
    php occ config:system:set trusted_domains "$i" --value="$host" >/dev/null
    i=$((i + 1))
done
while php occ config:system:get trusted_domains "$i" >/dev/null 2>&1; do
    php occ config:system:delete trusted_domains "$i" >/dev/null
    i=$((i + 1))
done
echo "Melissa: trusted_domains set to: $NEXTCLOUD_TRUSTED_DOMAINS"
"#;

/// The image the catalog pinned (tag 34.0.6-apache, multi-arch index digest).
pub const NEXTCLOUD_IMAGE: &str = "docker.io/library/nextcloud:34.0.6-apache@sha256:3031e7f87baaceb863a755921ebf463bc26ea71855c07ad7858e2ae25a5a0ad7";

/// The spec of an app installed from the old catalog, with the config it was installed with
/// (`None`: not a catalog app).
pub fn from_catalog(app: &str, config: &Map<String, Value>) -> Option<AppSpec> {
    match app {
        "nextcloud" => Some(nextcloud(config)),
        _ => None,
    }
}

/// The Nextcloud spec. `config` holds the old catalog fields (`admin_user`,
/// `php_memory_limit`, `upload_limit`); missing or odd values get the old defaults.
pub fn nextcloud(config: &Map<String, Value>) -> AppSpec {
    let text = |key: &str, default: &str, ok: &dyn Fn(&str) -> bool| {
        config
            .get(key)
            .and_then(Value::as_str)
            .filter(|v| ok(v))
            .unwrap_or(default)
            .to_string()
    };
    let user = text("admin_user", "admin", &|v| {
        !v.is_empty() && v.len() <= 64 && !v.chars().any(char::is_control)
    });
    let memory = text("php_memory_limit", "512M", &|v| {
        ["256M", "512M", "1G"].contains(&v)
    });
    let upload = text("upload_limit", "512M", &|v| {
        ["512M", "2G", "10G"].contains(&v)
    });
    let spec = json!({
        "description": "Nextcloud: files, calendar and contacts (SQLite, for one to a few users).",
        "version": "34.0.6",
        "secrets": [{"name": "admin_password", "length": 32}],
        "volumes": [{"name": "data", "size": "10Gi"}],
        "web": "http",
        "credentials": {"user": user, "password": "admin_password"},
        "kind": "files",
        "containers": [{
            "name": "nextcloud",
            "image": NEXTCLOUD_IMAGE,
            "ports": [{"name": "http", "port": 80}],
            "mounts": [{"volume": "data", "path": "/var/www/html"}],
            "env": {
                "SQLITE_DATABASE": "nextcloud",
                "NEXTCLOUD_ADMIN_USER": user,
                "NEXTCLOUD_ADMIN_PASSWORD": {"secret": "admin_password"},
                "NEXTCLOUD_TRUSTED_DOMAINS": {"builtin": "device_hosts"},
                "PHP_MEMORY_LIMIT": memory,
                "PHP_UPLOAD_LIMIT": upload,
            },
            "files": [{
                "path": "/docker-entrypoint-hooks.d/before-starting/50-melissa-trusted-domains.sh",
                "executable": true,
                "content": NEXTCLOUD_TRUSTED_DOMAINS_HOOK,
            }],
            "resources": {"cpu": "1", "memory": "1Gi", "cpu_request": "100m", "memory_request": "256Mi"},
            "probe": {
                "port": "http",
                "path": "/status.php",
                "host_header": true,
                "startup_timeout_secs": 900,
                "period_secs": 10,
            },
        }],
    });
    // A fixed, valid spec; the test below keeps it so.
    AppSpec::from_json(&spec).unwrap_or_else(|e| unreachable!("legacy nextcloud spec: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nextcloud_keeps_the_installed_config() {
        let config = json!({"admin_user": "owner", "upload_limit": "2G", "php_memory_limit": "9G"});
        let spec = nextcloud(config.as_object().unwrap());
        let env = &spec.containers[0].env;
        assert_eq!(
            env["NEXTCLOUD_ADMIN_USER"],
            crate::EnvValue::Text("owner".into())
        );
        assert_eq!(env["PHP_UPLOAD_LIMIT"], crate::EnvValue::Text("2G".into()));
        // An odd value falls back to the default.
        assert_eq!(
            env["PHP_MEMORY_LIMIT"],
            crate::EnvValue::Text("512M".into())
        );
        assert_eq!(spec.credentials.unwrap().user.as_deref(), Some("owner"));
        assert!(from_catalog("hello", &Map::new()).is_none());
    }
}
