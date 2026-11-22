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

Just want to try Melissa? See [QUICKSTART.md](QUICKSTART.md) (also covers a rustup setup without Nix).

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

## Installing Nix (any Linux)

Use the Determinate installer (flakes on by default): https://determinate.systems/nix-installer/, or the
official one (https://nixos.org/download) with `experimental-features = nix-command flakes` in
`~/.config/nix/nix.conf`. Fedora Atomic (Kinoite, Silverblue, bootc) needs the extra steps further down.
The first `devenv shell` downloads the Rust toolchain plus QEMU/OVMF for the VM tooling; later starts are fast.

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
