//! Manifest validation: everything the renderer relies on is checked here.

use std::collections::BTreeSet;

use crate::{ConfigField, EnvSource, FieldType, Manifest, ManifestError, SCHEMA_VERSION};

/// A parsed `registry/repository[:tag]@sha256:<digest>` reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef<'a> {
    pub registry: &'a str,
    pub repository: &'a str,
    pub tag: Option<&'a str>,
    /// 64 lowercase hex characters.
    pub digest: &'a str,
}

impl<'a> ImageRef<'a> {
    pub fn parse(image: &'a str) -> Result<Self, String> {
        let (name, digest) = image
            .split_once("@sha256:")
            .ok_or("must be pinned by digest: `registry/repo[:tag]@sha256:<64 hex>`")?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err("digest must be 64 lowercase hex characters".into());
        }
        let (registry, rest) = name
            .split_once('/')
            .ok_or("must name its registry (`docker.io/library/nextcloud`)")?;
        if !(registry.contains('.') || registry.contains(':') || registry == "localhost") {
            return Err(format!(
                "`{registry}` is not a registry host; name it explicitly (`docker.io/{name}`)"
            ));
        }
        // A tag's `:` comes after the last `/`.
        let (repository, tag) = match rest.rsplit_once(':') {
            Some((repo, tag)) if !tag.contains('/') => (repo, Some(tag)),
            _ => (rest, None),
        };
        let repo_ok = !repository.is_empty()
            && repository.split('/').all(|part| {
                !part.is_empty()
                    && part.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b)
                    })
            });
        if !repo_ok {
            return Err(format!("bad repository name `{repository}`"));
        }
        if let Some(tag) = tag {
            let tag_ok = !tag.is_empty()
                && tag.len() <= 128
                && tag
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
            if !tag_ok {
                return Err(format!("bad tag `{tag}`"));
            }
        }
        Ok(Self {
            registry,
            repository,
            tag,
            digest,
        })
    }
}

/// RFC 1123 label: lowercase alphanumerics and `-`, starts with a letter, ends alphanumeric.
pub(crate) fn dns_label(s: &str, max: usize) -> Result<(), String> {
    let ok = !s.is_empty()
        && s.len() <= max
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && !s.ends_with('-')
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "`{s}` must be 1-{max} lowercase letters, digits or `-`, starting with a letter"
        ))
    }
}

/// A registry host: DNS name with an optional port.
fn registry_host(s: &str) -> bool {
    let (host, port) = match s.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (s, None),
    };
    let host_ok = !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && l.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        });
    host_ok && port.is_none_or(|p| p.parse::<u16>().is_ok())
}

fn cpu_quantity(s: &str) -> bool {
    match s.strip_suffix('m') {
        Some(milli) => !milli.is_empty() && milli.bytes().all(|b| b.is_ascii_digit()),
        None => {
            let mut parts = s.splitn(2, '.');
            let whole = parts.next().unwrap_or_default();
            let frac = parts.next();
            !whole.is_empty()
                && whole.bytes().all(|b| b.is_ascii_digit())
                && frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
        }
    }
}

/// Binary-suffixed size in bytes (`512Mi`, `10Gi`); `None` if malformed.
pub(crate) fn binary_quantity(s: &str) -> Option<u64> {
    let (num, mult) = [
        ("Ki", 1u64 << 10),
        ("Mi", 1 << 20),
        ("Gi", 1 << 30),
        ("Ti", 1 << 40),
    ]
    .into_iter()
    .find_map(|(suffix, mult)| s.strip_suffix(suffix).map(|n| (n, mult)))?;
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    num.parse::<u64>().ok()?.checked_mul(mult)
}

fn cpu_millis(s: &str) -> Option<u64> {
    if let Some(m) = s.strip_suffix('m') {
        return m.parse().ok();
    }
    let value: f64 = s.parse().ok()?;
    // Small, validated inputs (cpu_quantity); rounding to whole millicores is what k8s does.
    Some((value * 1000.0).round() as u64)
}

fn env_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn field_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn http_path(s: &str) -> bool {
    s.starts_with('/')
        && s.len() <= 256
        && s.bytes()
            .all(|b| b.is_ascii_graphic() && !b"\"'<>\\`".contains(&b))
}

impl Manifest {
    /// Check the whole manifest. Errors name the offending field.
    pub fn validate(&self) -> Result<(), ManifestError> {
        fn err(path: impl Into<String>, message: impl Into<String>) -> ManifestError {
            ManifestError::invalid(path, message)
        }
        if self.schema != SCHEMA_VERSION {
            return Err(err(
                "schema",
                format!(
                    "unsupported schema {} (expected {SCHEMA_VERSION})",
                    self.schema
                ),
            ));
        }
        // `app-<name>` must stay a valid namespace and `<name>-secrets` a valid object name.
        dns_label(&self.name, 40).map_err(|m| err("name", m))?;
        if self.version.trim().is_empty()
            || self.version.len() > 64
            || self.version.chars().any(char::is_control)
        {
            return Err(err("version", "must be 1-64 printable characters"));
        }
        if self.description.trim().is_empty() || self.description.len() > 1000 {
            return Err(err("description", "must be 1-1000 characters"));
        }
        if self.registries.is_empty() {
            return Err(err("registries", "list at least one registry"));
        }
        for (i, r) in self.registries.iter().enumerate() {
            if !registry_host(r) {
                return Err(err(
                    format!("registries[{i}]"),
                    format!("`{r}` is not a registry host"),
                ));
            }
        }

        let mut names = BTreeSet::new();
        for (i, f) in self.config.iter().enumerate() {
            let path = format!("config[{i}]");
            if !field_name(&f.name) {
                return Err(err(
                    format!("{path}.name"),
                    format!("`{}` must be lower_snake_case", f.name),
                ));
            }
            if !names.insert(f.name.as_str()) {
                return Err(err(
                    format!("{path}.name"),
                    format!("duplicate field `{}`", f.name),
                ));
            }
            check_field(f).map_err(|m| err(&path, m))?;
        }

        let mut volumes = BTreeSet::new();
        for (i, v) in self.volumes.iter().enumerate() {
            let path = format!("volumes[{i}]");
            dns_label(&v.name, 40).map_err(|m| err(format!("{path}.name"), m))?;
            if !volumes.insert(v.name.as_str()) {
                return Err(err(
                    format!("{path}.name"),
                    format!("duplicate volume `{}`", v.name),
                ));
            }
            if !binary_quantity(&v.size).is_some_and(|b| b >= 1 << 20) {
                return Err(err(
                    format!("{path}.size"),
                    "must be a size like `512Mi` or `10Gi` (at least 1Mi)",
                ));
            }
        }

        if self.containers.is_empty() {
            return Err(err("containers", "at least one container"));
        }
        let mut containers = BTreeSet::new();
        let mut all_ports = BTreeSet::new();
        let mut port_numbers = BTreeSet::new();
        for (ci, c) in self.containers.iter().enumerate() {
            let path = format!("containers[{ci}]");
            dns_label(&c.name, 63).map_err(|m| err(format!("{path}.name"), m))?;
            if !containers.insert(c.name.as_str()) {
                return Err(err(
                    format!("{path}.name"),
                    format!("duplicate container `{}`", c.name),
                ));
            }
            let image = ImageRef::parse(&c.image).map_err(|m| err(format!("{path}.image"), m))?;
            if !self.registries.iter().any(|r| r == image.registry) {
                return Err(err(
                    format!("{path}.image"),
                    format!("registry `{}` is not in `registries`", image.registry),
                ));
            }
            let mut ports = BTreeSet::new();
            for (pi, p) in c.ports.iter().enumerate() {
                let ppath = format!("{path}.ports[{pi}]");
                dns_label(&p.name, 15).map_err(|m| err(format!("{ppath}.name"), m))?;
                if p.port == 0 {
                    return Err(err(format!("{ppath}.port"), "must be 1-65535"));
                }
                // Port names are shared by the pod's Service: unique across containers.
                if !all_ports.insert(p.name.as_str()) || !port_numbers.insert(p.port) {
                    return Err(err(&ppath, format!("duplicate port `{}`", p.name)));
                }
                ports.insert(p.name.as_str());
            }
            for (name, value) in &c.env {
                let epath = format!("{path}.env.{name}");
                if !env_name(name) {
                    return Err(err(&epath, "bad environment variable name"));
                }
                let source = value.source().ok_or_else(|| {
                    err(
                        &epath,
                        "set exactly one of `value`, `config`, `secret`, `builtin`",
                    )
                })?;
                match source {
                    EnvSource::Value(v) if v.chars().any(|c| c.is_control() && c != '\n') => {
                        return Err(err(&epath, "control characters in value"));
                    }
                    EnvSource::Config(f) => match self.field(f) {
                        Some(field) if field.kind != FieldType::Secret => {}
                        Some(_) => {
                            return Err(err(&epath, format!("`{f}` is a secret: use `secret`")))
                        }
                        None => return Err(err(&epath, format!("no config field `{f}`"))),
                    },
                    EnvSource::Secret(f) => match self.field(f) {
                        Some(field) if field.kind == FieldType::Secret => {}
                        Some(_) => {
                            return Err(err(&epath, format!("`{f}` is not a secret: use `config`")))
                        }
                        None => return Err(err(&epath, format!("no config field `{f}`"))),
                    },
                    _ => {}
                }
            }
            let mut mount_paths = BTreeSet::new();
            for (mi, m) in c.mounts.iter().enumerate() {
                let mpath = format!("{path}.mounts[{mi}]");
                if !volumes.contains(m.volume.as_str()) {
                    return Err(err(&mpath, format!("no volume `{}`", m.volume)));
                }
                if !m.path.starts_with('/')
                    || m.path.contains("..")
                    || m.path.chars().any(char::is_control)
                    || !mount_paths.insert(m.path.as_str())
                {
                    return Err(err(&mpath, "path must be a unique absolute path"));
                }
            }
            check_resources(&c.resources).map_err(|m| err(format!("{path}.resources"), m))?;
            if let Some(probe) = &c.probe {
                let ppath = format!("{path}.probe");
                if !http_path(&probe.path) {
                    return Err(err(format!("{ppath}.path"), "must be an absolute URL path"));
                }
                if !ports.contains(probe.port.as_str()) {
                    return Err(err(
                        format!("{ppath}.port"),
                        format!("no port `{}` on this container", probe.port),
                    ));
                }
                if probe.ingress_host_header && self.ingress.is_none() {
                    return Err(err(
                        format!("{ppath}.ingress_host_header"),
                        "the app has no ingress",
                    ));
                }
                if !(1..=300).contains(&probe.period_secs)
                    || !(probe.period_secs..=3600).contains(&probe.startup_timeout_secs)
                {
                    return Err(err(
                        &ppath,
                        "period_secs must be 1-300 and startup_timeout_secs period_secs-3600",
                    ));
                }
            }
        }
        // Every volume is used (an unused PVC is almost always a typo).
        for v in &self.volumes {
            if !self
                .containers
                .iter()
                .any(|c| c.mounts.iter().any(|m| m.volume == v.name))
            {
                return Err(err(
                    "volumes",
                    format!("volume `{}` is not mounted", v.name),
                ));
            }
        }
        if let Some(ingress) = &self.ingress {
            if !all_ports.contains(ingress.port.as_str()) {
                return Err(err(
                    "ingress.port",
                    format!("no container port `{}`", ingress.port),
                ));
            }
        }
        if let Some(creds) = &self.credentials {
            if let Some(user) = &creds.user {
                if self.field(user).is_none_or(|f| f.kind != FieldType::String) {
                    return Err(err(
                        "credentials.user",
                        format!("`{user}` is not a string config field"),
                    ));
                }
            }
            if self
                .field(&creds.password)
                .is_none_or(|f| f.kind != FieldType::Secret)
            {
                return Err(err(
                    "credentials.password",
                    format!("`{}` is not a secret config field", creds.password),
                ));
            }
        }
        Ok(())
    }
}

fn check_field(f: &ConfigField) -> Result<(), String> {
    if f.description.trim().is_empty() {
        return Err("description must not be empty".into());
    }
    let only = |present: bool, key: &str, kind: &str| -> Result<(), String> {
        if present {
            Err(format!("`{key}` is only allowed for type `{kind}`"))
        } else {
            Ok(())
        }
    };
    only(
        (f.min.is_some() || f.max.is_some()) && f.kind != FieldType::Integer,
        "min/max",
        "integer",
    )?;
    only(
        f.max_len.is_some() && f.kind != FieldType::String,
        "max_len",
        "string",
    )?;
    only(
        f.options.is_some() && f.kind != FieldType::Choice,
        "options",
        "choice",
    )?;
    only(
        f.length.is_some() && f.kind != FieldType::Secret,
        "length",
        "secret",
    )?;
    match f.kind {
        FieldType::Secret => {
            if f.default.is_some() {
                return Err("a secret has no default; it is generated at install".into());
            }
            if f.length.is_some_and(|l| !(16..=128).contains(&l)) {
                return Err("length must be 16-128".into());
            }
            Ok(())
        }
        FieldType::Choice if f.options.as_ref().is_none_or(Vec::is_empty) => {
            Err("a choice needs `options`".into())
        }
        FieldType::String if f.max_len.is_some_and(|m| m == 0 || m > 4096) => {
            Err("max_len must be 1-4096".into())
        }
        _ => {
            let default = f
                .default
                .as_ref()
                .ok_or("non-secret fields need a `default`")?;
            if let (Some(min), Some(max)) = (f.min, f.max) {
                if min > max {
                    return Err("min is greater than max".into());
                }
            }
            f.check(default).map_err(|m| format!("default {m}"))
        }
    }
}

fn check_resources(r: &crate::Resources) -> Result<(), String> {
    for (key, value) in [("cpu_request", &r.cpu_request), ("cpu_limit", &r.cpu_limit)] {
        if !cpu_quantity(value) || cpu_millis(value).is_none_or(|m| m == 0) {
            return Err(format!("{key} `{value}` must be like `100m` or `1`"));
        }
    }
    for (key, value) in [
        ("memory_request", &r.memory_request),
        ("memory_limit", &r.memory_limit),
    ] {
        if binary_quantity(value).is_none_or(|b| b == 0) {
            return Err(format!("{key} `{value}` must be like `256Mi` or `1Gi`"));
        }
    }
    if cpu_millis(&r.cpu_request) > cpu_millis(&r.cpu_limit) {
        return Err("cpu_request is above cpu_limit".into());
    }
    if binary_quantity(&r.memory_request) > binary_quantity(&r.memory_limit) {
        return Err("memory_request is above memory_limit".into());
    }
    Ok(())
}
