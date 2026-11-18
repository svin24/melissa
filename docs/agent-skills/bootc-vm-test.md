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

## Gotchas
- bootc-image-builder reads root's container storage, so images built rootless must be copied with
  `just image-to-root` first (reference lab: verified for bib).
- Faster loop for content-only changes: `bootc switch` an already-booted VM to a new tag instead of rebuilding
  the disk (reference lab finding).
- Queen binds to loopback inside the VM; test it over SSH (`curl -s localhost:8080/healthz`).
- No TPM/Secure Boot in `vm-run.sh` yet (add swtpm + secboot firmware when sealed boot is on the table).
