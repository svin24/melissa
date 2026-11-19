# The Melissa container: image, install, updates, tests

**When:** changing `packaging/container/` or `packaging/common/`, the update flow, or testing Melissa as it ships.
User docs: `packaging/container/README.md` (read it first; this file is the developer side). k3s details:
[k3s-in-container.md](k3s-in-container.md).

## Layout
- `packaging/common/` — files every packaging installs, paths = install paths (`packaging/common/README.md`). The
  Nix packaging (`packaging/nix/`) uses them too: **don't rename or move them**; put container-only changes in
  `packaging/container/rootfs/` as overrides (same name) or drop-ins.
- `packaging/container/Containerfile` — 3 stages: Rust build (+ cargo-about report), k3s fetch (sha256-checked from
  `k3s.lock`), runtime `FROM registry.fedoraproject.org/fedora:45` + `packages.txt` + common + `rootfs/`.
- `packaging/container/rootfs/` — k3s `20-container.yaml`, tmpfiles (`/etc/melissa` and the node password on the
  volume, the update request dir), the UI forward socket/service/filter, the lead agent drop-in
  (`ReadWritePaths=-/run/melissa-update-request`).
- `melissa.container` (Quadlet template), `install.sh`, `uninstall.sh`, `host/` (update helper + path unit; installed
  on the host, never in the image).
- `scripts/container-run.sh` — dev/test driver: `build`, `up`/`down`/`rm`/`purge` (a plain `podman run`, no Quadlet,
  no update helper), `shell`, `logs`, `preflight`, `smoke`.

## Build and test loop (dev laptop)
```
just container-build                     # rootless: localhost/melissa:dev (--network=host: pasta is SELinux-denied here)
just container-install                   # sudo: save → load into root's storage as localhost/melissa:latest → install.sh
just container-smoke                     # sudo: uses /var/lib/melissa-container/install.env (UI_PORT, BIND, ...)
sudo podman exec -it melissa bash -l     # inside: kubectl, melissa-server, melissa-setup, journalctl
just container-uninstall [--purge]
```
Re-running `just container-install` replaces the image and keeps the volume. Settings for install.sh go through the
environment (`UI_PORT=8090 BIND=0.0.0.0 just container-install`).

## Image lint (last steps of the Containerfile)
Must exist: the three Melissa binaries, k3s, bwrap, nft, iptables, systemd-socket-proxyd, melissa-setup. Must not be
installed: kernel-core, bootc, openssh-server, NetworkManager, tailscale, chrony, firewalld. Enabled: lead agent, k3s,
UI forward socket. Masked: systemd-logind and the sleep targets. `systemd-analyze verify` on our units.

## Why each piece
- `--privileged` (PodmanArgs): k3s mounts, netfilter, pod netns; bwrap nested namespaces. Rootful = host root.
- `--systemd=always`: tmpfs `/run` and `/tmp`, writable cgroup, SIGRTMIN+3 to stop.
- **logind and sleep units masked, `Tmpfs=/dev/input`**: a logind inside a privileged container sees the host's lid
  switch and power keys and suspended the host (found 2026-11-19). The mask is the protection; the nodev tmpfs
  hides the input nodes as defense in depth (root inside could still mknod one: PLAN 61d).
- Network: podman's default bridge; the lead agent stays on 127.0.0.1:8080 inside. `PublishPort=UI_BIND:UI_PORT:8081` →
  `melissa-ui-forward.socket` (systemd-socket-proxyd, DynamicUser) → 127.0.0.1:8080. `ui-forward-filter` loads
  `table inet melissa_ui_forward`: TCP 8081 accepted only on the default-route interface from the default gateway
  (the host); pods (cni0) and other containers are dropped. A drop in any nftables table is final. Host processes
  that connect to the container's bridge address directly get in (same trust as host loopback).
- Apps: `PublishPort=BIND:APP_PORT_BASE-+79:30000-30079` (netavark DNAT). install.sh writes `[apps] addresses`
  (127.0.0.1 for a loopback BIND, BIND itself, or the host's route-source address for 0.0.0.0) and `port_offset`.
  Through firewalld: forwarded DNAT traffic is accepted by `ct status dnat accept` in `filter_FORWARD`; Tailscale's
  `ts-forward` accepts from tailscale0. No zone change needed for BIND = the Tailscale address.
- `/var` is the `melissa-var` volume. The container is **recreated at every start** (Quadlet), so `/etc` resets:
  `/etc/melissa` and `/etc/rancher/node` are symlinks onto `/var/lib/melissa-container/` (tmpfiles creates the dirs).
- `Environment=XDG_CONFIG_HOME=/etc`: `podman exec ... melissa-server tool <name>` (in-process, as root) reads
  `/etc/melissa/server.toml` like the service. Without it app links named the container's bridge address.
- NetworkManager/resolved/networkd are not installed or are masked: podman provides network and DNS.

## Updates (host helper)
- Lead agent side (`crates/settings/src/updates.rs`): `UpdateEnv::request(Check|Apply|Rollback)` creates an empty file
  with that name in `/run/melissa-update-request`; `Restart` is `systemctl restart --no-block melissa-server.service`
  through the polkit rule. `helper_installed()` = both mounts exist; otherwise the UI shows host commands.
  `container_image()` reads `/run/.containerenv` (privileged containers get image name and ID there).
- Host side (`host/melissa-host-update`, root): `requests` (path unit) acts on the names check/apply/rollback only,
  deletes everything in the directory, never reads contents. `status` (also `ExecStartPost=` of melissa.service)
  writes `status.json` {image, auto_update, running, staged, available, previous}; each image
  {id, digest, created, version, revision} from `podman image inspect`. `note_running` tags the previously running
  image as `localhost/melissa-rollback:previous` whenever the running ID changes (apply, rollback, auto-update).
- Test it: `curl -X POST -H 'Content-Type: application/json' -d '{"confirm":true}' http://127.0.0.1:8080/api/settings/updates/apply`,
  then `sudo cat /var/lib/melissa-container/status/last.json`; `journalctl -u melissa-update`. For a local image,
  load a new build (`podman save` | `sudo podman load`, tag `localhost/melissa:latest`), check, apply.

## Verified (2026-11-19, rootful, dev laptop: Fedora Kinoite 45, podman 6.1.3, SELinux enforcing)
| Feature | Result |
|---|---|
| install.sh: preflight, Quadlet, `systemctl status melissa`, sysctl notice, timer | works |
| Web UI on host 127.0.0.1:8080; not on other host addresses | works |
| Smoke: k3s, Traefik, policies, RBAC, NodePort, pod can't reach UI (node:8080/8081, gateway:8080) | all PASS |
| logind + sleep units masked, `/dev/input` unopenable (smoke) | PASS |
| `install_app nextcloud` via `melissa-server tool --wait` + confirm; NodePort from the host | works (`status.php` installed:true) |
| `stop_app` / `start_app` | works (port refuses while stopped; data back after start) |
| `melissa-setup agent opencode --yes --no-login`, `melissa-server ask` (sandbox + egress) | works (Lead agent called `list_apps`) |
| Settings → Restart (polkit, logind masked) | works |
| Updates: status (version, revision, digest), check, apply (local image), rollback, roll forward, host CLI | works |
| Persistence: reboot, update recreate, uninstall + reinstall (Nextcloud data, agent install, server.toml) | works |
| `uninstall.sh` (keeps volume; restores sysctls, disables the timer it enabled) and `--purge` | works |
| BIND = host Tailscale IPv4: app links use it, NodePort answers there | works from the host; other tailnet devices not tried by the agent |
| Rootless | unsupported (worked with the bootc-based image earlier the same day) |

## Gotchas
- The build's cargo `target` cache (`id=melissa-target`) is shared by every build and cargo trusts file
  times: a build from an older tree (e.g. `git archive` of another commit) once left artifacts newer than the
  real sources, and the next image shipped stale crates. The Containerfile touches `crates/` before
  `cargo build` (2026-11-19); podman's own layer cache ignores times, so change content to force a rebuild.
- Several test containers: give each its own `NAME`, `UI_PORT`, `APP_PORT_BASE` and `CTR_IMAGE` tag
  (e.g. `localhost/melissa-test:after`) so root's `localhost/melissa:*` stays untouched.
- Root has its own image storage: build rootless, then `podman save` + `sudo podman load` (`just container-install`).
- A `melissa-server tool <name>` call that needs a confirmation blocks until approved
  (`melissa-server confirm list|approve N`); right after a recreate k3s may be restarting ("k3s API unreachable").
- `ss -ltn` shows no listener for the published ports: rootful netavark uses nftables DNAT only.
- Each recreate changes the bridge address: one k3s restart (~1 min).
- The host's request directory carries the container's `melissa` GID (on the dev laptop that number is `kube`).
