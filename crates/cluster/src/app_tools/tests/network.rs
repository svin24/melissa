//! Network permissions in the deploy path and `set_app_network`: links resolved and refused,
//! both sides of a link, approval (never "Always allow" for an agent), scope, audit, and no
//! pod restart.

use super::*;
use crate::render::network::{link_policy_name, EGRESS_POLICY};

/// A Postgres app installed next to the one under test (in the fake's `others`).
fn postgres_installed(fake: &Fake, network: Option<Value>) {
    let mut spec = json!({
        "description": "Postgres",
        "containers": [{
            "name": "db",
            "image": "postgres:17.2",
            "ports": [{"name": "db", "port": 5432}, {"name": "metrics", "port": 9187}],
        }],
    });
    if let Some(n) = network {
        spec["network"] = n;
    }
    let spec = AppSpec::from_json(&spec).unwrap();
    fake.state().others.insert(
        "postgres".into(),
        Namespace {
            metadata: ObjectMeta {
                name: Some("app-postgres".into()),
                labels: Some(BTreeMap::from([(
                    "melissa.io/app".to_string(),
                    "postgres".to_string(),
                )])),
                annotations: Some(BTreeMap::from([(
                    SPEC_ANNOTATION.to_string(),
                    spec.to_text(),
                )])),
                ..ObjectMeta::default()
            },
            ..Namespace::default()
        },
    );
}

/// Nextcloud's spec with `network`.
fn nc_with(network: Value) -> Value {
    let mut spec = nc();
    spec["network"] = network;
    spec
}

/// The NetworkPolicies applied so far, as `namespace/name`.
fn policies(fake: &Fake) -> Vec<String> {
    fake.state()
        .applied
        .iter()
        .filter_map(|o| match o {
            AppObject::NetworkPolicy(p) => Some(format!(
                "{}/{}",
                p.metadata.namespace.as_deref().unwrap_or("?"),
                p.metadata.name.as_deref().unwrap_or("?")
            )),
            _ => None,
        })
        .collect()
}

fn last_policy(fake: &Fake, ns: &str, name: &str) -> Value {
    let s = fake.state();
    let p = s
        .applied
        .iter()
        .rev()
        .find_map(|o| match o {
            AppObject::NetworkPolicy(p)
                if p.metadata.namespace.as_deref() == Some(ns)
                    && p.metadata.name.as_deref() == Some(name) =>
            {
                Some(p.clone())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no {ns}/{name}"));
    serde_json::to_value(p).unwrap()
}

/// The registry with the app tools and `set_app_network`.
fn network_registry(fake: &Arc<Fake>, audit: Option<AuditLog>) -> ToolRegistry {
    let mut tools = app_tools(settings(), Some(fake.clone()));
    tools.extend(network_tools(settings(), Some(fake.clone()), audit.clone()));
    ToolRegistry::new(tools, audit)
        .with_owners(Arc::new(Workspaces::new(settings(), Some(fake.clone()))))
        .with_agents(Arc::new(Names))
        .with_confirm_timeout(Duration::from_millis(300))
}

#[tokio::test]
async fn a_link_to_a_missing_app_or_port_is_refused_before_anything_is_applied() {
    let fake = Arc::new(Fake::default());
    let t = tools(Some(fake.clone()));
    let r = call(
        &t,
        "deploy_app",
        json!({"app": "nextcloud", "spec": nc_with(json!({"links": [{"app": "postgres", "port": "db"}]}))}),
    )
    .await;
    assert!(
        matches!(&r, Err(ToolError::BadArgs(m)) if m.contains("no app `postgres` on this device")),
        "{r:?}"
    );
    postgres_installed(&fake, None);
    let r = call(
        &t,
        "deploy_app",
        json!({"app": "nextcloud", "spec": nc_with(json!({"links": [{"app": "postgres", "port": "pg"}]}))}),
    )
    .await;
    assert!(
        matches!(&r, Err(ToolError::BadArgs(m)) if m == "`postgres` has no port `pg` (its ports: db, metrics)"),
        "{r:?}"
    );
    // A link to itself.
    let r = call(
        &t,
        "deploy_app",
        json!({"app": "nextcloud", "spec": nc_with(json!({"links": [{"app": "nextcloud"}]}))}),
    )
    .await;
    assert!(
        matches!(&r, Err(ToolError::BadArgs(m)) if m.contains("its own")),
        "{r:?}"
    );
    assert!(fake.state().applied.is_empty());
}

#[tokio::test]
async fn a_link_opens_both_sides_and_removing_it_closes_the_target_side() {
    let fake = Arc::new(Fake::default());
    postgres_installed(&fake, None);
    let t = tools(Some(fake.clone()));
    let out = call(
        &t,
        "deploy_app",
        json!({"app": "nextcloud", "spec": nc_with(json!({"internet": true, "links": [{"app": "postgres", "port": "db"}]}))}),
    )
    .await
    .unwrap();
    assert_eq!(out["network"]["internet"], true);
    assert_eq!(
        out["network"]["links"],
        json!([{"app": "postgres", "port": "db"}])
    );
    // Own policies before the Deployment; the target side after.
    let kinds: Vec<_> = fake.state().applied.iter().map(AppObject::kind).collect();
    let first_policy = kinds.iter().position(|k| *k == "NetworkPolicy").unwrap();
    let deployment = kinds.iter().position(|k| *k == "Deployment").unwrap();
    assert!(first_policy < deployment);
    assert_eq!(
        policies(&fake),
        [
            "app-nextcloud/melissa-isolation",
            "app-nextcloud/melissa-ports",
            "app-nextcloud/melissa-egress",
            "app-postgres/melissa-link-from-nextcloud",
        ]
    );
    let egress = last_policy(&fake, "app-nextcloud", EGRESS_POLICY);
    assert_eq!(
        egress["spec"]["egress"][1],
        json!({
            "to": [{"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "app-postgres"}}}],
            "ports": [{"port": 5432, "protocol": "TCP"}],
        })
    );
    let target = last_policy(&fake, "app-postgres", "melissa-link-from-nextcloud");
    assert_eq!(
        target["spec"]["ingress"][0]["ports"],
        json!([{"port": 5432, "protocol": "TCP"}])
    );

    // Postgres goes away; the link stays in the spec but is inactive (an update is not
    // blocked by it), with a warning.
    fake.state().others.clear();
    let out = call(
        &t,
        "update_app",
        json!({"app": "nextcloud", "spec": nc_with(json!({"internet": true, "links": [{"app": "postgres", "port": "db"}]}))}),
    )
    .await
    .unwrap();
    assert!(
        out["network_warnings"][0]
            .as_str()
            .unwrap()
            .starts_with("link to postgres (port db) is inactive: no app `postgres`"),
        "{out}"
    );

    // Dropping the link deletes the target side.
    postgres_installed(&fake, None);
    call(
        &t,
        "update_app",
        json!({"app": "nextcloud", "spec": nc_with(json!({"internet": true}))}),
    )
    .await
    .unwrap();
    assert!(fake
        .state()
        .deleted
        .contains(&format!("NetworkPolicy {}", link_policy_name("nextcloud"))));
    let egress = last_policy(&fake, "app-nextcloud", EGRESS_POLICY);
    assert_eq!(egress["spec"]["egress"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn deploying_a_link_target_again_reapplies_the_links_into_it() {
    let fake = Arc::new(Fake::default());
    // Postgres (the "other" here) links to Nextcloud, which is deployed now.
    postgres_installed(&fake, Some(json!({"links": [{"app": "nextcloud"}]})));
    let t = tools(Some(fake.clone()));
    let out = call(&t, "deploy_app", json!({"app": "nextcloud", "spec": nc()}))
        .await
        .unwrap();
    assert_eq!(out["network_applied"], json!(["links from postgres"]));
    let p = policies(&fake);
    assert!(
        p.contains(&"app-postgres/melissa-egress".to_string()),
        "{p:?}"
    );
    assert!(
        p.contains(&"app-nextcloud/melissa-link-from-postgres".to_string()),
        "{p:?}"
    );
    // All ports: no port list on either side.
    let target = last_policy(&fake, "app-nextcloud", "melissa-link-from-postgres");
    assert!(target["spec"]["ingress"][0].get("ports").is_none());
}

#[tokio::test]
async fn uninstall_removes_the_target_sides_of_its_links() {
    let fake = Arc::new(Fake::default());
    postgres_installed(&fake, None);
    let t = tools(Some(fake.clone()));
    call(
        &t,
        "deploy_app",
        json!({"app": "nextcloud", "spec": nc_with(json!({"links": [{"app": "postgres"}]}))}),
    )
    .await
    .unwrap();
    call(&t, "uninstall_app", json!({"app": "nextcloud"}))
        .await
        .unwrap();
    assert_eq!(
        fake.state().deleted,
        [format!("NetworkPolicy {}", link_policy_name("nextcloud"))]
    );
}

#[tokio::test]
async fn an_update_that_only_changes_the_network_keeps_the_pod_template() {
    let fake = Arc::new(Fake::default());
    let t = tools(Some(fake.clone()));
    call(&t, "deploy_app", json!({"app": "nextcloud", "spec": nc()}))
        .await
        .unwrap();
    let before = fake.state().deployment.clone().unwrap().spec.template;
    call(
        &t,
        "update_app",
        json!({"app": "nextcloud", "spec": nc_with(json!({"internet": true, "lan": true}))}),
    )
    .await
    .unwrap();
    let after = fake.state().deployment.clone().unwrap().spec.template;
    assert_eq!(before, after);
    let egress = last_policy(&fake, "app-nextcloud", EGRESS_POLICY);
    assert_eq!(egress["spec"]["egress"].as_array().unwrap().len(), 2);
    // app_status shows it (what the UI reads).
    let s = call(&t, "app_status", json!({"app": "nextcloud"}))
        .await
        .unwrap();
    assert_eq!(s["network"]["internet"], true);
    assert_eq!(s["network"]["lan"], true);
    assert_eq!(s["network"]["summary"], "internet; home network");
}

/// Nextcloud deployed for agent Ada, with its spec stored.
async fn nextcloud_for_ada(fake: &Arc<Fake>) {
    let t = tools(Some(fake.clone()));
    call(
        &t,
        "deploy_app",
        json!({"app": "nextcloud", "spec": nc(), "agent": "Ada"}),
    )
    .await
    .unwrap();
    fake.state().applied.clear();
}

#[tokio::test]
async fn an_agents_network_change_always_waits_for_the_owner_in_plain_words() {
    let fake = Arc::new(Fake::default());
    postgres_installed(&fake, None);
    nextcloud_for_ada(&fake).await;
    let path = std::env::temp_dir().join(format!(
        "melissa-network-audit-{}.jsonl",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let reg = network_registry(&fake, Some(AuditLog::new(&path)))
        .with_confirm_timeout(Duration::from_secs(10));
    let args = json!({"app": "nextcloud", "internet": true, "add_links": [{"app": "postgres", "port": "db"}]});

    // Ada's own app: asked, never as an "Always allow" rule, and her Full access can't answer.
    let (c, call) = asks(&reg, agent(ADA), "set_app_network", args.clone()).await;
    assert!(c.rule.is_none(), "no Always allow for network changes");
    assert!(
        c.reason.contains(
            "Let nextcloud reach the internet; Let nextcloud connect to postgres on its port db"
        ),
        "{}",
        c.reason
    );
    assert!(c.reason.contains("only the owner decides"), "{}", c.reason);
    reg.confirmations().resolve(c.id, Verdict::Approved);
    let out = call.await.unwrap().unwrap();
    assert_eq!(out["unchanged"], false);
    assert_eq!(out["network"]["internet"], true);

    // The stored spec changed; only NetworkPolicies were applied (no Deployment: no restart).
    let spec = AppSpec::from_text(&ns_annotation(&fake, SPEC_ANNOTATION).unwrap()).unwrap();
    assert!(spec.network_or_default().internet);
    let kinds: BTreeSet<&str> = fake.state().applied.iter().map(AppObject::kind).collect();
    assert_eq!(kinds, BTreeSet::from(["NetworkPolicy"]));
    assert!(policies(&fake).contains(&"app-postgres/melissa-link-from-nextcloud".to_string()));

    // Audited with who and what.
    let lines = audit_lines(&path);
    let rec = lines
        .iter()
        .find(|l| l["action"] == "app.network")
        .expect("an app.network record");
    let _ = std::fs::remove_file(&path);
    let detail = rec["detail"].as_str().unwrap();
    assert!(
        detail.starts_with("nextcloud: Let nextcloud reach the internet"),
        "{detail}"
    );
    assert!(rec["actor"].as_str().unwrap().contains("Ada"), "{rec}");

    // Narrowing asks too.
    let (c, call) = asks(
        &reg,
        agent(ADA),
        "set_app_network",
        json!({"app": "nextcloud", "internet": false}),
    )
    .await;
    assert!(c.reason.starts_with("Stop nextcloud reaching the internet"));
    reg.confirmations().resolve(c.id, Verdict::Denied);
    assert_eq!(call.await.unwrap(), Err(ToolError::Denied));

    // Another agent's app: only the user's own yes.
    let (c, call) = asks(&reg, agent(BO), "set_app_network", args.clone()).await;
    assert!(c.user_only);
    reg.confirmations().resolve(c.id, Verdict::Denied);
    assert_eq!(call.await.unwrap(), Err(ToolError::Denied));
}

#[tokio::test]
async fn the_owners_own_change_needs_no_confirmation_and_bad_links_are_refused() {
    let fake = Arc::new(Fake::default());
    nextcloud_for_ada(&fake).await;
    let reg = network_registry(&fake, None);
    let mut user = TaskContext {
        scope: Scope::User,
        attended: true,
        ..TaskContext::default()
    };
    let out = reg
        .call(
            0,
            &mut user,
            "set_app_network",
            json!({"app": "nextcloud", "lan": true}),
        )
        .await
        .unwrap();
    assert_eq!(
        out["changed"],
        json!(["Let nextcloud reach devices on the home network"])
    );
    assert_eq!(out["network"]["lan"], true);
    // A link to an app that isn't there: refused, nothing stored.
    let r = reg
        .call(
            0,
            &mut user,
            "set_app_network",
            json!({"app": "nextcloud", "add_links": [{"app": "postgres"}]}),
        )
        .await;
    assert!(
        matches!(&r, Err(ToolError::BadArgs(m)) if m.contains("no app `postgres`")),
        "{r:?}"
    );
    let spec = AppSpec::from_text(&ns_annotation(&fake, SPEC_ANNOTATION).unwrap()).unwrap();
    assert!(spec.network_or_default().links.is_empty());
    // Nothing to change: refused at once.
    let r = reg
        .call(0, &mut user, "set_app_network", json!({"app": "nextcloud"}))
        .await;
    assert!(matches!(&r, Err(ToolError::BadArgs(_))), "{r:?}");
}

#[tokio::test]
async fn a_deploy_that_grants_network_access_is_never_an_always_allow() {
    let fake = Arc::new(Fake::default());
    nextcloud_for_ada(&fake).await;
    let reg = network_registry(&fake, None);
    let (c, call) = asks(
        &reg,
        agent(ADA),
        "update_app",
        json!({"app": "nextcloud", "spec": nc_with(json!({"internet": true}))}),
    )
    .await;
    assert!(c.rule.is_none());
    assert!(c.reason.contains("may reach: internet"), "{}", c.reason);
    assert!(
        c.reason.contains("lets the app reach: internet"),
        "{}",
        c.reason
    );
    reg.confirmations().resolve(c.id, Verdict::Denied);
    assert_eq!(call.await.unwrap(), Err(ToolError::Denied));
}

#[tokio::test]
async fn app_network_shows_links_with_ports_incoming_links_and_options() {
    let fake = Arc::new(Fake::default());
    postgres_installed(
        &fake,
        Some(json!({"links": [{"app": "nextcloud", "port": "http"}]})),
    );
    let t = tools(Some(fake.clone()));
    call(
        &t,
        "deploy_app",
        json!({"app": "nextcloud", "spec": nc_with(json!({"lan": true, "links": [{"app": "postgres", "port": "db"}]}))}),
    )
    .await
    .unwrap();
    let net: BTreeMap<&'static str, Arc<dyn Tool>> =
        network_tools(settings(), Some(fake.clone()), None)
            .into_iter()
            .map(|t| (t.spec().name, t))
            .collect();
    let v = call(&net, "app_network", json!({"app": "nextcloud"}))
        .await
        .unwrap();
    assert_eq!(v["internet"], false);
    assert_eq!(v["lan"], true);
    assert_eq!(
        v["links"],
        json!([{"app": "postgres", "port": "db", "active": true, "problem": null,
                "ports": [{"name": "db", "port": 5432, "protocol": "tcp"}]}])
    );
    assert_eq!(
        v["incoming"],
        json!([{"app": "postgres", "port": "http", "active": true, "problem": null,
                "ports": [{"name": "http", "port": 80, "protocol": "tcp"}]}])
    );
    assert_eq!(
        v["link_options"],
        json!([{"app": "postgres", "ports": [
            {"name": "db", "port": 5432, "protocol": "tcp"},
            {"name": "metrics", "port": 9187, "protocol": "tcp"},
        ]}])
    );
    assert_eq!(net["app_network"].spec().labels, Labels::READ_ONLY);
}
