#!/usr/bin/env bash
# Pack the qcow2 that `just release-disk` built (dist/<variant>/qcow2/disk.qcow2) for download:
# a compressed qcow2 named after the version, plus its sha256 checksum file. Rootless.
# Run inside the dev shell (qemu-img comes from Nix).
set -euo pipefail
variant=${1:-headless}
src=dist/$variant/qcow2/disk.qcow2
[ -f "$src" ] || { echo "missing $src; run: just release-disk $variant (needs sudo)" >&2; exit 1; }
version=$(podman run --rm --network=host "localhost/melissa-$variant:dev" \
    sed -n 's/^MELISSA_VERSION=//p' /usr/lib/melissa/release)
name=melissa-$variant-$version-x86_64.qcow2
out=dist/$name
start=$(date +%s)
# -c: zlib-compressed clusters; the guest reads them fine and writes uncompressed.
qemu-img convert -p -c -O qcow2 "$src" "$out"
(cd dist && sha256sum "$name" > "$name.sha256")
qemu-img check -q "$out"
echo "packed in $(( $(date +%s) - start )) s:"
ls -lh "$out" "$out.sha256"
qemu-img info "$out" | grep -E 'virtual size|disk size'
cat "$out.sha256"
