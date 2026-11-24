//! Cluster access for the app tools, split in two traits:
//!
//! - [`AppRead`]: what `app_status` and `app_logs` need (objects and logs in one app's
//!   namespace). No writes.
//! - [`AppDeployer`]: server-side apply of rendered objects, create-once Secrets, deletes,
//!   and the replica patch of `stop_app`/`start_app`. Only the changing app tools hold one.
//! - [`AppExec`]: run one command in an app's pod (`app_exec`, `pods/exec`).
//!
//! All three are implemented by [`KubeApps`] over the write identity's kubeconfig
//! (`melissa/melissa-apps`, RBAC in `apps-rbac.yaml`). `cluster_health` and other read-only
//! tools keep using the read-only identity through [`crate::ClusterRead`].

use std::{future::Future, time::Duration};

use k8s_openapi::{
    api::{
        apps::v1::Deployment,
        core::v1::{ConfigMap, Namespace, PersistentVolumeClaim, Pod, Secret, Service},
        networking::v1::{Ingress, NetworkPolicy},
        node::v1::RuntimeClass,
        rbac::v1::RoleBinding,
    },
    apimachinery::pkg::apis::meta::v1::Status,
    serde::{de::DeserializeOwned, Serialize},
    NamespaceResourceScope,
};
use kube::{
    api::{
        Api, AttachParams, AttachedProcess, DeleteParams, ListParams, LogParams, Patch,
        PatchParams, PostParams,
    },
    Resource,
};
use melissa_agent::BoxFuture;
use melissa_appspec::{namespace_of, valid_app_name};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use crate::{
    cluster::map_err,
    render::{files_name, port_service_name, APP_LABEL, FIELD_MANAGER},
    ClusterError, LazyCluster,
};

/// One rendered object to apply. A handful exist per install, briefly, so variant sizes
/// don't matter.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum AppObject {
    Namespace(Namespace),
    RoleBinding(RoleBinding),
    Volume(PersistentVolumeClaim),
    ConfigMap(ConfigMap),
    Deployment(Box<Deployment>),
    Service(Service),
    Ingress(Ingress),
    /// In the app's namespace, or a link's target side in another app's namespace.
    NetworkPolicy(NetworkPolicy),
}

impl AppObject {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Namespace(_) => "Namespace",
            Self::RoleBinding(_) => "RoleBinding",
            Self::Volume(_) => "PersistentVolumeClaim",
            Self::ConfigMap(_) => "ConfigMap",
            Self::Deployment(_) => "Deployment",
            Self::Service(_) => "Service",
            Self::Ingress(_) => "Ingress",
            Self::NetworkPolicy(_) => "NetworkPolicy",
        }
    }
}

/// Kinds of app objects an update may delete by name (what an earlier spec had and the new
/// one doesn't).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedKind {
    Ingress,
    Service,
    ConfigMap,
    /// Only the setup password reader's binding (`render::CREDENTIALS_BINDING`).
    RoleBinding,
    /// A link's target side (`render::network::link_policy`) that is no longer wanted.
    NetworkPolicy,
}

impl NamedKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ingress => "Ingress",
            Self::Service => "Service",
            Self::ConfigMap => "ConfigMap",
            Self::RoleBinding => "RoleBinding",
            Self::NetworkPolicy => "NetworkPolicy",
        }
    }
}

/// Log request limits (enforced again by the API server).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRequest {
    pub pod: String,
    pub container: String,
    pub tail_lines: i64,
    pub limit_bytes: i64,
    pub previous: bool,
}

/// Read access to one app's namespace.
pub trait AppRead: Send + Sync {
    /// The names of all apps (namespaces `app-<name>` labelled `melissa.io/app=<name>`),
    /// sorted, including ones being deleted or uninstalled with their data kept.
    fn app_names(&self) -> BoxFuture<'_, Result<Vec<String>, ClusterError>>;
    /// The app's namespace, if it exists.
    fn namespace(&self, app: &str) -> BoxFuture<'_, Result<Option<Namespace>, ClusterError>>;
    fn deployment(&self, app: &str) -> BoxFuture<'_, Result<Option<Deployment>, ClusterError>>;
    fn ingress(&self, app: &str) -> BoxFuture<'_, Result<Option<Ingress>, ClusterError>>;
    fn volumes(&self, app: &str)
        -> BoxFuture<'_, Result<Vec<PersistentVolumeClaim>, ClusterError>>;
    fn pods(&self, app: &str) -> BoxFuture<'_, Result<Vec<Pod>, ClusterError>>;
    fn logs<'a>(
        &'a self,
        app: &'a str,
        request: &'a LogRequest,
    ) -> BoxFuture<'a, Result<String, ClusterError>>;
    /// The handler of the cluster's RuntimeClass `name` (`runsc` for `gvisor`); `None` when
    /// there is no such RuntimeClass (gVisor not set up on this device).
    fn runtime_class_handler<'a>(
        &'a self,
        name: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, ClusterError>>;
}

/// One command to run in a container of an app's pod (`app_exec`). No shell is involved: the
/// API server hands `command` to the container runtime as the argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecRequest {
    pub pod: String,
    pub container: String,
    pub command: Vec<String>,
    /// Written to the command's stdin, which is then closed. `None`: no stdin at all.
    pub stdin: Option<String>,
    /// After this long the call returns what it has (the command may keep running).
    pub timeout: Duration,
    /// At most this many bytes of stdout and of stderr are kept; the rest is read and dropped.
    pub max_output: usize,
}

/// What a command printed and how it ended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    /// The exit code, when the API server reported one.
    pub exit_code: Option<i32>,
    /// The call gave up after [`ExecRequest::timeout`].
    pub timed_out: bool,
    /// The API server's message when the command could not run or failed without an exit code
    /// (e.g. "executable file not found").
    pub error: Option<String>,
}

/// Running commands in an app's pods (`pods/exec`, bound per app namespace like the rest).
pub trait AppExec: Send + Sync {
    fn exec<'a>(
        &'a self,
        app: &'a str,
        request: &'a ExecRequest,
    ) -> BoxFuture<'a, Result<ExecOutput, ClusterError>>;
}

/// Write access, limited by RBAC to `app-*` namespaces.
pub trait AppDeployer: Send + Sync {
    /// Server-side apply (field manager `melissa`, forced).
    fn apply<'a>(&'a self, object: &'a AppObject) -> BoxFuture<'a, Result<(), ClusterError>>;
    /// Create `secret` unless one with its name exists; `Ok(false)` = kept the existing one.
    /// Never reads or overwrites an existing Secret.
    fn create_secret<'a>(&'a self, secret: &'a Secret)
        -> BoxFuture<'a, Result<bool, ClusterError>>;
    /// Delete the app's Deployment, Services (incl. the port Service) and Ingress (missing
    /// ones are fine). Keeps the namespace (with its ports and spec), PVCs and Secrets. Returns the
    /// kinds that were deleted.
    fn delete_workload<'a>(
        &'a self,
        app: &'a str,
    ) -> BoxFuture<'a, Result<Vec<&'static str>, ClusterError>>;
    /// Delete one object in the app's namespace; `Ok(false)` if it was not there.
    fn delete_named<'a>(
        &'a self,
        app: &'a str,
        kind: NamedKind,
        name: &'a str,
    ) -> BoxFuture<'a, Result<bool, ClusterError>>;
    /// Delete the app's namespace and everything in it, including its volumes' data.
    fn delete_namespace<'a>(&'a self, app: &'a str) -> BoxFuture<'a, Result<bool, ClusterError>>;
    /// Force delete only: delete every pod in the app's namespace at once (grace period 0).
    /// Returns how many were deleted.
    fn force_delete_pods<'a>(&'a self, app: &'a str) -> BoxFuture<'a, Result<usize, ClusterError>>;
    /// Force delete only: drop the finalizers of the app's volumes that are being deleted
    /// (their data goes with the namespace anyway). Returns how many were changed.
    fn clear_volume_finalizers<'a>(
        &'a self,
        app: &'a str,
    ) -> BoxFuture<'a, Result<usize, ClusterError>>;
    /// Merge-patch the app's Deployment (stop/start: replicas and the
    /// replicas-before-stop annotation).
    fn patch_deployment<'a>(
        &'a self,
        app: &'a str,
        patch: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<(), ClusterError>>;
    /// Merge-patch the app's namespace (the reassign path: owner label and annotations).
    fn patch_namespace<'a>(
        &'a self,
        app: &'a str,
        patch: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<(), ClusterError>>;
}

/// [`AppRead`] + [`AppDeployer`] over the write identity's kubeconfig, found lazily like the
/// lead agent's read-only one.
pub struct KubeApps {
    lazy: LazyCluster,
}

impl KubeApps {
    pub fn new(lazy: LazyCluster) -> Self {
        Self { lazy }
    }

    async fn client(&self) -> Result<kube::Client, ClusterError> {
        Ok(self.lazy.cluster().await?.client().clone())
    }

    async fn ns_api<K>(&self, app: &str) -> Result<Api<K>, ClusterError>
    where
        K: Resource<Scope = NamespaceResourceScope, DynamicType = ()>,
    {
        Ok(Api::namespaced(self.client().await?, &namespace_of(app)))
    }
}

fn is_status(e: &kube::Error, code: u16) -> bool {
    matches!(e, kube::Error::Api(s) if s.code == code)
}

/// `Ok(None)` for 404.
pub(crate) fn optional<T>(r: Result<T, kube::Error>) -> Result<Option<T>, ClusterError> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(e) if is_status(&e, 404) => Ok(None),
        Err(e) => Err(map_err(e)),
    }
}

/// Retry `f` while it is forbidden: a fresh RoleBinding takes a moment to reach the API
/// server's authorizer cache.
pub(crate) async fn retry_forbidden<T, F, Fut>(mut f: F) -> Result<T, ClusterError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ClusterError>>,
{
    let mut delay = Duration::from_millis(200);
    for _ in 0..8 {
        match f().await {
            Err(ClusterError::Forbidden(_)) => {
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(2));
            }
            other => return other,
        }
    }
    f().await
}

async fn apply_namespaced<K>(client: &kube::Client, object: &K) -> Result<(), ClusterError>
where
    K: Resource<Scope = NamespaceResourceScope, DynamicType = ()>
        + Clone
        + Serialize
        + DeserializeOwned
        + std::fmt::Debug,
{
    let meta = object.meta();
    let (Some(ns), Some(name)) = (meta.namespace.as_deref(), meta.name.as_deref()) else {
        return Err(ClusterError::Api("object without namespace or name".into()));
    };
    let api: Api<K> = Api::namespaced(client.clone(), ns);
    retry_forbidden(|| async {
        api.patch(
            name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(object),
        )
        .await
        .map(|_| ())
        .map_err(map_err)
    })
    .await
}

impl AppRead for KubeApps {
    fn app_names(&self) -> BoxFuture<'_, Result<Vec<String>, ClusterError>> {
        Box::pin(async move {
            let api: Api<Namespace> = Api::all(self.client().await?);
            let list = api
                .list_metadata(&ListParams::default().labels(APP_LABEL))
                .await
                .map_err(map_err)?;
            let mut names: Vec<String> = list
                .items
                .iter()
                .filter_map(|ns| {
                    let name = ns.metadata.name.as_deref()?.strip_prefix("app-")?;
                    let label = ns.metadata.labels.as_ref()?.get(APP_LABEL)?;
                    (label == name && valid_app_name(name)).then(|| name.to_string())
                })
                .collect();
            names.sort();
            Ok(names)
        })
    }

    fn namespace(&self, app: &str) -> BoxFuture<'_, Result<Option<Namespace>, ClusterError>> {
        let name = namespace_of(app);
        Box::pin(async move {
            let api: Api<Namespace> = Api::all(self.client().await?);
            optional(api.get(&name).await)
        })
    }

    fn deployment(&self, app: &str) -> BoxFuture<'_, Result<Option<Deployment>, ClusterError>> {
        let app = app.to_string();
        Box::pin(async move { optional(self.ns_api::<Deployment>(&app).await?.get(&app).await) })
    }

    fn ingress(&self, app: &str) -> BoxFuture<'_, Result<Option<Ingress>, ClusterError>> {
        let app = app.to_string();
        Box::pin(async move { optional(self.ns_api::<Ingress>(&app).await?.get(&app).await) })
    }

    fn volumes(
        &self,
        app: &str,
    ) -> BoxFuture<'_, Result<Vec<PersistentVolumeClaim>, ClusterError>> {
        let app = app.to_string();
        Box::pin(async move {
            let api = self.ns_api::<PersistentVolumeClaim>(&app).await?;
            let lp = ListParams::default().labels(&format!("{APP_LABEL}={app}"));
            Ok(api.list(&lp).await.map_err(map_err)?.items)
        })
    }

    fn pods(&self, app: &str) -> BoxFuture<'_, Result<Vec<Pod>, ClusterError>> {
        let app = app.to_string();
        Box::pin(async move {
            let api = self.ns_api::<Pod>(&app).await?;
            let lp = ListParams::default().labels(&format!("{APP_LABEL}={app}"));
            Ok(api.list(&lp).await.map_err(map_err)?.items)
        })
    }

    fn logs<'a>(
        &'a self,
        app: &'a str,
        request: &'a LogRequest,
    ) -> BoxFuture<'a, Result<String, ClusterError>> {
        Box::pin(async move {
            let api = self.ns_api::<Pod>(app).await?;
            let params = LogParams {
                container: Some(request.container.clone()),
                tail_lines: Some(request.tail_lines),
                limit_bytes: Some(request.limit_bytes),
                previous: request.previous,
                ..LogParams::default()
            };
            api.logs(&request.pod, &params).await.map_err(map_err)
        })
    }

    fn runtime_class_handler<'a>(
        &'a self,
        name: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, ClusterError>> {
        Box::pin(async move {
            let api: Api<RuntimeClass> = Api::all(self.client().await?);
            Ok(optional(api.get(name).await)?.map(|rc| rc.handler))
        })
    }
}

impl AppDeployer for KubeApps {
    fn apply<'a>(&'a self, object: &'a AppObject) -> BoxFuture<'a, Result<(), ClusterError>> {
        Box::pin(async move {
            let client = self.client().await?;
            match object {
                AppObject::Namespace(ns) => {
                    let name = ns.metadata.name.clone().unwrap_or_default();
                    let api: Api<Namespace> = Api::all(client);
                    api.patch(
                        &name,
                        &PatchParams::apply(FIELD_MANAGER).force(),
                        &Patch::Apply(ns),
                    )
                    .await
                    .map(|_| ())
                    .map_err(map_err)
                }
                AppObject::RoleBinding(o) => apply_namespaced(&client, o).await,
                AppObject::Volume(o) => apply_namespaced(&client, o).await,
                AppObject::ConfigMap(o) => apply_namespaced(&client, o).await,
                AppObject::Deployment(o) => apply_namespaced(&client, o.as_ref()).await,
                AppObject::Service(o) => apply_namespaced(&client, o).await,
                AppObject::Ingress(o) => apply_namespaced(&client, o).await,
                AppObject::NetworkPolicy(o) => apply_namespaced(&client, o).await,
            }
        })
    }

    fn create_secret<'a>(
        &'a self,
        secret: &'a Secret,
    ) -> BoxFuture<'a, Result<bool, ClusterError>> {
        Box::pin(async move {
            let ns = secret.metadata.namespace.clone().unwrap_or_default();
            let api: Api<Secret> = Api::namespaced(self.client().await?, &ns);
            let params = PostParams {
                field_manager: Some(FIELD_MANAGER.into()),
                ..PostParams::default()
            };
            retry_forbidden(|| async {
                match api.create(&params, secret).await {
                    Ok(_) => Ok(true),
                    // The app was installed before: keep its secrets (the data was set up
                    // with them). The write identity cannot read Secrets, by design.
                    Err(e) if is_status(&e, 409) => Ok(false),
                    Err(e) => Err(map_err(e)),
                }
            })
            .await
        })
    }

    fn delete_workload<'a>(
        &'a self,
        app: &'a str,
    ) -> BoxFuture<'a, Result<Vec<&'static str>, ClusterError>> {
        Box::pin(async move {
            let dp = DeleteParams::background();
            let mut deleted = Vec::new();
            let ingress = self.ns_api::<Ingress>(app).await?;
            if optional(ingress.delete(app, &dp).await)?.is_some() {
                deleted.push("Ingress");
            }
            let service = self.ns_api::<Service>(app).await?;
            if optional(service.delete(app, &dp).await)?.is_some() {
                deleted.push("Service");
            }
            if optional(service.delete(&port_service_name(app), &dp).await)?.is_some() {
                deleted.push("Service (device port)");
            }
            let deployment = self.ns_api::<Deployment>(app).await?;
            if optional(deployment.delete(app, &dp).await)?.is_some() {
                deleted.push("Deployment");
            }
            let files = self.ns_api::<ConfigMap>(app).await?;
            if optional(files.delete(&files_name(app), &dp).await)?.is_some() {
                deleted.push("ConfigMap");
            }
            Ok(deleted)
        })
    }

    fn delete_named<'a>(
        &'a self,
        app: &'a str,
        kind: NamedKind,
        name: &'a str,
    ) -> BoxFuture<'a, Result<bool, ClusterError>> {
        Box::pin(async move {
            let dp = DeleteParams::background();
            Ok(match kind {
                NamedKind::Ingress => {
                    optional(self.ns_api::<Ingress>(app).await?.delete(name, &dp).await)?.is_some()
                }
                NamedKind::Service => {
                    optional(self.ns_api::<Service>(app).await?.delete(name, &dp).await)?.is_some()
                }
                NamedKind::ConfigMap => {
                    optional(self.ns_api::<ConfigMap>(app).await?.delete(name, &dp).await)?
                        .is_some()
                }
                NamedKind::RoleBinding => optional(
                    self.ns_api::<RoleBinding>(app)
                        .await?
                        .delete(name, &dp)
                        .await,
                )?
                .is_some(),
                NamedKind::NetworkPolicy => optional(
                    self.ns_api::<NetworkPolicy>(app)
                        .await?
                        .delete(name, &dp)
                        .await,
                )?
                .is_some(),
            })
        })
    }

    fn delete_namespace<'a>(&'a self, app: &'a str) -> BoxFuture<'a, Result<bool, ClusterError>> {
        Box::pin(async move {
            let api: Api<Namespace> = Api::all(self.client().await?);
            Ok(optional(
                api.delete(&namespace_of(app), &DeleteParams::background())
                    .await,
            )?
            .is_some())
        })
    }

    fn force_delete_pods<'a>(&'a self, app: &'a str) -> BoxFuture<'a, Result<usize, ClusterError>> {
        Box::pin(async move {
            let api = self.ns_api::<Pod>(app).await?;
            let pods = api
                .list(&ListParams::default())
                .await
                .map_err(map_err)?
                .items;
            let now = DeleteParams {
                grace_period_seconds: Some(0),
                ..DeleteParams::background()
            };
            let mut n = 0;
            for name in pods.iter().filter_map(|p| p.metadata.name.as_deref()) {
                if optional(api.delete(name, &now).await)?.is_some() {
                    n += 1;
                }
            }
            Ok(n)
        })
    }

    fn clear_volume_finalizers<'a>(
        &'a self,
        app: &'a str,
    ) -> BoxFuture<'a, Result<usize, ClusterError>> {
        Box::pin(async move {
            let api = self.ns_api::<PersistentVolumeClaim>(app).await?;
            let claims = api
                .list(&ListParams::default())
                .await
                .map_err(map_err)?
                .items;
            let params = PatchParams {
                field_manager: Some(FIELD_MANAGER.into()),
                ..PatchParams::default()
            };
            let patch = serde_json::json!({"metadata": {"finalizers": null}});
            let mut n = 0;
            for c in &claims {
                let stuck = c.metadata.deletion_timestamp.is_some()
                    && c.metadata
                        .finalizers
                        .as_ref()
                        .is_some_and(|f| !f.is_empty());
                let Some(name) = c.metadata.name.as_deref().filter(|_| stuck) else {
                    continue;
                };
                if optional(api.patch(name, &params, &Patch::Merge(&patch)).await)?.is_some() {
                    n += 1;
                }
            }
            Ok(n)
        })
    }

    fn patch_deployment<'a>(
        &'a self,
        app: &'a str,
        patch: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<(), ClusterError>> {
        Box::pin(async move {
            let api = self.ns_api::<Deployment>(app).await?;
            let params = PatchParams {
                field_manager: Some(FIELD_MANAGER.into()),
                ..PatchParams::default()
            };
            api.patch(app, &params, &Patch::Merge(patch))
                .await
                .map(|_| ())
                .map_err(map_err)
        })
    }

    fn patch_namespace<'a>(
        &'a self,
        app: &'a str,
        patch: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<(), ClusterError>> {
        Box::pin(async move {
            let api: Api<Namespace> = Api::all(self.client().await?);
            let params = PatchParams {
                field_manager: Some(FIELD_MANAGER.into()),
                ..PatchParams::default()
            };
            api.patch(&namespace_of(app), &params, &Patch::Merge(patch))
                .await
                .map(|_| ())
                .map_err(map_err)
        })
    }
}

impl AppExec for KubeApps {
    fn exec<'a>(
        &'a self,
        app: &'a str,
        request: &'a ExecRequest,
    ) -> BoxFuture<'a, Result<ExecOutput, ClusterError>> {
        Box::pin(async move {
            let api = self.ns_api::<Pod>(app).await?;
            let params = AttachParams {
                container: Some(request.container.clone()),
                stdin: request.stdin.is_some(),
                stdout: true,
                stderr: true,
                tty: false,
                max_stdin_buf_size: None,
                max_stdout_buf_size: None,
                max_stderr_buf_size: None,
            };
            let mut out = ExecOutput::default();
            let run = async {
                let mut guard = AbortOnDrop(Some(
                    api.exec(&request.pod, request.command.clone(), &params)
                        .await
                        .map_err(map_err)?,
                ));
                let Some(process) = guard.0.as_mut() else {
                    return Err(ClusterError::Api("exec: no process".into()));
                };
                let status = process.take_status();
                let stdin = process.stdin();
                let (stdout, stderr) = (process.stdout(), process.stderr());
                let write = async {
                    if let (Some(mut w), Some(data)) = (stdin, request.stdin.as_deref()) {
                        // A command that exits without reading its input is fine.
                        let _ = w.write_all(data.as_bytes()).await;
                        let _ = w.shutdown().await;
                    }
                };
                let max = request.max_output;
                let (o, e, ()) = tokio::join!(
                    read_capped(stdout, max, &mut out.stdout),
                    read_capped(stderr, max, &mut out.stderr),
                    write,
                );
                out.stdout_truncated = o;
                out.stderr_truncated = e;
                let status = match status {
                    Some(s) => s.await,
                    None => None,
                };
                match (status, guard.0.take()) {
                    (Some(s), _) => {
                        (out.exit_code, out.error) = exec_status(&s);
                        Ok(())
                    }
                    // No status at all: the connection broke before the command ended.
                    (None, Some(p)) => p.join().await.map_err(|e| ClusterError::Api(e.to_string())),
                    (None, None) => Ok(()),
                }
            };
            match tokio::time::timeout(request.timeout, run).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(e),
                Err(_) => out.timed_out = true,
            }
            Ok(out)
        })
    }
}

/// Closes the exec connection when the call ends early (timeout): dropping an
/// [`AttachedProcess`] alone leaves its background task running. The command itself may keep
/// running in the container.
struct AbortOnDrop(Option<AttachedProcess>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            p.abort();
        }
    }
}

/// Read `reader` to its end, keeping the first `max` bytes in `into` (the rest is read and
/// dropped, so the command never blocks on a full pipe). Returns whether anything was dropped.
async fn read_capped<R: AsyncRead + Unpin>(
    reader: Option<R>,
    max: usize,
    into: &mut Vec<u8>,
) -> bool {
    let Some(mut reader) = reader else {
        return false;
    };
    let mut truncated = false;
    let mut buf = vec![0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = max.saturating_sub(into.len());
                into.extend_from_slice(&buf[..n.min(room)]);
                truncated |= n > room;
            }
        }
    }
    truncated
}

/// The exit code and error message of an exec status: `Success` is exit 0; a non-zero exit is
/// `Failure` / `NonZeroExitCode` with the code as an `ExitCode` cause; anything else (the
/// command could not start) has no code, only the message.
pub(crate) fn exec_status(status: &Status) -> (Option<i32>, Option<String>) {
    if status.status.as_deref() == Some("Success") {
        return (Some(0), None);
    }
    let code = status
        .details
        .as_ref()
        .and_then(|d| d.causes.as_ref())
        .into_iter()
        .flatten()
        .find(|c| c.reason.as_deref() == Some("ExitCode"))
        .and_then(|c| c.message.as_deref()?.trim().parse().ok());
    let message = match code {
        Some(_) => None,
        None => Some(
            status
                .message
                .clone()
                .unwrap_or_else(|| "the command failed".into()),
        ),
    };
    (code, message)
}

#[cfg(test)]
mod tests {
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{StatusCause, StatusDetails};

    use super::*;

    #[test]
    fn exec_statuses_give_exit_codes() {
        let ok = Status {
            status: Some("Success".into()),
            ..Status::default()
        };
        assert_eq!(exec_status(&ok), (Some(0), None));
        let exit3 = Status {
            status: Some("Failure".into()),
            reason: Some("NonZeroExitCode".into()),
            message: Some("command terminated with non-zero exit code: 3".into()),
            details: Some(StatusDetails {
                causes: Some(vec![StatusCause {
                    reason: Some("ExitCode".into()),
                    message: Some("3".into()),
                    ..StatusCause::default()
                }]),
                ..StatusDetails::default()
            }),
            ..Status::default()
        };
        assert_eq!(exec_status(&exit3), (Some(3), None));
        let missing = Status {
            status: Some("Failure".into()),
            message: Some("executable file not found in $PATH".into()),
            ..Status::default()
        };
        assert_eq!(
            exec_status(&missing),
            (None, Some("executable file not found in $PATH".into()))
        );
    }

    #[tokio::test]
    async fn output_is_capped_but_read_to_the_end() {
        let data = vec![b'x'; 20_000];
        let mut kept = Vec::new();
        assert!(read_capped(Some(&data[..]), 100, &mut kept).await);
        assert_eq!(kept.len(), 100);
        let mut kept = Vec::new();
        assert!(!read_capped(Some(&b"hello"[..]), 100, &mut kept).await);
        assert_eq!(kept, b"hello");
        assert!(!read_capped(None::<&[u8]>, 100, &mut Vec::new()).await);
    }
}
