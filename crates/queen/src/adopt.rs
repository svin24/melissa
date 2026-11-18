//! `melissa-queen app-adopt <app>`: the owner takes over an app a worker job created.
//!
//! Apps record their owner in the `melissa.io/owner` label of their namespace (`owner`, or
//! `job-<key>` for an app a worker created for its job). Agents can only change apps in
//! their scope, and nothing in the queen changes an existing owner label: the admission
//! policy `melissa-apps-namespaces` rejects any change of it by the queen's write identity.
//! Changing ownership is therefore an owner-only command, like `app-credentials`: it runs
//! with k3s's admin kubeconfig (root), which that policy does not match. It is deliberately
//! not an agent tool, not even one behind a confirmation: an agent that wants an app should
//! ask the owner, not ask the owner to click "approve" on a transfer.
//!
//! Adopting only ever moves an app to the owner (a job is short-lived; nothing should be
//! handed to one later). The previous owner is kept in `melissa.io/adopted-from`.

use std::path::Path;

use anyhow::Context;
use k8s_openapi::api::core::v1::Namespace;
use kube::{
    api::{Patch, PatchParams},
    Api,
};
use melissa_agent::tools::Owner;
use melissa_catalog::namespace_of;
use melissa_hive::render::{ADOPTED_AT_ANNOTATION, ADOPTED_FROM_ANNOTATION, OWNER_LABEL};
use serde_json::json;

use crate::credentials::admin_client;

/// Field manager of the owner's changes (not `melissa`, so the queen's later server-side
/// applies don't take these fields over silently).
const FIELD_MANAGER: &str = "melissa-owner";

pub async fn adopt(app: &str, kubeconfig: &Path) -> anyhow::Result<()> {
    let client = admin_client(app, kubeconfig, "app-adopt").await?;
    let api: Api<Namespace> = Api::all(client);
    let name = namespace_of(app);
    let ns = api
        .get_opt(&name)
        .await
        .with_context(|| format!("reading namespace {name}"))?
        .with_context(|| format!("`{app}` is not installed"))?;
    let current = ns
        .metadata
        .labels
        .as_ref()
        .and_then(|l| l.get(OWNER_LABEL))
        .cloned();
    let human = Owner::Human.label_value();
    if current.as_deref() == Some(human.as_str()) {
        println!("{app} already belongs to you");
        return Ok(());
    }
    let patch = adopt_patch(current.as_deref(), &now());
    api.patch(
        &name,
        &PatchParams {
            field_manager: Some(FIELD_MANAGER.into()),
            ..PatchParams::default()
        },
        &Patch::Merge(&patch),
    )
    .await
    .with_context(|| format!("updating namespace {name}"))?;
    println!(
        "{app} now belongs to you (was: {})",
        current
            .as_deref()
            .unwrap_or("unlabelled, already counted as yours")
    );
    Ok(())
}

/// The merge patch that hands an app to the owner.
fn adopt_patch(previous: Option<&str>, at: &str) -> serde_json::Value {
    json!({
        "metadata": {
            "labels": {OWNER_LABEL: Owner::Human.label_value()},
            "annotations": {
                ADOPTED_AT_ANNOTATION: at,
                ADOPTED_FROM_ANNOTATION: previous.unwrap_or("unlabelled"),
            },
        }
    })
}

fn now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    k8s_openapi::jiff::Timestamp::from_second(i64::try_from(secs).unwrap_or(0))
        .map(|t| t.to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_patch_moves_the_app_to_the_owner_and_records_the_previous_one() {
        let p = adopt_patch(Some("job-ab12"), "2026-11-18T10:00:00Z");
        assert_eq!(p["metadata"]["labels"]["melissa.io/owner"], "owner");
        assert_eq!(
            p["metadata"]["annotations"]["melissa.io/adopted-from"],
            "job-ab12"
        );
        assert_eq!(
            p["metadata"]["annotations"]["melissa.io/adopted-at"],
            "2026-11-18T10:00:00Z"
        );
    }
}
