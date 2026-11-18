#!/usr/bin/env bash
# Fast dev loop against a running headless (or desktop) test VM. Run from the repo root.
#
#   vm-dev.sh setup    [variant]  one-time per fresh VM: passwordless sudo for the test user,
#                                 trust the host's dev registry (10.0.2.2:5000, plain HTTP)
#   vm-dev.sh registry            start the dev registry on 127.0.0.1:5000 (rootless podman, data in vm/registry)
#   vm-dev.sh deploy   [variant]  push localhost/melissa-<variant>:dev to the registry and `bootc switch|upgrade
#                                 --apply` in the VM (only changed layers move), wait for the reboot
#   vm-dev.sh hotswap  [variant]  build just the Rust binaries (image build stage), copy them into the VM's
#                                 /usr via `bootc usr-overlay` (transient, gone after reboot), restart the queen
#   vm-dev.sh ssh      [variant] [cmd...]
#
# Test-VM only: these steps weaken the VM (passwordless sudo, insecure registry). Never run them on a real bee.
set -euo pipefail
cmd=${1:?usage: vm-dev.sh <setup|registry|deploy|hotswap|ssh> [variant]}
variant=${2:-headless}
port=${VM_SSH_PORT:-$([ "$variant" = desktop ] && echo 2223 || echo 2222)}
registry=127.0.0.1:5000
guest_registry=10.0.2.2:5000   # the host's loopback, seen from QEMU user networking
opts=(-q -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 -i vm/test_key)
vm() { ssh "${opts[@]}" -p "$port" bee@127.0.0.1 "$@"; }
wait_down_up() {
    timeout 120 bash -c "while ssh ${opts[*]} -p $port bee@127.0.0.1 true 2>/dev/null; do sleep 2; done" || true
    timeout 300 bash -c "until ssh ${opts[*]} -p $port bee@127.0.0.1 true 2>/dev/null; do sleep 3; done"
}

case $cmd in
setup)
    vm "sudo -S -p '' tee /etc/sudoers.d/90-melissa-test >/dev/null" < <(cat vm/test_password; echo; echo 'bee ALL=(ALL) NOPASSWD: ALL')
    vm 'sudo chmod 440 /etc/sudoers.d/90-melissa-test && sudo -n true && echo "passwordless sudo: ok"'
    vm "sudo tee /etc/containers/registries.conf.d/90-melissa-dev.conf >/dev/null" <<EOF
# Melissa test VM only: the host's dev registry over plain HTTP.
[[registry]]
location = "$guest_registry"
insecure = true
EOF
    echo "registry $guest_registry trusted in VM"
    ;;
registry)
    if podman container exists melissa-registry; then
        podman start melissa-registry >/dev/null
    else
        mkdir -p vm/registry
        # --network=host because rootless pasta is broken on the dev laptop; bind to loopback only.
        podman run -d --name melissa-registry --network=host \
            -e REGISTRY_HTTP_ADDR=127.0.0.1:5000 \
            -v "$PWD/vm/registry:/var/lib/registry:Z" \
            docker.io/library/registry:2 >/dev/null
    fi
    timeout 20 bash -c "until curl -sf http://$registry/v2/ >/dev/null; do sleep 1; done"
    echo "registry up on $registry"
    ;;
deploy)
    curl -sf "http://$registry/v2/" >/dev/null || "$0" registry
    podman push -q --tls-verify=false "localhost/melissa-$variant:dev" "$registry/melissa-$variant:dev"
    target="$guest_registry/melissa-$variant:dev"
    booted=$(vm "sudo bootc status --format=json" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(((d["status"]["booted"] or {}).get("image") or {}).get("image",{}).get("image",""))')
    want=$(podman run --rm --network=host "localhost/melissa-$variant:dev" cat /usr/lib/melissa/release | tr '\n' ' ')
    if [ "$booted" = "$target" ]; then
        out=$(vm "sudo bootc upgrade --apply" 2>&1) || true
    else
        out=$(vm "sudo bootc switch --apply $target" 2>&1) || true
    fi
    echo "$out" | grep -E 'layers|No changes|Queued|error' || true
    if echo "$out" | grep -q 'No changes'; then echo "VM already up to date"; exit 0; fi
    wait_down_up
    got=$(vm cat /usr/lib/melissa/release | tr '\n' ' ')
    [ "$got" = "$want" ] && echo "deployed: $got" || { echo "FAIL: VM runs '$got', want '$want'"; exit 1; }
    ;;
hotswap)
    podman build -q --network=host --target build -f image/Containerfile -t localhost/melissa-build:dev . >/dev/null
    tmp=$(mktemp -d)
    cid=$(podman create localhost/melissa-build:dev)
    podman cp "$cid:/out/." "$tmp/"
    podman rm -f "$cid" >/dev/null
    scp "${opts[@]}" -P "$port" "$tmp"/melissa-* bee@127.0.0.1:/tmp/
    rm -rf "$tmp"
    vm 'sudo bootc usr-overlay 2>/dev/null || true; sudo install -m 0755 /tmp/melissa-queen /tmp/melissa-bee /usr/bin/ \
        && rm -f /tmp/melissa-queen /tmp/melissa-bee \
        && (sudo systemctl try-restart melissa-queen.service; systemctl is-active melissa-queen.service || true)'
    echo "hot-swapped binaries (transient until reboot)"
    ;;
ssh)
    shift 2 || shift $#
    vm "$@"
    ;;
*)
    echo "unknown command: $cmd" >&2; exit 2 ;;
esac
