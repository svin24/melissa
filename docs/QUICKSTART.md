# Try Melissa from source

> **Pre-alpha.** Expect bugs and missing pieces. Run it on a machine you don't mind experimenting on,
> keep it on this machine (it listens on 127.0.0.1 only and has no login), and read what the agents
> ask to do before you approve it.

From source you get the lead agent, its agents and the web UI on your own Linux machine. You chat with the
Lead agent; it uses an AI coding CLI you are already logged in to (Claude Code, Grok CLI or OpenCode).
Installing apps needs k3s, which a plain source checkout does not have (see [Apps and k3s](#apps-and-k3s)).
For the full system with k3s, run the container instead ([Run the full system in a container](#run-the-full-system-in-a-container-rootful-podman)).

## What you need

- **Linux on x86_64.** Tested on Fedora 45. Other distributions should work; Ubuntu 23.10+ needs one
  extra step (see [Ubuntu and AppArmor](#ubuntu-and-apparmor)).
- **bubblewrap** (`bwrap`): every agent runs inside it. `sudo apt install bubblewrap`,
  `sudo dnf install bubblewrap` or `sudo pacman -S bubblewrap`. Fedora and most desktops have it already.
  Unprivileged user namespaces must be allowed (they are by default on Fedora, Debian 11+, Arch).
- **At least one agent CLI, installed and logged in** (run it once by hand and log in):
  - Claude Code: `curl -fsSL https://claude.ai/install.sh | bash`, then run `claude` and log in.
    Docs: https://code.claude.com/docs/en/setup. Use this native installer: an npm install outside
    `/usr` is not visible inside the sandbox.
  - Grok CLI (Grok Build, needs a SuperGrok or X Premium+ subscription): `curl -fsSL https://x.ai/cli/install.sh | bash`,
    then run `grok` and log in. Docs: https://docs.x.ai/build/overview.
  - OpenCode: `curl -fsSL https://opencode.ai/install | bash`. Docs: https://opencode.ai/docs.
    Works without an account (free default model).

  Melissa uses your existing login of these CLIs. It never sees a password, but the agents spend your
  subscription.
- **A Rust toolchain**, one of:
  - **Nix + devenv (recommended, what we use):** install Nix with flakes
    (https://determinate.systems/nix-installer/ or https://nixos.org/download), then devenv
    (https://devenv.sh/getting-started/). The first `devenv shell` downloads a lot (the Rust toolchain,
    and QEMU for the image tooling), later starts take seconds.
  - **rustup:** https://rustup.rs, then `rustup default stable` (tested with Rust 1.100; older releases
    may fail), a C compiler and linker (`build-essential` / `gcc`) and `just`
    (`cargo install just`, or your distribution's package). No devenv: run the `just` commands below
    directly, or the `cargo` lines they print.

## Run it

```
git clone <repo-url> melissa && cd melissa
devenv shell          # skip with rustup
just demo
```

The first build takes a minute or two (about 50 s on a 16-thread laptop, more on a small machine, plus
downloading the crates). Then you see something like:

```
Melissa (pre-alpha) is running.
  Open:     http://127.0.0.1:8080   (this machine only)
  State:    /home/you/.local/state/melissa
  Config:   /home/you/.config/melissa/server.toml  (not present: defaults)
  Sandbox:  ok (bubblewrap)
  Agents:
    claude          claude    found
    grok-acp        grok      not installed (`grok` not on PATH)
    claude-session  claude    found   <- the lead agent uses this
    ...
  k3s:      not found on this machine. Chat, agents and machine checks work; installing apps needs k3s.
  Stop with Ctrl-C.
```

Open **http://127.0.0.1:8080**. The lead agent starts with your first message; it uses the first agent of
`grok-acp`, `claude-session`, `opencode-acp` that is installed (and moves on to the next if one fails to
start, for example because it is not logged in).

You can also talk to her from a second terminal in the same folder: `just chat`, or
`just ask "describe this machine"`.

## Things to try

Without k3s these work:

1. **"Describe this machine."** The lead agent calls Melissa's `device_info` tool and tells you the OS, kernel,
   memory and disk space.
2. **"Create an agent named Ada and have her check this machine, then tell me what she found."** The lead agent
   creates a persistent agent, gives her a job and reports back (20-30 s). Talk to Ada directly afterwards:
   `just chat` then type, or `cargo run -q -p melissa-server -- ask --agent Ada "what did you find?"`.
3. **"Which apps can you install?"** She lists the catalog (Nextcloud for now).
4. **"Install Nextcloud."** Without k3s she answers that k3s is not running on this machine. Nothing is
   installed. That is expected from source.

Agents act only through Melissa's tools: no shell, no file access, and network only to their own AI
service. Anything risky waits for your approval in the confirmation inbox (in the UI, or
`cargo run -q -p melissa-server -- confirm list`).

## Apps and k3s

App installs need a k3s cluster. A source checkout looks for cluster credentials in
`/run/melissa-kube/` (written in the Melissa container) or at `kubeconfig = "..."` in
`~/.config/melissa/server.toml`, and app installs need a second, write-only identity
(`[apps] kubeconfig`) with Melissa's admission policies. Setting that up by hand is not documented for
strangers yet; use the container to try apps.

## Run the full system in a container (rootful podman)

The whole of Melissa (Lead agent, agent sandbox, k3s and apps) as one container on your Linux machine, started by
systemd. Needs root, podman 5+, cgroup v2 and free ports 8080 and 30000-30079. Full guide, settings and what it
changes on the host: [packaging/container/README.md](../packaging/container/README.md).

```
just container-build           # builds localhost/melissa:dev (no root)
just container-install         # sudo: installs the Quadlet, starts melissa.service, prints the UI URL
just container-smoke           # sudo: k3s, Traefik, policies, an app port, pods can't reach the UI
sudo podman exec -it melissa melissa-setup agent opencode     # free models; or claude / grok (log in)
sudo podman exec -it melissa melissa-server chat               # or the web UI: http://127.0.0.1:8080
systemctl status melissa
just container-uninstall       # keeps your data; `just container-uninstall --purge` deletes it
```

- **Security:** the container is rootful and `--privileged` (k3s and the agent sandbox need namespaces, mounts
  and netfilter): root in it is root on your machine. The kubelet needs a few host sysctls; install.sh sets them in
  `/etc/sysctl.d/90-melissa-kubelet.conf`, tells you, and uninstall restores the old values.
- **Updates:** Settings → Updates (check, apply, roll back) and a daily automatic update (`AUTO_UPDATE=off` to
  skip). Apps, chats and agent logins live on the `melissa-var` volume and survive updates and reinstalls.
- **Apps** answer on `BIND` (default 127.0.0.1) ports 30000-30079; `BIND=0.0.0.0` (LAN) or your Tailscale address
  (`BIND=$(tailscale ip -4)`) to open them from other devices; app links use that address.
- Rootless podman is not supported.

### Use it from another machine

The web UI listens on `127.0.0.1:8080` of the host only (it has no login yet). Reach it through SSH, which does the
authentication. `<host>` is the machine running Melissa, `<user>` your account there; `-N` keeps the tunnel
open without a shell:
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

**Warning:** Melissa has no login yet. Everyone and every device that can reach that address on your tailnet can
chat with the lead agent and **approve its actions**, including deleting apps and their data. Only do this on a tailnet
you alone use, or restrict who can reach the host on that port with Tailscale ACLs (grants) first.

## Run on NixOS — experimental

Melissa is a Nix flake with a NixOS module (`services.melissa`): the lead agent as a system service, k3s for apps,
the agent sandbox. In your system flake:

```nix
inputs.melissa.url = "path:/path/to/melissa";   # a checkout; nothing is published yet
# in nixosSystem { modules = [ ... ]; }:
melissa.nixosModules.default
({ pkgs, ... }: {
  services.melissa = {
    enable = true;
    apps.openFirewall = true;        # app ports 30000-30079 for the LAN
    # k3s.airgapImages = true;       # k3s's own images from the store (offline boxes)
    # The agents (providers) come from Nix; the binary must be named claude, grok or opencode.
    agents.packages = [ pkgs.opencode pkgs.claude-code ];   # claude-code is unfree: allow it
  };
})
```

Then `sudo nixos-rebuild switch`, and log in to an agent: `sudo melissa-setup login opencode` (or `claude`).
Settings → Providers doesn't install agents on NixOS; add them to `agents.packages`. (By hand,
`sudo melissa-setup agent <name>` still works: the vendors' CLIs run through nix-ld, which the module turns
on.) The web UI listens on 127.0.0.1:8080 only (no login yet): `ssh -L 8080:127.0.0.1:8080 you@box`.
Updates come with your system: `nix flake update && sudo nixos-rebuild switch` (rollback with
`sudo nixos-rebuild switch --rollback`); Settings → Updates shows the same commands.

- **Status:** verified in a NixOS VM test (`nix build .#checks.x86_64-linux.melissa-nixos -L`, 2026-11-19):
  web UI, a chat turn, the agent sandbox and network allowlist, k3s with Melissa's policies, an app install
  reachable from another machine, stop/start, Settings. Not yet on real hardware.
- **Other distributions with Nix** (Fedora, Debian, ...): a prototype bundle, `.#melissa-host`; k3s comes from the
  distribution. Details, options and gotchas: [agent-skills/nix-service.md](agent-skills/nix-service.md).

## Stop, state and reset

- **Stop:** Ctrl-C in the terminal running `just demo`. Agents stop with it.
- **State** (audit log, the lead agent's and agents' conversations, agent registry, agent working folders):
  `~/.local/state/melissa/`. **Config** (optional): `~/.config/melissa/server.toml`. Sandbox sockets:
  `$XDG_RUNTIME_DIR/melissa/`. Nothing is written into the repo except the build in `target/`.
- **Reset:** stop Melissa, then `rm -rf ~/.local/state/melissa` (forgets the conversation, agents and audit
  log). Your agent CLIs' own logins are not touched.
- **Try it without touching your normal dirs:**
  `XDG_STATE_HOME=/tmp/m/state XDG_CONFIG_HOME=/tmp/m/config just demo`.

## Known limitations

- Pre-alpha: the UI and the chat are new and change often; conversations may not survive an upgrade.
- No k3s from source, so no app installs (above).
- Loopback only. There are no accounts yet, so Melissa refuses to listen on a LAN address.
- Agents reach the internet only through Melissa's proxy, and only their own AI service's hosts. A CLI
  version that needs a new host fails with a network error; the audit log shows `egress.deny` for it
  (`grep egress ~/.local/state/melissa/audit.jsonl`).
- The sandbox needs bubblewrap and unprivileged user namespaces. Containers without them (some CI
  runners, Docker without `--privileged`, WSL1) cannot run agents; the UI and the echo backend still work.
- The sandbox expects the CLIs' default config folders (`~/.claude`, `~/.grok`, `~/.config/opencode`).
- Logins are refreshed by the CLIs inside the sandbox; this is not well tested yet. If an agent suddenly
  says it is not logged in, run the CLI once by hand.

## Ubuntu and AppArmor

Ubuntu 23.10 and later (including 24.04) block unprivileged user namespaces for programs without an
AppArmor profile that allows them. `just demo` then shows
`Sandbox: PROBLEM: bubblewrap cannot create namespaces: bwrap: setting up uid map: Permission denied`.
Pick one (not yet tested by us on Ubuntu; please tell us what worked):

- Allow it for `bwrap` only, with a small AppArmor profile:
  ```
  sudo tee /etc/apparmor.d/bwrap <<'EOF'
  abi <abi/4.0>,
  include <tunables/global>

  profile bwrap /usr/bin/bwrap flags=(unconfined) {
    userns,
    include if exists <local/bwrap>
  }
  EOF
  sudo apparmor_parser -r /etc/apparmor.d/bwrap
  ```
  Ubuntu's `apparmor-profiles` package also ships a `bwrap-userns-restrict` profile in
  `/usr/share/apparmor/extra-profiles/`; it is stricter and may stop the agents from working.
  Either way, any program can then use `bwrap` to create user namespaces, which is what the restriction was
  meant to prevent.
- Or switch the restriction off until the next reboot:
  `sudo sysctl kernel.apparmor_restrict_unprivileged_userns=0` (system-wide; less safe).

## Troubleshooting

- **An agent shows "not installed".** Its command is not on the `PATH` of the shell you started
  `just demo` from. Check with `which claude grok opencode`. The installers put them in `~/.local/bin`,
  `~/.grok/bin` and `~/.opencode/bin`; open a new terminal after installing.
- **"sandbox: bwrap not found".** Install bubblewrap (see [What you need](#what-you-need)) and restart.
- **"bubblewrap cannot create namespaces".** On Ubuntu see above. Elsewhere check
  `sysctl user.max_user_namespaces` (must be > 0) and, on older Debian,
  `sysctl kernel.unprivileged_userns_clone` (must be 1).
- **"cannot listen on 127.0.0.1:8080".** Something else uses the port, perhaps another Melissa. Stop it,
  or create `~/.config/melissa/server.toml` with `listen = "127.0.0.1:8090"` and open that port instead
  (`just chat`/`ask` read the same file). On a machine running the Melissa desktop image (e.g. the dev
  laptop) the system lead agent always holds 127.0.0.1:8080, so put `listen = "127.0.0.1:8090"` in
  `~/.config/melissa/server.toml` once for `just demo`.
- **The lead agent answers "not logged in" or every backend fails.** Run the CLI once by hand (`claude`,
  `grok`, `opencode`) and log in, then send the message again. To pin one agent, put this in
  `~/.config/melissa/server.toml`:
  ```toml
  [lead]
  backends = ["claude-session"]   # or "grok-acp", "opencode-acp"
  ```
- **Something else.** Run with the full log: `RUST_LOG=info just demo`, and look at the audit log in
  `~/.local/state/melissa/audit.jsonl`.

## Feedback

Please open an issue on the project's repository with what you tried, what happened, your distribution and
the summary `just demo` printed (remove anything private first). Ideas and "this was confusing" are just
as welcome as bug reports.
