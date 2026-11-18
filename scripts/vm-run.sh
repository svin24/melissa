#!/usr/bin/env bash
# Boot vm/<variant>/qcow2/disk.qcow2 in QEMU with UEFI + KVM, using a copy-on-write overlay.
# Run inside the dev shell (QEMU comes from Nix). SSH: ssh -p <port> -i vm/test_key bee@127.0.0.1
# Env: VM_DISPLAY=gtk to show a window (desktop variant), VM_MEM (MiB), VM_SSH_PORT, VM_FRESH=1,
#      VM_SECUREBOOT=0 to boot without Secure Boot/TPM (default: Secure Boot on with MS keys + swtpm TPM 2.0).
set -euo pipefail
variant=${1:?usage: vm-run.sh <desktop|headless>}
dir=vm/$variant
base=$dir/qcow2/disk.qcow2
[ -f "$base" ] || { echo "missing $base; run: just vm-disk $variant" >&2; exit 1; }
share=$(dirname "$(dirname "$(command -v qemu-system-x86_64)")")/share/qemu
overlay=$dir/overlay.qcow2
secureboot=${VM_SECUREBOOT:-1}
if [ "$secureboot" = 1 ]; then
    fw=${MELISSA_OVMF_DIR:?MELISSA_OVMF_DIR not set; run inside the dev shell}
    code=$fw/OVMF_CODE.ms.fd vars_template=$fw/OVMF_VARS.ms.fd
else
    code=$share/edk2-x86_64-code.fd vars_template=$share/edk2-i386-vars.fd
fi
if [ "${VM_FRESH:-0}" = 1 ] || [ ! -f "$overlay" ]; then
    rm -rf "$overlay" "$dir/efivars.fd" "$dir/tpm"
    qemu-img create -q -f qcow2 -b "$(realpath "$base")" -F qcow2 "$overlay"
    cp "$vars_template" "$dir/efivars.fd" && chmod 644 "$dir/efivars.fd"
fi
extra=()
if [ "$secureboot" = 1 ]; then
    mkdir -p "$dir/tpm"
    swtpm socket --tpm2 --tpmstate dir="$dir/tpm" --ctrl type=unixio,path="$dir/tpm/sock" --daemon
    extra+=(-machine q35,smm=on -global driver=cfi.pflash01,property=secure,value=on
            -chardev socket,id=tpm,path="$dir/tpm/sock" -tpmdev emulator,id=tpm0,chardev=tpm
            -device tpm-tis,tpmdev=tpm0)
else
    extra+=(-machine q35)
fi
port=${VM_SSH_PORT:-$([ "$variant" = desktop ] && echo 2223 || echo 2222)}
# Headless: also forward the k3s API and the ingress (HTTP/HTTPS) to host loopback ports.
fwd=""
if [ "$variant" = headless ]; then
    fwd=",hostfwd=tcp:127.0.0.1:16443-:6443,hostfwd=tcp:127.0.0.1:18080-:80,hostfwd=tcp:127.0.0.1:18443-:443"
fi
display=${VM_DISPLAY:-none}
mem=${VM_MEM:-$([ "$variant" = desktop ] && echo 6144 || echo 3072)}
exec qemu-system-x86_64 \
    -name "melissa-$variant" -enable-kvm -cpu host -smp 4 -m "$mem" "${extra[@]}" \
    -drive if=pflash,format=raw,readonly=on,file="$code" \
    -drive if=pflash,format=raw,file="$dir/efivars.fd" \
    -drive file="$overlay",if=virtio,format=qcow2 \
    -nic user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:"$port"-:22"$fwd" \
    -device virtio-vga -display "$display" \
    -serial file:"$dir/serial.log" \
    -monitor unix:"$dir/monitor.sock",server,nowait \
    -pidfile "$dir/qemu.pid"
