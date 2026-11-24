//! Sanity checks on the RBAC manifests the image ships (text level: no YAML parser in the
//! build). They guard the properties the app tools' security relies on; the VM test
//! (`docs/agent-skills/app-deploy.md`) checks the same against a live API server.

use std::path::PathBuf;

use crate::render::{
    port_service_name, APPS_SERVICE_ACCOUNT, APP_PORTS, ASSIGNED_AT_ANNOTATION,
    CREDENTIALS_BINDING, CREDENTIALS_DISMISSED_ANNOTATION, CREDENTIALS_ROLE,
    CREDENTIALS_SERVICE_ACCOUNT, DEVICE_LABEL, GVISOR_HANDLER, GVISOR_RUNTIME_CLASS,
    ISOLATION_LABEL, LEGACY_OWNER_LABEL, MELISSA_NAMESPACE, OLD_DEVICE_LABEL, OLD_OWNER_LABEL,
    OLD_WORKSPACE_LABEL, OWNER_AGENT_LABEL, PORTS_ANNOTATION, PORT_ANNOTATION, WORKLOAD_ROLE,
    WORKSPACE_LABEL,
};

fn manifest(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging/common/usr/lib/melissa/k3s/manifests")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// YAML documents without comment lines.
fn docs(text: &str) -> Vec<String> {
    text.split("\n---\n")
        .map(|d| {
            d.lines()
                .filter(|l| !l.trim_start().starts_with('#'))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|d| !d.trim().is_empty())
        .collect()
}

fn doc<'a>(docs: &'a [String], kind: &str, name: &str) -> &'a str {
    docs.iter()
        .find(|d| {
            d.contains(&format!("kind: {kind}\n")) && d.contains(&format!("\n  name: {name}\n"))
        })
        .unwrap_or_else(|| panic!("no {kind} {name}"))
}

/// The rule blocks (`- apiGroups: ...` up to the next one) of a ClusterRole.
fn rules(role: &str) -> Vec<String> {
    role.split("\n  - apiGroups:")
        .skip(1)
        .map(str::to_string)
        .collect()
}

#[test]
fn no_wildcards_anywhere() {
    for file in [
        "apps-rbac.yaml",
        "server-rbac.yaml",
        "credentials-rbac.yaml",
    ] {
        let text = docs(&manifest(file)).join("\n");
        assert!(!text.contains("\"*\""), "{file} has a wildcard");
        assert!(!text.contains("cluster-admin"), "{file}");
        assert!(!text.contains("escalate"), "{file}");
        assert!(!text.contains("impersonate"), "{file}");
    }
}

#[test]
fn the_read_only_identity_stays_read_only() {
    let docs = docs(&manifest("server-rbac.yaml"));
    let role = doc(&docs, "ClusterRole", "melissa-server-read");
    for rule in rules(role) {
        assert!(
            rule.contains("verbs: [\"get\", \"list\", \"watch\"]"),
            "{rule}"
        );
        assert!(!rule.contains("secrets"), "{rule}");
        // Cluster-wide: no logs of system pods. App logs are read through the write
        // identity, bound per app namespace only.
        assert!(!rule.contains("pods/log"), "{rule}");
    }
}

#[test]
fn the_write_identity_is_narrow() {
    let docs = docs(&manifest("apps-rbac.yaml"));
    assert_eq!(WORKLOAD_ROLE, "melissa-app-workload");
    let sa = doc(&docs, "ServiceAccount", APPS_SERVICE_ACCOUNT);
    assert!(sa.contains(&format!("namespace: {MELISSA_NAMESPACE}")));
    assert!(sa.contains("automountServiceAccountToken: false"));

    // Cluster-wide: namespaces, rolebindings, `bind` on exactly the workload role and the
    // setup password reader, and deleting exactly the reader's binding.
    let cluster = doc(&docs, "ClusterRole", "melissa-apps");
    let cluster_rules = rules(cluster);
    assert_eq!(cluster_rules.len(), 5, "{cluster}");
    for word in [
        "secrets",
        "pods",
        "deployments",
        "nodes",
        "persistentvolumes\"",
        "clusterrolebindings",
    ] {
        assert!(
            !cluster.contains(word),
            "melissa-apps ClusterRole mentions {word}"
        );
    }
    let bind = cluster_rules
        .iter()
        .find(|r| r.contains("\"bind\""))
        .expect("bind rule");
    assert!(
        bind.contains(&format!(
            "resourceNames: [\"{WORKLOAD_ROLE}\", \"{CREDENTIALS_ROLE}\"]"
        )),
        "{bind}"
    );
    let delete = cluster_rules
        .iter()
        .find(|r| r.contains("verbs: [\"delete\"]"))
        .expect("delete rule");
    assert!(
        delete.contains(&format!(
            "resources: [\"rolebindings\"]\n    verbs: [\"delete\"]\n    resourceNames: [\"{CREDENTIALS_BINDING}\"]"
        )),
        "{delete}"
    );

    // The workload role is never bound cluster-wide.
    assert!(!docs
        .iter()
        .any(|d| d.contains("kind: ClusterRoleBinding") && d.contains(WORKLOAD_ROLE)));
    let crb = doc(&docs, "ClusterRoleBinding", "melissa-apps");
    assert!(
        crb.contains("name: melissa-apps\n    namespace: melissa"),
        "{crb}"
    );

    // Secrets: written once, never read.
    let workload = doc(&docs, "ClusterRole", WORKLOAD_ROLE);
    let secret_rules: Vec<_> = rules(workload)
        .into_iter()
        .filter(|r| r.contains("\"secrets\""))
        .collect();
    assert_eq!(secret_rules.len(), 1);
    assert!(
        secret_rules[0].contains("verbs: [\"create\", \"delete\"]"),
        "{}",
        secret_rules[0]
    );
    // Logs and pods read-only.
    assert!(workload.contains(
        "resources: [\"pods\", \"pods/log\"]\n    verbs: [\"get\", \"list\", \"watch\"]"
    ));
    // Force delete: pods may be deleted (stuck ones), never created.
    let pod_rules: Vec<_> = rules(workload)
        .into_iter()
        .filter(|r| r.contains("\"pods\""))
        .collect();
    assert_eq!(pod_rules.len(), 2, "{pod_rules:?}");
    assert!(pod_rules
        .iter()
        .any(|r| r.contains("resources: [\"pods\"]\n    verbs: [\"delete\"]")));
    assert!(!pod_rules.iter().any(|r| r.contains("\"create\"")));
    // app_exec: exec (get + create, the WebSocket upgrade needs both) and nothing else of
    // its kind: no attach, no port-forward.
    let exec_rules: Vec<_> = rules(workload)
        .into_iter()
        .filter(|r| r.contains("pods/exec"))
        .collect();
    assert_eq!(exec_rules.len(), 1, "{exec_rules:?}");
    assert!(
        exec_rules[0].contains("resources: [\"pods/exec\"]\n    verbs: [\"get\", \"create\"]"),
        "{}",
        exec_rules[0]
    );
    for word in [
        "pods/attach",
        "pods/portforward",
        "serviceaccounts",
        "roles",
    ] {
        assert!(!workload.contains(word), "workload role mentions {word}");
    }
}

#[test]
fn admission_policies_guard_the_write_identity() {
    let docs = docs(&manifest("apps-rbac.yaml"));
    let user = format!("system:serviceaccount:{MELISSA_NAMESPACE}:{APPS_SERVICE_ACCOUNT}");
    for name in [
        "melissa-apps-namespaces",
        "melissa-apps-rolebindings",
        "melissa-apps-services",
        "melissa-apps-workloads",
        "melissa-apps-volumes",
    ] {
        let policy = doc(&docs, "ValidatingAdmissionPolicy", name);
        assert!(policy.contains("failurePolicy: Fail"), "{name}");
        assert!(
            policy.contains(&format!("request.userInfo.username == '{user}'")),
            "{name}"
        );
        // Kubernetes 1.37's CEL exposes `namespace` as is; `__namespace__` is "no such key"
        // and `has(x.__namespace__)` is silently false (found in the VM, 2026-11-18).
        assert!(!policy.contains("__namespace__"), "{name}");
        let binding = doc(&docs, "ValidatingAdmissionPolicyBinding", name);
        assert!(binding.contains(&format!("policyName: {name}")));
        assert!(binding.contains("validationActions: [Deny]"));
    }
    let ns = doc(
        &docs,
        "ValidatingAdmissionPolicy",
        "melissa-apps-namespaces",
    );
    assert!(ns.contains("startsWith('app-')"));
    assert!(ns.contains("['baseline', 'restricted']"));
    // Workspaces: workspace and device labels required on create, device frozen, the owner agent changes
    // only with a new assignment record, the old owner label may only go away.
    assert!(ns.contains(&format!(
        "object.metadata.labels['{WORKSPACE_LABEL}'] == object.metadata.name.substring(4)"
    )));
    assert!(ns.contains(&format!(
        "object.metadata.labels['{DEVICE_LABEL}'] == oldObject.metadata.labels['{DEVICE_LABEL}']"
    )));
    assert!(ns.contains("^d-[0-9a-f]{8}$"));
    assert!(ns.contains("^d-[0-9a-f]{8}[.]a[1-9][0-9]{0,8}$"));
    assert!(ns.contains(&format!(
        "object.metadata.labels['{OWNER_AGENT_LABEL}'] == oldObject.metadata.labels['{OWNER_AGENT_LABEL}'] ||\n            variables.newAssignment"
    )));
    assert!(ns.contains(&format!(
        "object.metadata.annotations['{ASSIGNED_AT_ANNOTATION}'] !="
    )));
    assert!(ns.contains(&format!(
        "!('{LEGACY_OWNER_LABEL}' in object.metadata.labels))"
    )));
    // Old label keys from before the rename: only kept or dropped, and the new keys take their
    // values in the new id shape (the one-time relabel).
    assert!(ns.contains(&format!(
        "['{OLD_WORKSPACE_LABEL}', '{OLD_DEVICE_LABEL}', '{OLD_OWNER_LABEL}'].all(k,"
    )));
    assert!(ns.contains(&format!(
        "'d-' + variables.oldLabels['{OLD_DEVICE_LABEL}'].substring(2)"
    )));
    assert!(ns.contains(&format!(
        "'d-' + variables.oldLabels['{OLD_OWNER_LABEL}'].substring(2, 11) + 'a' +"
    )));
    assert!(ns.contains("variables.newAssignment ||\n           variables.ownerRelabelled))"));
    // App ports: in Melissa's range, fixed once recorded.
    let (lo, hi) = (APP_PORTS.start(), APP_PORTS.end());
    assert_eq!(
        (*lo, *hi),
        (30000, 30079),
        "update the policies with the range"
    );
    assert!(ns.contains(&format!("'{PORT_ANNOTATION}'")));
    assert!(ns.contains("matches('^300[0-7][0-9]$')"));
    assert!(ns.contains(&format!(
        "object.metadata.annotations['{PORT_ANNOTATION}'] == oldObject.metadata.annotations['{PORT_ANNOTATION}']"
    )));
    let svc = doc(&docs, "ValidatingAdmissionPolicy", "melissa-apps-services");
    assert!(svc.contains(&format!(
        "object.spec.ports.all(p, has(p.nodePort) && p.nodePort >= {lo} && p.nodePort <= {hi})"
    )));
    assert!(svc.contains("request.namespace.substring(4) + '-port'"));
    assert!(svc.contains("p.protocol in ['TCP', 'UDP']"));
    assert!(ns.contains(&format!("'{PORTS_ANNOTATION}'")));
    assert!(ns.contains(":300[0-7][0-9]/(tcp|udp))*$')"));
    // Workloads: whatever spec an agent writes, never privileged or on the host.
    let wl = doc(&docs, "ValidatingAdmissionPolicy", "melissa-apps-workloads");
    for needle in [
        "variables.pod.hostNetwork",
        "variables.pod.hostPID",
        "has(v.persistentVolumeClaim) || has(v.configMap)",
        "!c.securityContext.privileged",
        "cap in variables.allowedCaps",
        "p.hostPort == 0",
        "'cpu' in c.resources.limits && 'memory' in c.resources.limits",
        "seccompProfile.type == 'RuntimeDefault'",
        "!variables.pod.automountServiceAccountToken",
        "'melissa.io/workspace' in namespaceObject.metadata.labels",
    ] {
        assert!(wl.contains(needle), "workloads policy lacks {needle}");
    }
    assert!(!wl.contains("SYS_ADMIN") && !wl.contains("NET_ADMIN"));
    let vol = doc(&docs, "ValidatingAdmissionPolicy", "melissa-apps-volumes");
    assert!(vol.contains("!has(object.spec.volumeName)"));
    assert!(!svc.contains("LoadBalancer"), "never allowed");
    assert_eq!(port_service_name("nextcloud"), "nextcloud-port");
    let rb = doc(
        &docs,
        "ValidatingAdmissionPolicy",
        "melissa-apps-rolebindings",
    );
    assert!(rb.contains(&format!("object.roleRef.name == '{WORKLOAD_ROLE}'")));
    assert!(rb.contains("object.subjects.size() == 1"));
    // The setup password reader: exactly its binding, role and subject; deletes only of that
    // binding; never again once dismissed (and the dismissal itself is frozen).
    assert_eq!(CREDENTIALS_BINDING, CREDENTIALS_ROLE);
    assert!(rb.contains(&format!("object.metadata.name == '{CREDENTIALS_BINDING}'")));
    assert!(rb.contains(&format!("object.roleRef.name == '{CREDENTIALS_ROLE}'")));
    assert!(rb.contains(&format!("s.name == '{CREDENTIALS_SERVICE_ACCOUNT}'")));
    assert!(
        rb.contains("request.operation == 'DELETE' || variables.workload || variables.credentials")
    );
    assert!(rb.contains(&format!(
        "request.operation != 'DELETE' || oldObject.metadata.name == '{CREDENTIALS_BINDING}'"
    )));
    assert!(rb.contains(&format!(
        "!variables.credentials || !has(namespaceObject.metadata.annotations) ||\n        !('{CREDENTIALS_DISMISSED_ANNOTATION}' in namespaceObject.metadata.annotations)"
    )));
    assert!(ns.contains(&format!(
        "object.metadata.annotations['{CREDENTIALS_DISMISSED_ANNOTATION}'] ==\n           oldObject.metadata.annotations['{CREDENTIALS_DISMISSED_ANNOTATION}']"
    )));
    // Policies come before the permissions they guard.
    let text = manifest("apps-rbac.yaml");
    let first_policy = text.find("kind: ValidatingAdmissionPolicy\n").unwrap();
    let first_role = text.find("kind: ClusterRole\n").unwrap();
    assert!(first_policy < first_role);
}

#[test]
fn the_setup_password_reader_only_gets_secrets_per_app() {
    let docs = docs(&manifest("credentials-rbac.yaml"));
    let sa = doc(&docs, "ServiceAccount", CREDENTIALS_SERVICE_ACCOUNT);
    assert!(sa.contains(&format!("namespace: {MELISSA_NAMESPACE}")));
    assert!(sa.contains("automountServiceAccountToken: false"));
    let role = doc(&docs, "ClusterRole", CREDENTIALS_ROLE);
    let role_rules = rules(role);
    assert_eq!(role_rules.len(), 1, "{role}");
    assert!(
        role_rules[0].contains("resources: [\"secrets\"]\n    verbs: [\"get\"]"),
        "{role}"
    );
    // Bound per app namespace only (by melissa-apps), never cluster-wide or in a manifest.
    for file in [
        "apps-rbac.yaml",
        "server-rbac.yaml",
        "credentials-rbac.yaml",
    ] {
        for d in docs_of(file) {
            assert!(
                !(d.contains("kind: ClusterRoleBinding") && d.contains(CREDENTIALS_ROLE)),
                "{file}"
            );
            assert!(
                !(d.contains("kind: RoleBinding\n") && d.contains(CREDENTIALS_SERVICE_ACCOUNT)),
                "{file}"
            );
        }
    }
}

fn docs_of(file: &str) -> Vec<String> {
    docs(&manifest(file))
}

/// gVisor (2026-11-23): an app labelled gvisor never runs on the default runtime, whoever
/// creates its pods; no other RuntimeClass is allowed; the gvisor RuntimeClass names runsc.
#[test]
fn gvisor_apps_never_land_on_the_default_runtime() {
    let docs = docs(&manifest("apps-rbac.yaml"));
    let pinned = |p: &str, pod: &str| {
        assert!(
            p.contains(&format!(
                "!variables.wantsGvisor ||\n        (has({pod}.runtimeClassName) && {pod}.runtimeClassName == '{GVISOR_RUNTIME_CLASS}')"
            )),
            "{p}"
        );
        assert!(p.contains(&format!(
            "!has({pod}.runtimeClassName) || {pod}.runtimeClassName == '{GVISOR_RUNTIME_CLASS}'"
        )));
        // The pod's own label or its namespace's.
        assert!(p.contains(&format!(
            "namespaceObject.metadata.labels['{ISOLATION_LABEL}'] == 'gvisor'"
        )));
    };
    // The write identity's Deployments.
    let workloads = doc(&docs, "ValidatingAdmissionPolicy", "melissa-apps-workloads");
    pinned(workloads, "variables.pod");
    assert!(workloads.contains(&format!(
        "object.spec.template.metadata.labels['{ISOLATION_LABEL}'] == 'gvisor'"
    )));
    // The pods themselves, from any creator, in app namespaces only.
    let pods = doc(&docs, "ValidatingAdmissionPolicy", "melissa-apps-isolation");
    pinned(pods, "object.spec");
    assert!(pods.contains("failurePolicy: Fail"));
    assert!(pods.contains("resources: [\"pods\"]"));
    assert!(pods.contains("melissa.io/managed: \"true\""));
    assert!(!pods.contains("request.userInfo.username"), "every creator");
    // The RuntimeClass keeps its handler.
    let rc = doc(
        &docs,
        "ValidatingAdmissionPolicy",
        "melissa-gvisor-runtimeclass",
    );
    assert!(rc.contains(&format!(
        "object.metadata.name != '{GVISOR_RUNTIME_CLASS}' || object.handler == '{GVISOR_HANDLER}'"
    )));
    for name in ["melissa-apps-isolation", "melissa-gvisor-runtimeclass"] {
        let binding = doc(&docs, "ValidatingAdmissionPolicyBinding", name);
        assert!(binding.contains("validationActions: [Deny]"), "{name}");
    }
    // The namespace label holds a known level.
    let ns = doc(
        &docs,
        "ValidatingAdmissionPolicy",
        "melissa-apps-namespaces",
    );
    assert!(ns.contains(&format!(
        "object.metadata.labels['{ISOLATION_LABEL}'] in ['standard', 'gvisor']"
    )));
    // The write identity may only look up that one RuntimeClass.
    let cluster = doc(&docs, "ClusterRole", "melissa-apps");
    assert!(cluster.contains(&format!(
        "resources: [\"runtimeclasses\"]\n    verbs: [\"get\"]\n    resourceNames: [\"{GVISOR_RUNTIME_CLASS}\"]"
    )));

    // The RuntimeClass manifest (installed only where runsc is).
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging/common/usr/lib/melissa/k3s/gvisor");
    let read = |f: &str| std::fs::read_to_string(path.join(f)).unwrap();
    let rc = read("runtimeclass.yaml");
    assert!(rc.contains("kind: RuntimeClass\n"));
    assert!(rc.contains(&format!("\n  name: {GVISOR_RUNTIME_CLASS}\n")));
    assert!(rc.contains(&format!("\nhandler: {GVISOR_HANDLER}\n")));
    // containerd's entry for that handler, with gVisor's options file, and the platform.
    let tmpl = read("config-v3.toml.tmpl");
    assert!(tmpl.lines().next().unwrap().contains("melissa-managed"));
    assert!(tmpl.contains("{{ template \"base\" . }}"));
    assert!(tmpl.contains(&format!(
        "containerd.runtimes.{GVISOR_HANDLER}]\n  runtime_type = \"io.containerd.runsc.v1\""
    )));
    assert!(tmpl.contains("ConfigPath = \"/usr/lib/melissa/k3s/gvisor/runsc.toml\""));
    assert!(read("runsc.toml").contains("platform = \"systrap\""));
}
