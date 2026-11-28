# Try Melissa

> **Pre-alpha.** Expect bugs and missing pieces. Run it on a machine you don't mind experimenting on,
> keep it on this machine (it listens on 127.0.0.1 only; the first person to open it claims it with a
> setup code from the host), and read what the agents ask to do before you approve it.

Two ways to install it:

1. **[Container](#1-container-rootful-podman)**: the main way. The whole system (lead agent, agent sandbox,
   k3s and apps) in one rootful podman container, started by systemd.
2. **[NixOS](#2-nixos-experimental)**: experimental. A NixOS module with k3s.

Developers can also run it from source without apps: [DEV-ENV.md](DEV-ENV.md#run-from-source-just-demo).
To remove Melissa again: [Uninstall](#uninstall).

You also need an account with one AI provider: Claude, Grok, OpenCode (works without an account, free
models), Gemini, Codex or Muse Code. Melissa uses your sign-in; it never sees a password, but the agents
spend your subscription.

## 1. Container (rootful podman)

Needs Linux with systemd, cgroup v2, podman 5+, root, `curl` (or `git`), and free ports 8080 and
30000-30079. Full guide, settings and what it changes on the host:
[packaging/container/README.md](../packaging/container/README.md).

```
curl -fsSL https://raw.githubusercontent.com/svin24/melissa/master/get.sh | sudo sh
```

Or download it, read it, then run it:
```
curl -fsSLO https://raw.githubusercontent.com/svin24/melissa/master/get.sh
less get.sh
sudo sh get.sh
```

[get.sh](../get.sh) checks the host (root, Linux, systemd, podman 5+, curl, tar), downloads Melissa's source
from GitHub (master; with `CHANNEL=stable` the latest release tag) into a temporary directory, runs
`packaging/container/install.sh` from it and deletes the directory. Settings go between `sudo` and `sh`:
`curl -fsSL https://raw.githubusercontent.com/svin24/melissa/master/get.sh | sudo CHANNEL=stable UI_PORT=8090 sh`. (`CHANNEL=stable curl ... | sudo sh` does not
work: sudo drops that environment.) Only check the host: `... | sudo sh -s -- --preflight`.

Or from a git clone:
```
git clone https://github.com/svin24/melissa.git melissa && cd melissa
sudo packaging/container/install.sh   # pulls the signed image, installs the Quadlet, starts melissa.service
```

It pulls `ghcr.io/svin24/melissa:edge`: the latest master commit that passed the release gate. Nothing is
built on your machine. Once releases exist, `sudo CHANNEL=stable packaging/container/install.sh` follows
tagged releases instead. install.sh makes podman refuse the image unless it carries Melissa's signature
(key: `packaging/container/melissa-signing.pub`). To check a signature by hand, with
[cosign](https://github.com/sigstore/cosign):
```
cosign verify --key packaging/container/melissa-signing.pub ghcr.io/svin24/melissa:edge
```

Or build the image yourself (needs `just`; compiles inside podman, no root for the build):
```
just container-build           # builds localhost/melissa:dev
just container-install         # sudo: installs it as localhost/melissa:latest, starts melissa.service
```

Open **http://127.0.0.1:8080** and enter the setup code, a username and a password: you become the owner
(others join through Settings → People). A short setup follows: a device name, a provider (install and sign
in right there), a model. Then chat with Melissa.

Useful afterwards:

```
sudo podman exec melissa melissa-server setup-code   # a new setup code (valid 1 hour)
sudo podman exec melissa melissa-server reset-owner  # forgot the password: a one-hour link
sudo podman exec -it melissa melissa-server chat     # chat from the terminal
systemctl status melissa
just container-smoke           # sudo: checks k3s, Traefik, policies, an app port, pod isolation
```

- **Security:** the container is rootful and `--privileged` (k3s and the agent sandbox need namespaces, mounts
  and netfilter): root in it is root on your machine. The kubelet needs a few host sysctls; install.sh sets them in
  `/etc/sysctl.d/90-melissa-kubelet.conf`, tells you, and uninstall restores the old values.
- **Updates:** Settings → Updates (check, update, roll back, the automatic update's switch and time) and an
  automatic update every day at 03:00 (`SCHEDULE=sun-04` for another time, `AUTO_UPDATE=off` to skip; a daily
  check still runs). An image without Melissa's signature is refused. Apps, chats and agent sign-ins live on the `melissa-var` volume and survive updates and reinstalls.
- **Apps** (web UIs and other ports such as a game server's, TCP or UDP) answer on ports 30000-30079.
  The ports are published on all host addresses, but Melissa's own filter decides who gets in: Settings →
  Apps, this device (default), your tailnet, or your home network. Agents ask you before they open apps to more devices. App links follow the setting.
- Rootless podman is not supported.

### Use it from another machine

The web UI listens on `127.0.0.1:8080` of the host only, and Melissa takes passwords from another device
only over HTTPS. The simplest way is Melissa's own Tailscale: Settings → Tailscale → **Enable**, sign in,
then **Open Melissa from your other devices** → Turn on. The UI is then at
`https://<name>.<tailnet>.ts.net` for your tailnet's devices.

Without Tailscale, reach it through SSH, which does the authentication. `<host>` is the machine running Melissa, `<user>` your account there; `-N`
keeps the tunnel open without a shell:
```
ssh -N -L 8080:127.0.0.1:8080 <user>@<host>
```
then browse to http://127.0.0.1:8080 on the other computer (another local port if 8080 is taken there:
`-L 18080:127.0.0.1:8080`). In `~/.ssh/config`: `Host melissa` / `HostName <host>` / `User <user>` /
`LocalForward 8080 127.0.0.1:8080`, then `ssh -N melissa`.

**The host runs Tailscale itself:** Melissa leaves it alone; Settings → Tailscale shows the command that
publishes the UI to your tailnet with a Tailscale certificate:
```
sudo tailscale serve --bg http://127.0.0.1:8080
```
It is then at `https://<host>.example-tailnet.ts.net` (your tailnet's name instead of `example-tailnet`);
`sudo tailscale serve reset` turns it off. An existing `--https=8443` setup keeps working. Melissa must
believe that proxy: the container's install.sh sets `[web] trust_forwarded = true` in server.toml (run it
again after updating from an older version); on NixOS set `services.melissa.settings.web.trust_forwarded =
true`, elsewhere add it under `[web]` in server.toml. (With it on, other users of the host who reach the UI's
port can claim any client address to the sign-in limits; the per-username limit still holds.)

**Plain HTTP on the LAN** (`UI_BIND` wider than loopback): pages load if the name is in `[web] hosts`, but
signing in from another device over plain HTTP is refused, since the password would cross the network
readable. Use Tailscale or SSH as above; HTTPS on the LAN itself comes later.

Everyone who can reach that address on your tailnet sees the sign-in page; only people with an account
(the owner and the members they invite) get further. Restricting who can reach the host on that port with
Tailscale ACLs (grants) still helps.

## 2. NixOS (experimental)

Melissa is a Nix flake with a NixOS module (`services.melissa`): the lead agent as a system service, k3s for apps,
the agent sandbox. Verified in a NixOS VM test (`nix build .#checks.x86_64-linux.melissa-nixos -L`,
2026-11-19: web UI, a chat turn, the agent sandbox and network allowlist, k3s with Melissa's policies, an app
install reachable from another machine, stop/start, Settings); not yet on real hardware. In your system flake:

```nix
inputs.melissa.url = "github:svin24/melissa";   # or "path:/path/to/a/checkout"
# in nixosSystem { modules = [ ... ]; }:
melissa.nixosModules.default
({ pkgs, ... }: {
  services.melissa = {
    enable = true;
    apps.openFirewall = true;        # app ports 30000-30079 for the LAN
    # k3s.airgapImages = true;       # k3s's own images from the store (offline boxes)
    # The agents (providers) come from Nix; the binary must be named claude, grok, opencode,
    # gemini, codex-acp or muse.
    agents.packages = [ pkgs.opencode pkgs.claude-code ];   # claude-code is unfree: allow it
  };
})
```

Then `sudo nixos-rebuild switch`, and sign in to an agent: `sudo melissa-setup login opencode` (or `claude`).
Settings → Providers doesn't install agents on NixOS; add them to `agents.packages`. (By hand,
`sudo melissa-setup agent <name>` still works: the vendors' CLIs run through nix-ld, which the module turns
on.) The web UI listens on 127.0.0.1:8080 only (sign-in runs over plain HTTP): reach it from another machine
as in [Use it from another machine](#use-it-from-another-machine). Claim it with
`sudo melissa-server setup-code`.
Updates come with your system: `nix flake update && sudo nixos-rebuild switch` (rollback with
`sudo nixos-rebuild switch --rollback`); Settings → Updates shows the same commands.

**Other distributions with Nix** (Fedora, Debian, ...): a prototype bundle, `.#melissa-host`; k3s comes from the
distribution. Details, options and gotchas: [agent-skills/nix-service.md](agent-skills/nix-service.md).

## Uninstall

### Container

```
sudo /usr/local/libexec/melissa/uninstall.sh            # any install, also get.sh's
sudo /usr/local/libexec/melissa/uninstall.sh --purge
sudo packaging/container/uninstall.sh [--purge]         # from a git clone; or: just container-uninstall
```
install.sh puts a copy of uninstall.sh (with `trust.sh` and `apparmor.sh`, which it uses) in
`/usr/local/libexec/melissa/`, and uninstall removes that copy too.

Both stop Melissa and remove the Quadlet, the update helper and its units, `/etc/modules-load.d/melissa.conf`
and the sysctl file (restoring the old values), remove the signature check for Melissa's image from
`/etc/containers/policy.json`, remove Melissa's update timers, and turn `podman-auto-update.timer` off again if an older install.sh turned
it on.

- **Without `--purge`** it keeps the `melissa-var` volume (apps, chats, sign-ins, config), root's images
  (the installed one, e.g. `ghcr.io/svin24/melissa:edge`, and `localhost/melissa-rollback:previous`) and
  `/var/lib/melissa-container/install.env`, so installing again continues where you were.
- **`--purge`** deletes those too (it signs Melissa's own Tailscale node out first).
- **Either way** a local build stays in your rootless podman: `podman rmi localhost/melissa:dev` (and
  `podman image prune` for the build layers). The kernel modules stay loaded until the next reboot.

### NixOS

Set `services.melissa.enable = false` (or remove the module) and run `sudo nixos-rebuild switch`. That removes
the services, the `melissa` user and groups, `/etc/melissa/server.toml` and the firewall ports. Left behind,
delete them if you want:

```
sudo rm -rf /var/lib/melissa /etc/melissa   # state, chats, agent sign-ins, voice model, Settings
sudo rm -rf /var/lib/rancher /etc/rancher /var/lib/kubelet /var/lib/cni   # k3s and app data (only if nothing else uses k3s)
sudo nix-collect-garbage -d                 # free the store (also deletes old generations)
```

Reboot first if k3s ran, so no pod mounts are left under `/var/lib/kubelet`.

## Known limitations

- Pre-alpha: the UI and the chat are new and change often; chats may not survive an upgrade.
- Loopback only: passwords go only over HTTPS or from this device, and there is no LAN HTTPS yet. Use
  Tailscale or SSH ([Use it from another machine](#use-it-from-another-machine)).
- Agents reach the internet only through Melissa's proxy. A CLI version that needs a new host for its AI
  service fails with a network error; the audit log shows `egress.deny` for it.
- Container: if installing an agent says "This container can't reach the internet", the host's firewall
  drops its traffic. Docker on the same host does that; install.sh fixes it for you (rerun it). See
  [Docker on the same host](../packaging/container/README.md#docker-on-the-same-host).
- Container with firewalld (Fedora): install.sh only reads it. With firewalld's default
  (`StrictForwardPorts=no`) nothing needs opening. With `StrictForwardPorts=yes` podman can't publish ports,
  so the preflight stops and prints the commands to change it. See
  [firewalld](../packaging/container/README.md#firewalld).
- Container on Ubuntu (26.04): install.sh installs and loads Melissa's own AppArmor profile,
  `/etc/apparmor.d/melissa-container`, and runs the container under it (a NOTICE says so). It lets only
  Melissa's container create user namespaces; Ubuntu's restriction stays on for everything else, and
  uninstall.sh removes the profile. Only if the profile can't be loaded (for example no `apparmor_parser`)
  does install.sh stop and print the last-resort steps and their caveats. Tested on Ubuntu CI runners, not yet
  on an Ubuntu install. See [Ubuntu and AppArmor](../packaging/container/README.md#ubuntu-and-apparmor).
- Sign-ins are refreshed by the agents inside the sandbox; this is not well tested yet. If an agent suddenly
  says it is not signed in, sign in again in Settings → Providers (NixOS: `sudo melissa-setup login <name>`).

## Feedback

Please [open an issue](https://github.com/svin24/melissa/issues) with what you tried, what happened, your distribution and
how you installed (remove anything private first). Ideas and "this was confusing" are just as welcome as
bug reports.
