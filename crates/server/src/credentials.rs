//! `melissa-server app-credentials <app>`: show the owner an app's generated credentials.
//!
//! Only for the owner, never for agents: it reads the app's Secret with k3s's admin
//! kubeconfig (`/etc/rancher/k3s/k3s.yaml`, root-only), so it needs `sudo`. It is not a tool
//! and not an API route; the lead agent's own identities cannot read Secrets at all (the write
//! identity may create them, never get them), and agents run as the lead agent's unprivileged
//! user inside a sandbox.

use std::path::Path;

use anyhow::{bail, Context};
use k8s_openapi::api::{apps::v1::Deployment, core::v1::Secret};
use kube::Api;
use melissa_catalog::{namespace_of, valid_app_name, Catalog};
use melissa_cluster::{render::CONFIG_ANNOTATION, KubeCluster};
use serde_json::Value;

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

pub async fn show(
    app: &str,
    catalog_dir: &Path,
    domain: &str,
    kubeconfig: &Path,
) -> anyhow::Result<()> {
    let client = admin_client(app, kubeconfig, "app-credentials").await?;
    let ns = namespace_of(app);
    let manifest = Catalog::load_dir(catalog_dir).apps.remove(app);
    let secret_name = format!("{app}-secrets");
    let secret = Api::<Secret>::namespaced(client.clone(), &ns)
        .get_opt(&secret_name)
        .await
        .with_context(|| format!("reading {ns}/{secret_name}"))?
        .with_context(|| format!("`{app}` has no generated credentials (not installed?)"))?;
    let deployment = Api::<Deployment>::namespaced(client, &ns)
        .get_opt(app)
        .await
        .with_context(|| format!("reading {ns}/{app}"))?;
    let config: Value = deployment
        .as_ref()
        .and_then(|d| d.metadata.annotations.as_ref())
        .and_then(|a| a.get(CONFIG_ANNOTATION))
        .and_then(|c| serde_json::from_str(c).ok())
        .unwrap_or(Value::Null);

    let mut secrets: Vec<(String, String)> = secret
        .data
        .unwrap_or_default()
        .into_iter()
        .map(|(k, v)| (k, String::from_utf8_lossy(&v.0).into_owned()))
        .collect();
    secrets.sort();
    let has_ingress = manifest.as_ref().is_none_or(|m| m.ingress.is_some());
    println!(
        "{app}{}{}",
        if has_ingress {
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
    let creds = manifest.as_ref().and_then(|m| m.credentials.as_ref());
    if let Some(user_field) = creds.and_then(|c| c.user.as_ref()) {
        let user = config[user_field.as_str()].as_str().unwrap_or("?");
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
