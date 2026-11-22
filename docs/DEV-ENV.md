# Dev environment: Nix + devenv + direnv

The repo ships a `devenv.nix` / `devenv.yaml` (primary), a `flake.nix` (alternative entry point),
and an `.envrc`. The shell provides Rust (stable via rust-overlay: rustc, cargo, clippy, rustfmt,
rust-analyzer, rust-src), cargo-nextest, cargo-watch, just, pkg-config, and pre-commit hooks
(rustfmt + clippy), plus Python with Playwright, Playwright's Chromium, Pillow and libfaketime
for the UI screenshots (`just ui-shots`, below). Container tooling (podman) comes from the host, not Nix; `just container-*` uses it.

```
direnv allow          # auto-loads the shell via .envrc (`use devenv`)
devenv shell          # or enter it manually
nix develop --no-pure-eval   # flake alternative
just check            # fmt + clippy + tests
```

Just want to try Melissa? Install it: [QUICKSTART.md](QUICKSTART.md). Running from source (also with rustup
instead of Nix): [Run from source](#run-from-source-just-demo).

## Repo layout

```
crates/agent     agent layer: ACP and Claude Code sessions, sandbox, tools, task board, audit log
crates/server    server: lead agent, web UI and API, CLI
crates/web       web UI (axum + maud + embedded htmx)
crates/cluster   k3s client and the app tools
crates/appspec   the app spec agents write to deploy an app
crates/auth      accounts, the owner's claim, invites, sessions
crates/settings  provider settings, agent installs, updates
crates/voice     speech to text on the device
crates/device    per-device daemon (stub)
packaging/       common/ (system files), container/ (image, Quadlet, install.sh), nix/ (flake package, NixOS module)
scripts/         container-run.sh, ui-shots/ (UI screenshots), tokens/ (design tokens)
docs/            PLAN.md, DECISIONS.md, QUICKSTART.md, DEV-ENV.md, agent-skills/
```

## Run from source (`just demo`)

The lead agent, its helpers and the web UI on your own Linux machine, using an AI coding CLI you are already
signed in to. There is no k3s, so **no app installs** ([Apps and k3s](#apps-and-k3s)). To try apps, install the
container ([QUICKSTART.md](QUICKSTART.md)).

### What you need

- **Linux on x86_64.** Tested on Fedora 45. Other distributions should work; Ubuntu 23.10+ needs one
  extra step (see [Ubuntu and AppArmor](#ubuntu-and-apparmor)).
- **bubblewrap** (`bwrap`): every agent runs inside it. `sudo apt install bubblewrap`,
  `sudo dnf install bubblewrap` or `sudo pacman -S bubblewrap`. Fedora and most desktops have it already.
  Unprivileged user namespaces must be allowed (they are by default on Fedora, Debian 11+, Arch).
- **At least one agent CLI, installed and signed in** (run it once by hand and sign in):
  - Claude Code: `curl -fsSL https://claude.ai/install.sh | bash`, then run `claude` and sign in.
    Docs: https://code.claude.com/docs/en/setup. Use this native installer: an npm install outside
    `/usr` is not visible inside the sandbox.
  - Grok CLI (Grok Build, needs a SuperGrok or X Premium+ subscription): `curl -fsSL https://x.ai/cli/install.sh | bash`,
    then run `grok` and sign in. Docs: https://docs.x.ai/build/overview.
  - OpenCode: `curl -fsSL https://opencode.ai/install | bash`. Docs: https://opencode.ai/docs.
    Works without an account (free default model).
- **A Rust toolchain**, one of:
  - **Nix + devenv (recommended, what we use):** install Nix with flakes
    (https://determinate.systems/nix-installer/ or https://nixos.org/download), then devenv
    (https://devenv.sh/getting-started/). The first `devenv shell` downloads a lot (the Rust toolchain,
    and Chromium for the UI screenshots), later starts take seconds.
  - **rustup:** https://rustup.rs, then `rustup default stable` (tested with Rust 1.100; older releases
    may fail), a C compiler and linker (`build-essential` / `gcc`) and `just`
    (`cargo install just`, or your distribution's package). No devenv: run the `just` commands below
    directly, or the `cargo` lines they print.

### Run it

```
git clone https://github.com/svin24/melissa.git melissa && cd melissa
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
  Claim:    open http://127.0.0.1:8080/claim and enter the setup code ABCDE-FGHJK
            (valid 1 hour; a new one: melissa-server setup-code)
```

Open **http://127.0.0.1:8080**, enter that setup code, a username and a password: you become the owner.
A short setup asks for a device name, a provider (one of the CLIs you signed in to) and a model. Forgot the
password: `melissa-server reset-owner` prints a one-hour link to set a new one. The CLI needs no password on
the same machine: from a second terminal in the same folder, `just chat` or `just ask "describe this machine"`.

### Things to try

1. **"Describe this machine."** The lead agent calls Melissa's `device_info` tool and tells you the OS, kernel,
   memory and disk space.
2. **"Create a helper named Ada and have her check this machine, then tell me what she found."** The lead agent
   creates a helper, gives her a job and reports back (20-30 s). Ada gets her own chat in the sidebar; or
   `cargo run -q -p melissa-server -- ask --agent Ada "what did you find?"`.
3. **"Set up a Minecraft server."** There is no fixed app list: a helper picks the image, ports and storage
   and asks you to approve what will run. Without k3s the answer is that k3s is not running on this machine.
   Nothing is installed. That is expected from source.

Agents run in a sandbox. They change the system only through Melissa's tools, see only their own file box,
and reach the internet only through Melissa's proxy (their AI service, and public web pages read-only).
Anything risky waits for your approval in the chat (or `cargo run -q -p melissa-server -- confirm list`).

### Apps and k3s

App installs need a k3s cluster. A source checkout looks for cluster credentials in
`/run/melissa-kube/` (written in the Melissa container) or at `kubeconfig = "..."` in
`~/.config/melissa/server.toml`, and app installs need a second, write-only identity
(`[apps] kubeconfig`) with Melissa's admission policies. Setting that up by hand is not documented for
strangers yet; use the container to try apps.

### Where it keeps things

- **Stop:** Ctrl-C in the terminal running `just demo`. Agents stop with it.
- **State** (audit log, the lead agent's and helpers' chats, the agent registry, file boxes, people and
  sign-ins): `~/.local/state/melissa/`. **Config** (optional): `~/.config/melissa/` (`server.toml`, and
  what Settings saves). **Downloads** (the voice model): `~/.local/share/melissa/`. Sockets and short-lived
  files: `$XDG_RUNTIME_DIR/melissa/`. Nothing is written into the repo except the build in `target/`.
- **Start over:** stop Melissa, then `rm -rf ~/.local/state/melissa` (forgets people, chats, helpers and the
  audit log). Your agent CLIs' own sign-ins are not touched.
- **Try it without touching your normal dirs:**
  `XDG_STATE_HOME=/tmp/m/state XDG_CONFIG_HOME=/tmp/m/config just demo`.

### Ubuntu and AppArmor

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

### Troubleshooting

- **An agent shows "not installed".** Its command is not on the `PATH` of the shell you started
  `just demo` from. Check with `which claude grok opencode`. The installers put them in `~/.local/bin`,
  `~/.grok/bin` and `~/.opencode/bin`; open a new terminal after installing.
- **"sandbox: bwrap not found".** Install bubblewrap (see [What you need](#what-you-need)) and restart.
- **"bubblewrap cannot create namespaces".** On Ubuntu see above. Elsewhere check
  `sysctl user.max_user_namespaces` (must be > 0) and, on older Debian,
  `sysctl kernel.unprivileged_userns_clone` (must be 1).
- **"cannot listen on 127.0.0.1:8080".** Something else uses the port, perhaps another Melissa (the
  container holds 127.0.0.1:8080). Stop it, or create `~/.config/melissa/server.toml` with
  `listen = "127.0.0.1:8090"` and open that port instead (`just chat`/`ask` read the same file).
- **The lead agent answers "not logged in" or every backend fails.** Run the CLI once by hand (`claude`,
  `grok`, `opencode`) and sign in, then send the message again. To pin one agent, put this in
  `~/.config/melissa/server.toml`:
  ```toml
  [lead]
  backends = ["claude-session"]   # or "grok-acp", "opencode-acp"
  ```
- **Something else.** Run with the full log: `RUST_LOG=info just demo`, and look at the audit log in
  `~/.local/state/melissa/audit.jsonl`.

### Uninstall (from source)

Stop it with Ctrl-C, then delete what it kept:

```
rm -rf ~/.local/state/melissa ~/.config/melissa ~/.local/share/melissa "$XDG_RUNTIME_DIR/melissa"
rm -rf ~/.local/share/melissa-agents   # only if you added a provider from the registry in Settings
rm -rf melissa                         # the clone, with target/ and .devenv/
nix-collect-garbage                    # optional: the dev shell's store paths
```

`$XDG_RUNTIME_DIR/melissa` is also cleared when you log out. Your agent CLIs (Claude Code, Grok, OpenCode)
and their sign-ins are your own and are not touched.

## UI screenshots (before/after)

For UI changes: shoot every screen before and after, then diff.

```
just ui-shots                 # this checkout -> target/ui-shots/<rev>[-dirty]/index.html
just ui-diff <before> <after> # two shot dirs or revisions, e.g. `just ui-diff master abc1234-dirty`
just ui-shots --rev master    # shoot another revision (temporary git worktree, built there)
just ui-shots --only settings # only screens whose name contains "settings"
```

- What it does (`scripts/ui-shots/`): builds `melissa-server`, starts it on a free `localhost`
  port with HOME and every XDG directory in a fresh temp dir (never `~/.config/melissa`, never
  the Melissa on port 8080), claims it as `owner`, seeds three helpers with icons and chats
  through `fake_agent.py`, a scripted ACP agent (steps, a permission card, a queued message,
  a model switch, models for the picker), then shoots each screen at 1440, 1024 and 390 px in
  dark and light. Panels that scroll get extra pages (`settings-agents~2`). About 3 minutes,
  about 310 PNGs (~35 MB). The temp dir is removed at the end.
- Same pixels every run: the server runs under libfaketime from a fixed date, in UTC;
  animations, transitions and the caret are off; Chromium rasterizes in software on one
  thread; fonts come from Nix. Host facts (host name, OS, kernel, uptime, memory) and "N min
  ago" are replaced in the page before each shot. Two runs of the same code diff to 0.
- `ui-diff` writes before | after | changed-pixels-in-red images, `summary.txt` and
  `index.html`. `--threshold <percent>` lists only shots above it, `--tolerance <0-255>` ignores
  small per-pixel differences, `--fail` exits 1 when something changed.
- Apps without k3s: the harness builds `melissa-server` with the `fake-apps` feature
  (`crates/web/src/fake_apps.rs`, never in normal builds) and points `MELISSA_FAKE_APPS` at a
  JSON file of `list_apps`/`app_status`/`app_logs` answers (`FAKE_APPS` in `server.py`). It
  invites three members (ada, ben with apps, cy without) and shoots Apps as the owner
  (`apps`, `apps-by-person`, `app-details`), as a member (`apps-member`, `app-details-member`,
  `apps-empty`) and without an app platform (`apps-not-set-up`, the file removed). Revisions
  from before the feature are built without it.
- Provider limits without sign-ins: the same build has the `fake-usage` feature
  (`crates/web/src/fake_usage.rs`); `MELISSA_FAKE_USAGE` points at `FAKE_USAGE` in `server.py` (windows that
  are plenty, low and nearly gone, an unavailable provider, one without limits, cost and tokens). Shots:
  `usage`, `usage-cost`, `usage-tokens` (owner) and `usage-member`. "Resets in …" counts down from the
  check, so it is replaced like "N min ago".
- Not covered: stopping an app (the confirmation needs a real `stop_app`), Settings → Reset's
  confirm (container install only), the collapsed sidebar at 390 px (no such state on a phone).
  They are listed as skipped in `index.html`.
- Offline: Chromium, Playwright and Pillow come from the dev shell (`devenv.nix`); the only
  network use is the browser talking to the throwaway server on localhost.
- Shots show this machine's UI only (target/ is gitignored); don't commit them.

## Design tokens (Penpot and the stylesheet)

The design library in the local Penpot is the source of the UI's values. Its token file is
committed as `crates/web/design/tokens.json` (sets `core`, `theme/light`, `theme/dark`); the
stylesheet's `:root` (`crates/web/assets/melissa.css`) is written by hand. A Penpot token
`a.b.c` is the CSS variable `--a-b-c`. `cargo nextest run -p melissa-web tokens::` fails when
the two disagree: a token on one side only, or a different value in the light or dark theme
(after resolving aliases; `0.25rem` = `4`, `150ms` = `150`, `rgb(…/0.45)` = `#…73`).

- **Changed in Penpot:** export the tokens (Tokens → Export, one file) over
  `crates/web/design/tokens.json`, run the test, and change `:root` until it passes. Carry the
  change into `scripts/tokens/tokens_gen.py` too, so the next `just tokens` keeps it.
- **Changed in code:** edit `scripts/tokens/tokens_gen.py` and `:root` together, then
  `just tokens` (writes `tokens.json` and runs the test). Import the new `tokens.json` into
  Penpot (Tokens → Import) so the design library follows.
- **Many tokens at once:** `just tokens css` prints the whole token block as CSS to paste.
- Names only the stylesheet has (the old aliases such as `--ink`, the Apps list's layout
  values) are listed in `crate::tokens::CSS_ONLY`; anything else in `:root` must be in
  `tokens.json`. Later `:root` blocks (themes, `pointer: coarse`) may only override tokens.

## Installing Nix (any Linux)

Use the Determinate installer (flakes on by default): https://determinate.systems/nix-installer/, or the
official one (https://nixos.org/download) with `experimental-features = nix-command flakes` in
`~/.config/nix/nix.conf`. Fedora Atomic (Kinoite, Silverblue, bootc) needs the extra steps further down.
The first `devenv shell` downloads the Rust toolchain plus Playwright's Chromium for the UI screenshots; later
starts are fast.

## Installing devenv and direnv

Once Nix works (below), either:
- `nix profile install nixpkgs#devenv nixpkgs#direnv nixpkgs#nix-direnv`, or
- add `devenv`, `direnv` and `nix-direnv` to a Home Manager config (preferred; keeps it declarative).

Hook direnv into your shell (`eval "$(direnv hook bash)"`) without letting Home Manager replace `~/.bashrc`
if you manage bash by hand.

## Nix on Fedora Kinoite 45 (immutable `/`)

Status: applied on the dev laptop (Nix store at `/var/home/nix`, bind-mounted at `/nix`; dev shell in use
since 2026-11-18). On someone else's machine: do not change `/etc/ostree` or reboot without its owner's yes.

Why it needs extra steps: Kinoite 42+ uses composefs with a read-only `/`, so installers cannot create
`/nix` (the Determinate installer's `nix-directory.service` fails on `chattr -i /`). The known fix is a
transient root overlay, which lets `/nix` exist as a mount point backed by persistent storage:

1. Enable the transient root (needs a reboot):
   ```
   sudo tee /etc/ostree/prepare-root.conf <<'EOT'
   [composefs]
   enabled = yes
   [root]
   transient = true
   EOT
   sudo rpm-ostree initramfs-etc --track=/etc/ostree/prepare-root.conf
   sudo systemctl reboot
   ```
2. After the reboot, install Nix with the Determinate installer's ostree planner, keeping the store in
   `/var/home/nix` (bind-mounted at `/nix`):
   ```
   curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix \
     | sh -s -- install ostree --persistence=/var/home/nix
   ```
   Flakes are enabled by default with this installer.
3. Verify: `nix --version`, `nix run nixpkgs#hello`.

Trade-offs: with `root.transient = true`, files written outside `/var` and `/etc` (for example directly in
`/usr` or `/opt`) are discarded on each reboot. Normal Kinoite use (flatpak, containers, rpm-ostree
layering, bootc images) is unaffected. Alternatives if this is unwanted: disable composefs via a kernel
argument (not recommended), or use a rootless approach such as nix-portable, or a toolbox/distrobox
container with rustup until Nix is ready.

Interim (no Nix): create a toolbox/distrobox container and install `rustup`, `cargo-nextest`, `just` there.
