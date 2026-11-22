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

Needs Linux with systemd, cgroup v2, podman 5+, root, `git`, `just`, and free ports 8080 and 30000-30079.
Full guide, settings and what it changes on the host:
[packaging/container/README.md](../packaging/container/README.md).

```
git clone https://github.com/svin24/melissa.git melissa && cd melissa
just container-build           # builds localhost/melissa:dev (no root; compiles inside podman)
just container-install         # sudo: installs the Quadlet, starts melissa.service, prints the setup code
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
- **Updates:** Settings → Updates (check, apply, roll back) and a daily automatic update (`AUTO_UPDATE=off` to
  skip). Apps, chats and agent sign-ins live on the `melissa-var` volume and survive updates and reinstalls.
- **Apps** (web UIs and other ports such as a game server's, TCP or UDP) answer on ports 30000-30079.
  Who may open them is a setting, Settings → Apps: this device (default), your tailnet, or your home
  network. Agents ask you before they open apps to more devices. App links follow the setting.
- Rootless podman is not supported.

### Use it from another machine

The web UI listens on `127.0.0.1:8080` of the host only (sign-in runs over plain HTTP). Reach it through SSH,
which does the authentication. `<host>` is the machine running Melissa, `<user>` your account there; `-N`
keeps the tunnel open without a shell:
```
ssh -N -L 8080:127.0.0.1:8080 <user>@<host>
```
then browse to http://127.0.0.1:8080 on the other computer (another local port if 8080 is taken there:
`-L 18080:127.0.0.1:8080`). In `~/.ssh/config`: `Host melissa` / `HostName <host>` / `User <user>` /
`LocalForward 8080 127.0.0.1:8080`, then `ssh -N melissa`.

**Optional, Tailscale:** on the host, `tailscale serve` publishes the UI to your tailnet with a Tailscale
certificate:
```
sudo tailscale serve --bg --https=8443 http://127.0.0.1:8080
```
It is then at `https://<host>.example.ts.net:8443` (your tailnet's name instead of `example.ts.net`);
`tailscale serve --https=8443 off` turns it off.

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
sudo packaging/container/uninstall.sh            # or: just container-uninstall
sudo packaging/container/uninstall.sh --purge    # or: just container-uninstall --purge
```

Both stop Melissa and remove the Quadlet, the update helper and its units, `/etc/modules-load.d/melissa.conf`
and the sysctl file (restoring the old values), and turn `podman-auto-update.timer` off again if install.sh
turned it on.

- **Without `--purge`** it keeps the `melissa-var` volume (apps, chats, sign-ins, config), root's images
  (`localhost/melissa:latest` and `localhost/melissa-rollback:previous`) and
  `/var/lib/melissa-container/install.env`, so installing again continues where you were.
- **`--purge`** deletes those too (it signs Melissa's own Tailscale node out first).
- **Either way** your own build stays in your rootless podman: `podman rmi localhost/melissa:dev` (and
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
- Loopback only: sign-in runs over plain HTTP, so Melissa refuses to listen on a LAN address. Use SSH or
  `tailscale serve` ([Use it from another machine](#use-it-from-another-machine)).
- Agents reach the internet only through Melissa's proxy. A CLI version that needs a new host for its AI
  service fails with a network error; the audit log shows `egress.deny` for it.
- Sign-ins are refreshed by the agents inside the sandbox; this is not well tested yet. If an agent suddenly
  says it is not signed in, sign in again in Settings → Providers (NixOS: `sudo melissa-setup login <name>`).

## Feedback

Please [open an issue](https://github.com/svin24/melissa/issues) with what you tried, what happened, your distribution and
how you installed (remove anything private first). Ideas and "this was confusing" are just as welcome as
bug reports.
