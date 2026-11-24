//! Spec validation: everything the renderer relies on is checked here.

use std::collections::BTreeSet;

use crate::{
    AppSpec, EnvSource, Protocol, Resources, SpecError, FILES_VOLUME, MAX_ARGS, MAX_ARG_BYTES,
    MAX_CONTAINERS, MAX_CPU_MILLIS, MAX_ENV, MAX_ENV_VALUE_BYTES, MAX_FILES, MAX_FILE_BYTES,
    MAX_MEMORY_BYTES, MAX_PORTS, MAX_PUBLIC_PORTS, MAX_SECRETS, MAX_SPEC_BYTES, MAX_VOLUMES,
    MAX_VOLUME_BYTES,
};

/// A parsed, fully qualified `registry/repository[:tag][@sha256:<digest>]` reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef<'a> {
    pub registry: &'a str,
    pub repository: &'a str,
    pub tag: Option<&'a str>,
    /// 64 lowercase hex characters.
    pub digest: Option<&'a str>,
}

impl<'a> ImageRef<'a> {
    /// Parse a fully qualified reference (see [`normalize_image`] for short names).
    pub fn parse(image: &'a str) -> Result<Self, String> {
        let (name, digest) = match image.split_once('@') {
            Some((name, d)) => {
                let hex = d
                    .strip_prefix("sha256:")
                    .ok_or("a digest must be `@sha256:<64 hex>`")?;
                if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
                    return Err("digest must be 64 lowercase hex characters".into());
                }
                (name, Some(hex))
            }
            None => (image, None),
        };
        let (registry, rest) = name
            .split_once('/')
            .ok_or("must name its registry (`docker.io/library/nginx`)")?;
        if !registry_host(registry) {
            return Err(format!("`{registry}` is not a registry host"));
        }
        // A tag's `:` comes after the last `/`.
        let (repository, tag) = match rest.rsplit_once(':') {
            Some((repo, tag)) if !tag.contains('/') => (repo, Some(tag)),
            _ => (rest, None),
        };
        let repo_ok = !repository.is_empty()
            && repository.len() <= 255
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

/// Expand Docker Hub short names (`nginx:1.27` → `docker.io/library/nginx:1.27`,
/// `itzg/minecraft-server:java21` → `docker.io/itzg/minecraft-server:java21`) and check the
/// reference. The image must be pinned: a tag other than `latest`, or a digest.
pub fn normalize_image(image: &str) -> Result<String, String> {
    let image = image.trim();
    if image.is_empty() || image.len() > 512 || image.chars().any(|c| c.is_whitespace()) {
        return Err("must be an image reference like `docker.io/library/nginx:1.27`".into());
    }
    let first = image.split('/').next().unwrap_or_default();
    let qualified = if image.contains('/')
        && (first.contains('.') || first.contains(':') || first == "localhost")
    {
        image.to_string()
    } else if image.contains('/') {
        format!("docker.io/{image}")
    } else {
        format!("docker.io/library/{image}")
    };
    let r = ImageRef::parse(&qualified)?;
    if r.digest.is_none() && r.tag.is_none_or(|t| t == "latest") {
        return Err(format!(
            "`{image}` is not pinned: name a version tag (e.g. `{}:<version>`) or a digest; \
             `latest` changes under the app",
            r.repository
        ));
    }
    Ok(qualified)
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
    if num.is_empty() || num.len() > 12 || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    num.parse::<u64>().ok()?.checked_mul(mult)
}

pub(crate) fn cpu_millis(s: &str) -> Option<u64> {
    if !cpu_quantity(s) || s.len() > 12 {
        return None;
    }
    if let Some(m) = s.strip_suffix('m') {
        return m.parse().ok();
    }
    let value: f64 = s.parse().ok()?;
    // Small, validated inputs; rounding to whole millicores is what k8s does.
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

fn abs_path(s: &str) -> bool {
    s.starts_with('/')
        && s.len() <= 1024
        && !s.split('/').any(|part| part == "..")
        && !s.chars().any(char::is_control)
}

fn printable(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}

impl AppSpec {
    /// Check the whole spec and normalize image references. Errors name the offending field.
    pub fn validate(&mut self) -> Result<(), SpecError> {
        fn err(path: impl Into<String>, message: impl Into<String>) -> SpecError {
            SpecError::invalid(path, message)
        }
        if !printable(&self.description, 300) {
            return Err(err("description", "must be 1-300 printable characters"));
        }
        if let Some(v) = &self.version {
            if !printable(v, 64) {
                return Err(err("version", "must be 1-64 printable characters"));
            }
        }

        if self.secrets.len() > MAX_SECRETS {
            return Err(err("secrets", format!("at most {MAX_SECRETS}")));
        }
        let mut secrets = BTreeSet::new();
        for (i, s) in self.secrets.iter().enumerate() {
            let path = format!("secrets[{i}]");
            if !field_name(&s.name) {
                return Err(err(
                    format!("{path}.name"),
                    format!("`{}` must be lower_snake_case", s.name),
                ));
            }
            if !secrets.insert(s.name.as_str()) {
                return Err(err(
                    format!("{path}.name"),
                    format!("duplicate secret `{}`", s.name),
                ));
            }
            if s.length.is_some_and(|l| !(16..=128).contains(&l)) {
                return Err(err(format!("{path}.length"), "must be 16-128"));
            }
        }

        if self.volumes.len() > MAX_VOLUMES {
            return Err(err("volumes", format!("at most {MAX_VOLUMES}")));
        }
        let mut volumes = BTreeSet::new();
        for (i, v) in self.volumes.iter().enumerate() {
            let path = format!("volumes[{i}]");
            dns_label(&v.name, 40).map_err(|m| err(format!("{path}.name"), m))?;
            if v.name == FILES_VOLUME {
                return Err(err(
                    format!("{path}.name"),
                    format!("`{FILES_VOLUME}` is reserved"),
                ));
            }
            if !volumes.insert(v.name.as_str()) {
                return Err(err(
                    format!("{path}.name"),
                    format!("duplicate volume `{}`", v.name),
                ));
            }
            if !binary_quantity(&v.size).is_some_and(|b| (1 << 20..=MAX_VOLUME_BYTES).contains(&b))
            {
                return Err(err(
                    format!("{path}.size"),
                    "must be a size like `512Mi` or `10Gi` (1Mi to 4Ti)",
                ));
            }
        }

        if self.containers.is_empty() || self.containers.len() > MAX_CONTAINERS {
            return Err(err(
                "containers",
                format!("1 to {MAX_CONTAINERS} containers"),
            ));
        }
        let mut containers = BTreeSet::new();
        let mut port_names = BTreeSet::new();
        let mut port_numbers = BTreeSet::new();
        let has_web = self.web.is_some();
        for (ci, c) in self.containers.iter_mut().enumerate() {
            let path = format!("containers[{ci}]");
            dns_label(&c.name, 63).map_err(|m| err(format!("{path}.name"), m))?;
            if !containers.insert(c.name.clone()) {
                return Err(err(
                    format!("{path}.name"),
                    format!("duplicate container `{}`", c.name),
                ));
            }
            c.image = normalize_image(&c.image).map_err(|m| err(format!("{path}.image"), m))?;
            for (key, list) in [("command", &c.command), ("args", &c.args)] {
                if list.len() > MAX_ARGS
                    || list
                        .iter()
                        .any(|a| a.len() > MAX_ARG_BYTES || a.contains('\0'))
                {
                    return Err(err(
                        format!("{path}.{key}"),
                        format!("at most {MAX_ARGS} items of {MAX_ARG_BYTES} bytes"),
                    ));
                }
            }
            for id in [c.run_as_user, c.run_as_group].into_iter().flatten() {
                if !(0..=2_147_483_647).contains(&id) {
                    return Err(err(&path, "run_as_user/run_as_group must be 0-2147483647"));
                }
            }
            let mut tcp_ports = BTreeSet::new();
            for (pi, p) in c.ports.iter().enumerate() {
                let ppath = format!("{path}.ports[{pi}]");
                dns_label(&p.name, 15).map_err(|m| err(format!("{ppath}.name"), m))?;
                if p.port == 0 {
                    return Err(err(format!("{ppath}.port"), "must be 1-65535"));
                }
                // Port names are shared by the pod's Service: unique across containers.
                if !port_names.insert(p.name.clone()) {
                    return Err(err(&ppath, format!("duplicate port name `{}`", p.name)));
                }
                if !port_numbers.insert((p.port, p.protocol)) {
                    return Err(err(
                        &ppath,
                        format!("port {}/{} is listed twice", p.port, p.protocol.lower()),
                    ));
                }
                if p.protocol == Protocol::Tcp {
                    tcp_ports.insert(p.name.clone());
                }
            }
            if c.env.len() > MAX_ENV {
                return Err(err(format!("{path}.env"), format!("at most {MAX_ENV}")));
            }
            for (name, value) in &c.env {
                let epath = format!("{path}.env.{name}");
                if !env_name(name) {
                    return Err(err(&epath, "bad environment variable name"));
                }
                let source = value.source().ok_or_else(|| {
                    err(
                        &epath,
                        "a value (string, number, boolean), or exactly one of `secret`, `builtin`",
                    )
                })?;
                match source {
                    EnvSource::Value(v)
                        if v.len() > MAX_ENV_VALUE_BYTES
                            || v.chars().any(|c| c.is_control() && c != '\n' && c != '\t') =>
                    {
                        return Err(err(
                            &epath,
                            format!("at most {MAX_ENV_VALUE_BYTES} bytes of text"),
                        ));
                    }
                    EnvSource::Secret(s) if !secrets.contains(s) => {
                        return Err(err(
                            &epath,
                            format!("no secret `{s}`: declare it in `secrets`"),
                        ));
                    }
                    _ => {}
                }
            }
            let mut mount_paths = BTreeSet::new();
            for (mi, m) in c.mounts.iter().enumerate() {
                let mpath = format!("{path}.mounts[{mi}]");
                if !volumes.contains(m.volume.as_str()) {
                    return Err(err(&mpath, format!("no volume `{}`", m.volume)));
                }
                if !abs_path(&m.path) || m.path == "/" || !mount_paths.insert(m.path.clone()) {
                    return Err(err(&mpath, "path must be a unique absolute path"));
                }
            }
            if c.files.len() > MAX_FILES {
                return Err(err(
                    format!("{path}.files"),
                    format!("at most {MAX_FILES} files"),
                ));
            }
            for (fi, f) in c.files.iter().enumerate() {
                let fpath = format!("{path}.files[{fi}]");
                let inside_mount = c.mounts.iter().any(|m| {
                    f.path == m.path
                        || f.path
                            .starts_with(&format!("{}/", m.path.trim_end_matches('/')))
                });
                if !abs_path(&f.path)
                    || f.path.ends_with('/')
                    || !mount_paths.insert(f.path.clone())
                {
                    return Err(err(
                        format!("{fpath}.path"),
                        "must be a unique absolute file path",
                    ));
                }
                if inside_mount {
                    return Err(err(
                        format!("{fpath}.path"),
                        "must not be inside a volume mount (the file would land in the app's data)",
                    ));
                }
                if f.content.is_empty()
                    || f.content.len() > MAX_FILE_BYTES
                    || f.content.contains('\0')
                {
                    return Err(err(
                        format!("{fpath}.content"),
                        format!("must be 1-{MAX_FILE_BYTES} bytes of text"),
                    ));
                }
            }
            check_resources(c.resources.as_ref())
                .map_err(|m| err(format!("{path}.resources"), m))?;
            if let Some(probe) = &c.probe {
                let ppath = format!("{path}.probe");
                if let Some(p) = &probe.path {
                    if !http_path(p) {
                        return Err(err(format!("{ppath}.path"), "must be an absolute URL path"));
                    }
                }
                if !tcp_ports.contains(&probe.port) {
                    return Err(err(
                        format!("{ppath}.port"),
                        format!("no TCP port `{}` on this container", probe.port),
                    ));
                }
                if probe.host_header && (!has_web || probe.path.is_none()) {
                    return Err(err(
                        format!("{ppath}.host_header"),
                        "needs an HTTP `path` and a `web` port",
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
        if self.ports().count() > MAX_PORTS {
            return Err(err("containers", format!("at most {MAX_PORTS} ports")));
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
        if let Some(web) = &self.web {
            match self.port(web) {
                Some(p) if p.protocol == Protocol::Tcp => {}
                Some(_) => return Err(err("web", "the web port must be TCP")),
                None => return Err(err("web", format!("no port `{web}`"))),
            }
        }
        if self.public_ports().len() > MAX_PUBLIC_PORTS {
            return Err(err(
                "containers",
                format!("at most {MAX_PUBLIC_PORTS} public ports (the web port included)"),
            ));
        }
        if let Some(creds) = &self.credentials {
            if let Some(user) = &creds.user {
                if !printable(user, 64) {
                    return Err(err("credentials.user", "must be 1-64 printable characters"));
                }
            }
            if self.secret(&creds.password).is_none() {
                return Err(err(
                    "credentials.password",
                    format!("no secret `{}`: declare it in `secrets`", creds.password),
                ));
            }
        }
        // Link syntax here; the self-link check needs the app's name (`Network::validate_for`).
        if let Some(n) = &self.network {
            n.validate_for("")?;
        }
        if self.to_text().len() > MAX_SPEC_BYTES {
            return Err(err(
                "spec",
                format!("larger than {MAX_SPEC_BYTES} bytes; use fewer or smaller files"),
            ));
        }
        Ok(())
    }
}

fn check_resources(r: Option<&Resources>) -> Result<(), String> {
    if let Some(r) = r {
        for (key, value) in [("cpu", &r.cpu), ("cpu_request", &r.cpu_request)] {
            if let Some(v) = value {
                if cpu_millis(v).is_none_or(|m| m == 0 || m > MAX_CPU_MILLIS) {
                    return Err(format!(
                        "{key} `{v}` must be like `500m` or `2` (at most 16)"
                    ));
                }
            }
        }
        for (key, value) in [("memory", &r.memory), ("memory_request", &r.memory_request)] {
            if let Some(v) = value {
                if binary_quantity(v).is_none_or(|b| !(4 << 20..=MAX_MEMORY_BYTES).contains(&b)) {
                    return Err(format!(
                        "{key} `{v}` must be like `256Mi` or `2Gi` (4Mi to 64Gi)"
                    ));
                }
            }
        }
    }
    let r = Resources::resolved(r);
    if cpu_millis(&r.cpu_request) > cpu_millis(&r.cpu_limit) {
        return Err("cpu_request is above cpu".into());
    }
    if binary_quantity(&r.memory_request) > binary_quantity(&r.memory_limit) {
        return Err("memory_request is above memory".into());
    }
    Ok(())
}
