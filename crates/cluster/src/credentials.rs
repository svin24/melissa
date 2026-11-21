//! An app's setup password, for the owner only (docs/DECISIONS.md, 2026-11-21).
//!
//! When an install generates an app's password, the web UI shows a setup password card in the
//! chat. The card reveals or copies the password on request (server to browser only, audited)
//! until the owner dismisses it; after that Melissa can't read it again. No agent and no tool
//! ever reaches this module: it is not a tool, and the agents' identities can't read Secrets.
//!
//! Three identities take part:
//! - the write identity (`melissa-apps`, [`AppRead`] + [`AppDeployer`]) reads the stored spec
//!   (which field is the password, the login name), binds the reader to the app's namespace
//!   right before a reveal, and on dismiss records the dismissal on the namespace and removes
//!   the binding again;
//! - the reader (`melissa-credentials`, `credentials-rbac.yaml`) may only `get` Secrets, and only
//!   in namespaces where that binding exists; the server asks for the one Secret by name;
//! - the admission policy refuses the binding in a namespace marked dismissed, and the mark can't
//!   be removed, so a dismissal holds even if this code had a bug.
//!
//! Dismissing never touches the app's Secret: the app reads its password from it at every start.

use std::{fmt, path::Path, sync::Arc};

use k8s_openapi::api::core::v1::Secret;
use kube::Api;
use melissa_agent::BoxFuture;
use melissa_appspec::{namespace_of, valid_app_name, AppSpec};
use serde_json::json;

use crate::{
    app_tools::{now_rfc3339, shared_secrets_of, stored_spec},
    apps::{optional, retry_forbidden, AppDeployer, AppObject, AppRead, NamedKind},
    cluster::{LazyCluster, RUNTIME_DIR},
    render::{
        credentials_binding, secret_object_name, CREDENTIALS_BINDING,
        CREDENTIALS_DISMISSED_ANNOTATION,
    },
    ClusterError,
};

/// File name of the reader's kubeconfig inside [`RUNTIME_DIR`].
pub const CREDENTIALS_RUNTIME_FILE: &str = "credentials.kubeconfig";

/// A revealed password. Its `Debug` never prints the value, so it can't end up in a log line.
pub struct SetupPassword(String);

impl SetupPassword {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// The value, for the one response that shows it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SetupPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SetupPassword(..)")
    }
}

/// What the setup password card can show for an app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupLogin {
    /// Melissa can still show the password. `user`: the login name, if the app has one.
    Available { user: Option<String> },
    /// The owner dismissed it; Melissa can't read it any more.
    Dismissed,
    /// No such app, or it has no generated password.
    None,
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialsError {
    #[error("no setup password for this app")]
    NotFound,
    #[error("the setup password was dismissed")]
    Dismissed,
    #[error(transparent)]
    Cluster(#[from] ClusterError),
}

/// Where the password is: its Secret and key, and the login name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Location {
    secret: String,
    key: String,
    user: Option<String>,
    dismissed: bool,
}

/// The reader: `get` of one Secret by name in an app's namespace.
pub trait SecretReader: Send + Sync {
    /// The value of `key` in Secret `name` of app `app`; `None` if either is missing.
    fn read<'a>(
        &'a self,
        app: &'a str,
        name: &'a str,
        key: &'a str,
    ) -> BoxFuture<'a, Result<Option<SetupPassword>, ClusterError>>;
}

/// [`SecretReader`] over the reader's kubeconfig (`credentials.kubeconfig`), found lazily.
pub struct KubeSecretReader {
    lazy: LazyCluster,
}

impl KubeSecretReader {
    pub fn new(lazy: LazyCluster) -> Self {
        Self { lazy }
    }

    /// The configured path, then `RUNTIME_DIR/credentials.kubeconfig`.
    pub fn for_runtime(configured: Option<&Path>) -> Self {
        let runtime = std::path::PathBuf::from(RUNTIME_DIR);
        let candidates = configured
            .map(Path::to_path_buf)
            .into_iter()
            .chain([runtime.join(CREDENTIALS_RUNTIME_FILE)])
            .collect();
        Self::new(LazyCluster::new(candidates, Some(runtime)))
    }
}

impl SecretReader for KubeSecretReader {
    fn read<'a>(
        &'a self,
        app: &'a str,
        name: &'a str,
        key: &'a str,
    ) -> BoxFuture<'a, Result<Option<SetupPassword>, ClusterError>> {
        Box::pin(async move {
            let client = self.lazy.cluster().await?.client().clone();
            let api: Api<Secret> = Api::namespaced(client, &namespace_of(app));
            // A binding applied a moment ago may not have reached the authorizer yet.
            let secret = retry_forbidden(|| async { optional(api.get(name).await) }).await?;
            Ok(secret
                .and_then(|s| s.data)
                .and_then(|mut d| d.remove(key))
                .map(|v| SetupPassword(String::from_utf8_lossy(&v.0).into_owned())))
        })
    }
}

/// The owner's setup password path: what the card shows, reveal and dismiss.
#[derive(Clone)]
pub struct AppCredentials {
    read: Arc<dyn AppRead>,
    write: Arc<dyn AppDeployer>,
    reader: Arc<dyn SecretReader>,
}

impl AppCredentials {
    pub fn new(
        read: Arc<dyn AppRead>,
        write: Arc<dyn AppDeployer>,
        reader: Arc<dyn SecretReader>,
    ) -> Self {
        Self {
            read,
            write,
            reader,
        }
    }

    async fn locate(&self, app: &str) -> Result<Option<Location>, ClusterError> {
        if !valid_app_name(app) {
            return Ok(None);
        }
        let Some(ns) = self.read.namespace(app).await? else {
            return Ok(None);
        };
        let deployment = self.read.deployment(app).await?;
        let Some(spec) = stored_spec(app, &ns, deployment.as_ref()) else {
            return Ok(None);
        };
        let dismissed = ns
            .metadata
            .annotations
            .as_ref()
            .is_some_and(|a| a.contains_key(CREDENTIALS_DISMISSED_ANNOTATION));
        Ok(location(app, &spec, &shared_secrets_of(&ns), dismissed))
    }

    /// What the card shows for `app`. Reads no Secret.
    pub async fn login(&self, app: &str) -> Result<SetupLogin, ClusterError> {
        Ok(match self.locate(app).await? {
            None => SetupLogin::None,
            Some(l) if l.dismissed => SetupLogin::Dismissed,
            Some(l) => SetupLogin::Available { user: l.user },
        })
    }

    /// The password, for the owner's reveal or copy. Binds the reader to the app's namespace
    /// first (refused by the cluster once dismissed).
    pub async fn reveal(&self, app: &str) -> Result<SetupPassword, CredentialsError> {
        let loc = self.locate(app).await?.ok_or(CredentialsError::NotFound)?;
        if loc.dismissed {
            return Err(CredentialsError::Dismissed);
        }
        self.write
            .apply(&AppObject::RoleBinding(credentials_binding(app)))
            .await?;
        self.reader
            .read(app, &loc.secret, &loc.key)
            .await?
            .ok_or(CredentialsError::NotFound)
    }

    /// The owner dismissed the setup password: record it on the namespace (the cluster then
    /// refuses the reader's binding for good) and remove the binding. The app's Secret stays.
    /// Dismissing twice is fine.
    pub async fn dismiss(&self, app: &str) -> Result<(), CredentialsError> {
        let loc = self.locate(app).await?.ok_or(CredentialsError::NotFound)?;
        if !loc.dismissed {
            let patch = json!({"metadata": {"annotations": {
                CREDENTIALS_DISMISSED_ANNOTATION: now_rfc3339(),
            }}});
            self.write.patch_namespace(app, &patch).await?;
        }
        self.write
            .delete_named(app, NamedKind::RoleBinding, CREDENTIALS_BINDING)
            .await?;
        Ok(())
    }
}

/// The password's Secret and key, from the spec's `credentials`. `None` without credentials.
fn location(
    app: &str,
    spec: &AppSpec,
    shared: &std::collections::BTreeSet<String>,
    dismissed: bool,
) -> Option<Location> {
    let creds = spec.credentials.as_ref()?;
    spec.secret(&creds.password)?;
    Some(Location {
        secret: secret_object_name(app, &creds.password, shared),
        key: creds.password.clone(),
        user: creds.user.clone(),
        dismissed,
    })
}

#[cfg(test)]
mod tests;
