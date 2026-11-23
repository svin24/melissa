use std::{collections::BTreeMap, sync::Mutex};

use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{Namespace, PersistentVolumeClaim, Pod},
    networking::v1::Ingress,
};
use kube::api::ObjectMeta;
use melissa_appspec::legacy;
use serde_json::{Map, Value};

use super::*;
use crate::{apps::LogRequest, render::SPEC_ANNOTATION};

/// One app namespace in memory, and what was done to it.
#[derive(Default)]
struct Fake {
    namespace: Mutex<Option<Namespace>>,
    /// The reader's binding exists.
    bound: Mutex<bool>,
    /// The app's Secret: (object name, key) → value.
    secrets: BTreeMap<(String, String), String>,
    reads: Mutex<Vec<String>>,
}

impl Fake {
    fn with_spec(spec: &AppSpec) -> Self {
        let ns = Namespace {
            metadata: ObjectMeta {
                name: Some("app-nextcloud".into()),
                annotations: Some(BTreeMap::from([(SPEC_ANNOTATION.into(), spec.to_text())])),
                ..ObjectMeta::default()
            },
            ..Namespace::default()
        };
        Self {
            namespace: Mutex::new(Some(ns)),
            secrets: BTreeMap::from([(
                (
                    "nextcloud-secret-admin-password".into(),
                    "admin_password".into(),
                ),
                "s3cret-value".into(),
            )]),
            ..Self::default()
        }
    }

    fn annotation(&self, key: &str) -> Option<String> {
        self.namespace
            .lock()
            .unwrap()
            .as_ref()?
            .metadata
            .annotations
            .as_ref()?
            .get(key)
            .cloned()
    }
}

fn unused<T>() -> BoxFuture<'static, Result<T, ClusterError>> {
    Box::pin(async { Err(ClusterError::Api("not used here".into())) })
}

impl AppRead for Fake {
    fn app_names(&self) -> BoxFuture<'_, Result<Vec<String>, ClusterError>> {
        unused()
    }
    fn namespace(&self, app: &str) -> BoxFuture<'_, Result<Option<Namespace>, ClusterError>> {
        let ns = (app == "nextcloud")
            .then(|| self.namespace.lock().unwrap().clone())
            .flatten();
        Box::pin(async move { Ok(ns) })
    }
    fn deployment(&self, _: &str) -> BoxFuture<'_, Result<Option<Deployment>, ClusterError>> {
        Box::pin(async { Ok(None) })
    }
    fn ingress(&self, _: &str) -> BoxFuture<'_, Result<Option<Ingress>, ClusterError>> {
        unused()
    }
    fn volumes(&self, _: &str) -> BoxFuture<'_, Result<Vec<PersistentVolumeClaim>, ClusterError>> {
        unused()
    }
    fn pods(&self, _: &str) -> BoxFuture<'_, Result<Vec<Pod>, ClusterError>> {
        unused()
    }
    fn logs<'a>(
        &'a self,
        _: &'a str,
        _: &'a LogRequest,
    ) -> BoxFuture<'a, Result<String, ClusterError>> {
        unused()
    }
    fn runtime_class_handler<'a>(
        &'a self,
        _: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, ClusterError>> {
        unused()
    }
}

impl AppDeployer for Fake {
    fn apply<'a>(&'a self, object: &'a AppObject) -> BoxFuture<'a, Result<(), ClusterError>> {
        Box::pin(async move {
            let AppObject::RoleBinding(rb) = object else {
                return Err(ClusterError::Api("only the binding".into()));
            };
            assert_eq!(rb, &credentials_binding("nextcloud"));
            // The admission policy: never again once dismissed.
            if self.annotation(CREDENTIALS_DISMISSED_ANNOTATION).is_some() {
                return Err(ClusterError::Forbidden("dismissed".into()));
            }
            *self.bound.lock().unwrap() = true;
            Ok(())
        })
    }
    fn create_secret<'a>(&'a self, _: &'a Secret) -> BoxFuture<'a, Result<bool, ClusterError>> {
        unused()
    }
    fn delete_workload<'a>(
        &'a self,
        _: &'a str,
    ) -> BoxFuture<'a, Result<Vec<&'static str>, ClusterError>> {
        unused()
    }
    fn delete_named<'a>(
        &'a self,
        _: &'a str,
        kind: NamedKind,
        name: &'a str,
    ) -> BoxFuture<'a, Result<bool, ClusterError>> {
        Box::pin(async move {
            assert_eq!((kind, name), (NamedKind::RoleBinding, CREDENTIALS_BINDING));
            Ok(std::mem::replace(&mut *self.bound.lock().unwrap(), false))
        })
    }
    fn delete_namespace<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<bool, ClusterError>> {
        unused()
    }
    fn force_delete_pods<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<usize, ClusterError>> {
        unused()
    }
    fn clear_volume_finalizers<'a>(
        &'a self,
        _: &'a str,
    ) -> BoxFuture<'a, Result<usize, ClusterError>> {
        unused()
    }
    fn patch_deployment<'a>(
        &'a self,
        _: &'a str,
        _: &'a Value,
    ) -> BoxFuture<'a, Result<(), ClusterError>> {
        unused()
    }
    fn patch_namespace<'a>(
        &'a self,
        _: &'a str,
        patch: &'a Value,
    ) -> BoxFuture<'a, Result<(), ClusterError>> {
        Box::pin(async move {
            let mut guard = self.namespace.lock().unwrap();
            let Some(ns) = guard.as_mut() else {
                return Err(ClusterError::Api("no namespace".into()));
            };
            let annotations = ns.metadata.annotations.get_or_insert_with(BTreeMap::new);
            for (k, v) in patch["metadata"]["annotations"].as_object().unwrap() {
                annotations.insert(k.clone(), v.as_str().unwrap().to_string());
            }
            Ok(())
        })
    }
}

impl SecretReader for Fake {
    fn read<'a>(
        &'a self,
        _: &'a str,
        name: &'a str,
        key: &'a str,
    ) -> BoxFuture<'a, Result<Option<SetupPassword>, ClusterError>> {
        Box::pin(async move {
            // RBAC: only with the binding in place.
            if !*self.bound.lock().unwrap() {
                return Err(ClusterError::Forbidden("no binding".into()));
            }
            self.reads.lock().unwrap().push(format!("{name}/{key}"));
            Ok(self
                .secrets
                .get(&(name.to_string(), key.to_string()))
                .cloned()
                .map(SetupPassword::new))
        })
    }
}

fn nextcloud() -> AppSpec {
    legacy::nextcloud(&Map::new())
}

fn creds(fake: &Arc<Fake>) -> AppCredentials {
    AppCredentials::new(fake.clone(), fake.clone(), fake.clone())
}

#[tokio::test]
async fn reveal_binds_the_reader_and_gets_one_secret_by_name() {
    let fake = Arc::new(Fake::with_spec(&nextcloud()));
    let c = creds(&fake);
    assert_eq!(
        c.login("nextcloud").await.unwrap(),
        SetupLogin::Available {
            user: Some("admin".into())
        }
    );
    // Reading the card's state touches no Secret.
    assert!(fake.reads.lock().unwrap().is_empty());
    let pw = c.reveal("nextcloud").await.unwrap();
    assert_eq!(pw.expose(), "s3cret-value");
    assert!(!format!("{pw:?}").contains("s3cret"));
    assert_eq!(
        *fake.reads.lock().unwrap(),
        ["nextcloud-secret-admin-password/admin_password"]
    );
}

#[tokio::test]
async fn dismiss_is_for_good_and_keeps_the_apps_secret() {
    let fake = Arc::new(Fake::with_spec(&nextcloud()));
    let c = creds(&fake);
    c.reveal("nextcloud").await.unwrap();
    assert!(*fake.bound.lock().unwrap());

    c.dismiss("nextcloud").await.unwrap();
    assert!(!*fake.bound.lock().unwrap(), "binding removed");
    let at = fake.annotation(CREDENTIALS_DISMISSED_ANNOTATION).unwrap();
    assert!(at.ends_with('Z'), "{at}");
    assert_eq!(c.login("nextcloud").await.unwrap(), SetupLogin::Dismissed);
    assert!(matches!(
        c.reveal("nextcloud").await,
        Err(CredentialsError::Dismissed)
    ));
    // The Secret itself is untouched (the app reads it at every start).
    assert_eq!(fake.secrets.len(), 1);
    // Twice is fine and keeps the first time.
    c.dismiss("nextcloud").await.unwrap();
    assert_eq!(
        fake.annotation(CREDENTIALS_DISMISSED_ANNOTATION).unwrap(),
        at
    );
}

#[tokio::test]
async fn apps_without_a_generated_password_have_none() {
    let mut spec = nextcloud();
    spec.credentials = None;
    let fake = Arc::new(Fake::with_spec(&spec));
    let c = creds(&fake);
    assert_eq!(c.login("nextcloud").await.unwrap(), SetupLogin::None);
    assert!(matches!(
        c.reveal("nextcloud").await,
        Err(CredentialsError::NotFound)
    ));
    assert_eq!(c.login("other").await.unwrap(), SetupLogin::None);
    assert_eq!(c.login("../x").await.unwrap(), SetupLogin::None);
}
