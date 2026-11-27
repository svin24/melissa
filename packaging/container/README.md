# Melissa in a container (rootful podman, started by systemd)

Melissa runs as one **rootful** podman container on an existing Linux host: systemd as PID 1 inside
runs the server (web UI), k3s (apps) and the agent sandbox (bubblewrap + egress proxy). systemd on the
host starts it through a Quadlet, so `systemctl start|stop|status melissa` works and it comes up at boot.

Files: `Containerfile` (image), `packages.txt`, `rootfs/` (container-only files on top of
`packaging/common/`, e.g. the Settings → Providers Install/Sign-in units and their polkit rule
`51-melissa-agent-install.rules`), `melissa.container` (Quadlet template), `install.sh`, `uninstall.sh`,
`host/` (the update helper and its path unit, installed on the host), `trust.sh` and `melissa-signing.pub`
(the signature check for the published image; see "Updates"), `escape-test.sh` and
`Containerfile.escape-test` (the sandbox escape suite inside the image; see "Release gate").

## Requirements
- Linux with systemd, cgroup v2 (all of cpu, cpuset, io, memory, pids), podman 5 or newer, root.
- Kernel modules `br_netfilter` and `overlay` (install.sh loads them and adds
  `/etc/modules-load.d/melissa.conf`).
- Free host ports: the UI port (8080) and the app range (30000–30079).
- The host provides networking, DNS, time and SSH. None of that is in the image. Tailscale is the host's
  when it runs it; otherwise the image carries its own, off until the owner enables it (Settings → Tailscale).
- Ubuntu: no AppArmor profile that restricts `bwrap` (see "Ubuntu and AppArmor"; the preflight checks it).

## Install
Without a checkout (see [get.sh](../../get.sh); `curl -fsSLO` it first to read it):
```
curl -fsSL https://raw.githubusercontent.com/svin24/melissa/master/get.sh | sudo sh
curl -fsSL https://raw.githubusercontent.com/svin24/melissa/master/get.sh | sudo CHANNEL=stable sh       # settings between sudo and sh
curl -fsSL https://raw.githubusercontent.com/svin24/melissa/master/get.sh | sudo sh -s -- --preflight    # only check the host
```
get.sh checks for root, Linux, systemd, podman 5+, curl and tar, downloads the source tarball from
codeload.github.com (`master`, or with `CHANNEL=stable` the tag that github.com/svin24/melissa/releases/latest
redirects to; `MELISSA_REF` overrides both) into a `mktemp -d` directory, extracts `packaging/container/`
and runs install.sh from there with the same environment. It stops when there is no stable release yet.

From a source checkout, the published image (no local build):
```
sudo packaging/container/install.sh          # or: just install
just container-smoke                         # sudo: k3s, Traefik, policies, RBAC, NodePort, pod-can't-reach-UI, UI
```
It pulls `ghcr.io/svin24/melissa:edge` (`CHANNEL=edge`, the default): every master commit that passed the
release gate. `CHANNEL=stable` follows tagged releases (`vX.Y.Z`) once there are any. Before the first pull,
install.sh makes podman require Melissa's signature on that repository (see "Updates"). `VERIFY=no` skips
that; then only TLS to the registry protects the download.

Check a signature by hand with [cosign](https://github.com/sigstore/cosign):
```
cosign verify --key packaging/container/melissa-signing.pub ghcr.io/svin24/melissa:edge
```

A local build instead:
```
just container-build          # rootless build: localhost/melissa:dev
just container-install        # sudo: loads it into root's storage as localhost/melissa:latest, runs install.sh
```
Or by hand:
```
podman save -o melissa.tar localhost/melissa:dev
sudo IMAGE=localhost/melissa:latest IMAGE_ARCHIVE=melissa.tar packaging/container/install.sh
```
A reinstall keeps the image recorded in `/var/lib/melissa-container/install.env` unless `IMAGE` is set.

Settings (environment of install.sh): `CHANNEL` (edge), `IMAGE` (`ghcr.io/svin24/melissa:$CHANNEL`),
`VERIFY` (yes), `UI_PORT` (8080), `UI_BIND` (127.0.0.1), `APP_PORT_BASE`
(30000), `BIND` (0.0.0.0: app ports on every address; Melissa's filter lets only this device in until you
change Settings → Apps, see "Who may open apps"), `APP_ADDRESSES`
(see "Network"), `ALLOW_ADDRESS_MISMATCH` (no), `AUTO_UPDATE` (`registry` | `local` | `off`),
`SET_SYSCTLS` (yes), `DOCKER_FORWARD` (auto; see "Docker on the same host"), `NET_CHECK` (yes).
`install.sh --preflight` only checks.
Running install.sh again updates the installation (the data stays).

### Migrating a local-build install
```
sudo IMAGE=ghcr.io/svin24/melissa:edge packaging/container/install.sh
```
switches an installation from `localhost/melissa:latest` to the published image. It keeps the `melissa-var`
volume (apps, chats, logins, config) and sets automatic updates to `registry` (add `AUTO_UPDATE=off` to
keep them off).

Then open the web UI and claim it (see "Signing in"): the first-run setup installs a provider and signs
it in on the page (OpenCode works with free models and no account). Later ones: Settings → Providers.
Or from the host:
```
sudo podman exec -it melissa melissa-setup agent opencode     # free models, no account
sudo podman exec -it melissa melissa-setup agent claude       # Claude subscription (logs in)
sudo podman exec -it melissa melissa-server chat
```

## What install.sh changes on the host
- `/etc/containers/systemd/melissa.container` (Quadlet → `melissa.service`), the volume `melissa-var`.
- `/etc/systemd/system/melissa-update.{path,service}`, `/usr/local/libexec/melissa/melissa-host-update`,
  `/usr/local/libexec/melissa/{uninstall,trust,apparmor}.sh` (uninstall without the checkout),
  `/usr/local/share/melissa/README.md`, `/var/lib/melissa-container/` (install settings, update status
  and requests).
- `/etc/modules-load.d/melissa.conf` (br_netfilter, overlay).
- Published image only (unless `VERIFY=no`): one entry for `ghcr.io/svin24/melissa` in
  `/etc/containers/policy.json` (created from `/usr/share/containers/policy.json` if missing; the rest of
  the file is kept), the key at `/etc/pki/containers/melissa.pub` and
  `/etc/containers/registries.d/melissa.yaml` (read the signatures stored next to the image). uninstall
  removes all three, and deletes `policy.json` again if install.sh created it and it is back to the default.
- **Host sysctls** in `/etc/sysctl.d/90-melissa-kubelet.conf`: `vm.overcommit_memory=1`,
  `vm.panic_on_oom=0`, `kernel.panic=10`, `kernel.panic_on_oops=1`, `kernel.keys.root_maxkeys=1000000`,
  `kernel.keys.root_maxbytes=25000000`. These are not namespaced: the kubelet inside the privileged
  container sets them on the host anyway (any k3s does), so install.sh sets them openly, prints a notice
  and keeps the old values; uninstall.sh restores them. `SET_SYSCTLS=no` leaves them to the kubelet.
- Unless `AUTO_UPDATE=off`: enables `podman-auto-update.timer` (if it was off; uninstall turns it off again).
- Only when Docker is on the host: `/etc/systemd/system/melissa-docker-forward.service` and two rules in
  Docker's `DOCKER-USER` chain (see "Docker on the same host"; uninstall removes both).
- Only when the host has AppArmor (Ubuntu): Melissa's profile `/etc/apparmor.d/melissa-container`, loaded,
  for the container alone (see "Ubuntu and AppArmor"; uninstall unloads and removes it).

## Uninstall
```
sudo packaging/container/uninstall.sh            # keeps the data volume and the images
sudo packaging/container/uninstall.sh --purge    # also deletes melissa-var (apps, chats, logins, config) and the images
```
Without a checkout (get.sh installs): `sudo /usr/local/libexec/melissa/uninstall.sh [--purge]`, the copy
install.sh keeps there.

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

`APP_ADDRESSES` defaults from `BIND`: 127.0.0.1 for loopback, `BIND` itself for one address; for
`0.0.0.0` the default route's address, the host's Tailscale addresses and 127.0.0.1 (links then offer
those that Settings → Apps lets in). Every address must be one where `BIND` publishes the ports
(`0.0.0.0` covers all, loopback only loopback, one address only itself), or app links get "connection
refused": the preflight refuses such a mix (e.g. `BIND=127.0.0.1` with a Tailscale address). On a
reinstall, an existing server.toml whose `[apps] addresses` differ from an explicit `APP_ADDRESSES`, or
name addresses `BIND` doesn't cover, or are just `127.0.0.1` (the old loopback default) with a wider
`BIND`, gets just that key rewritten (install.sh prints old and new). `ALLOW_ADDRESS_MISMATCH=yes` skips
the coverage check, for addresses something else forwards. The server is told `BIND` too
(`MELISSA_PUBLISH_BIND` in melissa.container, passed to melissa-server.service) and leaves addresses it
doesn't cover out of the links it gives agents (`reachability` in `app_status`). It can't test them by
connecting: from inside the container a connect to the host's published address fails (no hairpin
through podman's DNAT) even when it works from the host and other machines. An install from before
this has no `MELISSA_PUBLISH_BIND`; rerun install.sh to add it (until then failed probes are only noted).

### Who may open apps (Settings → Apps)
App ports are published on every host address by default; Melissa decides who gets through, with the
setting **Apps open to**: *This device* (default: the same as the old loopback-only install), *Tailnet*
(also Tailscale's 100.64.0.0/10 and fd7a:115c:a1e0::/48) or *Home network* (also the private ranges:
10/8, 172.16/12, 192.168/16, 169.254/16, IPv6 ULA and link-local). Each includes the ones before it.
The owner changes it on the Settings page; an agent can propose it with `set_app_access` when an app
link is left out (`reachability.access` in `app_status`), and the owner approves every widening in the
chat. It applies at once: no restart, no sudo.

How: `melissa-app-access.service` (root, inside the container) loads the nftables table
`melissa_app_access`. It matches traffic to 30000–30079 (TCP and UDP) on its source address in
`prerouting`, before k3s's NodePort DNAT, except from the container itself (`lo`) and pods (`cni0`).
"This device" is the bridge gateway: rootful netavark masquerades connections to the host's
`127.0.0.1:<port>` to it. Everything else is dropped (a drop is final whatever kube-proxy's tables say).
The unit reads the table back to verify it and writes `/run/melissa-app-access/status.json`, which the
page and the app links read. **Fail closed**: if the wanted table can't be loaded or verified it falls
back to "this device", then to letting nothing in; k3s starts only once the table exists; links name
only addresses the filter in force lets in, and the page says what went wrong. Check it with
`sudo podman exec melissa nft list table inet melissa_app_access` and
`sudo podman exec melissa melissa-server app-access status`.

Limits: the container can't see the host's interfaces, so it matches by source range: a host directly on
a carrier-grade NAT network (100.64.0.0/10 outside Tailscale) or on an untrusted private network (hotel
Wi-Fi) lets those peers in under Tailnet or Home network. podman's default network has no IPv6, so IPv6
clients can't reach apps whatever the setting. A host firewall may still apply on top (firewalld lets
podman's published ports through by default, see "firewalld"; others may need 30000–30079 opened). For a restriction at the host as well, set
`BIND` (e.g. `BIND=127.0.0.1`); the server then leaves out addresses `BIND` doesn't cover, and
`set_app_access` can't widen past it.

### Tailscale (Settings → Tailscale)
install.sh checks whether the host runs Tailscale (`tailscale0` exists or `tailscaled.service` is active;
`HOST_TAILSCALE=yes|no` overrides it) and tells the container (`MELISSA_HOST_TAILSCALE` in melissa.container).
- **The host runs it:** Melissa uses the host's as before; Settings → Tailscale only says "Tailscale is not
  managed by Melissa on this system", and Melissa never starts its own.
- **It doesn't:** the image carries the official static Tailscale binaries (pinned in `tailscale.lock`), **off
  by default**. Settings → Tailscale → **Enable** starts `melissa-tailscaled.service` inside the container
  (kernel mode through `/dev/net/tun`, interface `tailscale0`, node name `melissa-<host>` or `TAILSCALE_NAME`,
  state in the `melissa-var` volume under `/var/lib/melissa-tailscale`). **Sign in** opens Tailscale's sign-in
  page in a new tab: whoever opens that link can add this device to their tailnet, so it goes only to the
  browser that asked (never to an agent, a log or the chat). **Disable** asks once, logs the node out and stops
  it. An agent can propose turning it on (`enable_tailscale`); you approve it in the chat.

Apps over Melissa's own Tailscale: tailnet devices reach the app ports on the node's tailnet address or
MagicDNS name (at the NodePort, 30000–30079, whatever `APP_PORT_BASE` is) once Settings → Apps is *Tailnet*
or wider; under *This device* nothing from `tailscale0` gets in. Everything else tailnet devices send to the
container (the web UI, k3s's API, the kubelet) is dropped (`melissa_app_access`'s `input` chain), at every
setting. Melissa's own Tailscale doesn't serve the web UI on the tailnet yet (use the host's `tailscale serve`,
below).

While it is enabled, tailscaled talks to Tailscale's coordination server and DERP relays; no logs are sent to
Tailscale (`--no-logs-no-support`). Check it with `sudo podman exec melissa tailscale status`.
`uninstall.sh --purge` logs the node out before deleting its state. Changing `HOST_TAILSCALE` later needs
install.sh again.

### Signing in
The first person to open the UI claims it: Melissa asks for a one-time **setup code**, printed at the end
of `install.sh`, in its log when it starts (`sudo podman logs melissa`) and by `sudo podman exec melissa
melissa-server setup-code` (valid 1 hour; kept across restarts until it is used or expires). Then a short
setup in the web UI asks for a device name, a provider and a model. That person becomes the owner and invites others from Settings →
People (single-use links, 24 hours). Lost the owner password: `sudo podman exec melissa melissa-server
reset-owner` prints a one-hour link that sets a new one. The CLI (`melissa-server chat`, `ask`, `confirm`)
needs no password: it uses a local socket only root and Melissa's user can open, and acts as the owner.

### From another machine
Sign-in runs over plain HTTP, which sends passwords readable on the network. Keep `UI_BIND=127.0.0.1` and use:
- SSH: `ssh -L 8080:127.0.0.1:8080 you@host`, then open http://127.0.0.1:8080 on your machine.
- Tailscale on the host: `sudo tailscale serve --bg 8080` serves it on your tailnet over HTTPS. (Melissa's
  own Tailscale doesn't serve the UI yet.)
  Only for a tailnet of people you trust (no Funnel).
- For apps on the tailnet or the LAN: Settings → Apps (a host firewall other than firewalld must allow
  30000–30079; see "firewalld").

### Docker on the same host
Docker sets the iptables `FORWARD` policy to DROP and accepts only its own bridges. An nftables accept
(netavark's, for podman's network) doesn't stop the next hook, so Docker's chain still drops the
container's traffic: no DNS, no downloads, no app ports from the LAN.

install.sh sees this (Docker's `DOCKER-USER` chain with a DROP policy, or `docker.service`/`docker.socket`
running or enabled) and installs `melissa-docker-forward.service`. It inserts two rules for podman's
subnet (read from `podman network inspect podman`) into `DOCKER-USER`, Docker's chain for your own rules,
and runs again whenever Docker starts. Without Docker it does nothing. `DOCKER_FORWARD=no` only prints
the commands. By hand (10.88.0.0/16 is podman's default; rules made like this are gone after a reboot):
```
sudo iptables -I DOCKER-USER -s 10.88.0.0/16 -j ACCEPT
sudo iptables -I DOCKER-USER -d 10.88.0.0/16 -j ACCEPT
```
Undo: `sudo iptables -D DOCKER-USER -s 10.88.0.0/16 -j ACCEPT` and the same with `-d`. uninstall.sh
removes the unit and its two rules (only when install.sh added them).

### No network in the container
After the start, install.sh looks up `example.com` from inside the container (a few seconds at most;
`NET_CHECK=no` skips it) and warns when that fails. `melissa-setup` checks the same before a download
and stops at once: "This container can't reach the internet". Likely causes:
- Docker on the host (above): check `sudo iptables -S DOCKER-USER` lists the two rules.
- Another firewall whose forward policy drops podman's network (10.88.0.0/16 by default).
- IP forwarding off on the host: `sysctl net.ipv4.ip_forward` should be 1 (podman turns it on).
Test: `sudo podman exec melissa getent ahosts example.com`.

### firewalld
install.sh only reads firewalld (Fedora, RHEL, openSUSE); it never changes it. podman (netavark, nftables
driver) publishes the web UI and the app ports with its own DNAT rules, and firewalld 2.x
(`StrictForwardPorts`, `/etc/firewalld/firewalld.conf`) decides what happens to them before any zone does:
- **`StrictForwardPorts=no`** (the default): firewalld accepts all DNAT'd traffic, so zone ports and services
  play no part. Melissa's own filter (Settings → Apps) decides who may open apps. The preflight prints the
  default zone and this setting.
- **`StrictForwardPorts=yes`**: firewalld lets through only its own forward-ports, and netavark refuses to
  start any container that publishes ports ("Port forwarding not possible as firewalld StrictForwardPorts
  enabled"), Melissa's included (also the loopback web UI). Adding ports or a service to a zone does not help.
  The preflight stops and prints the commands: set `StrictForwardPorts=no` in `firewalld.conf`, then
  `sudo firewall-cmd --reload`. That holds for every rootful container on the host. The alternative,
  `firewall_driver = "firewalld"` in `containers.conf`, is marked experimental by podman, changes all podman
  networks, and is not tested with Melissa.

netavark puts podman's subnet (10.88.0.0/16) in the `trusted` zone as a runtime-only change, so
`firewall-cmd --reload` drops that entry (netavark's own nftables table stays) until Melissa's container
restarts. To have netavark restore it on every reload: `sudo systemctl enable --now
netavark-firewalld-reload.service` (off by default on Fedora).

### Ubuntu and AppArmor
Supported: Ubuntu 26.04 (the latest; other releases untested). Ubuntu restricts unprivileged user namespaces
(`kernel.apparmor_restrict_unprivileged_userns = 1`) and loads `/etc/apparmor.d/bwrap-userns-restrict`. Its
`bwrap` profile attaches to `/usr/bin/bwrap` by path, inside containers too, and runs what bwrap starts as
`unpriv_bwrap`, which denies every capability: `setpriv` could not switch to the agent's user, and every agent
sandbox would fail.

So on a host with AppArmor, install.sh installs Melissa's own profile and runs the container under it:
- `/etc/apparmor.d/melissa-container` (from `host/apparmor/melissa-container`): `flags=(unconfined)` plus
  `userns,`, the shape of Ubuntu's own profiles for programs that need user namespaces. It is attached by
  name, not by path: only Melissa's container gets it (`PodmanArgs=--security-opt apparmor=melissa-container`
  in the Quadlet; not `AppArmor=`, which Quadlet has only from podman 5.8). Local additions go in
  `/etc/apparmor.d/local/melissa-container`.
- install.sh loads it with `apparmor_parser -r`, checks that it is loaded, records `APPARMOR_PROFILE=yes` in
  install.env and says so (NOTICE). It is loaded again at every boot like every profile there.
  uninstall.sh unloads and deletes it.
- The image's sandbox uses its own copy of bwrap, `/usr/libexec/melissa/bwrap`, which Ubuntu's path-attached
  `bwrap` profile does not match (`find_bwrap` tries it first; `/usr/bin/bwrap` and `PATH` after it).
- The host's restriction stays on for everything else: the sysctl stays 1 and `bwrap-userns-restrict` stays
  loaded. Nothing in `/etc/apparmor.d/disable/` is touched.
- CI's release gate runs the escape suite (and the gVisor test) on Ubuntu 26.04 this way, with the restriction
  asserted on, and checks inside the container that it runs under `melissa-container` ("Release gate").
- Not tried on a real Ubuntu install by us yet; only on the CI runners.

Why the whole container and not a profile for `/usr/libexec/melissa/bwrap`: a path-attached profile would
also match that path in any other container on the host, including pods from images someone crafted with a
file at that path; a profile attached by name applies only where podman is told to use it (docs/DECISIONS.md,
2026-11-27).

Fallback, only when the host restricts and the profile cannot be loaded (no `apparmor_parser`, or it rejects
the profile, e.g. an AppArmor older than 4): the preflight or the install stops, says why, and prints the last
resort, turning Ubuntu's restriction off for the whole host. On a stock Ubuntu host that is:
```
sudo ln -s /etc/apparmor.d/bwrap-userns-restrict /etc/apparmor.d/disable/
sudo apparmor_parser -R /etc/apparmor.d/bwrap-userns-restrict
# only while kernel.apparmor_restrict_unprivileged_userns is 1:
echo 'kernel.apparmor_restrict_unprivileged_userns = 0' | sudo tee /etc/sysctl.d/60-melissa-userns.conf
sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
```
Undo:
```
sudo rm /etc/apparmor.d/disable/bwrap-userns-restrict && sudo apparmor_parser -r /etc/apparmor.d/bwrap-userns-restrict
sudo rm /etc/sysctl.d/60-melissa-userns.conf && sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=1
```
Caveats of the fallback: it turns off an Ubuntu security restriction for the whole host, not just for Melissa;
any program can then use bwrap (with the sysctl at 0: anything) to create user namespaces; install.sh never
runs these commands and uninstall.sh does not undo them. Either way the container is rootful and
`--privileged`: root in it is root on the host.

## Data and persistence
The container is **recreated from the image at every start** (and on updates). Everything kept lives on
the `melissa-var` volume (`/var`): k3s and app data, the server's state, chats, agent installs and logins
(`/var/lib/melissa/home`). Two things from `/etc` are symlinked onto it: `/etc/melissa` (server.toml,
Settings' providers.toml) and `/etc/rancher/node` (k3s's node password).

### Factory reset (Settings → Reset)
Erases Melissa's data and starts her again **unclaimed**, with a new device id and a new setup code.
- **Erased:** people and sign-ins, helpers, chats, files, schedules and permissions, every app and its data
  (the whole k3s datastore), provider sign-ins and API keys (`/var/lib/melissa/home`), Settings
  (`providers.toml`, `ui.toml`, `app-access.toml`, `tailscale.toml`), Melissa's own Tailscale (logged out
  first), the audit log (it starts again with one "factory reset" line) and the journal.
- **Kept:** the image and Melissa's version, `server.toml`, k3s's node password, the voice model, and
  everything on the host (`install.env`, the Quadlet file, the drop-ins; the host's Tailscale is never
  touched).
- **How:** the owner (password entered again) opens Settings → Reset, types `melissa` and confirms. Not
  while an update waits to install. From the host: `sudo podman exec melissa melissa-server factory-reset
  --confirm melissa`. When the web UI doesn't start: `sudo podman exec melissa
  /usr/libexec/melissa/factory-reset --confirm melissa` (runs the reset without the server).
- The server writes a request marker (`/var/lib/melissa-container/reset/factory-reset.json`);
  `melissa-factory-reset.path` starts the root unit `melissa-factory-reset.service`, which stops the server,
  Tailscale and k3s, removes a fixed list (`melissa_settings::factory_reset`), deletes the marker last and
  starts everything again. A marker left by a crash is finished at the next start. Progress:
  `sudo podman exec melissa journalctl -u melissa-factory-reset`. The new setup code:
  `sudo podman exec melissa melissa-server setup-code`.
- To remove everything instead (the volume too), see Uninstall.

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
- **Signature check** (published image): podman's `/etc/containers/policy.json` requires a `sigstoreSigned`
  signature by Melissa's key (`keyPath`, `matchRepository`) for `ghcr.io/svin24/melissa`. It applies to every
  pull of that repository: install, Settings → Updates and `podman-auto-update`. If the signature is missing
  or does not match, podman refuses the pull, the running image stays, and Settings → Updates shows the error.
  Other images are not affected. Local builds (`localhost/`) are not signed and not checked.
- Version: `/usr/lib/melissa/release` in the image (`MELISSA_VERSION`, `MELISSA_REVISION`) and the
  image labels; Settings shows them with the image digest from the helper (or the image name and ID from
  `/run/.containerenv` without it).

## Release gate (sandbox escape suite)
An image is releasable only after the sandbox escape suite (docs/agent-skills/sandbox-escape-tests.md) passed
**inside it**:

```sh
just container-build            # localhost/melissa:dev from the current commit
just escape-test-image          # or: just escape-test-image localhost/melissa:<tag>
```

`escape-test.sh` builds the Containerfile's `escape-build` stage (the suite's test binary and the test-only
probe helper; the image itself never contains them, its lint checks that), adds them under
`/opt/melissa-escape` to a throwaway copy of the image with k3s and the server masked, copies that into root's
storage, boots it like the Quadlet (rootful, `--privileged --systemd=always`, podman's network, no published
ports, container name `melissa-escape-test`, so an installed `melissa` keeps running; on a host with AppArmor
under Melissa's profile, loaded with `sudo packaging/container/apparmor.sh load` and checked) and runs the suite as
`melissa` with the server unit's hardening. Needs sudo. Output: the probe table and
`target/escape-test-image.log`.

On a pass, and only when the image's revision label is the commit the suite was built from (no local changes
under `crates/`), it tags the image `localhost/melissa:escape-tested`. Any run on an image first removes that
tag from it, so a failing run revokes an earlier pass. CI runs this gate before every publish and pushes only
`:escape-tested`, and only when its image ID is the one it built (see "Published images and CI").

## Published images and CI
`.github/workflows/container.yml` builds the image on GitHub's `ubuntu-26.04` runner the same way as
`just container-build`, runs the release gate, then publishes to `ghcr.io/svin24/melissa`:

| Trigger | Tags |
|---|---|
| push to `master` | `sha-<rev>` (never moved), then `edge` |
| tag `vX.Y.Z` | `X.Y.Z` (never moved), then `X.Y` and `stable` |
| manual run | none: build and gate only |

- A tag `vX.Y.Z` must equal the version of every crate (`scripts/release-check.sh`, `just release-check`).
- The image is pushed once, to the immutable tag. It is signed by digest with Melissa's cosign key (legacy
  signature format, which podman reads; recorded in the Rekor transparency log) and gets a GitHub build
  provenance attestation. The moving tags are added afterwards, registry-side, to the same digest.
- A last job checks the result as a device would: `cosign verify` without credentials, and a podman pull
  with the policy install.sh writes; the same pull with another key must be refused.
- Before the gate the job loads Melissa's AppArmor profile (`apparmor.sh load`, what install.sh does) and
  asserts that Ubuntu's restriction is still on (`kernel.apparmor_restrict_unprivileged_userns` is 1,
  `bwrap-userns-restrict` loaded). escape-test.sh and gvisor-test.sh run the image under the profile and
  check it applies, so a pass shows the image works on a stock Ubuntu 26.04 ("Ubuntu and AppArmor" above).
- `gvisor-test.sh` also runs, but is advisory for now: a failure does not block publishing.
- The signing key's private half lives in the GitHub environments `edge` and `stable`
  (`COSIGN_PRIVATE_KEY`, `COSIGN_PASSWORD`), never in the repo.

## gVisor for apps
Apps run under gVisor by default (the app spec's `isolation`, docs/agent-skills/app-deploy.md): the image
carries runsc and its containerd shim (`gvisor.lock`, about 120 MB unpacked), and `k3s-prepare` gives k3s the
`runsc` runtime and the `gvisor` RuntimeClass at every start. Nothing to set up on the host: the systrap
platform needs no KVM. The startup summary (`journalctl -u melissa-server` inside) says `gVisor: on (runsc
release-…)`. Check an image end to end, next to an installed `melissa` (own name, volume and tag; no ports;
removed afterwards; needs sudo and the network for a busybox image):

```sh
just container-build
just gvisor-test-image          # or: just gvisor-test-image localhost/melissa:<tag>
```

## Security
- `--privileged` rootful: the container has **root on the host**. k3s needs mounts, netfilter and network
  namespaces; the agent sandbox needs nested namespaces. Inside, the server runs as the unprivileged
  `melissa` user and each agent in its own bubblewrap sandbox with an egress proxy, as its own host user
  from `melissa`'s subordinate ids (`/etc/subuid`: `melissa:1879048192:65536`, set in the image; agent users,
  DECISIONS). With rootful podman those ids are the host's own: no host user has them.
- SELinux reads as disabled inside (k3s `selinux: false`); the host confines the container as podman does.
- The update channel carries three fixed words as file names; the host helper takes no arguments from
  the container.

## Rootless (unsupported)
Rootless podman needs cpuset delegation for the user and pasta networking (`--host-lo-to-ns-lo`); it
worked on 2026-11-19 with the earlier image but is no longer tested. `scripts/container-run.sh` still has
the rootless path for experiments.
