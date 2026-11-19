//! `melissa-server app-credentials <app>`: show the owner an app's generated credentials.
//!
//! Only for the owner, never for agents: it reads the app's Secret with k3s's admin
//! kubeconfig (`/etc/rancher/k3s/k3s.yaml`, root-only), so it needs `sudo`. It is not a tool
//! and not an API route; the lead agent's own identities cannot read Secrets at all (the write
//! identity may create them, never get them), and agents run as the lead agent's unprivileged
//! user inside a sandbox.

use std::path::Path;

use anyhow::{bail, Context};
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{Namespace, Secret},
};
use kube::{api::ListParams, Api};
use melissa_appspec::{legacy, namespace_of, valid_app_name, AppSpec};
use melissa_cluster::{
    render::{APP_LABEL, CONFIG_ANNOTATION, SPEC_ANNOTATION},
    KubeCluster,
};
use serde_json::{Map, Value};

/// k3s's admin kubeconfig (mode 0600, root), see `config.yaml.d/10-melissa.yaml`.
pub const ADMIN_KUBECONFIG: &str = "/etc/rancher/k3s/k3s.yaml";

/// A client with k3s's admin kubeconfig, for owner-only commands (`sudo melissa-server
/// <command> <app>`). Fails with a hint when not run as root.
pub async fn admin_client(
    app: &str,
    kubeconfig: &Path,
    command: &str,
) -> anyhow::Result<kube::Client> {
    if !valid_app_name(app) {
        bail!("`{app}` is not an app name");
    }
    match std::fs::File::open(kubeconfig) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => bail!(
            "{command} is for the owner and needs root (k3s's admin kubeconfig): \
             sudo melissa-server {command} {app}"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!(
                "no k3s on this device ({} is missing)",
                kubeconfig.display()
            )
        }
        Err(e) => return Err(e).with_context(|| format!("opening {}", kubeconfig.display())),
    }
    let cluster = KubeCluster::from_kubeconfig(kubeconfig).await?;
    Ok(cluster.client().clone())
}

pub async fn show(app: &str, domain: &str, kubeconfig: &Path) -> anyhow::Result<()> {
    let client = admin_client(app, kubeconfig, "app-credentials").await?;
    let ns_name = namespace_of(app);
    let ns = Api::<Namespace>::all(client.clone())
        .get_opt(&ns_name)
        .await
        .with_context(|| format!("reading namespace {ns_name}"))?
        .with_context(|| format!("`{app}` is not deployed"))?;
    let deployment = Api::<Deployment>::namespaced(client.clone(), &ns_name)
        .get_opt(app)
        .await
        .with_context(|| format!("reading {ns_name}/{app}"))?;
    // The stored spec, or for an app from the old catalog, the one rebuilt from its config.
    let annotation = |key: &str| {
        ns.metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get(key))
            .cloned()
    };
    let spec = match annotation(SPEC_ANNOTATION) {
        Some(text) => AppSpec::from_text(&text).ok(),
        None => {
            let config: Map<String, Value> = deployment
                .as_ref()
                .and_then(|d| d.metadata.annotations.as_ref())
                .and_then(|a| a.get(CONFIG_ANNOTATION))
                .and_then(|c| serde_json::from_str(c).ok())
                .unwrap_or_default();
            legacy::from_catalog(app, &config)
        }
    };
    // Every generated Secret of the app (one per field; one shared one for catalog installs).
    let list = Api::<Secret>::namespaced(client, &ns_name)
        .list(&ListParams::default().labels(&format!("{APP_LABEL}={app}")))
        .await
        .with_context(|| format!("reading the Secrets in {ns_name}"))?;
    let mut secrets: Vec<(String, String)> = list
        .items
        .into_iter()
        .flat_map(|s| s.data.unwrap_or_default())
        .map(|(k, v)| (k, String::from_utf8_lossy(&v.0).into_owned()))
        .collect();
    secrets.sort();
    if secrets.is_empty() {
        bail!("`{app}` has no generated credentials");
    }
    let has_web = spec.as_ref().is_none_or(|s| s.web.is_some());
    println!(
        "{app}{}{}",
        if has_web {
            format!("  http://{app}.{domain}")
        } else {
            String::new()
        },
        if deployment.is_none() {
            "  (uninstalled, data kept)"
        } else {
            ""
        }
    );
    let creds = spec.as_ref().and_then(|s| s.credentials.as_ref());
    if let Some(user) = creds.and_then(|c| c.user.as_ref()) {
        println!("  user:     {user}");
    }
    for (key, value) in &secrets {
        let label = match creds {
            Some(c) if &c.password == key => "password".to_string(),
            _ => key.clone(),
        };
        println!("  {:<9} {value}", format!("{label}:"));
    }
    Ok(())
}
