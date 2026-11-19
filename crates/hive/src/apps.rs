//! Cluster access for the app tools, split in two traits:
//!
//! - [`AppRead`]: what `app_status` and `app_logs` need (objects and logs in one app's
//!   namespace). No writes.
//! - [`AppDeployer`]: server-side apply of rendered objects, create-once Secrets, deletes,
//!   and the replica patch of `stop_app`/`start_app`. Only the changing app tools hold one.
//!
//! Both are implemented by [`KubeApps`] over the write identity's kubeconfig
//! (`melissa/melissa-apps`, RBAC in `apps-rbac.yaml`). `cluster_health` and other read-only
//! tools keep using the read-only identity through [`crate::ClusterRead`].

use std::{future::Future, time::Duration};

use k8s_openapi::{
    api::{
        apps::v1::Deployment,
        core::v1::{ConfigMap, Namespace, PersistentVolumeClaim, Pod, Secret, Service},
        networking::v1::Ingress,
        rbac::v1::RoleBinding,
    },
    serde::{de::DeserializeOwned, Serialize},
    NamespaceResourceScope,
};
use kube::{
    api::{Api, DeleteParams, ListParams, LogParams, Patch, PatchParams, PostParams},
    Resource,
};
use melissa_agent::BoxFuture;
use melissa_catalog::namespace_of;

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
    /// ones are fine). Keeps the namespace (with its port), PVCs and Secret. Returns the
    /// kinds that were deleted.
    fn delete_workload<'a>(
        &'a self,
        app: &'a str,
    ) -> BoxFuture<'a, Result<Vec<&'static str>, ClusterError>>;
    /// Delete the app's namespace and everything in it, including its volumes' data.
    fn delete_namespace<'a>(&'a self, app: &'a str) -> BoxFuture<'a, Result<bool, ClusterError>>;
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
/// queen's read-only one.
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
fn optional<T>(r: Result<T, kube::Error>) -> Result<Option<T>, ClusterError> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(e) if is_status(&e, 404) => Ok(None),
        Err(e) => Err(map_err(e)),
    }
}

/// Retry `f` while it is forbidden: a fresh RoleBinding takes a moment to reach the API
/// server's authorizer cache.
async fn retry_forbidden<T, F, Fut>(mut f: F) -> Result<T, ClusterError>
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
