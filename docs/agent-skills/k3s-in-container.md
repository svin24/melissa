# k3s in the Melissa container

**When:** changing k3s, its config, or anything that runs in the cluster; bumping the k3s version.

## How it's built (packaging/container/Containerfile, stage `k3s`)
- Versions and sha256 hashes are pinned in `packaging/common/usr/lib/melissa/k3s.lock` (sourced as shell). The fetch
  stage downloads the k3s binary, `k3s-airgap-images-amd64.tar.zst` and the k3s `LICENSE` from the release tag
  (raw.githubusercontent.com, `+` as `%2B`) and `sha256sum -c`s them. No install script. k3s has no NOTICE file.
  The lock also pins `k3s-selinux` (rpm.rancher.io, coreos build) for SELinux hosts; the container doesn't use it.
- Image: `/usr/bin/k3s` + `kubectl`/`crictl`/`ctr` symlinks, `/usr/share/licenses/k3s/LICENSE`, airgap tarball at
  `/usr/lib/melissa/k3s/images/k3s-airgap-images-<ver>-amd64.tar.zst`, `/usr/lib/melissa/k3s.lock`. **`iptables-nft`
  is installed**: k3s uses the iptables on PATH; its bundled one picked the legacy backend in a fresh container and
  needs the `ip_tables` module (absent on modern hosts), so kube-proxy exited (found 2026-11-19).
- `k3s.service` (`packaging/common`, enabled in the image). `ExecStartPre=/usr/libexec/melissa/k3s-prepare` links the
  airgap tarball into `/var/lib/rancher/k3s/agent/images/` (version in the link name, because `/usr` mtimes are 0),
  removes dangling links from older versions, installs `/usr/lib/melissa/k3s/manifests/*.yaml` into the server
  manifests dir, pins the node IP (below) and runs `restorecon` once (a no-op without SELinux).
- Config: k3s only reads `/etc/rancher/k3s/config.yaml(.d)`. Common `10-melissa.yaml` (kubeconfig 0600,
  `melissa.io/managed=true` label); container `20-container.yaml`: `selinux: false` (SELinux reads as disabled
  inside; the host confines the container), kubelet `KubeletInUserNamespace=true` (only matters rootless, a no-op
  rootful), kube-proxy `conntrack-max-per-core=0` (leave the host's conntrack size alone, as kind does). Lead agent
  writes `50-*.yaml`; local overrides `90-local.yaml`.
- Inside the container: own network namespace, so k3s's `cni0`, `flannel.1`, VXLAN 8472, 6443/10250 and kube-proxy
  rules never touch the host's (verified next to a host k3s). containerd uses overlayfs on the `/var` volume. The
  kubelet writes host-wide sysctls (install.sh sets them openly, see packaging/container/README.md).
- Kernel modules come from the host: install.sh loads `br_netfilter` and `overlay` and adds
  `/etc/modules-load.d/melissa.conf`; the container has no `/lib/modules` (`modprobe` in k3s.service is a no-op).

## App installs: second identity (2026-11-18)
- `apps-rbac.yaml` (installed as `melissa-apps-rbac.yaml`): ServiceAccount `melissa/melissa-apps` with
  admission policies that pin it to `app-*` namespaces; `melissa-kubeconfig` also writes
  `/run/melissa-kube/apps.kubeconfig`. Details and verified results: [app-deploy.md](app-deploy.md).
- `k3s-prepare` reinstalls every manifest from `/usr` on each k3s start, so live edits under
  `/var/lib/rancher/k3s/server/manifests` don't survive a k3s restart.

## Lead agent access to the API (read-only, 2026-11-18)
- Manifest `packaging/common/usr/lib/melissa/k3s/manifests/server-rbac.yaml` (installed by `k3s-prepare` as
  `melissa-server-rbac.yaml`): namespace `melissa`, ServiceAccount `melissa-server`, Secret `melissa-server-token`
  (type `kubernetes.io/service-account-token`), ClusterRole/Binding `melissa-server-read` = get/list/watch on
  nodes, namespaces, pods. No Secrets, no writes. Widen it only together with a reviewed tool.
- `melissa-kubeconfig.service` (root oneshot, `WantedBy=k3s.service`, `Restart=on-failure`)
  runs `/usr/libexec/melissa/melissa-kubeconfig`: waits ≤300 s for the token, writes
  `/run/melissa-kube/server.kubeconfig` (root:`melissa-kube` 0640; dir 0750 from `tmpfiles.d/melissa-kube.conf`;
  group from `sysusers.d/melissa.conf`; server `https://127.0.0.1:6443`, CA + token from the Secret).
- `melissa-server.service` is **not** ordered after k3s or the oneshot (since 2026-11-18): system user `melissa` +
  `SupplementaryGroups=melissa-kube`. The lead agent (`cluster::LazyCluster`) looks for the file on every cluster call and
  rebuilds its client when the contents change, so it starts at once, attaches when k3s is up and follows rotations
  without a restart. `cluster_health`: no dir → "k3s is not running on this agent"; dir but no file → "k3s is
  starting"; file but API down → `k3s_reachable: false` with the connect error.
- Why not systemd credentials any more: `ImportCredential=`/`LoadCredential=` copy once at service start, so a late
  or rotated kubeconfig needed a lead agent restart (kills agent tasks). See DECISIONS.md.
- Rotate/revoke: `sudo podman exec melissa /usr/libexec/melissa/melissa-kubeconfig rotate` (recreates the ServiceAccount, rewrites the
  kubeconfig; the lead agent picks it up on its next cluster call).
- Run a tool as the lead agent would (same file and group):
  `sudo podman exec melissa systemd-run --quiet --wait --pipe --collect -p User=melissa -p SupplementaryGroups=melissa-kube
  -p PrivateTmp=yes -E XDG_STATE_HOME=/tmp -E XDG_CONFIG_HOME=/etc /usr/bin/melissa-server tool cluster_health`.
  Client details: [kube-rs-client.md](kube-rs-client.md).

## Bumping k3s
1. Never skip a Kubernetes minor version. 2. Edit `packaging/common/usr/lib/melissa/k3s.lock`: version, hashes from
the release's `sha256sum-amd64.txt`, `K3S_LICENSE_SHA256` as described in the lock file header. 3. `just
container-build`, `just container-install` (keeps the volume), `just container-smoke`. Back up `/var/lib/rancher/k3s/
server/db` and `server/token` from the volume first if you may need to go back (an image rollback doesn't roll back
the datastore). Channel versions: `curl -s https://update.k3s.io/v1-release/channels` (stable = v1.37.2+k3s1 on
2026-11-18).

## Verified (2026-11-19, rootful container on the dev laptop)
- `just container-smoke`: k3s active, node Ready, system pods (Traefik) running, overlayfs snapshotter, Traefik
  update check off, the three ValidatingAdmissionPolicies bound, RBAC (`melissa-apps` can't touch kube-system),
  policy denies a non-app namespace, Traefik answers inside, a NodePort from the host, a pod can't reach the UI,
  logind/sleep masked and `/dev/input` unopenable.
- First start ~1–2 min (certificates, airgap import). Every recreate (start, update, rollback) gives the container
  a new bridge address: k3s restarts once ("NodeIPs changed") and recovers within ~1 min (PLAN question 61a).
- Earlier, in the bootc VM (archived): k3s-selinux with SELinux enforcing, no AVC denials.

## Verified: lead agent access (2026-11-18, bootc VM; same files in the container)
- Fresh path (namespace deleted, image deployed): manifest applied, token filled, kubeconfig written 3.5 s after
  the oneshot started; lead agent logged `k3s client configured`. `cluster_health` through the lead agent's credential:
  node Ready, `v1.37.2+k3s1`, kube-system 5 Running / 3 Succeeded; a pod with a bad image showed up as
  `Pending` / `ImagePullBackOff`. With k3s stopped: `k3s_reachable: false` (Connect error), no crash. Without a
  credential: "k3s is not running on this agent".
- With the minted token: get nodes/pods allowed; get secrets, create namespace, delete pods denied.
- Smoke test still passes; no AVC denials.

## Verified: lead agent starts before k3s (2026-11-18, bootc VM)
- Boot probe (test-only unit polling `/healthz` and `cluster_health` every 2 s): lead agent `listening` 07:23:36.9,
  `/healthz` 200 at 07:23:39.1; k3s `Started` (notify, API up) 07:23:45.9; kubeconfig written 07:23:46.3.
  `cluster_health`: "k3s is starting: the lead agent's cluster credentials are not issued yet" → `k3s_reachable: true`,
  node ready, same lead agent PID, `NRestarts=0`.
- Lead agent running throughout: `systemctl stop k3s` → `k3s_reachable: false` ("ServiceError: client error (Connect)");
  start → healthy; `melissa-kubeconfig rotate` → new file hash, still healthy; lead agent PID and `NRestarts` unchanged.
- `/run/melissa-kube` is `drwxr-x--- root melissa-kube`, file `-rw-r----- root melissa-kube`; the `agent` user and
  `nobody` get "Permission denied".
- k3s: kubelet runs with the pinned IPv4 `--node-ip` while an IPv6 address is on the NIC; no `NodeIPs changed`.

## Gotchas
- **Secret-based SA tokens are deterministic:** no `iat`/`exp` claims and RS256 signatures are deterministic, so a
  recreated Secret with the same name gets the *same* token (old one stays valid). Rotation must recreate the
  ServiceAccount (new UID). Verified 2026-11-18 on v1.37.2.
- **Node IP is pinned to IPv4** (fixed 2026-11-18). Before, k3s exited twice per boot with `NodeIPs changed`
  (`NRestarts=2`). Root cause: k3s advertises the default interface's IPv4 *and* global IPv6 address, but SLAAC adds
  the IPv6 one ~2 s after `network-online.target` (an IPv4 lease is enough for NetworkManager-wait-online). Run 1
  starts with v4 while the stored Node has v4+v6 → updates it to v4 and exits; run 2 sees v4+v6 vs the stored v4 →
  exits; run 3 is stable. Fix: `k3s-prepare` (through the shared `k3s-node-ip`, which the NixOS module also uses) writes `MELISSA_K3S_ARGS=--node-ip=<IPv4 of the default route>` to
  `/run/melissa/k3s.env` (`EnvironmentFile=`, which systemd re-reads per command, so ExecStartPre can write it for
  ExecStart). The cluster is single-stack IPv4 anyway, and changing IPv6 prefixes/privacy addresses would otherwise
  restart k3s at runtime too. A `node-ip` in `config.yaml.d/90-local.yaml` wins (k3s-prepare then adds nothing); no
  IPv4 default route (IPv6-only agent) → k3s autodetects as before. The first boot after this update still restarts
  once (stored v4+v6 vs pinned v4). k3s has no `K3S_NODE_IP` env var; the flag is passed on the command line.
  Rejected: `ipv6.may-fail=no` in NetworkManager (breaks networks without IPv6), hard-coding an address.
  API clients must still treat short k3s outages as "unreachable" (k3s restarts on real IPv4 changes).
- **Traefik chart 41.x** (k3s v1.37): telemetry is `global.checkNewVersion` (default **true** → Traefik checks online
  for new versions) and `global.sendAnonymousUsage` (default false). The old `globalArguments` key is ignored. Melissa's
  HelmChartConfig sets both false; the smoke test asserts `--global.checkNewVersion=false` in Traefik's args.
- Anyone who can create HelmChart objects is effectively cluster-admin; only the lead agent should, from reviewed manifests.
- An image rollback restores the old k3s binary but not `/var` (datastore, on the volume). Rolling back across a Kubernetes minor needs
  a datastore snapshot taken before staging the update (back up `server/db` **and** `server/token`).
- App images (e.g. the smoke test's `traefik/whoami`) are pulled from public registries; that's a third-party call
  and needs a PLAN decision for real apps (mirror/cache?).
