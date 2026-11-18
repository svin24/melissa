//! Sanity checks on the RBAC manifests the image ships (text level: no YAML parser in the
//! build). They guard the properties the app tools' security relies on; the VM test
//! (`docs/agent-skills/app-catalog.md`) checks the same against a live API server.

use std::path::PathBuf;

use melissa_hive::render::{APPS_SERVICE_ACCOUNT, MELISSA_NAMESPACE, WORKLOAD_ROLE};

fn manifest(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../image/rootfs/usr/lib/melissa/k3s/manifests")
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
    for file in ["apps-rbac.yaml", "queen-rbac.yaml"] {
        let text = docs(&manifest(file)).join("\n");
        assert!(!text.contains("\"*\""), "{file} has a wildcard");
        assert!(!text.contains("cluster-admin"), "{file}");
        assert!(!text.contains("escalate"), "{file}");
        assert!(!text.contains("impersonate"), "{file}");
    }
}

#[test]
fn the_read_only_identity_stays_read_only() {
    let docs = docs(&manifest("queen-rbac.yaml"));
    let role = doc(&docs, "ClusterRole", "melissa-queen-read");
    for rule in rules(role) {
        assert!(
            rule.contains("verbs: [\"get\", \"list\", \"watch\"]"),
            "{rule}"
        );
        assert!(!rule.contains("secrets"), "{rule}");
    }
}

#[test]
fn the_write_identity_is_narrow() {
    let docs = docs(&manifest("apps-rbac.yaml"));
    assert_eq!(WORKLOAD_ROLE, "melissa-app-workload");
    let sa = doc(&docs, "ServiceAccount", APPS_SERVICE_ACCOUNT);
    assert!(sa.contains(&format!("namespace: {MELISSA_NAMESPACE}")));
    assert!(sa.contains("automountServiceAccountToken: false"));

    // Cluster-wide: namespaces, rolebindings, and `bind` on exactly the workload role.
    let cluster = doc(&docs, "ClusterRole", "melissa-apps");
    let cluster_rules = rules(cluster);
    assert_eq!(cluster_rules.len(), 3, "{cluster}");
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
        bind.contains(&format!("resourceNames: [\"{WORKLOAD_ROLE}\"]")),
        "{bind}"
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
    // Logs and pods read-only; no exec/attach/port-forward.
    assert!(workload.contains(
        "resources: [\"pods\", \"pods/log\"]\n    verbs: [\"get\", \"list\", \"watch\"]"
    ));
    for word in [
        "pods/exec",
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
    let rb = doc(
        &docs,
        "ValidatingAdmissionPolicy",
        "melissa-apps-rolebindings",
    );
    assert!(rb.contains(&format!("object.roleRef.name == '{WORKLOAD_ROLE}'")));
    assert!(rb.contains("object.subjects.size() == 1"));
    // Policies come before the permissions they guard.
    let text = manifest("apps-rbac.yaml");
    let first_policy = text.find("kind: ValidatingAdmissionPolicy\n").unwrap();
    let first_role = text.find("kind: ClusterRole\n").unwrap();
    assert!(first_policy < first_role);
}
