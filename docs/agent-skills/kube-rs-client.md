# kube-rs client in Melissa (crates/hive)

**When:** adding a cluster tool, giving the queen more Kubernetes permissions, or bumping kube/k8s-openapi.

## Layout
- `crates/hive/src/cluster.rs`: `ClusterRead` trait (one method per object kind a tool needs, returning
  k8s-openapi types), `KubeCluster` (the real client), `ClusterError` (NotConfigured / Unreachable / Forbidden /
  Api / Config / NotConfigured / Starting), `LazyCluster` (the queen's `ClusterRead`: on every call finds the first
  existing kubeconfig of systemd credential `melissa.kubeconfig`, `kubeconfig` in `queen.toml`,
  `/run/melissa-kube/queen.kubeconfig`; rebuilds the client only when the file contents change; `Starting` when
  `/run/melissa-kube` exists without a file). Never cache a `KubeCluster` at startup: k3s may come up much later.
- Tools (`health.rs`) take `Option<Arc<dyn ClusterRead>>`; `None` = no cluster on this bee. Unit tests use a fake
  that returns hand-built `Node`/`Pod` objects (all k8s-openapi structs implement `Default`).
- Writes (2026-11-18): `apps.rs` has `AppRead` + `AppDeployer`, implemented by `KubeApps` over a second
  identity (`LazyCluster::for_apps`, `/run/melissa-kube/apps.kubeconfig`, RBAC in `apps-rbac.yaml`). Only
  the app tools hold them; read-only tools keep `ClusterRead`. See [app-catalog.md](app-catalog.md).
- Server-side apply: `Api::patch(name, &PatchParams::apply("melissa").force(), &Patch::Apply(&obj))`;
  k8s-openapi types serialize `apiVersion`/`kind` themselves. Create-once: `Api::create` and treat 409
  as "exists". 404 from get/delete: `kube::Error::Api(s) if s.code == 404`. A fresh RoleBinding may take
  a moment to reach the authorizer: `apps.rs` retries 403s for a few seconds.
- k8s-openapi 0.29: `Deployment.spec` is not an `Option`; `RoleRef.api_group` is.

## Steps for a new cluster tool
1. Add the read method to `ClusterRead` + `KubeCluster` (`Api::<T>::all(client).list(&ListParams::default())`).
2. Add the verbs/resources to the ClusterRole (get/list/watch only unless the tool is labelled with an effect).
3. Label honestly: anything returning free text from apps (logs, events, container messages) is `reads_untrusted`.
   `cluster_health` returns only names, counts and sanitized reason codes, so it stays `READ_ONLY`.
4. Fake-based unit tests; then in the VM: `sudo systemd-run --wait --pipe -p DynamicUser=yes
   -p SupplementaryGroups=melissa-kube -E XDG_STATE_HOME=/tmp -E XDG_CONFIG_HOME=/etc -p PrivateTmp=yes
   /usr/bin/melissa-queen tool <name>` (same kubeconfig file and group as the service).

## Versions and features (2026-11-18)
- `kube = { version = "5.1", default-features = false, features = ["client", "rustls-tls", "ring"] }`,
  `k8s-openapi = { version = "0.29", features = ["v1_37"] }` (newest feature; matches k3s v1.37.2). Bump the
  k8s-openapi feature together with `image/k3s.lock`.
- No OpenSSL: `cargo tree -i openssl-sys` must stay empty. `ring` needs a C compiler (dev shell `cc`, `gcc` in the
  image build stage); `aws-lc-rs` would also need cmake, so we avoid it.
- kube 5.x: `kube::Error::Api(status)` carries `status.code` and `status.message`; transport failures are
  `HyperError` / `Service` / `HttpError` → map to "unreachable".
- `Config::from_custom_kubeconfig` + `Client::try_from` do not contact the server; the first request does. A
  client pointed at a dead port returns `Unreachable` quickly (unit test uses `127.0.0.1:1`).

## Gotchas
- `Client::try_from` needs a Tokio runtime (it spawns a buffer task): build it inside async code.
- `apiserver_version()` (`/version`) is readable by any authenticated client (`system:public-info-viewer`), so it
  is a good reachability probe that doesn't depend on RBAC.
