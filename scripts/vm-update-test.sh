#!/usr/bin/env bash
# Update + rollback test against a running test VM (see scripts/vm-run.sh):
#   1. copy localhost/melissa-<variant>:dev into the VM and `bootc switch` to it, reboot, check the release file
#   2. `bootc rollback`, reboot, check the previous release is back
# Uses the throwaway test credentials in vm/. Prints PASS/FAIL per step.
set -euo pipefail
variant=${1:?usage: vm-update-test.sh <desktop|headless>}
port=${VM_SSH_PORT:-$([ "$variant" = desktop ] && echo 2223 || echo 2222)}
opts=(-q -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 -i vm/test_key)
vm() { ssh "${opts[@]}" -p "$port" bee@127.0.0.1 "$@"; }
vm_sudo() { vm "sudo -S -p '' $*" < vm/test_password; }
release() { vm cat /usr/lib/melissa/release | tr '\n' ' '; }
reboot_and_wait() {
    vm_sudo systemctl reboot || true
    timeout 120 bash -c "while ssh ${opts[*]} -p $port bee@127.0.0.1 true 2>/dev/null; do sleep 2; done"
    timeout 300 bash -c "until ssh ${opts[*]} -p $port bee@127.0.0.1 true 2>/dev/null; do sleep 3; done"
}
check() { if [ "$1" = "$2" ]; then echo "PASS: $3"; else echo "FAIL: $3 (got '$1', want '$2')"; exit 1; fi; }

before=$(release)
echo "booted: $before"
tar=/var/tmp/melissa-$variant-update.tar
rm -f "$tar"
podman save -o "$tar" "localhost/melissa-$variant:dev"
scp "${opts[@]}" -P "$port" "$tar" bee@127.0.0.1:/var/tmp/update.tar
rm -f "$tar"
vm_sudo podman load -q -i /var/tmp/update.tar
vm_sudo rm -f /var/tmp/update.tar
want=$(podman run --rm --network=host "localhost/melissa-$variant:dev" cat /usr/lib/melissa/release | tr '\n' ' ')

vm_sudo bootc switch --quiet --transport containers-storage "localhost/melissa-$variant:dev"
reboot_and_wait
check "$(release)" "$want" "switched to new image"
vm "mokutil --sb-state 2>/dev/null | grep -q 'SecureBoot enabled'" && echo "PASS: Secure Boot still enabled" \
    || echo "WARN: Secure Boot not enabled (VM_SECUREBOOT=0?)"

vm_sudo bootc rollback
reboot_and_wait
check "$(release)" "$before" "rolled back to previous image"
vm_sudo bootc status | sed -n '1,25p'
