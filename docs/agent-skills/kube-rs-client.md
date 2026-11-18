# kube-rs client in Melissa (crates/hive)

**When:** adding a cluster tool, giving the queen more Kubernetes permissions, or bumping kube/k8s-openapi.

## Layout
- `crates/hive/src/cluster.rs`: `ClusterRead` trait (one method per object kind a tool needs, returning
  k8s-openapi types), `KubeCluster` (the real client), `ClusterError` (NotConfigured / Unreachable / Forbidden /
  Api / Config), `find_kubeconfig` (systemd credential `melissa.kubeconfig` first, then the `kubeconfig` path in
  `queen.toml`).
- Tools (`health.rs`) take `Option<Arc<dyn ClusterRead>>`; `None` = no cluster on this bee. Unit tests use a fake
  that returns hand-built `Node`/`Pod` objects (all k8s-openapi structs implement `Default`).
- Writes: add a separate trait (e.g. `ClusterWrite`) and matching RBAC in
  `image/rootfs/usr/lib/melissa/k3s/manifests/queen-rbac.yaml`, so read-only tools never hold write methods.

## Steps for a new cluster tool
1. Add the read method to `ClusterRead` + `KubeCluster` (`Api::<T>::all(client).list(&ListParams::default())`).
2. Add the verbs/resources to the ClusterRole (get/list/watch only unless the tool is labelled with an effect).
3. Label honestly: anything returning free text from apps (logs, events, container messages) is `reads_untrusted`.
   `cluster_health` returns only names, counts and sanitized reason codes, so it stays `READ_ONLY`.
4. Fake-based unit tests; then in the VM: `sudo systemd-run --wait --pipe -p DynamicUser=yes
   -p ImportCredential=melissa.kubeconfig -E XDG_STATE_HOME=/tmp -E XDG_CONFIG_HOME=/etc -p PrivateTmp=yes
   /usr/bin/melissa-queen tool <name>` (same credential path as the service).

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
