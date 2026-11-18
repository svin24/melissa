# Building the Melissa bootc images

**When:** changing anything under `image/`, adding image packages, or preparing a laptop switch.

## Layout
- `image/Containerfile`: one file, `--build-arg VARIANT=desktop|headless` and `BASE=...`.
  Stage 1 builds `melissa-queen`/`melissa-bee` on `fedora:45` (dnf `cargo rust gcc`); stage 2 is the OS.
- `image/packages-{common,desktop,headless}.txt`: extra RPMs (owner wants extras in the image, not layered).
- `image/rootfs/` (both) and `image/<variant>/` (variant only) are copied over `/`.
- Bases (verified `containers.bootc=1` with `skopeo inspect`, 2026-11-18):
  desktop `quay.io/fedora-ostree-desktops/kinoite:45`, headless `quay.io/fedora/fedora-bootc:45`.

## Commands
```
devenv shell -- just image-build headless     # or desktop; rootless, tags localhost/melissa-<variant>:dev
devenv shell -- just image-lint headless
devenv shell -- just image-to-root headless   # needed before bootc-image-builder / bootc switch (root storage)
```

## Layer order (reference-lab finding, unverified here)
Packages first (big, rare), static files next, binaries + `/usr/lib/melissa/release` last. An update that only
changes the last layer downloads kilobytes. `RUN bootc container lint` is the final step.

## Gotchas
- **pasta broken on the dev laptop** (verified 2026-11-18): rootless `podman run/build` fails with
  `pasta failed ... Failed to remount /: Permission denied`; audit log shows an SELinux AVC (`pasta_t` search on
  `fusefs_t`). `--network=host` works and is used by `just image-build`.
- Defaults belong in `/usr/lib/...`, not `/etc`: `/etc` is 3-way merged, locally edited files win forever.
- `/var` is shared across deployments; rollback does not undo `/var` state.
- The laptop currently has host-side changes (verified with `rpm-ostree status`): layered `tailscale` and
  `InitramfsEtc: /etc/ostree/prepare-root.conf` (transient root for Nix). `bootc switch` to a container image may
  drop these (unverified), so the desktop image ships Tailscale and `/usr/lib/ostree/prepare-root.conf` with
  `root.transient = true`, and rebuilds the initramfs with dracut. Verify `/nix` still mounts after a switch.
- Changing `prepare-root.conf` needs an initramfs rebuild inside the image (`dracut --force --no-hostonly`).
- The stock `bootc-fetch-apply-updates.timer` downloads and reboots on its own; Melissa images mask it.
- Desktop queen = user unit (`systemctl --global enable`), so it sees the user's `claude`/`grok`/`opencode`
  logins. Headless queen = system unit with `DynamicUser`, `XDG_CONFIG_HOME=/etc`, `XDG_STATE_HOME=/var/lib`.

## Verified results (2026-11-18)
- Headless image 2.13 GB, desktop 7.8 GB (rootless build). `bootc container lint`: 14 passed, 0 warnings after
  removing `/run/dnf` in the package step (lint warns `nonempty-run-tmp` otherwise).
- In both: `/usr/lib/melissa/release`, tailscale 1.104.7 from pkgs.tailscale.com, `melissa-queen` runs,
  `bootc-fetch-apply-updates.timer` masked.
- Desktop: `lsinitrd` shows `usr/lib/ostree/prepare-root.conf` with `root.transient = true` in the rebuilt
  initramfs. dracut prints `dracut-install: ERROR: installing '/root'` but exits 0; the regenerated initramfs is
  247 MB vs 115 MB stock (open: match Fedora's dracut options to shrink it).
- Inspect an image: `podman run --rm --network=host localhost/melissa-<v>:dev bash -c '...'`;
  initramfs content: `lsinitrd /usr/lib/modules/<kver>/initramfs.img <path>`.

## Secure Boot guard (Containerfile, before lint)
- One kernel dir under `/usr/lib/modules`; `kernel-core` signed by the Fedora release key; no `.ko` files in
  `extra/` or `updates/`; no `akmod-*`/`kmod-<driver>` packages.
- RPM signatures in these images are in `%{RSAHEADER:pgpsig}` (`%{SIGPGP}` is empty). Fedora keys are not imported
  into the image's rpmdb (only Tailscale's is); compare against `/usr/share/pki/rpm-gpg/RPM-GPG-KEY-fedora-<N>-primary`
  with `gpg --show-keys`. `/etc/pki/rpm-gpg` is empty in fedora-bootc.
- Don't match `rpm -qa 'kmod-*'`: it hits Fedora's own `kmod-libs` (false positive seen 2026-11-18).
- This checks package metadata, not the PE signature Secure Boot verifies; the real test is a VM boot with
  Secure Boot on (`scripts/vm-run.sh` default).

## Moving the laptop onto the desktop image (owner's explicit yes required; deferred, see PLAN)
The laptop has host-side rpm-ostree changes (layered `tailscale`, `initramfs-etc`). `bootc switch` is expected to
refuse local rpm-ostree modifications (unverified), so use rpm-ostree to drop them and rebase in one go:
```
devenv shell -- just image-build desktop
podman save -o /var/tmp/melissa-desktop.tar localhost/melissa-desktop:dev
sudo podman load -i /var/tmp/melissa-desktop.tar && rm /var/tmp/melissa-desktop.tar
rpm-ostree status
sudo rpm-ostree rebase --uninstall=tailscale ostree-unverified-image:containers-storage:localhost/melissa-desktop:dev
sudo rpm-ostree initramfs-etc --untrack=/etc/ostree/prepare-root.conf   # image ships it in /usr/lib now
systemctl reboot
# verify: /usr/lib/melissa/release, mokutil --sb-state, findmnt /nix, tailscale status, sudo bootc status
# rollback: sudo rpm-ostree rollback && systemctl reboot (or pick the old GRUB entry)
```
If `--uninstall` is refused: `sudo rpm-ostree reset`, then rebase without it. Later updates: `bootc switch
--transport containers-storage localhost/melissa-desktop:dev`.
