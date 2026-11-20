# Melissa in a container (rootful podman, started by systemd)

Melissa runs as one **rootful** podman container on an existing Linux host: systemd as PID 1 inside
runs the server (web UI), k3s (apps) and the agent sandbox (bubblewrap + egress proxy). systemd on the
host starts it through a Quadlet, so `systemctl start|stop|status melissa` works and it comes up at boot.

Files: `Containerfile` (image), `packages.txt`, `rootfs/` (container-only files on top of
`packaging/common/`, e.g. the Settings → Providers Install/Sign-in units and their polkit rule
`51-melissa-agent-install.rules`), `melissa.container` (Quadlet template), `install.sh`, `uninstall.sh`,
`host/` (the update helper and its path unit, installed on the host).

## Requirements
- Linux with systemd, cgroup v2 (all of cpu, cpuset, io, memory, pids), podman 5 or newer, root.
- Kernel modules `br_netfilter` and `overlay` (install.sh loads them and adds
  `/etc/modules-load.d/melissa.conf`).
- Free host ports: the UI port (8080) and the app range (30000–30079).
- The host provides networking, DNS, time, SSH and (optionally) Tailscale. None of that is in the image.

## Install
From a source checkout (until a published image exists):
```
just container-build          # rootless build: localhost/melissa:dev
just container-install        # sudo: loads it into root's storage as localhost/melissa:latest, runs install.sh
just container-smoke          # sudo: k3s, Traefik, policies, RBAC, NodePort, pod-can't-reach-UI, UI
```
Or by hand:
```
podman save -o melissa.tar localhost/melissa:dev
sudo IMAGE_ARCHIVE=melissa.tar packaging/container/install.sh
sudo IMAGE=registry.example.com/melissa:latest packaging/container/install.sh   # a registry image
```
Settings (environment of install.sh): `UI_PORT` (8080), `UI_BIND` (127.0.0.1), `APP_PORT_BASE`
(30000), `BIND` (127.0.0.1; `0.0.0.0` makes apps reachable from the LAN), `APP_ADDRESSES`
(see "Network"), `ALLOW_ADDRESS_MISMATCH` (no), `AUTO_UPDATE` (`registry` | `local` | `off`),
`SET_SYSCTLS` (yes). `install.sh --preflight` only checks.
Running install.sh again updates the installation (the data stays).

Then set up an agent in the web UI: Settings → Providers → Install (OpenCode works with free models
and no account; Claude and Grok then need an API key or token, added on the same page). Or from the host:
```
sudo podman exec -it melissa melissa-setup agent opencode     # free models, no account
sudo podman exec -it melissa melissa-setup agent claude       # Claude subscription (logs in)
sudo podman exec -it melissa melissa-server chat
```

## What install.sh changes on the host
- `/etc/containers/systemd/melissa.container` (Quadlet → `melissa.service`), the volume `melissa-var`.
- `/etc/systemd/system/melissa-update.{path,service}`, `/usr/local/libexec/melissa/melissa-host-update`,
  `/usr/local/share/melissa/README.md`, `/var/lib/melissa-container/` (install settings, update status
  and requests).
- `/etc/modules-load.d/melissa.conf` (br_netfilter, overlay).
- **Host sysctls** in `/etc/sysctl.d/90-melissa-kubelet.conf`: `vm.overcommit_memory=1`,
  `vm.panic_on_oom=0`, `kernel.panic=10`, `kernel.panic_on_oops=1`, `kernel.keys.root_maxkeys=1000000`,
  `kernel.keys.root_maxbytes=25000000`. These are not namespaced: the kubelet inside the privileged
  container sets them on the host anyway (any k3s does), so install.sh sets them openly, prints a notice
  and keeps the old values; uninstall.sh restores them. `SET_SYSCTLS=no` leaves them to the kubelet.
- Unless `AUTO_UPDATE=off`: enables `podman-auto-update.timer` (if it was off; uninstall turns it off again).

## Uninstall
```
sudo packaging/container/uninstall.sh            # keeps the data volume and the images
sudo packaging/container/uninstall.sh --purge    # also deletes melissa-var (apps, chats, logins, config) and the images
```

## Network and the web UI
The container sits on podman's default bridge (`podman`, 10.88.0.0/16; must not overlap k3s's
10.42.0.0/16 and 10.43.0.0/16 inside). The server listens on `127.0.0.1:8080` **inside** only. Host
`UI_BIND:UI_PORT` is forwarded to the container's `:8081`, where `melissa-ui-forward.socket`
(`systemd-socket-proxyd`) hands connections to the server. An nftables table (`ui-forward-filter`) accepts
`:8081` only from the bridge gateway on the default-route interface, so **pods and other containers
can't reach the UI**; the smoke test checks that from inside a pod's network namespace.

Apps are reached by their NodePort: container 30000–30079 → host `BIND:APP_PORT_BASE…+79`. install.sh
writes `[apps] addresses` (and `port_offset` for another base) into the server's config so app links
name the right address. Traefik's :80/:443 inside are not published.

`APP_ADDRESSES` defaults from `BIND`: 127.0.0.1 for loopback, `BIND` itself for one address, the
default route's address for `0.0.0.0`. Every address must be one where `BIND` publishes the ports
(`0.0.0.0` covers all, loopback only loopback, one address only itself), or app links get "connection
refused": the preflight refuses such a mix (e.g. `BIND=127.0.0.1` with a Tailscale address). On a
reinstall, an existing server.toml whose `[apps] addresses` differ from an explicit `APP_ADDRESSES`, or
name addresses `BIND` doesn't cover, gets just that key rewritten (install.sh prints old and new).
`ALLOW_ADDRESS_MISMATCH=yes` skips both, for addresses something else forwards. Apps on Tailscale:
`BIND=<the host's Tailscale address>` (or `0.0.0.0`). At run time the app tools also try each address
(TCP) and leave dead ones out of the links they give agents (`reachability` in `app_status`).

### From another machine
The UI has **no login yet**: anyone who reaches it controls Melissa. Keep `UI_BIND=127.0.0.1` and use:
- SSH: `ssh -L 8080:127.0.0.1:8080 you@host`, then open http://127.0.0.1:8080 on your machine.
- Tailscale (on the host): `sudo tailscale serve --bg 8080` serves it on your tailnet over HTTPS.
  Only for a tailnet of people you trust (no Funnel).
- For apps on the LAN: `BIND=0.0.0.0` (the host firewall must allow 30000–30079).

## Data and persistence
The container is **recreated from the image at every start** (and on updates). Everything kept lives on
the `melissa-var` volume (`/var`): k3s and app data, the server's state, chats, agent installs and logins
(`/var/lib/melissa/home`). Two things from `/etc` are symlinked onto it: `/etc/melissa` (server.toml,
Settings' providers.toml) and `/etc/rancher/node` (k3s's node password).

## Updates (Settings → Updates)
- **Automatic**: Quadlet `AutoUpdate=registry` (registry images) or `local` (images loaded on the host),
  run daily by `podman-auto-update.timer`; podman rolls back by itself if the new container fails.
- **Check / Apply / Roll back** in Settings. The server can't run the host's podman. It creates an empty
  file named `check`, `apply` or `rollback` in `/run/melissa-update-request` (bind mount of
  `/var/lib/melissa-container/request`). `melissa-update.path` starts `melissa-host-update requests`, which
  acts on those three names only (never reads contents; anything else is deleted) and writes
  `status.json`/`last.json` to `/var/lib/melissa-container/status` (read-only inside at
  `/run/melissa-update`).
  - check: registry + auto-update → `podman auto-update --dry-run`; `AUTO_UPDATE=off` → pulls (no restart);
    local image → nothing to fetch (load a newer one with `podman load`, tag it as the image name).
  - apply: pull (registry), restart on the new image, wait for the server; if it doesn't come up, retag
    the old image and restart on it.
  - rollback: the image Melissa ran before is kept as `localhost/melissa-rollback:previous` (also after a
    timer update); rollback retags it and restarts. With automatic updates on, the next timer run may
    install the newer image again.
- Same from the host: `sudo /usr/local/libexec/melissa/melissa-host-update check|apply|rollback|status`.
- Version: `/usr/lib/melissa/release` in the image (`MELISSA_VERSION`, `MELISSA_REVISION`) and the
  image labels; Settings shows them with the image digest from the helper (or the image name and ID from
  `/run/.containerenv` without it).

## Security
- `--privileged` rootful: the container has **root on the host**. k3s needs mounts, netfilter and network
  namespaces; the agent sandbox needs nested namespaces. Inside, the server runs as the unprivileged
  `melissa` user and each agent in its own bubblewrap sandbox with an egress proxy.
- SELinux reads as disabled inside (k3s `selinux: false`); the host confines the container as podman does.
- The update channel carries three fixed words as file names; the host helper takes no arguments from
  the container.

## Rootless (unsupported)
Rootless podman needs cpuset delegation for the user and pasta networking (`--host-lo-to-ns-lo`); it
worked on 2026-11-19 with the earlier image but is no longer tested. `scripts/container-run.sh` still has
the rootless path for experiments.
