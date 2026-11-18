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

## Gotchas
- **Traefik chart 41.x** (k3s v1.37): telemetry is `global.checkNewVersion` (default **true** → Traefik checks online
  for new versions) and `global.sendAnonymousUsage` (default false). The old `globalArguments` key is ignored. Melissa's
  HelmChartConfig sets both false; the smoke test asserts `--global.checkNewVersion=false` in Traefik's args.
- Anyone who can create HelmChart objects is effectively cluster-admin; only the queen should, from reviewed manifests.
- `bootc rollback` restores the old k3s binary but not `/var` (datastore). Rolling back across a Kubernetes minor needs
  a datastore snapshot taken before staging the update (back up `server/db` **and** `server/token`).
- App images (e.g. the smoke test's `traefik/whoami`) are pulled from public registries; that's a third-party call
  and needs a PLAN decision for real apps (mirror/cache?).
