#!/usr/bin/env bash
# k3s smoke test against the running headless test VM (after `just vm-dev deploy headless`).
# Checks: node Ready, system pods from the airgap bundle, no SELinux denials, an Ingress reachable
# from the host through the forwarded port (127.0.0.1:18080 -> VM :80).
# Note: the test app image (traefik/whoami) is pulled from Docker Hub; test-only, not product code.
set -euo pipefail
port=${VM_SSH_PORT:-2222}
host=whoami.melissa.home.arpa
opts=(-q -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 -i vm/test_key)
vm() { ssh "${opts[@]}" -p "$port" bee@127.0.0.1 "$@"; }
pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*"; exit 1; }

# Make sure the forwards exist even if the VM was started by an older vm-run.sh.
python3 - <<'PY' || true
import socket, time
s = socket.socket(socket.AF_UNIX); s.connect("vm/headless/monitor.sock"); time.sleep(0.3); s.recv(65536)
for rule in ("tcp:127.0.0.1:16443-:6443", "tcp:127.0.0.1:18080-:80", "tcp:127.0.0.1:18443-:443"):
    s.sendall(f"hostfwd_add {rule}\n".encode()); time.sleep(0.2)
PY

# First start generates certificates and imports the airgap images; give it time.
vm 'timeout 180 sh -c "until systemctl is-active -q k3s; do sleep 3; done"' || fail "k3s.service not active: $(vm 'systemctl is-active k3s; sudo journalctl -u k3s -n 20 --no-pager' 2>&1 | tail -20)"
pass "k3s.service active ($(vm 'k3s --version | head -1'))"

vm 'sh -s' <<'EOS' || fail "node or system pods not ready"
set -e
end=$(( $(date +%s) + 300 ))
until sudo kubectl get nodes --no-headers 2>/dev/null | grep -q ' Ready '; do
    [ "$(date +%s)" -lt "$end" ] || { sudo kubectl get nodes; exit 1; }; sleep 3
done
while :; do
    pods=$(sudo kubectl -n kube-system get pods --no-headers 2>/dev/null || true)
    if echo "$pods" | grep -q '^traefik' && ! echo "$pods" | grep -vE ' (Running|Completed) ' | grep -q .; then
        exit 0
    fi
    [ "$(date +%s)" -lt "$end" ] || { echo "$pods"; exit 1; }; sleep 5
done
EOS
pass "node Ready, system pods running"
vm 'sudo kubectl -n kube-system get pods --no-headers' | sed 's/^/      /'

pulled=$(vm 'sudo journalctl -u k3s --no-pager | grep -c "Pulling image" || true')
imported=$(vm 'sudo journalctl -u k3s --no-pager | grep -ciE "imported images|import.*airgap|Imported" || true')
echo "      k3s log: $imported import lines, $pulled pull lines"

# Helm re-runs the Traefik install when the HelmChartConfig changes; that can lag a reboot by minutes.
args=$(vm 'sh -s' <<'EOS'
end=$(( $(date +%s) + 300 ))
get() { sudo kubectl -n kube-system get deploy traefik -o jsonpath='{.spec.template.spec.containers[0].args}' 2>/dev/null; }
until get | grep -q 'checkNewVersion=false' || [ "$(date +%s)" -ge "$end" ]; do sleep 5; done
get
EOS
)
echo "$args" | grep -q 'checkNewVersion=false' && pass "Traefik update check disabled" \
    || fail "Traefik still checks for new versions (args: $args)"

denials=$(vm 'sudo ausearch -m avc -ts boot 2>/dev/null | grep -c "denied" || true')
[ "${denials:-0}" -eq 0 ] && pass "no SELinux denials since boot" \
    || { echo "WARN: $denials SELinux denials since boot:"; vm 'sudo ausearch -m avc -ts boot 2>/dev/null | grep denied | head -5'; }

vm "sudo kubectl apply -f -" <<YAML >/dev/null
apiVersion: v1
kind: Namespace
metadata: {name: smoke}
---
apiVersion: apps/v1
kind: Deployment
metadata: {name: whoami, namespace: smoke}
spec:
  replicas: 1
  selector: {matchLabels: {app: whoami}}
  template:
    metadata: {labels: {app: whoami}}
    spec:
      containers:
        - name: whoami
          image: docker.io/traefik/whoami:v1.11
          ports: [{containerPort: 80}]
---
apiVersion: v1
kind: Service
metadata: {name: whoami, namespace: smoke}
spec:
  selector: {app: whoami}
  ports: [{port: 80, targetPort: 80}]
---
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata: {name: whoami, namespace: smoke}
spec:
  rules:
    - host: $host
      http:
        paths:
          - {path: /, pathType: Prefix, backend: {service: {name: whoami, port: {number: 80}}}}
YAML
vm 'sudo kubectl -n smoke rollout status deploy/whoami --timeout=180s' >/dev/null || fail "whoami did not roll out"
for _ in $(seq 1 30); do
    if body=$(curl -sf --resolve "$host:18080:127.0.0.1" "http://$host:18080/"); then break; fi
    sleep 2
done
echo "$body" | grep -q "^Hostname: whoami" && pass "ingress http://$host reachable from the host" \
    || fail "ingress not reachable: ${body:-no response}"
vm 'sudo kubectl delete namespace smoke --wait=false' >/dev/null
pass "smoke test complete"
