# Try Melissa from source

> **Pre-alpha.** Expect bugs and missing pieces. Run it on a machine you don't mind experimenting on,
> keep it on this machine (it listens on 127.0.0.1 only and has no login), and read what the agents
> ask to do before you approve it.

From source you get the Queen, her bees and the web UI on your own Linux machine. You chat with the
Queen; she uses an AI coding CLI you are already logged in to (Claude Code, Grok CLI or OpenCode).
Installing apps needs k3s, which a plain source checkout does not have (see [Apps and k3s](#apps-and-k3s)).
For the full system with k3s, use the VM image instead.

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
  Config:   /home/you/.config/melissa/queen.toml  (not present: defaults)
  Sandbox:  ok (bubblewrap)
  Agents:
    claude          claude    found
    grok-acp        grok      not installed (`grok` not on PATH)
    claude-session  claude    found   <- the Queen uses this
    ...
  k3s:      not found on this machine. Chat, bees and machine checks work; installing apps needs k3s.
  Stop with Ctrl-C.
```

Open **http://127.0.0.1:8080**. The Queen starts with your first message; she uses the first agent of
`grok-acp`, `claude-session`, `opencode-acp` that is installed (and moves on to the next if one fails to
start, for example because it is not logged in).

You can also talk to her from a second terminal in the same folder: `just chat`, or
`just ask "describe this machine"`.

## Things to try

Without k3s these work:

1. **"Describe this machine."** The Queen calls Melissa's `comb_info` tool and tells you the OS, kernel,
   memory and disk space.
2. **"Create a bee named Ada and have her check this machine, then tell me what she found."** The Queen
   creates a persistent bee, gives her a job and reports back (20-30 s). Talk to Ada directly afterwards:
   `just chat` then type, or `cargo run -q -p melissa-queen -- ask --bee Ada "what did you find?"`.
3. **"Which apps can you install?"** She lists the catalog (Nextcloud for now).
4. **"Install Nextcloud."** Without k3s she answers that k3s is not running on this machine. Nothing is
   installed. That is expected from source.

Agents act only through Melissa's tools: no shell, no file access, and network only to their own AI
service. Anything risky waits for your approval in the confirmation inbox (in the UI, or
`cargo run -q -p melissa-queen -- confirm list`).

## Apps and k3s

App installs need a k3s cluster. A source checkout looks for cluster credentials in
`/run/melissa-kube/` (written by the Melissa images) or at `kubeconfig = "..."` in
`~/.config/melissa/queen.toml`, and app installs need a second, write-only identity
(`[apps] kubeconfig`) with Melissa's admission policies. Setting that up by hand is not documented for
strangers yet; use the VM image to try apps.

## Try it in a container (Linux, podman) — experimental

A third way besides the VM and `just demo`: the headless image itself, run by podman with systemd as PID 1.
Needs podman 5+ with pasta, cgroup v2, and the image built from this checkout (`just image-build headless`).

```
scripts/container-run.sh preflight   # checks cgroup delegation, kernel modules, pasta
scripts/container-run.sh build       # adds the small container layer (image/container)
scripts/container-run.sh up          # web UI: http://127.0.0.1:18080
scripts/container-run.sh smoke       # k3s Ready, Traefik, policies, an app port from the host (~3 min first time)
scripts/container-run.sh shell       # then: melissa-setup agent opencode, melissa-queen chat
scripts/container-run.sh down        # stop; `purge` deletes the container and its data volume
```

- **Status:** verified rootless on Fedora 45 (2026-11-19): web UI, agents (sandbox and network allowlist),
  `melissa-setup`, k3s with Traefik, Nextcloud from the catalog at `http://127.0.0.1:30000`, stop/start of apps,
  and a chat with the Queen. Rootful (`sudo`) verified the same day for the web UI, k3s, Nextcloud and
  stop/start (agents and chat not tried rootful).
- **Rootful:** run the same commands with `sudo` (root has its own image storage: `podman save` the image and
  `sudo podman load -i` it, or build as root). It uses podman's bridge network instead of pasta and needs no
  `cpuset` change; a small filtered forward inside the container passes the host's UI connections to the
  Queen, and pods can't use it. Root in the container is root on your machine.
- **Rootless k3s needs one host change** (as root, once): let your user's containers use the `cpuset` cgroup
  controller, which Fedora does not delegate by default. `preflight` checks it and prints the commands:
  ```
  sudo mkdir -p /etc/systemd/system/user@.service.d
  printf '[Service]\nDelegate=cpu cpuset io memory pids\n' | sudo tee /etc/systemd/system/user@.service.d/delegate.conf
  sudo systemctl daemon-reload
  ```
- **App links** say `http://127.0.0.1:30000` (and up): `up` writes that address into the Queen's config. If you
  move the app ports (`APP_PORT_BASE=31000`), add the difference yourself: the links still name 30000 and up.
- **Security:** the container runs `--privileged` (k3s and the agent sandbox need namespaces, mounts and
  netfilter). Rootless that is limited to your own user; run as root it is root on your machine. The web UI has
  no login and is published on 127.0.0.1 only; app ports default to 127.0.0.1 too (`BIND=0.0.0.0` for the LAN).
- **Not in a container:** Secure Boot, `bootc` updates and rollback (rebuild and recreate instead; `/var` is a
  volume and survives), the first-boot account prompt, SSH, Tailscale. Host ports: `UI_PORT`, `APP_PORT_BASE`.
- Details and gotchas: [agent-skills/container-mode.md](agent-skills/container-mode.md).

## Stop, state and reset

- **Stop:** Ctrl-C in the terminal running `just demo`. Agents stop with it.
- **State** (audit log, the Queen's and bees' conversations, bee registry, agent working folders):
  `~/.local/state/melissa/`. **Config** (optional): `~/.config/melissa/queen.toml`. Sandbox sockets:
  `$XDG_RUNTIME_DIR/melissa/`. Nothing is written into the repo except the build in `target/`.
- **Reset:** stop Melissa, then `rm -rf ~/.local/state/melissa` (forgets the conversation, bees and audit
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
  or create `~/.config/melissa/queen.toml` with `listen = "127.0.0.1:8090"` and open that port instead
  (`just chat`/`ask` read the same file). On a machine running the Melissa desktop image (e.g. the dev
  laptop) the system queen always holds 127.0.0.1:8080, so put `listen = "127.0.0.1:8090"` in
  `~/.config/melissa/queen.toml` once for `just demo`.
- **The Queen answers "not logged in" or every backend fails.** Run the CLI once by hand (`claude`,
  `grok`, `opencode`) and log in, then send the message again. To pin one agent, put this in
  `~/.config/melissa/queen.toml`:
  ```toml
  [queen]
  backends = ["claude-session"]   # or "grok-acp", "opencode-acp"
  ```
- **Something else.** Run with the full log: `RUST_LOG=info just demo`, and look at the audit log in
  `~/.local/state/melissa/audit.jsonl`.

## Feedback

Please open an issue on the project's repository with what you tried, what happened, your distribution and
the summary `just demo` printed (remove anything private first). Ideas and "this was confusing" are just
as welcome as bug reports.
