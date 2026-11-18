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

Improvement (todo): `vm-update-test.sh` copies the whole image (7.8 GB for desktop) even when one layer changed.
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
