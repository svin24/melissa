# Container mode: the headless image under podman (systemd as PID 1)

Status 2026-11-19: **spike, partly verified.** Rootless on the dev laptop (Kinoite 45 on the Melissa desktop
image, podman 6.1.3, SELinux enforcing, cgroup v2): queen + web UI, agent sandbox + egress, `melissa-setup`,
persistence all work. **k3s did not start** (missing `cpuset` delegation, below), so Traefik, admission policies,
app installs and NodePorts are **unverified** in a container.

Files: `image/container/Containerfile` (thin layer on `localhost/melissa-headless:dev`),
`image/container/rootfs/` (k3s drop-in, tmpfiles), `scripts/container-run.sh` (build/up/status/shell/down/rm/purge/preflight).

## Run
```
scripts/container-run.sh preflight         # cgroup controllers, br_netfilter/overlay, pasta
scripts/container-run.sh build             # IMAGE=... to layer on another headless image
scripts/container-run.sh up                # UI: http://127.0.0.1:18080
scripts/container-run.sh shell             # root shell: melissa-setup, melissa-queen chat, kubectl
```
Effective `podman run` (rootless):
```
podman run -d --name melissa --hostname melissa --privileged --systemd=always \
  --network=pasta:--host-lo-to-ns-lo -p 127.0.0.1:18080:8080 -p 127.0.0.1:30000-30079:30000-30079 \
  -v melissa-var:/var --stop-timeout 60 localhost/melissa-container:dev /sbin/init
```
On the dev laptop (host k3s owns 6443/10250 and maybe NodePorts; pasta broken):
`PASTA_DIR=$(nix build --no-link --print-out-paths nixpkgs#passt)/bin NAME=melissa-spike APP_PORT_BASE=31000`.

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
  recreate; k3s's node password is moved to `/var/lib/melissa-container/rancher-node` (symlink) or k3s refuses
  to rejoin its own datastore. No Melissa code writes /etc today; a local `/etc/melissa/queen.toml` would be lost.
- Masked in the layer: NetworkManager(-wait-online), systemd-resolved (+ its sockets) — they hide pasta's DNS
  (169.254.1.1) from nss-resolve, giving "Could not resolve host"; tailscaled, sshd, chronyd, bootloader-update,
  bootc-fetch-apply-updates.timer, melissa-firstboot (needs /dev/tty1); kernel-fs mounts a container can't make.
- k3s drop-in `20-container.yaml`: `selinux: false` (SELinux reads as disabled inside; the outer container is
  the confinement), kubelet `KubeletInUserNamespace=true`, kube-proxy `conntrack-max-per-core=0` (kind does too).

## k3s blocker (rootless): cpuset
`k3s ... level=fatal msg="Error: failed to find cpuset cgroup (v2)"`. Fedora delegates only `cpu io memory pids`
to `user@UID.service`. Faking `cgroup.controllers` with a bind mount fails (kubelet opens it with
`openat2(RESOLVE_NO_XDEV)`: "invalid cross-device link"). Fix (root, once, no reboot expected):
```
sudo mkdir -p /etc/systemd/system/user@.service.d
printf '[Service]\nDelegate=cpu cpuset io memory pids\n' | sudo tee /etc/systemd/system/user@.service.d/delegate.conf
sudo systemctl daemon-reload
cat /sys/fs/cgroup/user.slice/user-$(id -u).slice/user@$(id -u).service/cgroup.controllers   # needs cpuset
```
If cpuset still is missing, log out and in (restarting user@UID kills user services such as t3code.service).
Rootful (`sudo scripts/container-run.sh ...`, image copied into root's storage) has every controller.
Remaining rootless risks once cpuset is there: flannel vxlan in a userns, overlayfs snapshotter on the volume,
netfilter module autoload (load `br_netfilter`/`overlay` on the host first), localhost NodePorts (kube-proxy needs
`route_localnet`, settable in the container's own netns).

## Verified (rootless, 2026-11-19, image 2b3845e)
- UI: `curl http://127.0.0.1:18080/` → 200 from the host; queen still on 127.0.0.1:8080 inside.
- Sandbox: `crates/agent/tests/egress.rs` built in `localhost/melissa-build:dev`, copied in, run as `melissa`:
  2 passed, no SKIP. bwrap `--unshare-all` sees only `lo`.
- `melissa-setup agent opencode --yes --no-login` installs; `melissa-queen ask` → opencode-acp in the sandbox
  through the egress proxy answers.
- `down`/`up` and `rm`/`up` keep agent installs and state (uid of `melissa` stays 974).
- Not tried: interactive `melissa-setup login claude|grok` (use `podman exec -it`), rootful, k3s, apps.

## Lost vs the VM/bare metal (by design)
Secure Boot, `bootc upgrade`/rollback (replace the container: `rm` + `build` + `up`, /var stays), firstboot account
prompt (use `podman exec`), SSH, Tailscale, NTP, the host's own firewall zones. App links show the container's
hostname/address; from the host use `http://127.0.0.1:<APP_PORT_BASE + n>`.
