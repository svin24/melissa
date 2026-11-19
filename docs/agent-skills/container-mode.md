# Container mode: the headless image under podman (systemd as PID 1)

Status 2026-11-19: **verified rootless** on the dev laptop (Kinoite 45 on the Melissa desktop image, podman
6.1.3, SELinux enforcing, cgroup v2, host k3s running alongside), image 2b3845e. Everything below works once the
host delegates `cpuset` (host prerequisite, below). Rootful is not verified.

| Feature | Rootless | Notes |
|---|---|---|
| Queen + web UI on host 127.0.0.1 | works | pasta `--host-lo-to-ns-lo`; queen stays loopback-only |
| Agent sandbox (bwrap) + egress proxy | works | spike: `crates/agent/tests/egress.rs` 2 passed |
| `melissa-setup agent opencode --yes --no-login` | works | |
| Chat turn (`melissa-queen ask`, opencode-acp, free model) | works | Queen called `list_apps`, answered with the link |
| k3s node Ready, CoreDNS, local-path, metrics-server | works | ~30 s after `up` on a warm volume |
| flannel vxlan in the userns | works | no host-gw fallback needed |
| containerd overlayfs snapshotter on the `/var` volume (btrfs) | works | kernel unprivileged overlay; no fuse-overlayfs |
| Traefik (+ `checkNewVersion=false`), ingress on :80 inside | works | not published to the host (apps use NodePorts) |
| Our ValidatingAdmissionPolicies + RBAC | works | `smoke` checks a deny and `can-i` |
| `install_app nextcloud` via `melissa-queen tool` | works | 513 MB pull ≈ 2 min; ready |
| App NodePort from the host (127.0.0.1) | works | pasta port forward; kube-proxy NodePort on the container's addresses |
| `stop_app` / `start_app` | works | port refuses while stopped, back with data after start |
| App links / Nextcloud trusted domains | works | `127.0.0.1` from `[apps] addresses` written by `up` |
| `rm` + `up` (recreate) | works | k3s, apps, agents and `/etc/melissa/queen.toml` survive |
| Rootful (`sudo`) | not tried | needs root; expected to work (all controllers) |
| `BIND=0.0.0.0` (LAN) | not tried | auto-detected address is the host's LAN IP (pasta copies it) |
| Interactive `melissa-setup login claude/grok` | not tried | needs the owner's login (`podman exec -it`) |

Files: `image/container/Containerfile` (thin layer on `localhost/melissa-headless:dev`),
`image/container/rootfs/` (k3s drop-in, tmpfiles), `scripts/container-run.sh`
(build/up/status/smoke/shell/logs/down/rm/purge/preflight).

## Run
```
scripts/container-run.sh preflight         # cgroup controllers, br_netfilter/overlay, pasta
scripts/container-run.sh build             # IMAGE=... to layer on another headless image
scripts/container-run.sh up                # UI: http://127.0.0.1:18080; writes [apps] addresses
scripts/container-run.sh smoke             # k3s, Traefik, policies, RBAC, NodePort 30079 from the host, UI
scripts/container-run.sh shell             # root shell: melissa-setup, melissa-queen chat, kubectl
```
Effective `podman run` (rootless):
```
podman run -d --name melissa --hostname melissa --privileged --systemd=always \
  --network=pasta:--host-lo-to-ns-lo -p 127.0.0.1:18080:8080 -p 127.0.0.1:30000-30079:30000-30079 \
  -v melissa-var:/var --stop-timeout 60 localhost/melissa-container:dev /sbin/init
```
On the dev laptop (host k3s owns 6443/10250 and maybe NodePorts; the queen on 8080; distro pasta denied by SELinux):
```
export PASTA_DIR=$(nix build --no-link --print-out-paths nixpkgs#passt)/bin NAME=melissa-ck APP_PORT_BASE=31000 UI_PORT=18090
scripts/container-run.sh preflight && scripts/container-run.sh build && scripts/container-run.sh up && scripts/container-run.sh smoke
podman exec melissa-ck melissa-queen tool --wait install_app '{"app":"nextcloud"}' &   # unattended: needs a confirmation
podman exec melissa-ck melissa-queen confirm approve 1
podman exec melissa-ck melissa-queen tool app_status '{"app":"nextcloud"}'           # until "phase": "ready"
curl http://127.0.0.1:31000/status.php
podman exec melissa-ck melissa-setup agent opencode --yes --no-login
podman exec melissa-ck melissa-queen ask "Which apps are installed, and at what link?"
scripts/container-run.sh purge
```

## Why each piece
- `--privileged`: k3s mounts, writes netfilter rules, creates pod netns; bwrap needs nested user+net namespaces.
  Rootless this is "all caps inside my user namespace" (no host root). Rootful it is host root: say so to testers.
- `--systemd=always`: podman mounts tmpfs on /run, /tmp, makes the cgroup writable, stops with SIGRTMIN+3. No
  need for `--tmpfs /run` or `/dev/kmsg` flags (rootless can't read `/dev/kmsg` anyway; the k3s drop-in sets
  `KubeletInUserNamespace=true` for that).
- Network: **podman 6 removed slirp4netns** ("use pasta"). `--network=host` collides with a host k3s and,
  rootless, k3s can't create interfaces in the host netns. So pasta. **`--host-lo-to-ns-lo`** delivers host
  loopback connections to the container's own 127.0.0.1, so the queen keeps listening on loopback only (its
  config refuses non-loopback: no login yet). Without it pasta forwards to the container's public address and the
  UI is unreachable; a socket proxy on a public address would expose the unauthenticated UI to every pod.
- `/var` is a named volume (podman copies the image's /var on first use). `/etc` comes from the image and resets on
  recreate, so two things are symlinked onto the volume (created by tmpfiles): k3s's node password
  (`/etc/rancher/node` → `/var/lib/melissa-container/rancher-node`; else k3s refuses to rejoin its own datastore)
  and the Queen's config (`/etc/melissa` → `/var/lib/melissa-container/etc-melissa`). systemd's
  `ConfigurationDirectory=melissa` accepts the symlink.
- App addresses: inside, pasta gives the container a copy of the host's LAN interface (same name and address,
  e.g. `wlp2s0 192.168.1.x`), so automatic detection would link apps to the host's LAN IP even though the ports
  are only published on 127.0.0.1. `up` therefore writes `/etc/melissa/queen.toml` with `[apps] addresses`
  (once; an existing file is left alone): `127.0.0.1` for a loopback `BIND`, `BIND` itself for one address,
  nothing for `0.0.0.0` (then the auto-detected LAN address is right). `APP_ADDRESSES=a,b` overrides. Apps
  installed before the address change keep their old trusted hosts (reinstall or set them by hand).
- App links name the published port: NodePort (30000+n, from the namespace annotation) plus `[apps]
  port_offset` (the app tools report it as `port`, the NodePort as `node_port`; the web UI uses the visitor's
  Host plus `port`). `up` writes `port_offset = APP_PORT_BASE - 30000` with the addresses at the first start; if
  the config already exists and lacks the matching offset, `up` prints a note instead (unit tests cover the URL
  building; not yet re-run in a live container).
- Masked in the layer: NetworkManager(-wait-online), systemd-resolved (+ its sockets) — they hide pasta's DNS
  (169.254.1.1) from nss-resolve, giving "Could not resolve host"; tailscaled, sshd, chronyd, bootloader-update,
  bootc-fetch-apply-updates.timer, melissa-firstboot (needs /dev/tty1); kernel-fs mounts a container can't make.
- k3s drop-in `20-container.yaml`: `selinux: false` (SELinux reads as disabled inside; the outer container is
  the confinement), kubelet `KubeletInUserNamespace=true`, kube-proxy `conntrack-max-per-core=0` (kind does too).

## Host prerequisite (rootless k3s): cpuset delegation
Without it: `k3s ... level=fatal msg="Error: failed to find cpuset cgroup (v2)"`. Fedora delegates only
`cpu io memory pids` to `user@UID.service`. Faking `cgroup.controllers` with a bind mount fails (kubelet opens it
with `openat2(RESOLVE_NO_XDEV)`: "invalid cross-device link"). `preflight` checks the controllers and prints the
fix (root, once; on the dev laptop it applied live, no reboot or relogin):
```
sudo mkdir -p /etc/systemd/system/user@.service.d
printf '[Service]\nDelegate=cpu cpuset io memory pids\n' | sudo tee /etc/systemd/system/user@.service.d/delegate.conf
sudo systemctl daemon-reload
cat /sys/fs/cgroup/user.slice/user-$(id -u).slice/user@$(id -u).service/cgroup.controllers   # needs cpuset
```
If cpuset still is missing, log out and in (restarting user@UID kills user services such as t3code.service).
Rootful (`sudo scripts/container-run.sh ...`, image copied into root's storage) has every controller.
The risks named by the spike did not bite: flannel vxlan, overlayfs on the volume and NodePorts all work
(kube-proxy sets `route_localnet=1` in the container's netns; host loopback reaches NodePorts via pasta).
Harmless k3s errors in a userns: "Failed to set rlimit on max file handles", "oom_score_adj: permission denied".

**Desktop image:** recommended to ship the same drop-in as `/usr/lib/systemd/system/user@.service.d/` (it only
lets a user's own containers use cpuset, as systemd already allows cpu/io/memory/pids). Not added: owner review
(PLAN question 59).

## Verified earlier (spike, rootless, 2026-11-19, image 2b3845e)
- UI: `curl http://127.0.0.1:18080/` → 200 from the host; queen still on 127.0.0.1:8080 inside.
- Sandbox: `crates/agent/tests/egress.rs` built in `localhost/melissa-build:dev`, copied in, run as `melissa`:
  2 passed, no SKIP. bwrap `--unshare-all` sees only `lo`.
- `melissa-setup agent opencode --yes --no-login` installs; `melissa-queen ask` → opencode-acp in the sandbox
  through the egress proxy answers.
- `down`/`up` and `rm`/`up` keep agent installs and state (uid of `melissa` stays 974).
- k3s, apps and a chat turn: see the table at the top.

## Lost vs the VM/bare metal (by design)
Secure Boot, `bootc upgrade`/rollback (replace the container: `rm` + `build` + `up`, /var stays), firstboot account
prompt (use `podman exec`), SSH, Tailscale, NTP, the host's own firewall zones. Traefik's :80/:443 (DNS-name app
URLs) are not published to the host; apps are reached by their NodePort.
