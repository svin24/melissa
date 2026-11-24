//! App network permissions in the deploy path, and `set_app_network`.
//!
//! The spec's `network` block (internet, home network, links to other apps) becomes
//! NetworkPolicies ([`crate::render::network`]). Here: links resolved against the other apps'
//! stored specs (a new link to a missing app or port is refused), the target side of each link
//! applied in the other app's namespace (and deleted when the link goes), and links into an app
//! re-applied when that app is deployed again.
//!
//! `set_app_network` changes only the `network` block of an app's stored spec and re-applies
//! only its NetworkPolicies: no pod restarts. An agent's call always waits for the owner
//! ([`Tool::owner_approval`]: no "Always allow" rule can answer it), with the change in plain
//! words on the card ("Let vuls-web reach the internet"). The owner's own change (scope
//! `User`, the JSON API and `melissa-server app network`) needs no confirmation when
//! attended. Every change is audited (`app.network`).

use std::net::IpAddr;

use k8s_openapi::api::networking::v1::NetworkPolicy;
use melissa_agent::{AuditLog, AuditRecord};
use melissa_appspec::{Link, Network};

use super::*;
use crate::render::network::{
    app_policies, link_policy, link_policy_name, merge_links, LinkPorts, EGRESS_POLICY,
};

/// This device's own addresses, which no app may reach (the Kubernetes API, Melissa, the
/// other apps' device ports): its IPv4 and IPv6 addresses, the addresses people use for it
/// when they are IPs, and in the container the gateway (the host, which publishes Melissa's
/// ports) and the publish bind.
pub(crate) fn device_addresses(settings: &AppSettings) -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = Vec::new();
    if let Ok(trie) = std::fs::read_to_string("/proc/net/fib_trie") {
        out.extend(device::local_ipv4(&trie).into_iter().map(IpAddr::V4));
    }
    out.extend(device::default_route_address().map(IpAddr::V4));
    if let Ok(v6) = std::fs::read_to_string("/proc/net/if_inet6") {
        out.extend(local_ipv6(&v6));
    }
    out.extend(
        settings
            .addresses()
            .iter()
            .filter_map(|a| a.parse::<IpAddr>().ok()),
    );
    if let Ok(bind) = std::env::var("MELISSA_PUBLISH_BIND") {
        out.extend(bind.parse::<IpAddr>().ok());
        let route = std::fs::read_to_string("/proc/net/route").unwrap_or_default();
        out.extend(app_access::default_gateway(&route).map(IpAddr::V4));
    }
    out.retain(|ip| !ip.is_unspecified() && !ip.is_loopback());
    out.sort();
    out.dedup();
    out
}

/// Global and unique local IPv6 addresses from `/proc/net/if_inet6` text (32 hex digits per
/// line; link-local and loopback left out).
fn local_ipv6(text: &str) -> Vec<IpAddr> {
    text.lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter_map(|hex| u128::from_str_radix(hex, 16).ok())
        .map(std::net::Ipv6Addr::from)
        .filter(|a| !a.is_loopback() && (a.segments()[0] & 0xffc0) != 0xfe80)
        .map(IpAddr::V6)
        .collect()
}

/// The network block as tools and the UI show it (defaults filled in).
pub(crate) fn network_json(n: &Network) -> Value {
    json!({
        "internet": n.internet,
        "lan": n.lan,
        "links": n.links,
        "summary": n.words(),
    })
}

/// `app`'s links resolved against the other apps' stored specs. A link that `old` (the
/// network before this change) did not have must name an installed app and one of its ports,
/// else the call is refused; a link that was there already but no longer resolves is left out
/// with a warning (the other app was removed or changed).
pub(crate) async fn resolve_links(
    read: &dyn AppRead,
    app: &str,
    network: &Network,
    old: Option<&Network>,
) -> Result<(Vec<LinkPorts>, Vec<String>), ToolError> {
    network
        .validate_for(app)
        .map_err(|e| ToolError::BadArgs(format!("spec: {e}")))?;
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    for link in &network.links {
        let is_new = old.is_none_or(|o| !o.links.contains(link));
        let resolved = resolve_one(read, link).await?;
        match resolved {
            Ok(ports) => out.push(LinkPorts {
                app: link.app.clone(),
                ports,
            }),
            Err(why) if is_new => return Err(ToolError::BadArgs(why)),
            Err(why) => warnings.push(format!("link to {} is inactive: {why}", link.words())),
        }
    }
    Ok((merge_links(out), warnings))
}

/// One link's ports, or why it can't be made.
async fn resolve_one(
    read: &dyn AppRead,
    link: &Link,
) -> Result<Result<Vec<(u16, Protocol)>, String>, ToolError> {
    let target = &link.app;
    let ns = read.namespace(target).await.map_err(failed)?;
    let spec = match &ns {
        Some(ns) if !is_terminating(ns) => {
            annotation(ns, SPEC_ANNOTATION).and_then(|t| AppSpec::from_text(t).ok())
        }
        _ => None,
    };
    let Some(spec) = spec else {
        return Ok(Err(format!(
            "no app `{target}` on this device to link to (list_apps shows the apps)"
        )));
    };
    Ok(match &link.port {
        None => Ok(Vec::new()),
        Some(p) => match spec.port(p) {
            Some(port) => Ok(vec![(port.port, port.protocol)]),
            None => {
                let names: Vec<&str> = spec.ports().map(|p| p.name.as_str()).collect();
                Err(format!(
                    "`{target}` has no port `{p}` (its ports: {})",
                    if names.is_empty() {
                        "none".to_string()
                    } else {
                        names.join(", ")
                    }
                ))
            }
        },
    })
}

/// The target sides of `app`'s links: apply one policy per linked app, delete the ones of
/// apps `old` linked to and `links` no longer does. Returns what changed (`Kind ns/name`).
pub(crate) async fn sync_link_targets(
    deploy: &dyn AppDeployer,
    app: &str,
    links: &[LinkPorts],
    old: Option<&Network>,
) -> Result<Vec<String>, ToolError> {
    let mut done = Vec::new();
    for l in links {
        deploy
            .apply(&AppObject::NetworkPolicy(link_policy(app, l)))
            .await
            .map_err(failed)?;
        done.push(format!(
            "NetworkPolicy {}/{}",
            namespace_of(&l.app),
            link_policy_name(app)
        ));
    }
    let mut gone: Vec<&str> = old
        .map(|o| o.links.iter().map(|l| l.app.as_str()).collect())
        .unwrap_or_default();
    gone.sort_unstable();
    gone.dedup();
    for target in gone {
        if links.iter().any(|l| l.app == target) {
            continue;
        }
        if deploy
            .delete_named(target, NamedKind::NetworkPolicy, &link_policy_name(app))
            .await
            .map_err(failed)?
        {
            done.push(format!(
                "removed NetworkPolicy {}/{}",
                namespace_of(target),
                link_policy_name(app)
            ));
        }
    }
    Ok(done)
}

/// Links into `app` from the other apps: re-apply each linking app's egress and its policy
/// in `app`'s namespace (after `app` was deployed again or its ports changed). Links that no
/// longer resolve are skipped, as in [`resolve_links`].
pub(crate) async fn refresh_incoming(
    read: &dyn AppRead,
    deploy: &dyn AppDeployer,
    app: &str,
    device: &[IpAddr],
) -> Result<Vec<String>, ToolError> {
    let mut done = Vec::new();
    for source in read.app_names().await.map_err(failed)? {
        if source == app {
            continue;
        }
        let Some(ns) = read.namespace(&source).await.map_err(failed)? else {
            continue;
        };
        if is_terminating(&ns) {
            continue;
        }
        let Some(spec) = annotation(&ns, SPEC_ANNOTATION).and_then(|t| AppSpec::from_text(t).ok())
        else {
            continue;
        };
        let network = spec.network_or_default();
        if !network.links.iter().any(|l| l.app == app) {
            continue;
        }
        // Every link of theirs counts as old here: never refused, only skipped.
        let (links, _) = resolve_links(read, &source, &network, Some(&network)).await?;
        let egress = own_policy(&source, &spec, &links, device, EGRESS_POLICY);
        for object in egress.into_iter().map(AppObject::NetworkPolicy).chain(
            links
                .iter()
                .filter(|l| l.app == app)
                .map(|l| AppObject::NetworkPolicy(link_policy(&source, l))),
        ) {
            deploy.apply(&object).await.map_err(failed)?;
        }
        done.push(format!("links from {source}"));
    }
    Ok(done)
}

/// One of `app`'s own policies, by name.
fn own_policy(
    app: &str,
    spec: &AppSpec,
    links: &[LinkPorts],
    device: &[IpAddr],
    name: &str,
) -> Option<NetworkPolicy> {
    app_policies(app, spec, links, device)
        .into_iter()
        .find(|p| p.metadata.name.as_deref() == Some(name))
}

/// `app` is going away (uninstall, force delete): its policies in the apps it linked to go.
pub(crate) async fn remove_link_targets(
    read: &dyn AppRead,
    deploy: &dyn AppDeployer,
    app: &str,
) -> Result<(), ToolError> {
    let Some(ns) = read.namespace(app).await.map_err(failed)? else {
        return Ok(());
    };
    let Some(spec) = annotation(&ns, SPEC_ANNOTATION).and_then(|t| AppSpec::from_text(t).ok())
    else {
        return Ok(());
    };
    sync_link_targets(deploy, app, &[], spec.network.as_ref()).await?;
    Ok(())
}

// ---- set_app_network ---------------------------------------------------------------------

/// What one `set_app_network` call changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct NetworkChange {
    pub internet: Option<bool>,
    pub lan: Option<bool>,
    pub add: Vec<Link>,
    /// A link without `port` removes every link to that app.
    pub remove: Vec<Link>,
}

const CHANGE_ARGS: &[&str] = &["app", "internet", "lan", "add_links", "remove_links"];

fn links_arg(args: &Map<String, Value>, key: &str) -> Result<Vec<Link>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(v) => serde_json::from_value::<Vec<Link>>(v.clone()).map_err(|e| {
            ToolError::BadArgs(format!(
                "`{key}` must be a list of {{app, port?}} objects: {e}"
            ))
        }),
    }
}

fn bool_arg(args: &Map<String, Value>, key: &str) -> Result<Option<bool>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(ToolError::BadArgs(format!("`{key}` must be true or false"))),
    }
}

/// The arguments of `set_app_network` (`app` and the change), checked.
pub(crate) fn change_args(args: &Value) -> Result<(String, NetworkChange), ToolError> {
    let args = arg_object(args)?;
    reject_unknown(args, CHANGE_ARGS)?;
    let app = app_arg(args)?;
    let change = NetworkChange {
        internet: bool_arg(args, "internet")?,
        lan: bool_arg(args, "lan")?,
        add: links_arg(args, "add_links")?,
        remove: links_arg(args, "remove_links")?,
    };
    if change == NetworkChange::default() {
        return Err(ToolError::BadArgs(
            "nothing to change: give `internet`, `lan`, `add_links` or `remove_links`".into(),
        ));
    }
    // The links to add, checked like a spec's (names, ports, no self-link).
    Network {
        links: change.add.clone(),
        ..Network::default()
    }
    .validate_for(&app)
    .map_err(|e| ToolError::BadArgs(e.to_string().replace("network.links", "add_links")))?;
    for (i, l) in change.remove.iter().enumerate() {
        if !valid_app_name(&l.app) {
            return Err(ToolError::BadArgs(format!(
                "remove_links[{i}].app: `{}` is not an app name",
                l.app
            )));
        }
    }
    Ok((app, change))
}

/// `current` with `change` applied (removals first, then additions).
pub(crate) fn changed(current: &Network, change: &NetworkChange) -> Network {
    let mut n = current.clone();
    if let Some(b) = change.internet {
        n.internet = b;
    }
    if let Some(b) = change.lan {
        n.lan = b;
    }
    n.links.retain(|l| {
        !change
            .remove
            .iter()
            .any(|r| r.app == l.app && (r.port.is_none() || r.port == l.port))
    });
    for l in &change.add {
        if !n.links.contains(l) {
            n.links.push(l.clone());
        }
    }
    n
}

/// The change in plain words, one sentence per part ("Let vuls-web reach the internet").
pub(crate) fn change_words(app: &str, change: &NetworkChange) -> Vec<String> {
    let mut out = Vec::new();
    let mut flag = |on: Option<bool>, what: &str| match on {
        Some(true) => out.push(format!("Let {app} reach {what}")),
        Some(false) => out.push(format!("Stop {app} reaching {what}")),
        None => {}
    };
    flag(change.internet, "the internet");
    flag(change.lan, "devices on the home network");
    for l in &change.add {
        out.push(match &l.port {
            Some(p) => format!("Let {app} connect to {} on its port {p}", l.app),
            None => format!("Let {app} connect to {} on all its ports", l.app),
        });
    }
    for l in &change.remove {
        out.push(match &l.port {
            Some(p) => format!("Stop {app} connecting to {} on its port {p}", l.app),
            None => format!("Stop {app} connecting to {}", l.app),
        });
    }
    out
}

/// Why the owner is asked (an agent's call): every change of what an app may reach.
pub(crate) const NETWORK_IS_THE_OWNERS: &str =
    "changes what an app may reach on the network (only the owner decides)";

struct SetAppNetwork {
    settings: Arc<AppSettings>,
    read: Option<Arc<dyn AppRead>>,
    deploy: Option<Arc<dyn AppDeployer>>,
    audit: Option<AuditLog>,
}

/// The network tools over one cluster connection (`None`: no k3s): `app_network` (read)
/// and `set_app_network`. `audit`: where `app.network` records go.
pub fn network_tools<C>(
    settings: AppSettings,
    cluster: Option<Arc<C>>,
    audit: Option<AuditLog>,
) -> Vec<Arc<dyn Tool>>
where
    C: AppRead + AppDeployer + 'static,
{
    let read = cluster.clone().map(|c| c as Arc<dyn AppRead>);
    vec![
        Arc::new(AppNetwork { read: read.clone() }),
        Arc::new(SetAppNetwork {
            settings: Arc::new(settings),
            read,
            deploy: cluster.map(|c| c as Arc<dyn AppDeployer>),
            audit,
        }),
    ]
}

// ---- app_network (read) ------------------------------------------------------------------

/// The named ports of a spec: `[{name, port, protocol}]`.
fn ports_json(spec: &AppSpec) -> Vec<Value> {
    spec.ports()
        .map(|p| json!({"name": p.name, "port": p.port, "protocol": p.protocol.lower()}))
        .collect()
}

/// `app`'s network in full, for people and agents: its permissions with each link's ports
/// resolved (`active`: the other app and port exist), the links other apps have into it
/// (`incoming`), and the other apps' named ports (`link_options`, what a link may name).
pub(crate) async fn network_view(read: &dyn AppRead, app: &str) -> Result<Value, ToolError> {
    let ns = read
        .namespace(app)
        .await
        .map_err(failed)?
        .ok_or_else(|| ToolError::Failed(format!("`{app}` is not installed")))?;
    let deployment = read.deployment(app).await.map_err(failed)?;
    let spec = stored_spec(app, &ns, deployment.as_ref())
        .ok_or_else(|| ToolError::Failed(format!("`{app}` has no spec")))?;
    let network = spec.network_or_default();
    // Every other app's spec, once.
    let mut others: BTreeMap<String, AppSpec> = BTreeMap::new();
    for name in read.app_names().await.map_err(failed)? {
        if name == app {
            continue;
        }
        let Some(ns) = read.namespace(&name).await.map_err(failed)? else {
            continue;
        };
        if is_terminating(&ns) {
            continue;
        }
        if let Some(s) = annotation(&ns, SPEC_ANNOTATION).and_then(|t| AppSpec::from_text(t).ok()) {
            others.insert(name, s);
        }
    }
    let link_json = |l: &Link, target: Option<&AppSpec>| {
        let (ports, problem): (Vec<Value>, Option<String>) = match (target, &l.port) {
            (None, _) => (Vec::new(), Some(format!("no app `{}`", l.app))),
            (Some(t), None) => (ports_json(t), None),
            (Some(t), Some(p)) => match t.port(p) {
                Some(port) => (
                    vec![
                        json!({"name": port.name, "port": port.port, "protocol": port.protocol.lower()}),
                    ],
                    None,
                ),
                None => (Vec::new(), Some(format!("`{}` has no port `{p}`", l.app))),
            },
        };
        json!({
            "app": l.app,
            "port": l.port,
            "ports": ports,
            "active": problem.is_none(),
            "problem": problem,
        })
    };
    let links: Vec<Value> = network
        .links
        .iter()
        .map(|l| link_json(l, others.get(&l.app)))
        .collect();
    let incoming: Vec<Value> = others
        .iter()
        .flat_map(|(source, s)| {
            s.network_or_default()
                .links
                .into_iter()
                .filter(|l| l.app == app)
                .map(|l| {
                    let mut v = link_json(&l, Some(&spec));
                    v["app"] = json!(source);
                    v
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let options: Vec<Value> = others
        .iter()
        .map(|(name, s)| json!({"app": name, "ports": ports_json(s)}))
        .collect();
    Ok(json!({
        "app": app,
        "internet": network.internet,
        "lan": network.lan,
        "summary": network.words(),
        "links": links,
        "incoming": incoming,
        "link_options": options,
    }))
}

struct AppNetwork {
    read: Option<Arc<dyn AppRead>>,
}

impl Tool for AppNetwork {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "app_network",
            description: "What a deployed app may reach on the network: `internet`, `lan`, \
                `links` to other apps (with their port names and numbers, `active` false if \
                the other app or port is gone), `incoming` links from other apps, and \
                `link_options` (the other apps and their named ports, what a link may name). \
                Change it with set_app_network.",
            labels: Labels::READ_ONLY,
            params: json!({
                "type": "object",
                "properties": {"app": {"type": "string"}},
                "required": ["app"],
                "additionalProperties": false,
            }),
        }
    }

    fn call<'a>(&'a self, args: Value) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(async move {
            let args = arg_object(&args)?;
            reject_unknown(args, &["app"])?;
            let app = app_arg(args)?;
            let Some(read) = &self.read else {
                return Err(no_cluster());
            };
            network_view(read.as_ref(), &app).await
        })
    }

    fn target(&self) -> Option<Target> {
        Some(app_target(Access::Read, "check the network of"))
    }
}

impl SetAppNetwork {
    async fn run(&self, task: TaskId, scope: &Scope, args: Value) -> Result<Value, ToolError> {
        let (app, change) = change_args(&args)?;
        let (Some(read), Some(deploy)) = (&self.read, &self.deploy) else {
            return Err(no_cluster());
        };
        let ns = read
            .namespace(&app)
            .await
            .map_err(failed)?
            .ok_or_else(|| ToolError::Failed(format!("`{app}` is not installed")))?;
        if is_terminating(&ns) {
            return Err(ToolError::Failed(format!("`{app}` is being deleted")));
        }
        let deployment = read.deployment(&app).await.map_err(failed)?;
        let mut spec = stored_spec(&app, &ns, deployment.as_ref()).ok_or_else(|| {
            ToolError::Failed(format!(
                "`{app}` has no spec to change (update_app sets one)"
            ))
        })?;
        let before = spec.network_or_default();
        let after = changed(&before, &change);
        let (links, warnings) = resolve_links(read.as_ref(), &app, &after, Some(&before)).await?;
        spec.set_network(after.clone());
        // The spec first: what is stored is what the policies say, also if applying fails
        // half-way (the next deploy or call applies the rest).
        let patch = json!({"metadata": {"annotations": {SPEC_ANNOTATION: spec.to_text()}}});
        deploy.patch_namespace(&app, &patch).await.map_err(failed)?;
        let device = device_addresses(&self.settings);
        let mut applied = Vec::new();
        for p in app_policies(&app, &spec, &links, &device) {
            let name = p.metadata.name.clone().unwrap_or_default();
            deploy
                .apply(&AppObject::NetworkPolicy(p))
                .await
                .map_err(failed)?;
            applied.push(format!("NetworkPolicy {}/{name}", namespace_of(&app)));
        }
        applied.extend(sync_link_targets(deploy.as_ref(), &app, &links, Some(&before)).await?);
        let words = change_words(&app, &change);
        let unchanged = before == after;
        if let Some(log) = &self.audit {
            let actor = scope.actor(task, self.settings.agents.as_deref());
            let detail = format!(
                "{app}: {}{}; now may reach: {}",
                words.join("; "),
                if unchanged { " (no change)" } else { "" },
                after.words()
            );
            if let Err(e) =
                log.append(&AuditRecord::now("app.network", task, "", detail).by(&actor))
            {
                tracing::error!(path = %log.path().display(), "audit log write failed: {e}");
            }
        }
        Ok(json!({
            "app": app,
            "changed": words,
            "unchanged": unchanged,
            "network": network_json(&after),
            "before": network_json(&before),
            "applied": applied,
            "warnings": warnings,
            "note": "applied at once; the app was not restarted",
        }))
    }
}

impl Tool for SetAppNetwork {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "set_app_network",
            description: "Change what a deployed app may reach on the network, without \
                restarting it: `internet` (public internet: downloads, updates, external \
                APIs), `lan` (devices on the home network) and links to other apps on this \
                device (`add_links`/`remove_links`: [{app, port?}], port = the other app's \
                port name; leave it out for all its ports). Apps reach nothing of that by \
                default. The owner approves every change and sees it in plain words; ask \
                only for what the app needs. app_status shows the current `network`.",
            labels: Labels {
                effect: Effect::Reversible,
                ..Labels::READ_ONLY
            },
            params: json!({
                "type": "object",
                "properties": {
                    "app": {"type": "string"},
                    "internet": {"type": "boolean", "description": "Allow (true) or block (false) the public internet"},
                    "lan": {"type": "boolean", "description": "Allow (true) or block (false) devices on the home network"},
                    "add_links": {"type": "array", "items": {"type": "object", "properties": {"app": {"type": "string"}, "port": {"type": "string"}}, "required": ["app"], "additionalProperties": false}},
                    "remove_links": {"type": "array", "items": {"type": "object", "properties": {"app": {"type": "string"}, "port": {"type": "string"}}, "required": ["app"], "additionalProperties": false}},
                },
                "required": ["app"],
                "additionalProperties": false,
            }),
        }
    }

    fn call<'a>(&'a self, args: Value) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(self.run(0, &Scope::User, args))
    }

    fn call_in<'a>(
        &'a self,
        task: TaskId,
        ctx: &'a mut TaskContext,
        args: Value,
    ) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(self.run(task, &ctx.scope, args))
    }

    fn target(&self) -> Option<Target> {
        Some(app_target(Access::Change, "change the network of"))
    }

    fn check_args(&self, args: &Value) -> Result<(), ToolError> {
        change_args(args).map(|_| ())
    }

    fn owner_approval(&self, ctx: &TaskContext, _: &Value) -> Result<Option<String>, ToolError> {
        // The owner's own change (Settings, the JSON API, the CLI) is theirs to make.
        Ok((ctx.scope != Scope::User).then(|| NETWORK_IS_THE_OWNERS.to_string()))
    }

    fn describe_call(&self, args: &Value) -> Option<String> {
        let (app, change) = change_args(args).ok()?;
        Some(change_words(&app, &change).join("; "))
    }
}

/// For `deploy_app`/`update_app`: a spec that lets the app reach anything asks the owner
/// every time (no "Always allow"), unless the user deploys it themselves.
pub(crate) fn deploy_owner_approval(ctx: &TaskContext, args: &Value) -> Option<String> {
    if ctx.scope == Scope::User {
        return None;
    }
    let spec = arg_object(args).ok().and_then(|a| spec_arg(a).ok())??;
    let n = spec.network_or_default();
    (!n.is_default()).then(|| {
        format!(
            "lets the app reach: {} ({NETWORK_IS_THE_OWNERS})",
            n.words()
        )
    })
}

/// For the `tool.call` audit line of `deploy_app`/`update_app`: the network the spec grants.
pub(crate) fn deploy_audit_detail(args: &Value) -> Option<String> {
    let spec = arg_object(args).ok().and_then(|a| spec_arg(a).ok())??;
    Some(format!(
        "app.network {}: may reach {}",
        args.get("app")?.as_str()?,
        spec.network_or_default().words()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv6_addresses_from_proc() {
        let text = "fd000000000000000000000000000001 02 40 00 80 eth0\n\
                    fe800000000000000000000000000001 02 40 20 80 eth0\n\
                    00000000000000000000000000000001 01 80 10 80 lo\n";
        assert_eq!(local_ipv6(text), vec!["fd00::1".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn changes_apply_and_read_plainly() {
        let (app, change) = change_args(&json!({
            "app": "vuls-web",
            "internet": true,
            "lan": false,
            "add_links": [{"app": "postgres", "port": "db"}],
            "remove_links": [{"app": "redis"}],
        }))
        .unwrap();
        assert_eq!(
            change_words(&app, &change),
            [
                "Let vuls-web reach the internet",
                "Stop vuls-web reaching devices on the home network",
                "Let vuls-web connect to postgres on its port db",
                "Stop vuls-web connecting to redis",
            ]
        );
        let before = Network {
            lan: true,
            links: vec![
                Link {
                    app: "redis".into(),
                    port: Some("a".into()),
                },
                Link {
                    app: "redis".into(),
                    port: None,
                },
                Link {
                    app: "mqtt".into(),
                    port: None,
                },
            ],
            ..Network::default()
        };
        let after = changed(&before, &change);
        assert!(after.internet && !after.lan);
        assert_eq!(
            after.links,
            vec![
                Link {
                    app: "mqtt".into(),
                    port: None
                },
                Link {
                    app: "postgres".into(),
                    port: Some("db".into())
                },
            ]
        );
    }

    #[test]
    fn bad_changes_are_refused_before_anyone_is_asked() {
        let err = |v: Value| change_args(&v).unwrap_err().to_string();
        assert!(err(json!({"app": "x"})).contains("nothing to change"));
        assert!(err(json!({"app": "x", "internet": "yes"})).contains("true or false"));
        assert!(err(json!({"app": "x", "domains": ["a"]})).contains("unknown argument"));
        assert!(err(json!({"app": "x", "add_links": [{"app": "x"}]})).contains("its own"));
        assert!(
            err(json!({"app": "x", "add_links": [{"app": "pg", "ports": "db"}]}))
                .contains("unknown field")
        );
        assert!(
            err(json!({"app": "x", "remove_links": [{"app": "PG"}]})).contains("not an app name")
        );
    }
}
