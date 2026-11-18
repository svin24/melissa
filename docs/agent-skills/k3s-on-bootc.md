# k3s in the Melissa images

**When:** changing k3s, its config, or anything that runs in the cluster; bumping the k3s version.

## How it's built (image/Containerfile, stage `k3s`)
- Versions and sha256 hashes are pinned in `image/k3s.lock` (sourced as shell). The fetch stage downloads the k3s
  binary, `k3s-airgap-images-amd64.tar.zst` and `k3s-selinux` from rpm.rancher.io (`coreos` build; there is no
  Fedora build) and `sha256sum -c`s all three. No install script.
- Final image: `/usr/bin/k3s` + `kubectl`/`crictl`/`ctr` symlinks, k3s-selinux installed with dnf, airgap tarball at
  `/usr/lib/melissa/k3s/images/k3s-airgap-images-<ver>-amd64.tar.zst`, `/usr/lib/melissa/k3s.lock`.
- `k3s.service` (ours, in `/usr/lib/systemd/system`) is enabled on headless only. `ExecStartPre=/usr/libexec/melissa/k3s-prepare`
  links the airgap tarball into `/var/lib/rancher/k3s/agent/images/` (version in the link name, because `/usr` mtimes
  are 0), removes dangling links from older versions, installs `/usr/lib/melissa/k3s/manifests/*.yaml` into the server
  manifests dir, and runs `restorecon` once on k3s paths.
- Config: k3s only reads `/etc/rancher/k3s/config.yaml(.d)`, so the image ships `config.yaml.d/10-melissa.yaml`
  (`selinux: true`, kubeconfig 0600, `melissa.io/managed=true` label). Queen writes `50-*.yaml`; local: `90-local.yaml`.

## Queen access to the API (read-only, 2026-11-18)
- Manifest `image/rootfs/usr/lib/melissa/k3s/manifests/queen-rbac.yaml` (installed by `k3s-prepare` as
  `melissa-queen-rbac.yaml`): namespace `melissa`, ServiceAccount `melissa-queen`, Secret `melissa-queen-token`
  (type `kubernetes.io/service-account-token`), ClusterRole/Binding `melissa-queen-read` = get/list/watch on
  nodes, namespaces, pods. No Secrets, no writes. Widen it only together with a reviewed tool.
- `melissa-kubeconfig.service` (root oneshot, `WantedBy=k3s.service`, `Restart=on-failure`, enabled on headless)
  runs `/usr/libexec/melissa/melissa-kubeconfig`: waits ≤300 s for the token, writes
  `/run/melissa-kube/queen.kubeconfig` (root:`melissa-kube` 0640; dir 0750 from `tmpfiles.d/melissa-kube.conf`,
  headless only; group from `sysusers.d/melissa.conf`; server `https://127.0.0.1:6443`, CA + token from the Secret).
- `melissa-queen.service` is **not** ordered after k3s or the oneshot (since 2026-11-18): DynamicUser +
  `SupplementaryGroups=melissa-kube`. The queen (`hive::LazyCluster`) looks for the file on every cluster call and
  rebuilds its client when the contents change, so it starts at once, attaches when k3s is up and follows rotations
  without a restart. `cluster_health`: no dir → "k3s is not running on this bee"; dir but no file → "k3s is
  starting"; file but API down → `k3s_reachable: false` with the connect error.
- Why not systemd credentials any more: `ImportCredential=`/`LoadCredential=` copy once at service start, so a late
  or rotated kubeconfig needed a queen restart (kills agent tasks). See DECISIONS.md.
- Rotate/revoke: `sudo /usr/libexec/melissa/melissa-kubeconfig rotate` (recreates the ServiceAccount, rewrites the
  kubeconfig; the queen picks it up on its next cluster call).
- Run a tool as the queen would (same file and group):
  `sudo systemd-run --quiet --wait --pipe --collect -p DynamicUser=yes -p SupplementaryGroups=melissa-kube
  -p PrivateTmp=yes -E XDG_STATE_HOME=/tmp -E XDG_CONFIG_HOME=/etc /usr/bin/melissa-queen tool cluster_health`.
  Client details: [kube-rs-client.md](kube-rs-client.md).

## Bumping k3s
1. Never skip a Kubernetes minor version. 2. Edit `image/k3s.lock`: version, hashes from the release's
`sha256sum-amd64.txt`. 3. Build, `just vm-dev deploy headless`, `just vm-k3s-smoke`.
Channel versions: `curl -s https://update.k3s.io/v1-release/channels` (stable = v1.37.2+k3s1 on 2026-11-18).

## Verified (2026-11-18, headless VM, 3 GiB RAM)
- k3s v1.37.2+k3s1 with k3s-selinux 1.6 (coreos) on fedora-bootc 45, SELinux enforcing: **no AVC denials**, node Ready,
  CoreDNS/Traefik/local-path/metrics-server running, system images imported from the airgap bundle (0 pulls).
- Ingress works from the host: `curl --resolve whoami.melissa.home.arpa:18080:127.0.0.1 http://whoami.melissa.home.arpa:18080/`
  (VM forwards 127.0.0.1:16443→6443, 18080→80, 18443→443; `scripts/vm-k3s-smoke.sh` adds them live with `hostfwd_add`).
- Image with k3s: 2.75 GB headless; deploy of the k3s layers moved 572 MB in 47 s.
- First start takes ~30–60 s (certificates, airgap import); `systemctl is-active` says `activating` meanwhile.

## Verified: queen access (2026-11-18, headless VM)
- Fresh path (namespace deleted, image deployed): manifest applied, token filled, kubeconfig written 3.5 s after
  the oneshot started; queen logged `k3s client configured`. `cluster_health` through the queen's credential:
  node Ready, `v1.37.2+k3s1`, kube-system 5 Running / 3 Succeeded; a pod with a bad image showed up as
  `Pending` / `ImagePullBackOff`. With k3s stopped: `k3s_reachable: false` (Connect error), no crash. Without a
  credential: "k3s is not running on this bee".
- With the minted token: get nodes/pods allowed; get secrets, create namespace, delete pods denied.
- Smoke test still passes; no AVC denials.

## Verified: queen starts before k3s (2026-11-18, headless VM)
- Boot probe (test-only unit polling `/healthz` and `cluster_health` every 2 s): queen `listening` 07:23:36.9,
  `/healthz` 200 at 07:23:39.1; k3s `Started` (notify, API up) 07:23:45.9; kubeconfig written 07:23:46.3.
  `cluster_health`: "k3s is starting: the queen's cluster credentials are not issued yet" → `k3s_reachable: true`,
  node ready, same queen PID, `NRestarts=0`.
- Queen running throughout: `systemctl stop k3s` → `k3s_reachable: false` ("ServiceError: client error (Connect)");
  start → healthy; `melissa-kubeconfig rotate` → new file hash, still healthy; queen PID and `NRestarts` unchanged.
- `/run/melissa-kube` is `drwxr-x--- root melissa-kube`, file `-rw-r----- root melissa-kube`; the `bee` user and
  `nobody` get "Permission denied".
- k3s: kubelet runs with `--node-ip=10.0.2.15` while `fec0::…/64` is on the NIC; `NRestarts=0`, no `NodeIPs changed`
  (the first boot after the update restarted once, as expected). `vm-k3s-smoke.sh` passes; 0 AVC denials, enforcing.
- **`ausearch` is not in the images:** the smoke test's SELinux check used it and so always passed, including in
  the earlier "no AVC denials" results above. It now greps the journal for `avc:  denied` and checks `getenforce`.

## Gotchas
- **Secret-based SA tokens are deterministic:** no `iat`/`exp` claims and RS256 signatures are deterministic, so a
  recreated Secret with the same name gets the *same* token (old one stays valid). Rotation must recreate the
  ServiceAccount (new UID). Verified 2026-11-18 on v1.37.2.
- **Node IP is pinned to IPv4** (fixed 2026-11-18). Before, k3s exited twice per boot with `NodeIPs changed`
  (`NRestarts=2`). Root cause: k3s advertises the default interface's IPv4 *and* global IPv6 address, but SLAAC adds
  the IPv6 one ~2 s after `network-online.target` (an IPv4 lease is enough for NetworkManager-wait-online). Run 1
  starts with v4 while the stored Node has v4+v6 → updates it to v4 and exits; run 2 sees v4+v6 vs the stored v4 →
  exits; run 3 is stable. Fix: `k3s-prepare` writes `MELISSA_K3S_ARGS=--node-ip=<IPv4 of the default route>` to
  `/run/melissa/k3s.env` (`EnvironmentFile=`, which systemd re-reads per command, so ExecStartPre can write it for
  ExecStart). The cluster is single-stack IPv4 anyway, and changing IPv6 prefixes/privacy addresses would otherwise
  restart k3s at runtime too. A `node-ip` in `config.yaml.d/90-local.yaml` wins (k3s-prepare then adds nothing); no
  IPv4 default route (IPv6-only bee) → k3s autodetects as before. The first boot after this update still restarts
  once (stored v4+v6 vs pinned v4). k3s has no `K3S_NODE_IP` env var; the flag is passed on the command line.
  Rejected: `ipv6.may-fail=no` in NetworkManager (breaks networks without IPv6), hard-coding an address.
  API clients must still treat short k3s outages as "unreachable" (k3s restarts on real IPv4 changes).
- **Traefik chart 41.x** (k3s v1.37): telemetry is `global.checkNewVersion` (default **true** → Traefik checks online
  for new versions) and `global.sendAnonymousUsage` (default false). The old `globalArguments` key is ignored. Melissa's
  HelmChartConfig sets both false; the smoke test asserts `--global.checkNewVersion=false` in Traefik's args.
- Anyone who can create HelmChart objects is effectively cluster-admin; only the queen should, from reviewed manifests.
- `bootc rollback` restores the old k3s binary but not `/var` (datastore). Rolling back across a Kubernetes minor needs
  a datastore snapshot taken before staging the update (back up `server/db` **and** `server/token`).
- App images (e.g. the smoke test's `traefik/whoami`) are pulled from public registries; that's a third-party call
  and needs a PLAN decision for real apps (mirror/cache?).
