# Testing images in QEMU VMs

**When:** before any image goes onto real hardware; for multi-bee tests later.

## Steps
```
devenv shell -- just image-build headless
devenv shell -- just image-to-root headless   # sudo
devenv shell -- just vm-disk headless         # sudo; bootc-image-builder -> vm/headless/qcow2/disk.qcow2
devenv shell -- just vm-run headless          # QEMU, KVM, UEFI, overlay disk, serial log in vm/headless/
ssh -p 2222 -i vm/test_key bee@127.0.0.1      # desktop variant: port 2223, VM_DISPLAY=gtk for a window
```
- `scripts/vm-prep.sh` makes a throwaway SSH key, a random password and a bib `config.toml` with user `bee`
  under `vm/` (gitignored). Never ship these.
- `VM_FRESH=1` recreates the overlay (clean first boot). Stop via the monitor socket:
  `echo system_powerdown | socat - unix-connect:vm/<variant>/monitor.sock` or kill `vm/<variant>/qemu.pid`.
- QEMU comes from the dev shell (nixpkgs `qemu_kvm` 11.1, ships edk2 firmware under `share/qemu`).

## Update + rollback test
`devenv shell -- just vm-update-test headless` with the VM running: copies the current local image into the VM
(`podman save` → scp → `sudo podman load`), `bootc switch --transport containers-storage`, reboots, checks
`/usr/lib/melissa/release` and Secure Boot, then `bootc rollback`, reboots, checks the old release is back.

## Fast dev loop (use this day to day) — verified 2026-11-18
```
devenv shell -- just vm-run headless &        # or run it in the background
just vm-dev setup headless                    # once per fresh VM: passwordless sudo + trust 10.0.2.2:5000
just vm-dev registry                          # rootless registry:2 on 127.0.0.1:5000 (data in vm/registry)
just image-build headless && just vm-dev deploy headless   # push + bootc switch/upgrade --apply + reboot + check
just vm-dev hotswap headless                  # Rust-only change: build stage -> bootc usr-overlay -> restart queen
just vm-dev ssh headless 'journalctl -u melissa-queen -n 20'
```
- deploy moved 5 layers / 1.7 MB and took 67 s including reboot (vs 6.5 min copying a full image).
- hotswap takes seconds; `/usr` becomes a transient overlay until the next reboot (`bootc usr-overlay`).
- QEMU user networking: the guest reaches the host's loopback as `10.0.2.2`, so the registry stays on 127.0.0.1.
- `setup` makes the test VM weaker (NOPASSWD sudo, insecure registry). Never on a real bee.

Older approach: `vm-update-test.sh` copies the whole image (7.8 GB for desktop) even when one layer changed.
Run a local registry (`registry:2` bound to 127.0.0.1:5000, reachable from the VM as `10.0.2.2:5000` under QEMU
user networking, marked insecure in a `registries.conf.d` drop-in in the test VM only) and `bootc switch` to it,
so only changed layers move, like real updates.

## Verified (2026-11-18)
- Headless VM boots with Secure Boot **enabled** (nixpkgs `OVMF.override { secureBoot = true; msVarsTemplate = true; }`,
  `OVMF_CODE.ms.fd`/`OVMF_VARS.ms.fd`, `-machine q35,smm=on`, `-global driver=cfi.pflash01,property=secure,value=on`),
  swtpm TPM 2.0 at `/dev/tpm0`, SSH ~30 s after start, ~430 MB RAM idle, queen + tailscaled active.
- Desktop VM (6 GiB RAM): Secure Boot enabled, transient root active (`upperdir=/run/ostree/.private/root/upper`),
  Plasma 6.8.3, ~1.9 GB used after boot. `vm-update-test desktop` passed: switch + reboot + rollback + reboot, Secure
  Boot and transient root kept; 6.5 min total, mostly copying the 7.8 GB image into the VM.
- Check results by reading the VM state, not a truncated log: `bootc status --format=json` (booted/rollback/staged).
- bib default disk is 10 GiB (8.6 GiB root); too small for the 7.8 GB desktop image, so `vm-prep.sh` sets
  `[[customizations.filesystem]] mountpoint="/" minsize="40 GiB"`.
- `bootc status` needs root even for reading.

## Seeing the screen (desktop VM)
`scripts/vm-screenshot.py desktop` → `vm/desktop/screen.png` via the QEMU monitor `screendump` (stdlib only), then
read the PNG. Verified 2026-11-18: desktop image boots to Plasma's first-run "Welcome to Plasma Desktop" setup with
Secure Boot on (kernel logs `Lockdown: ... hibernation is restricted`).

## Gotchas
- Kinoite ships `sshd` **disabled**; fedora-bootc enables it. The images enable `sshd.service` and ship
  `/etc/ssh/sshd_config.d/40-melissa.conf` (keys only, no root login).
- bootc-image-builder reads root's container storage, so images built rootless must be copied with
  `just image-to-root` first (reference lab: verified for bib).
- Faster loop for content-only changes: `bootc switch` an already-booted VM to a new tag instead of rebuilding
  the disk (reference lab finding).
- Queen binds to loopback inside the VM; test it over SSH (`curl -s localhost:8080/healthz`).
- No TPM/Secure Boot in `vm-run.sh` yet (add swtpm + secboot firmware when sealed boot is on the table).

## Published tester image (pre-alpha demo) — 2026-11-18
Tester guide: [../TRY-THE-VM.md](../TRY-THE-VM.md). Pieces in the headless image:
- **No account in the image.** `image/release/config.toml` (bib) has only the 40 GiB root size. Never add users,
  passwords or keys there; testers create theirs on first boot.
- **`melissa-firstboot.service`** (headless, `/usr/libexec/melissa/melissa-firstboot`): on tty1, ordered before
  `getty-pre.target`, `getty@tty1` **and `kmsconvt@tty1`**: asks user name, password (≥ 8), optional SSH key
  (validated with `ssh-keygen -l`), else offers SSH password login (`/etc/ssh/sshd_config.d/30-melissa-password-login.conf`,
  read before `40-melissa.conf`; first value wins). User goes in `wheel`. Stamp `/var/lib/melissa-firstboot.done`;
  exits at once if a human user (UID 1000-59999, login shell) exists, e.g. from a bib config.
- **Welcome texts:** `/etc/issue.d/50-melissa.issue` (console, before login), `/usr/lib/motd.d/50-melissa` (SSH:
  only `/etc/pam.d/sshd` has `pam_motd` in fedora-bootc), `melissa-setup` (status, address, next steps).
- **`melissa-setup agent <claude|grok|opencode>`**: vendor installer (claude.ai/install.sh, x.ai/cli/install.sh,
  opencode.ai/install) downloaded with curl as user `melissa`, run with `runuser` + clean env, `HOME=/var/lib/melissa/home`;
  then `claude auth login`, `grok login --device-auth`, or OpenCode free models (optional `opencode auth login`);
  then restarts the queen (it detects CLIs at start). `melissa-setup login <name>` repeats the login.
- **Build:** `just image-build headless` → `just image-to-root headless` (sudo) → `just release-disk headless`
  (sudo, bib → `dist/headless/qcow2/disk.qcow2`) → `just release-pack headless` (rootless: `qemu-img convert -c`,
  `dist/melissa-headless-<version>-x86_64.qcow2` + `.sha256`). `dist/` is gitignored.

### Verified in the dev VM (2026-11-18, deploy of this image onto the existing test VM, 3 GiB RAM)
- Old DynamicUser state migrated by `melissa-state-migrate.service`; queen `active`, `NRestarts=0`, 0 AVC denials.
- First boot for real (stamp removed + `MELISSA_FIRSTBOOT_FORCE=1` drop-in, reboot): prompt on tty1, console login
  held back; answers typed through the QEMU monitor (`sendkey`) → user in `wheel`, stamp written, login prompt,
  console login with the new password, `melissa-setup` output correct.
- `sudo melissa-setup agent opencode --yes`: 33 s, OpenCode 1.19.6; the Queen (opencode-acp, sandboxed, system
  service) answered a bee_info question in 13 s, created two bees, and installed Nextcloud (cached image, kept data)
  in 32 s by chat. Web UI through `ssh -L 18088:127.0.0.1:8080`: `/` 200, `/healthz` ok. Nextcloud through
  `ssh -L 18081:127.0.0.1:80` + Host `nextcloud.melissa.home.arpa:18081`: login page 200, OCS as admin 200.
- Not verified: the bib build of `release-disk` and a boot of that exact disk (needs sudo); `claude`/`grok` logins
  (need accounts); virt-manager/Boxes; the tester's QEMU command (the dev VM uses `scripts/vm-run.sh` with Secure Boot).

### Gotchas
- **kmscon owns tty1 on Fedora 45** (`kmsconvt@tty1`, the `autovt@` alias), not agetty, and it is not ordered after
  `getty-pre.target`. Without `Before=kmsconvt@tty1.service` it drew over the prompt and its `TTYVHangup` killed the
  script (`status=1/HUP`). Starting the unit by hand on a running system races logind's autovt: test with a reboot.
- **agetty's `\4`** in issue files picked k3s's `cni0` (10.42.0.1) once k3s was up. Don't use it; `melissa-setup`
  prints the default-route source address.
- `read -p` prompts go to stderr: the unit needs `StandardError=inherit` (tty), not `journal`.
- Kernel/audit messages print over console prompts; the script runs `dmesg --console-off` while prompting.
- Switching a unit from `DynamicUser=yes` to `User=` makes systemd migrate `/var/lib/private/<dir>`, but SELinux
  denies PID 1 (`init_t`) unlinking the `var_lib_t` symlink → `status=238/STATE_DIRECTORY`. Do it in a root oneshot.
- Testing console input: `sendkey` per character over `vm/<variant>/monitor.sock` (QEMU monitor), then
  `scripts/vm-screenshot.py`.
