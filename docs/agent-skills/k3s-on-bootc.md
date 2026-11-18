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
- `melissa-kubeconfig.service` (root oneshot, `WantedBy=k3s.service`, enabled on headless) runs
  `/usr/libexec/melissa/melissa-kubeconfig`: waits ≤300 s for the token, writes `/run/credstore/melissa.kubeconfig`
  (0600, dir 0700; server `https://127.0.0.1:6443`, CA + token from the Secret).
- `melissa-queen.service`: `ImportCredential=melissa.kubeconfig` + `After=melissa-kubeconfig.service`. The DynamicUser
  queen reads its copy at `$CREDENTIALS_DIRECTORY/melissa.kubeconfig` (root 0440 + ACL for the dynamic user). No
  credential → queen starts anyway and `cluster_health` says k3s isn't running.
- Rotate/revoke: `sudo /usr/libexec/melissa/melissa-kubeconfig rotate` (recreates the ServiceAccount, rewrites the
  kubeconfig, restarts the queen).
- Run a tool as the queen would (same credential path):
  `sudo systemd-run --quiet --wait --pipe --collect -p DynamicUser=yes -p ImportCredential=melissa.kubeconfig
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

## Gotchas
- **Secret-based SA tokens are deterministic:** no `iat`/`exp` claims and RS256 signatures are deterministic, so a
  recreated Secret with the same name gets the *same* token (old one stays valid). Rotation must recreate the
  ServiceAccount (new UID). Verified 2026-11-18 on v1.37.2.
- **k3s restarts itself at boot** (seen on two boots, also before the queen work): it exits with status 1 right after
  the node's IPv6 address appears (`NodeIPs changed`), systemd restarts it (`NRestarts=2`). API clients must treat
  short outages as "unreachable", not failures. Root cause not investigated (IPv6 SLAAC on QEMU user networking?).
- **Traefik chart 41.x** (k3s v1.37): telemetry is `global.checkNewVersion` (default **true** → Traefik checks online
  for new versions) and `global.sendAnonymousUsage` (default false). The old `globalArguments` key is ignored. Melissa's
  HelmChartConfig sets both false; the smoke test asserts `--global.checkNewVersion=false` in Traefik's args.
- Anyone who can create HelmChart objects is effectively cluster-admin; only the queen should, from reviewed manifests.
- `bootc rollback` restores the old k3s binary but not `/var` (datastore). Rolling back across a Kubernetes minor needs
  a datastore snapshot taken before staging the update (back up `server/db` **and** `server/token`).
- App images (e.g. the smoke test's `traefik/whoami`) are pulled from public registries; that's a third-party call
  and needs a PLAN decision for real apps (mirror/cache?).
