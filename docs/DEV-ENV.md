# Dev environment: Nix + devenv + direnv

The repo ships a `devenv.nix` / `devenv.yaml` (primary), a `flake.nix` (alternative entry point),
and an `.envrc`. The shell provides Rust (stable via rust-overlay: rustc, cargo, clippy, rustfmt,
rust-analyzer, rust-src), cargo-nextest, cargo-watch, just, pkg-config, and pre-commit hooks
(rustfmt + clippy). Container tooling (podman) comes from the host, not Nix; `just container-*` uses it.

```
direnv allow          # auto-loads the shell via .envrc (`use devenv`)
devenv shell          # or enter it manually
nix develop --no-pure-eval   # flake alternative
just check            # fmt + clippy + tests
```

Just want to try Melissa? See [QUICKSTART.md](QUICKSTART.md) (also covers a rustup setup without Nix).

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
