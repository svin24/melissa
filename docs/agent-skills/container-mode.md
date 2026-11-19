# Container mode: the headless image under podman (systemd as PID 1)

Status 2026-11-19: **verified rootless** on the dev laptop (Kinoite 45 on the Melissa desktop image, podman
6.1.3, SELinux enforcing, cgroup v2, host k3s running alongside), image 2b3845e. Everything below works once the
host delegates `cpuset` (host prerequisite, below). **Rootful verified** the same day on the same laptop (bridge
network, UI forward with an nftables filter; see "Rootful" below).

| Feature | Rootless | Rootful (`sudo`) | Notes |
|---|---|---|---|
| Queen + web UI on host 127.0.0.1 | works | works | rootless: pasta `--host-lo-to-ns-lo`; rootful: `melissa-ui-forward` :8081 → 127.0.0.1:8080, nft-filtered |
| A pod can't reach the UI | works | works | `smoke` tries node:8080, node:8081 and (rootful) gateway:UI_PORT from CoreDNS's netns |
| Agent sandbox (bwrap) + egress proxy | works | not tried | spike: `crates/agent/tests/egress.rs` 2 passed (rootless) |
| `melissa-setup agent opencode --yes --no-login` | works | not tried | |
| Chat turn (`melissa-queen ask`, opencode-acp, free model) | works | not tried | Queen called `list_apps`, answered with the link |
| k3s node Ready, CoreDNS, local-path, metrics-server | works | works | ~30 s after `up` on a warm volume, ~2 min fresh |
| flannel vxlan | works | works | own netns: no clash with the host k3s's `flannel.1`/`cni0` |
| containerd overlayfs snapshotter on the `/var` volume (btrfs) | works | works | no fuse-overlayfs |
| Traefik (+ `checkNewVersion=false`), ingress on :80 inside | works | works | not published to the host (apps use NodePorts) |
| Our ValidatingAdmissionPolicies + RBAC | works | works | `smoke` checks a deny and `can-i` |
| `install_app nextcloud` via `melissa-queen tool` | works | works | 513 MB pull ≈ 2 min; ready |
| App NodePort from the host (127.0.0.1) | works | works | rootless: pasta forward; rootful: netavark DNAT to eth0, kube-proxy NodePort |
| `stop_app` / `start_app` | works | works | port refuses while stopped, back with data after start |
| App links / Nextcloud trusted domains | works | works | `127.0.0.1` from `[apps] addresses` written by `up` |
| `podman restart` / `rm` + `up` (recreate) | works | restart works | rootful: forward comes back at boot (flag on the volume) |
| `BIND=0.0.0.0` (LAN) | not tried | not tried | rootful `up` writes the host's own address (route source); rootless: auto-detected |
| Interactive `melissa-setup login claude/grok` | not tried | not tried | needs the owner's login (`podman exec -it`) |

Files: `image/container/Containerfile` (thin layer on `localhost/melissa-headless:dev`),
`image/container/rootfs/` (k3s drop-in, tmpfiles, rootful UI forward units + filter), `scripts/container-run.sh`
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

## Rootful (`sudo`)
Rootful podman refuses pasta ("pasta networking is only supported for rootless mode"), so `up` run as root
(EUID 0) uses the default netavark bridge (`podman`, 10.88.0.0/16). Its `-p` forwards are nftables DNAT on the
host and land on the container's `eth0`, never on its `lo`, so the loopback-only queen is unreachable directly.
The narrowest fix kept in the container layer:
- `melissa-ui-forward.socket` listens on `0.0.0.0:8081`; `melissa-ui-forward.service` is
  `systemd-socket-proxyd 127.0.0.1:8080` (DynamicUser, no capabilities). The queen still binds only loopback and
  sees 127.0.0.1. `up` maps `-p 127.0.0.1:UI_PORT:8081`.
- Before listening, `/usr/libexec/melissa-container/ui-forward-filter` loads `table inet melissa_ui_forward`:
  TCP to 8081 is accepted only on the default-route interface (`eth0`) from the default gateway (the host's
  bridge address, 10.88.0.1; netavark masquerades host-loopback forwards to it); everything else to 8081 is
  dropped. Pods arrive on `cni0` (dropped even when kube-proxy DNATs them), other bridge containers have their
  own source address (dropped). A drop in any nftables table is final, so k3s's and kube-proxy's tables can't
  undo it. Host processes connecting to `10.88.0.x:8081` directly get in: the same trust as host loopback.
- Gate: the socket has `ConditionPathExists=/var/lib/melissa-container/ui-forward`. Rootful `up` writes that flag
  (on the volume, so restarts keep it) and starts the socket; rootless never writes it, so the socket is a
  skipped condition there (not failed) and rootless behaviour is unchanged. (`ConditionEnvironment=` with a
  `podman -e` variable does not work: PID 1's manager environment doesn't see it.)
- Verified 2026-11-19: `smoke` all PASS (incl. pod isolation); host → `10.88.0.x:8081` 200; another bridge
  container → 000; negative control: with the nft table deleted a pod reaches :8081 (200), with it back 000.

Effective `podman run` (rootful):
```
podman run -d --name melissa --hostname melissa --privileged --systemd=always \
  --network=bridge -p 127.0.0.1:18080:8081 -p 127.0.0.1:30000-30079:30000-30079 \
  -v melissa-var:/var --stop-timeout 60 localhost/melissa-container:dev /sbin/init
```
Root has its own image storage: copy the image over (or build as root):
```
podman save -o /tmp/melissa-container.tar localhost/melissa-container:dev
sudo podman load -i /tmp/melissa-container.tar
sudo env NAME=melissa-rootful UI_PORT=18095 APP_PORT_BASE=31000 scripts/container-run.sh preflight
sudo env NAME=melissa-rootful UI_PORT=18095 APP_PORT_BASE=31000 scripts/container-run.sh up
sudo env NAME=melissa-rootful UI_PORT=18095 APP_PORT_BASE=31000 scripts/container-run.sh smoke
sudo podman exec melissa-rootful melissa-queen tool --wait install_app '{"app":"nextcloud"}' &   # then:
sudo podman exec melissa-rootful melissa-queen confirm approve 1
curl http://127.0.0.1:31000/status.php
sudo env NAME=melissa-rootful scripts/container-run.sh purge; sudo podman rmi localhost/melissa-container:dev
```
`preflight` rootful: no cpuset delegation check (root has every controller) and no pasta; instead the bridge
network must run a container and its subnet must not overlap k3s's 10.42.0.0/16 / 10.43.0.0/16 inside.

Next to a host k3s (dev laptop, both running): no conflict found. The container's k3s has its own netns, so its
`cni0`, `flannel.1`, VXLAN port 8472, 6443/10250 and kube-proxy rules don't touch the host's. On the host,
netavark adds `table inet netavark` (DNAT for 127.0.0.1:UI_PORT and the app range, masquerade for 10.88.0.0/16,
forward accepts) and sets `route_localnet=1` on `podman0` (its usual localhost-forward setup); firewalld, the host
k3s's iptables-nft tables and its node stayed as they were (node Ready throughout). netavark leaves its (empty)
table behind after the last container goes. **Rootful caveats:** `--privileged` is real host root; the kubelet
inside writes host-wide sysctls `vm.overcommit_memory=1`, `kernel.panic=10`, `kernel.panic_on_oops=1` (rootless
it can't). They were already those values here because of the host k3s; on another host they change.

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
  UI is unreachable; an unfiltered socket proxy on a public address would expose the unauthenticated UI to every
  pod (rootful uses one, but behind the nftables filter: see "Rootful").
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
Rootful (`sudo scripts/container-run.sh ...`, image copied into root's storage) has every controller (no drop-in).
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
