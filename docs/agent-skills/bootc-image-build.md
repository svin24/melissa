# Building the Melissa bootc images

**When:** changing anything under `image/`, adding image packages, or preparing a laptop switch.

## Layout
- `image/Containerfile`: one file, `--build-arg VARIANT=desktop|headless` and `BASE=...`.
  Stage 1 builds `melissa-queen`/`melissa-comb` and `melissa-netbridge` (the agent sandboxes' network bridge, a
  binary of `melissa-agent`; must sit next to `melissa-queen`) on `fedora:45` (dnf `cargo rust gcc`); stage 2 is
  the OS. Verified 2026-11-18: headless image builds, lint passes, `/usr/bin/melissa-netbridge` links Fedora libc.
  Stage 1 also writes the crate license report (`/out/licenses/`, from `cargo about generate`); binaries are in
  `/out/bin/`. cargo-about is `cargo install`ed there (needs `--features cli`, about 1m40s, cached as its own layer;
  keep `CARGO_ABOUT_VERSION` equal to the dev shell's `cargo about --version`).
- Licenses in the image: `/usr/share/licenses/melissa/` (report, ACKNOWLEDGMENTS.md, LICENSE once it exists) and
  `/usr/share/licenses/k3s/LICENSE`. See ACKNOWLEDGMENTS.md.
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
- Both images run the same server (since 2026-11-18): system `melissa-queen.service` as the fixed system user
  `melissa` (DynamicUser until 2026-11-18; `melissa-state-migrate.service` moves old state), `XDG_CONFIG_HOME=/etc`,
  `XDG_STATE_HOME=/var/lib`, `HOME=/var/lib/melissa/home` (agent CLIs + logins, `sudo melissa-setup agent <name>`),
  UI on 127.0.0.1:8080, plus `k3s.service` and `melissa-kubeconfig.service`. There is no per-user queen unit any
  more (it would fight the system queen for port 8080). Headless-only: `melissa-firstboot` (in `image/headless/`).
- Desktop-only: `/usr/lib/firewalld/zones/melissa-k3s.xml` (Kinoite runs firewalld; the zone accepts k3s's pod
  10.42.0.0/16 and service 10.43.0.0/16 sources so pods reach CoreDNS/API; nothing opens to the LAN), and
  `/usr/lib/firewalld/services/melissa-apps.xml` (apps' ports 30000-30079/tcp; FedoraWorkstation already
  accepts 1025-65535, so it is only needed in stricter zones: `firewall-cmd --add-service=melissa-apps`).
- **Plasma Setup** (Kinoite's first-run wizard, `plasma-setup.service`) runs before the login screen unless
  `/etc/plasma-setup-done` exists; it does not look at existing users. A bib disk with a user (the test VM) shows it;
  `sudo touch /etc/plasma-setup-done` to get the login screen. A laptop already set up keeps its `/etc` flag.

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

## Desktop image verified in the VM (2026-11-18, image c5631dc, 6 GiB RAM, Secure Boot on)
Simulated the laptop: VM on the previous desktop image with `rpm-ostree initramfs-etc --track=/etc/ostree/prepare-root.conf`
(a layered package could not be simulated: the RPM tried was already in the base). Then exactly the owner's steps,
with the registry transport instead of containers-storage: `sudo rpm-ostree rebase
ostree-unverified-registry:10.0.2.2:5000/melissa-desktop:dev` + `sudo rpm-ostree initramfs-etc --untrack=...` (both
stack on the staged deployment), reboot. Results:
- Secure Boot enabled; SELinux enforcing; transient root active (`upperdir=/run/ostree/.private/root/upper`) from
  the image's own initramfs (InitramfsEtc gone).
- `melissa-queen` active as `melissa` (`NRestarts=0`), `/healthz` ok; `k3s` active, node Ready ~1 min after boot,
  all kube-system pods Running (coredns, traefik, svclb, metrics-server) with firewalld active (`melissa-k3s` zone),
  ingress on :80 answers 404 for an unknown host; `/run/melissa-kube/{queen,apps}.kubeconfig` root:melissa-kube 0640.
- `tailscaled`, `sshd`, `plasmalogin` active; `melissa-firstboot` not present, no stamp; `sudo melissa-setup` and
  plain `melissa-setup` work (desktop line "On this machine: open http://127.0.0.1:8080").
- AVC denials: 9, all stock Fedora (`tuned`/`tuned-ppd`/`fedora-third-party` writing `__pycache__` on the transient
  root, `tuned` chcon); none from k3s or Melissa.
- `rpm-ostree rollback` + reboot: old image (InitramfsEtc tracked again), Secure Boot + transient root kept; rollback
  again + reboot: new image, k3s node Ready, queen ok, login greeter on screen.
- Only failed unit: `mcelog.service` (no MCE in a VM; stock).

## Moving the laptop onto the desktop image (owner runs it; explicit yes)
`scripts/laptop-switch.sh` prints every command and asks before running it; nothing reboots on its own.
```
devenv shell -- just image-build desktop          # rootless, ~7.8 GB; lint must pass
scripts/laptop-switch.sh preflight                # read-only (sudo only to look at root's storage)
scripts/laptop-switch.sh switch                   # does the three steps below, each after a "y"
#   podman save localhost/melissa-desktop:dev | sudo podman load      (or: just image-to-root desktop)
#   sudo rpm-ostree rebase --uninstall=tailscale ostree-unverified-image:containers-storage:localhost/melissa-desktop:dev
#   sudo rpm-ostree initramfs-etc --untrack=/etc/ostree/prepare-root.conf
systemctl reboot
scripts/laptop-switch.sh verify                   # release, Secure Boot, transient root, /nix, nix, tailscale,
                                                  # queen/k3s/kubeconfig, healthz, node, AVCs
```
- Why rpm-ostree, not `bootc switch`: bootc refuses deployments with local rpm-ostree changes (layered `tailscale`,
  `initramfs-etc`). Untrack after the rebase, not before: if the rebase failed, an untracked stock Kinoite would boot
  without transient root and `/nix` would not mount. If `--uninstall` is refused: `sudo rpm-ostree reset`, then rebase.
- `/var` is kept: Tailscale state (`/var/lib/tailscale`), `/var/home` (KDE, flatpaks, Nix store via `/nix` →
  `/var/home/nix`, agent CLI logins in the owner's home) all stay. The `/nix` mount units live in `/etc`, which is
  3-way merged and kept.
- Agent logins: the system queen uses only the `melissa` user's home: `sudo melissa-setup agent claude|grok|opencode`.
  The owner's own CLI logins in their home are untouched and keep working for their own use (and for `just demo`).
- Port 8080 belongs to the system queen now: for `just demo` put `listen = "127.0.0.1:8090"` in
  `~/.config/melissa/queen.toml` (the queen already fails with "cannot listen on 127.0.0.1:8080 ..." otherwise).
- **Rollback:** `scripts/laptop-switch.sh rollback` (= `sudo rpm-ostree rollback`) then `systemctl reboot`, or pick
  the older entry in the GRUB menu. `/var` changes (k3s data in `/var/lib/rancher`, `/var/lib/melissa`) stay.
- Later updates: `just image-build desktop`, copy to root storage again, `sudo rpm-ostree upgrade` (or `bootc
  upgrade` once no local rpm-ostree changes remain), reboot.
- Laptop risk (unverified): `k3s-prepare` pins the node IP to the default route's IPv4 when k3s starts. After a
  network change k3s keeps the old IP until it restarts (`sudo systemctl restart k3s` re-pins); local apps on the
  laptop are not affected in the meantime as far as known. k3s also adds RAM/CPU load on the laptop (not measured).
