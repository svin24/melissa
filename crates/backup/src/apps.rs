//! Each app's logical export: what [`crate::run`] backs up per app, and the cluster access it
//! needs ([`Cluster`]), as root with k3s's admin kubeconfig ([`Kubectl`]).
//!
//! An app's backup holds `app.json` ([`AppExport`]: the stored spec, the workspace labels and
//! ownership annotations, the volumes and where their data was), `secrets.json` (the values of
//! the app's generated Secrets: without them restored data comes back with unknown
//! credentials, PLAN Q35) and the volumes' data. A volume's data is found through its
//! PersistentVolume object's path (local-path's `hostPath`/`local`), never by guessing a
//! directory name, and only below k3s's local-path storage.
//!
//! Reading Secret values happens only here, in the root unit: the server's identities can't
//! read Secrets, and no agent tool reaches this crate.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use base64::Engine as _;
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{Namespace, PersistentVolume, PersistentVolumeClaim, Pod, Secret},
};
use melissa_appspec::{namespace_of, valid_app_name, AppSpec};
use melissa_cluster::render::{
    pvc_name, secret_object_name, APP_LABEL, ASSIGNED_AT_ANNOTATION, ASSIGNED_BY_ANNOTATION,
    DEVICE_LABEL, INSTALLED_AT_ANNOTATION, INSTALLED_BY_ANNOTATION, INSTALLED_FOR_ANNOTATION,
    ORIGIN_ANNOTATION, OWNER_AGENT_LABEL, SPEC_ANNOTATION, WORKSPACE_LABEL,
};
use serde::{Deserialize, Serialize};

use crate::BackupError;

/// Where local-path keeps volumes (k3s's default `--default-local-storage-path`).
pub const LOCAL_PATH_ROOT: &str = "/var/lib/rancher/k3s/storage";
pub const EXPORT_FORMAT: u32 = 1;
pub const EXPORT_FILE: &str = "app.json";
pub const SECRETS_FILE: &str = "secrets.json";

/// Labels and annotations of the app's namespace that travel with it (workspace, owner, who
/// installed it). Ports and the spec are handled separately.
pub const KEPT_LABELS: &[&str] = &[WORKSPACE_LABEL, DEVICE_LABEL, OWNER_AGENT_LABEL];
pub const KEPT_ANNOTATIONS: &[&str] = &[
    ASSIGNED_BY_ANNOTATION,
    ASSIGNED_AT_ANNOTATION,
    INSTALLED_BY_ANNOTATION,
    INSTALLED_AT_ANNOTATION,
    INSTALLED_FOR_ANNOTATION,
    ORIGIN_ANNOTATION,
];

/// One volume of an app and where its data is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Volume {
    /// The spec's volume name, or `pvc:<name>` for a claim no spec volume names (kept from an
    /// earlier spec).
    pub volume: String,
    pub pvc: String,
    pub path: PathBuf,
}

/// An app as the cluster has it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
    pub app: String,
    /// `melissa.io/spec` as stored.
    pub spec_text: Option<String>,
    pub labels: BTreeMap<String, String>,
    pub annotations: BTreeMap<String, String>,
    /// The Deployment's replicas (0: stopped, or no Deployment: uninstalled with data kept).
    pub replicas: i32,
    pub volumes: Vec<Volume>,
}

impl AppInfo {
    /// The stored spec, if it still validates.
    pub fn spec(&self) -> Option<AppSpec> {
        self.spec_text
            .as_deref()
            .and_then(|t| AppSpec::from_text(t).ok())
    }

    pub fn volume(&self, name: &str) -> Option<&Volume> {
        self.volumes.iter().find(|v| v.volume == name)
    }
}

/// `app.json` in an app's backup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppExport {
    pub format: u32,
    pub app: String,
    pub spec: Option<String>,
    pub labels: BTreeMap<String, String>,
    pub annotations: BTreeMap<String, String>,
    pub replicas: i32,
    /// `dump`, `stop`, `live` or `skip`: how this copy was made.
    pub mode: String,
    pub volumes: Vec<Volume>,
    /// Volumes whose data is not in this backup (excluded whole, or mode `skip`).
    #[serde(default)]
    pub volumes_left_out: Vec<String>,
    /// The dump's file name, when a dump snapshot belongs to this backup.
    #[serde(default)]
    pub dump: Option<String>,
}

impl AppExport {
    pub fn of(info: &AppInfo, mode: &str) -> Self {
        Self {
            format: EXPORT_FORMAT,
            app: info.app.clone(),
            spec: info.spec_text.clone(),
            labels: info.labels.clone(),
            annotations: info.annotations.clone(),
            replicas: info.replicas,
            mode: mode.into(),
            volumes: info.volumes.clone(),
            volumes_left_out: Vec::new(),
            dump: None,
        }
    }

    /// Read `app.json` from a restored snapshot. Everything in a backup is untrusted (a drive
    /// can be changed while it is away): every name and path is checked here before a restore
    /// uses it. Volume paths must be plain and inside `storage_root` (k3s's local-path
    /// storage, [`LOCAL_PATH_ROOT`] on a device).
    pub fn parse(text: &str, storage_root: &Path) -> Result<Self, BackupError> {
        let bad = |why: String| BackupError::Invalid(format!("app.json in the backup: {why}"));
        let e: Self = serde_json::from_str(text).map_err(|e| bad(e.to_string()))?;
        if e.format > EXPORT_FORMAT {
            return Err(BackupError::Invalid(format!(
                "the app backup was made by a newer Melissa (format {})",
                e.format
            )));
        }
        if !valid_app_name(&e.app) {
            return Err(bad("it names no valid app".into()));
        }
        if !["dump", "stop", "live", "skip"].contains(&e.mode.as_str()) {
            return Err(bad(format!("unknown mode `{}`", short(&e.mode))));
        }
        if !(0..=MAX_REPLICAS).contains(&e.replicas) {
            return Err(bad("replicas out of range".into()));
        }
        if e.spec.as_ref().is_some_and(|s| s.len() > MAX_SPEC_BYTES) {
            return Err(bad("the spec is too large".into()));
        }
        if e.volumes.len() > MAX_VOLUMES || e.volumes_left_out.len() > MAX_VOLUMES {
            return Err(bad("too many volumes".into()));
        }
        let root = storage_root;
        for (i, v) in e.volumes.iter().enumerate() {
            if !volume_name_ok(&v.volume) {
                return Err(bad(format!("bad volume name `{}`", short(&v.volume))));
            }
            if !k8s_name(&v.pvc, 253) {
                return Err(bad(format!("bad claim name `{}`", short(&v.pvc))));
            }
            if !volume_path_ok(&v.path, root) {
                return Err(bad(format!(
                    "volume {} is at {}, outside {}",
                    v.volume,
                    short(&v.path.to_string_lossy()),
                    root.display()
                )));
            }
            if e.volumes[..i]
                .iter()
                .any(|o| o.volume == v.volume || o.path == v.path)
            {
                return Err(bad(format!("volume {} is listed twice", v.volume)));
            }
        }
        if let Some(n) = e.volumes_left_out.iter().find(|n| !volume_name_ok(n)) {
            return Err(bad(format!("bad volume name `{}`", short(n))));
        }
        if let Some(d) = &e.dump {
            if !melissa_appspec::backup::file_name(d) {
                return Err(bad(format!("bad dump file name `{}`", short(d))));
            }
        }
        let labels_ok = e.labels.iter().all(|(k, v)| {
            KEPT_LABELS.contains(&k.as_str()) && v.len() <= 63 && !v.contains(char::is_control)
        });
        let annotations_ok = e.annotations.iter().all(|(k, v)| {
            KEPT_ANNOTATIONS.contains(&k.as_str()) && v.len() <= 1024 && !v.contains('\0')
        });
        if !labels_ok || !annotations_ok {
            return Err(bad("unexpected labels or annotations".into()));
        }
        Ok(e)
    }
}

/// Limits for what a backup's `app.json` may hold.
const MAX_REPLICAS: i32 = 100;
const MAX_SPEC_BYTES: usize = 256 * 1024;
const MAX_VOLUMES: usize = 64;
const MAX_SECRETS: usize = 64;
const MAX_SECRET_KEYS: usize = 64;

/// The first characters of an untrusted string, for an error message.
fn short(s: &str) -> String {
    s.chars()
        .take(80)
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// A Kubernetes object name: lowercase letters, digits, `-` and `.`, starting and ending
/// with a letter or digit.
fn k8s_name(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        && s.starts_with(|c: char| c.is_ascii_alphanumeric())
        && s.ends_with(|c: char| c.is_ascii_alphanumeric())
}

/// A spec volume name, or `pvc:<claim>` for a claim no spec volume names.
fn volume_name_ok(s: &str) -> bool {
    match s.strip_prefix("pvc:") {
        Some(claim) => k8s_name(claim, 253),
        None => k8s_name(s, 63) && !s.contains('.'),
    }
}

/// A Secret's data key (Kubernetes: `[-._a-zA-Z0-9]+`, at most 253).
fn secret_key_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b))
}

/// Read `secrets.json` from a restored snapshot: only the Secrets in `allowed` (the app's own
/// generated ones, from its spec on this device now), with well-formed keys and base64 values.
pub fn parse_secrets(text: &str, allowed: &[String]) -> Result<Vec<SecretData>, BackupError> {
    let bad = |why: String| BackupError::Invalid(format!("secrets in the backup: {why}"));
    let secrets: Vec<SecretData> = serde_json::from_str(text).map_err(|e| bad(e.to_string()))?;
    if secrets.len() > MAX_SECRETS {
        return Err(bad("too many".into()));
    }
    for (i, s) in secrets.iter().enumerate() {
        if !allowed.contains(&s.name) {
            return Err(bad(format!(
                "`{}` is not one of the app's secrets",
                short(&s.name)
            )));
        }
        if secrets[..i].iter().any(|o| o.name == s.name) {
            return Err(bad(format!("`{}` is listed twice", s.name)));
        }
        if s.data.len() > MAX_SECRET_KEYS {
            return Err(bad(format!("`{}` has too many keys", s.name)));
        }
        for (k, v) in &s.data {
            if !secret_key_ok(k) {
                return Err(bad(format!("`{}` has a bad key", s.name)));
            }
            if base64::engine::general_purpose::STANDARD.decode(v).is_err() {
                return Err(bad(format!("`{}` has a value that is not base64", s.name)));
            }
        }
    }
    Ok(secrets)
}

/// One Secret's values.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretData {
    pub name: String,
    /// Key → base64 of the value (as Kubernetes stores them).
    pub data: BTreeMap<String, String>,
}

impl std::fmt::Debug for SecretData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretData")
            .field("name", &self.name)
            .field("keys", &self.data.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// The names of an app's generated Secrets (from its spec).
pub fn secret_names(app: &str, spec: Option<&AppSpec>) -> Vec<String> {
    spec.map(|s| {
        s.secrets
            .iter()
            .map(|f| secret_object_name(app, &f.name))
            .collect()
    })
    .unwrap_or_default()
}

/// What the backup needs from the cluster. Implemented by [`Kubectl`]; tests use a fake.
pub trait Cluster {
    /// The names of all apps (namespaces labelled `melissa.io/app`).
    fn apps(&self) -> Result<Vec<String>, BackupError>;
    /// One app; `None` when it isn't there.
    fn app(&self, app: &str) -> Result<Option<AppInfo>, BackupError>;
    /// The values of these Secrets in the app's namespace (missing ones are left out).
    fn secrets(&self, app: &str, names: &[String]) -> Result<Vec<SecretData>, BackupError>;
    /// Create or replace these Secrets' values.
    fn apply_secrets(&self, app: &str, secrets: &[SecretData]) -> Result<(), BackupError>;
    /// Set the Deployment's replicas.
    fn scale(&self, app: &str, replicas: i32) -> Result<(), BackupError>;
    /// Wait until no pod of the app is left.
    fn wait_stopped(&self, app: &str, timeout: Duration) -> Result<(), BackupError>;
    /// The newest pod whose `container` is running and ready, waiting up to `timeout`
    /// (`Duration::ZERO`: just look).
    fn ready_pod(
        &self,
        app: &str,
        container: &str,
        timeout: Duration,
    ) -> Result<Option<String>, BackupError>;
    /// The argv that runs `command` in `container` of `pod` (stdin attached when `stdin`).
    fn exec_argv(
        &self,
        app: &str,
        pod: &str,
        container: &str,
        command: &[String],
        stdin: bool,
    ) -> Vec<String>;
}

/// [`Cluster`] over `kubectl` with k3s's admin kubeconfig (root only).
#[derive(Debug, Clone)]
pub struct Kubectl {
    pub kubectl: PathBuf,
    pub kubeconfig: PathBuf,
    /// Volumes outside this directory are refused.
    pub storage_root: PathBuf,
}

impl Kubectl {
    pub fn system() -> Self {
        Self {
            kubectl: crate::KUBECTL_BIN.into(),
            kubeconfig: crate::ADMIN_KUBECONFIG.into(),
            storage_root: LOCAL_PATH_ROOT.into(),
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.kubectl);
        cmd.env_clear()
            .env("PATH", "/usr/bin:/bin")
            .arg("--kubeconfig")
            .arg(&self.kubeconfig)
            .args(args)
            .stdin(Stdio::null());
        cmd
    }

    fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<String, BackupError> {
        let mut cmd = self.command(args);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let out = match stdin {
            None => cmd.output(),
            Some(data) => {
                cmd.stdin(Stdio::piped());
                let mut child = cmd
                    .spawn()
                    .map_err(|e| BackupError::Cluster(format!("cannot run kubectl: {e}")))?;
                if let Some(mut pipe) = child.stdin.take() {
                    use std::io::Write;
                    pipe.write_all(data)
                        .map_err(|e| BackupError::Cluster(format!("kubectl stdin: {e}")))?;
                }
                child.wait_with_output()
            }
        }
        .map_err(|e| BackupError::Cluster(format!("cannot run kubectl: {e}")))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(BackupError::Cluster(format!(
                "kubectl {}: {}",
                args.first().copied().unwrap_or_default(),
                err.trim().chars().take(400).collect::<String>()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// `kubectl get ... -o json` of one object; `None` when it doesn't exist.
    fn get<T: serde::de::DeserializeOwned>(&self, args: &[&str]) -> Result<Option<T>, BackupError> {
        let mut all = vec!["get"];
        all.extend_from_slice(args);
        all.extend_from_slice(&["--ignore-not-found", "-o", "json"]);
        let out = self.run(&all, None)?;
        if out.trim().is_empty() {
            return Ok(None);
        }
        serde_json::from_str(&out)
            .map(Some)
            .map_err(|e| BackupError::Cluster(format!("reading {}: {e}", args.join(" "))))
    }

    /// `kubectl get <kind> -o json` items.
    fn list<T: serde::de::DeserializeOwned>(&self, args: &[&str]) -> Result<Vec<T>, BackupError> {
        #[derive(Deserialize)]
        struct List<T> {
            items: Vec<T>,
        }
        let mut all = vec!["get"];
        all.extend_from_slice(args);
        all.extend_from_slice(&["-o", "json"]);
        let out = self.run(&all, None)?;
        let list: List<T> = serde_json::from_str(&out)
            .map_err(|e| BackupError::Cluster(format!("reading {}: {e}", args.join(" "))))?;
        Ok(list.items)
    }

    fn pods(&self, app: &str) -> Result<Vec<Pod>, BackupError> {
        self.list(&["pods", "-n", &namespace_of(app)])
    }
}

/// The data directory of a PersistentVolume (local-path: `hostPath` or `local`).
pub fn pv_path(pv: &PersistentVolume) -> Option<PathBuf> {
    let spec = pv.spec.as_ref()?;
    spec.host_path
        .as_ref()
        .map(|h| h.path.clone())
        .or_else(|| spec.local.as_ref().map(|l| l.path.clone()))
        .map(PathBuf::from)
}

/// A volume path is usable: plain, absolute and inside `root`.
pub fn volume_path_ok(path: &Path, root: &Path) -> bool {
    crate::config::plain_absolute(path) && path.starts_with(root) && path != root
}

fn container_ready(pod: &Pod, container: &str) -> bool {
    pod.metadata.deletion_timestamp.is_none()
        && pod
            .status
            .as_ref()
            .and_then(|s| s.container_statuses.as_ref())
            .is_some_and(|cs| {
                cs.iter().any(|c| {
                    c.name == container
                        && c.ready
                        && c.state.as_ref().is_some_and(|s| s.running.is_some())
                })
            })
}

impl Cluster for Kubectl {
    fn apps(&self) -> Result<Vec<String>, BackupError> {
        let namespaces: Vec<Namespace> = self.list(&["namespaces", "-l", APP_LABEL])?;
        let mut apps: Vec<String> = namespaces
            .iter()
            .filter_map(|ns| {
                let name = ns.metadata.name.as_deref()?;
                let app = ns.metadata.labels.as_ref()?.get(APP_LABEL)?;
                (valid_app_name(app) && name == namespace_of(app)).then(|| app.clone())
            })
            .collect();
        apps.sort();
        apps.dedup();
        Ok(apps)
    }

    fn app(&self, app: &str) -> Result<Option<AppInfo>, BackupError> {
        if !valid_app_name(app) {
            return Err(BackupError::Invalid(format!("`{app}` is not an app name")));
        }
        let ns_name = namespace_of(app);
        let Some(ns) = self.get::<Namespace>(&["namespace", &ns_name])? else {
            return Ok(None);
        };
        let labels = ns.metadata.labels.clone().unwrap_or_default();
        let annotations = ns.metadata.annotations.clone().unwrap_or_default();
        let spec_text = annotations.get(SPEC_ANNOTATION).cloned();
        let deployment = self.get::<Deployment>(&["deployment", app, "-n", &ns_name])?;
        let replicas = deployment.and_then(|d| d.spec.replicas).unwrap_or(0);
        let spec = spec_text
            .as_deref()
            .and_then(|t| AppSpec::from_text(t).ok());
        let claims: Vec<PersistentVolumeClaim> =
            self.list(&["persistentvolumeclaims", "-n", &ns_name])?;
        let mut volumes = Vec::new();
        for claim in claims {
            let Some(pvc) = claim.metadata.name.clone() else {
                continue;
            };
            let Some(pv_name) = claim.spec.as_ref().and_then(|s| s.volume_name.clone()) else {
                // Not bound yet: no data.
                continue;
            };
            let Some(pv) = self.get::<PersistentVolume>(&["persistentvolume", &pv_name])? else {
                continue;
            };
            let path = pv_path(&pv).ok_or_else(|| {
                BackupError::Cluster(format!("volume {pvc} of {app} has no local path"))
            })?;
            if !volume_path_ok(&path, &self.storage_root) {
                return Err(BackupError::Cluster(format!(
                    "volume {pvc} of {app} is at {}, outside {}",
                    path.display(),
                    self.storage_root.display()
                )));
            }
            let volume = spec
                .as_ref()
                .and_then(|s| s.volumes.iter().find(|v| pvc_name(app, &v.name) == pvc))
                .map_or_else(|| format!("pvc:{pvc}"), |v| v.name.clone());
            volumes.push(Volume { volume, pvc, path });
        }
        volumes.sort_by(|a, b| a.volume.cmp(&b.volume));
        Ok(Some(AppInfo {
            app: app.into(),
            spec_text,
            labels: labels
                .into_iter()
                .filter(|(k, _)| KEPT_LABELS.contains(&k.as_str()))
                .collect(),
            annotations: annotations
                .into_iter()
                .filter(|(k, _)| KEPT_ANNOTATIONS.contains(&k.as_str()))
                .collect(),
            replicas,
            volumes,
        }))
    }

    fn secrets(&self, app: &str, names: &[String]) -> Result<Vec<SecretData>, BackupError> {
        let ns = namespace_of(app);
        let mut out = Vec::new();
        for name in names {
            let Some(secret) = self.get::<Secret>(&["secret", name, "-n", &ns])? else {
                continue;
            };
            let data = secret
                .data
                .unwrap_or_default()
                .into_iter()
                .map(|(k, v)| (k, base64::engine::general_purpose::STANDARD.encode(v.0)))
                .collect();
            out.push(SecretData {
                name: name.clone(),
                data,
            });
        }
        Ok(out)
    }

    fn apply_secrets(&self, app: &str, secrets: &[SecretData]) -> Result<(), BackupError> {
        let ns = namespace_of(app);
        for s in secrets {
            let object = serde_json::json!({
                "apiVersion": "v1",
                "kind": "Secret",
                "metadata": {
                    "name": s.name,
                    "namespace": ns,
                    "labels": {APP_LABEL: app},
                },
                "type": "Opaque",
                "data": s.data,
            });
            let body =
                serde_json::to_vec(&object).map_err(|e| BackupError::Failed(e.to_string()))?;
            self.run(
                &[
                    "apply",
                    "--server-side",
                    "--force-conflicts",
                    "--field-manager=melissa-backup",
                    "-f",
                    "-",
                ],
                Some(&body),
            )?;
        }
        Ok(())
    }

    fn scale(&self, app: &str, replicas: i32) -> Result<(), BackupError> {
        let r = format!("--replicas={replicas}");
        let d = format!("deployment/{app}");
        self.run(&["scale", "-n", &namespace_of(app), &d, &r], None)
            .map(|_| ())
    }

    fn wait_stopped(&self, app: &str, timeout: Duration) -> Result<(), BackupError> {
        let until = Instant::now() + timeout;
        loop {
            if self.pods(app)?.is_empty() {
                return Ok(());
            }
            if Instant::now() >= until {
                return Err(BackupError::Cluster(format!(
                    "{app} did not stop within {} s",
                    timeout.as_secs()
                )));
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    }

    fn ready_pod(
        &self,
        app: &str,
        container: &str,
        timeout: Duration,
    ) -> Result<Option<String>, BackupError> {
        let until = Instant::now() + timeout;
        loop {
            let mut pods: Vec<Pod> = self
                .pods(app)?
                .into_iter()
                .filter(|p| container_ready(p, container))
                .collect();
            pods.sort_by_key(|p| p.metadata.creation_timestamp.clone().map(|t| t.0));
            if let Some(name) = pods.last().and_then(|p| p.metadata.name.clone()) {
                return Ok(Some(name));
            }
            if Instant::now() >= until {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_secs(3));
        }
    }

    fn exec_argv(
        &self,
        app: &str,
        pod: &str,
        container: &str,
        command: &[String],
        stdin: bool,
    ) -> Vec<String> {
        let mut argv = vec![
            self.kubectl.to_string_lossy().into_owned(),
            "--kubeconfig".into(),
            self.kubeconfig.to_string_lossy().into_owned(),
            "exec".into(),
        ];
        if stdin {
            argv.push("-i".into());
        }
        argv.extend([
            "-n".into(),
            namespace_of(app),
            pod.into(),
            "-c".into(),
            container.into(),
            "--".into(),
        ]);
        argv.extend(command.iter().cloned());
        argv
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_paths_come_from_the_pv_and_stay_in_storage() {
        let pv: PersistentVolume = serde_json::from_value(serde_json::json!({
            "metadata": {"name": "pvc-1"},
            "spec": {"hostPath": {"path": "/var/lib/rancher/k3s/storage/pvc-1_app-x_x-data"}}
        }))
        .unwrap();
        let p = pv_path(&pv).unwrap();
        let root = Path::new(LOCAL_PATH_ROOT);
        assert!(volume_path_ok(&p, root));
        let local: PersistentVolume = serde_json::from_value(serde_json::json!({
            "spec": {"local": {"path": "/var/lib/rancher/k3s/storage/pvc-2"}}
        }))
        .unwrap();
        assert!(pv_path(&local).is_some());
        for bad in [
            "/etc",
            "/var/lib/rancher/k3s/storage",
            "/var/lib/rancher/k3s/storage/../server",
            "relative",
        ] {
            assert!(!volume_path_ok(Path::new(bad), root), "{bad}");
        }
    }

    #[test]
    fn exec_goes_through_the_admin_kubeconfig_into_one_container() {
        let k = Kubectl::system();
        let argv = k.exec_argv("db", "db-123", "pg", &["pg_dumpall".into()], true);
        assert_eq!(
            argv.join(" "),
            "/usr/bin/kubectl --kubeconfig /etc/rancher/k3s/k3s.yaml exec -i -n app-db db-123 -c pg -- pg_dumpall"
        );
    }

    #[test]
    fn exports_parse_strictly_and_secrets_never_print() {
        let info = AppInfo {
            app: "notes".into(),
            spec_text: None,
            labels: BTreeMap::new(),
            annotations: BTreeMap::new(),
            replicas: 1,
            volumes: vec![],
        };
        let e = AppExport::of(&info, "stop");
        let text = serde_json::to_string(&e).unwrap();
        let root = Path::new(LOCAL_PATH_ROOT);
        assert_eq!(AppExport::parse(&text, root).unwrap(), e);
        assert!(AppExport::parse(&text.replace("\"format\":1", "\"format\":7"), root).is_err());
        assert!(AppExport::parse(&text.replace("notes", "Not An App"), root).is_err());
        let s = SecretData {
            name: "notes-secret-x".into(),
            data: BTreeMap::from([("x".into(), "c2VjcmV0".into())]),
        };
        let dbg = format!("{s:?}");
        assert!(!dbg.contains("c2VjcmV0"), "{dbg}");
    }

    fn export_json(volumes: serde_json::Value, extra: serde_json::Value) -> String {
        let mut v = serde_json::json!({
            "format": 1, "app": "notes", "spec": null, "labels": {}, "annotations": {},
            "replicas": 1, "mode": "stop", "volumes": volumes,
        });
        if let (Some(o), Some(x)) = (v.as_object_mut(), extra.as_object()) {
            o.extend(x.clone());
        }
        v.to_string()
    }

    #[test]
    fn a_tampered_app_json_is_refused() {
        let root = Path::new(LOCAL_PATH_ROOT);
        let ok_vol = serde_json::json!([{"volume": "data", "pvc": "notes-data",
            "path": "/var/lib/rancher/k3s/storage/pvc-1_app-notes_notes-data"}]);
        AppExport::parse(&export_json(ok_vol.clone(), serde_json::json!({})), root).unwrap();
        let evil_volumes = [
            // Root's directories into an app volume (or the other way round).
            serde_json::json!([{"volume": "data", "pvc": "notes-data", "path": "/etc"}]),
            serde_json::json!([{"volume": "data", "pvc": "notes-data", "path": "/root/.ssh"}]),
            serde_json::json!([{"volume": "data", "pvc": "notes-data",
                "path": "/var/lib/rancher/k3s/storage/../server"}]),
            serde_json::json!([{"volume": "data", "pvc": "notes-data",
                "path": "/var/lib/rancher/k3s/storage"}]),
            serde_json::json!([{"volume": "data", "pvc": "notes-data",
                "path": "var/lib/rancher/k3s/storage/x"}]),
            serde_json::json!([{"volume": "../x", "pvc": "notes-data",
                "path": "/var/lib/rancher/k3s/storage/x"}]),
            serde_json::json!([{"volume": "data", "pvc": "../../x",
                "path": "/var/lib/rancher/k3s/storage/x"}]),
            serde_json::json!([
                {"volume": "data", "pvc": "a", "path": "/var/lib/rancher/k3s/storage/x"},
                {"volume": "db", "pvc": "b", "path": "/var/lib/rancher/k3s/storage/x"}]),
        ];
        for v in evil_volumes {
            assert!(
                AppExport::parse(&export_json(v.clone(), serde_json::json!({})), root).is_err(),
                "{v}"
            );
        }
        for extra in [
            serde_json::json!({"mode": "sh -c reboot"}),
            serde_json::json!({"replicas": 100000}),
            serde_json::json!({"replicas": -1}),
            serde_json::json!({"dump": "../../etc/shadow"}),
            serde_json::json!({"volumes_left_out": ["../x"]}),
            serde_json::json!({"labels": {"melissa.io/app": "other"}}),
            serde_json::json!({"annotations": {"kubectl.kubernetes.io/x": "y"}}),
            serde_json::json!({"app": "../etc"}),
        ] {
            assert!(
                AppExport::parse(&export_json(ok_vol.clone(), extra.clone()), root).is_err(),
                "{extra}"
            );
        }
    }

    #[test]
    fn a_tampered_secrets_json_is_refused() {
        let allowed = vec!["notes-secret-admin-password".to_string()];
        let good = r#"[{"name":"notes-secret-admin-password","data":{"admin_password":"czNj"}}]"#;
        assert_eq!(parse_secrets(good, &allowed).unwrap().len(), 1);
        for evil in [
            // Another app's or a system Secret.
            r#"[{"name":"k3s-serving","data":{}}]"#,
            r#"[{"name":"../x","data":{}}]"#,
            r#"[{"name":"notes-secret-admin-password","data":{"../k":"czNj"}}]"#,
            r#"[{"name":"notes-secret-admin-password","data":{"k":"not base64!"}}]"#,
            r#"[{"name":"notes-secret-admin-password","data":{}},
                {"name":"notes-secret-admin-password","data":{}}]"#,
            r#"{"name":"x"}"#,
        ] {
            assert!(parse_secrets(evil, &allowed).is_err(), "{evil}");
        }
    }

    #[test]
    fn ready_means_running_ready_and_not_deleting() {
        let pod: Pod = serde_json::from_value(serde_json::json!({
            "metadata": {"name": "p"},
            "status": {"containerStatuses": [{"name": "db", "ready": true, "restartCount": 0,
                "image": "x", "imageID": "", "state": {"running": {}}}]}
        }))
        .unwrap();
        assert!(container_ready(&pod, "db"));
        assert!(!container_ready(&pod, "web"));
    }
}
