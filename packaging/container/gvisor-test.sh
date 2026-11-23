#!/usr/bin/env bash
# gVisor end to end inside a built Melissa image (docs/DECISIONS.md 2026-11-23), the way the installed
# container runs: rootful podman, --privileged, systemd as PID 1, /var on a volume, k3s inside.
#
#   packaging/container/gvisor-test.sh [IMAGE]      (default localhost/melissa:dev; `just gvisor-test-image`)
#
# Through the product's own path (melissa-server tool --wait deploy_app, confirmed with
# `melissa-server confirm approve`), it deploys one tiny busybox app with the default isolation and one
# with `standard`, then checks:
# - the startup summary's gVisor line, containerd's runsc entry, the `gvisor` RuntimeClass (runsc);
# - the confirmation names the isolation level;
# - the default app runs under gVisor (runtimeClassName, /proc/version and dmesg show gVisor's kernel),
#   the standard one on the device's kernel;
# - cheap escape probes in the gVisor pod (research doc): no /dev/kvm, /dev/mem, /dev/net/tun; mount,
#   /proc/sys writes fail; the server UI on the node is unreachable;
# - fail closed: a pod or Deployment labelled gvisor without the RuntimeClass is refused by admission,
#   another RuntimeClass is refused, and with the RuntimeClass gone a gVisor deploy fails with the reason
#   (nothing runs on runc instead);
# - start time and memory of both pods (printed, not judged).
#
# Separate from the installed `melissa` container: its own name, volume and image tag in root's storage,
# no published ports; all removed afterwards. Needs podman, sudo, network (the test pulls busybox), the
# image built (`just container-build`). Log: target/gvisor-test-image.log. Exit status 0 only on a pass.
set -euo pipefail

IMAGE=${1:-localhost/melissa:dev}
NAME=melissa-gvisor-test
VOLUME=$NAME-var
TEST_IMAGE=localhost/melissa-gvisor-test:run
BUSYBOX=docker.io/library/busybox:1.37
cd "$(dirname "$0")/../.."
mkdir -p target
LOG=target/gvisor-test-image.log
exec > >(tee "$LOG") 2>&1

fail=0
pass() { echo "PASS: $*"; }
bad() { echo "FAIL: $*"; fail=1; }
in_ctr() { sudo podman exec "$NAME" "$@"; }
kc() { in_ctr kubectl "$@"; }
secs() { awk -v a="$1" -v b="$2" 'BEGIN { printf "%.1f", a - b }'; }
wait_for() { # seconds, description, command...
    local end=$(( $(date +%s) + $1 )) what=$2; shift 2
    until "$@" >/dev/null 2>&1; do
        [ "$(date +%s)" -lt "$end" ] || { echo "FAIL: $what (timed out)"; return 1; }
        sleep 3
    done
}

cleanup() {
    sudo podman rm -f -t 10 "$NAME" >/dev/null 2>&1 || true
    sudo podman volume rm -f "$VOLUME" >/dev/null 2>&1 || true
    sudo podman rmi -f "$TEST_IMAGE" >/dev/null 2>&1 || true
    podman untag "$TEST_IMAGE" >/dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup

echo "== copying $IMAGE into root's storage as $TEST_IMAGE"
podman tag "$IMAGE" "$TEST_IMAGE"
archive=target/melissa-gvisor-test.tar
rm -f "$archive"
podman save -o "$archive" "$TEST_IMAGE"
sudo podman load -q -i "$archive" >/dev/null
rm -f "$archive"

echo "== booting $NAME (rootful, --privileged, systemd, /var on $VOLUME, no ports)"
sudo podman run -d --name "$NAME" --hostname "$NAME" --privileged --systemd=always \
    --tmpfs /dev/input -e XDG_CONFIG_HOME=/etc --network podman -v "$VOLUME:/var" \
    --stop-timeout 30 "$TEST_IMAGE" /sbin/init >/dev/null

wait_for 300 "k3s.service active" in_ctr systemctl is-active -q k3s || exit 1
wait_for 300 "node Ready" sh -c "sudo podman exec $NAME kubectl get nodes --no-headers | grep -q ' Ready '" || exit 1
pass "k3s node Ready"

# ---- set-up ---------------------------------------------------------------------------------
echo "== gVisor set-up"
in_ctr runsc --version | head -n1
wait_for 120 "server startup summary" sh -c "sudo podman exec $NAME journalctl -u melissa-server --no-pager | grep -q 'gVisor:'"
line=$(in_ctr journalctl -u melissa-server --no-pager | grep -o 'gVisor: .*' | tail -n1)
case "$line" in *"on (runsc release-"*) pass "startup summary: $line" ;; *) bad "startup summary: $line" ;; esac
if in_ctr grep -q 'containerd.runtimes.runsc' /var/lib/rancher/k3s/agent/etc/containerd/config.toml; then
    pass "containerd config has the runsc runtime"
else bad "containerd config has no runsc runtime"; fi
wait_for 120 "RuntimeClass gvisor" kc get runtimeclass gvisor || fail=1
[ "$(kc get runtimeclass gvisor -o jsonpath='{.handler}')" = runsc ] \
    && pass "RuntimeClass gvisor -> runsc ($(kc get runtimeclass gvisor -o jsonpath='{.overhead.podFixed}'))" \
    || bad "RuntimeClass gvisor does not name runsc"
for p in melissa-apps-isolation melissa-gvisor-runtimeclass melissa-apps-workloads; do
    wait_for 120 "policy $p" kc get validatingadmissionpolicybinding "$p" && pass "admission policy $p bound" || fail=1
done
wait_for 180 "melissa-kubeconfig" in_ctr test -s /run/melissa-kube/apps.kubeconfig || exit 1
# The server's API and its owner exist once it answers.
wait_for 120 "server API" in_ctr melissa-server confirm list || exit 1

# ---- deploy through the product -------------------------------------------------------------
# deploy <app> <isolation or ''>: tool --wait in the background, check and approve its confirmation.
deploy() {
    local app=$1 iso=$2 spec args id reason
    spec=$(printf '{"description":"gVisor test app","containers":[{"name":"main","image":"%s","command":["sh","-c","echo up; exec sleep 86400"],"resources":{"cpu":"200m","memory":"64Mi"}}]%s}' \
        "$BUSYBOX" "${iso:+,\"isolation\":\"$iso\"}")
    args=$(printf '{"app":"%s","spec":%s}' "$app" "$spec")
    in_ctr rm -f "/tmp/$app.out" "/tmp/$app.err"
    sudo podman exec -d "$NAME" sh -c "melissa-server tool --wait deploy_app '$args' >/tmp/$app.out 2>/tmp/$app.err" >/dev/null
    wait_for 60 "confirmation for $app" in_ctr grep -q 'waiting for confirmation #' "/tmp/$app.err" || return 1
    id=$(in_ctr sed -n 's/.*waiting for confirmation #\([0-9]*\).*/\1/p' "/tmp/$app.err" | head -n1)
    reason=$(in_ctr sed -n 's/.*waiting for confirmation #[0-9]* (\(.*\)): approve.*/\1/p' "/tmp/$app.err" | head -n1)
    echo "   confirmation #$id: $reason"
    in_ctr melissa-server confirm approve "$id" >/dev/null
    wait_for 120 "deploy_app $app result" sh -c "sudo podman exec $NAME sh -c 'test -s /tmp/$app.out || grep -qi error /tmp/$app.err'"
    CONFIRM_REASON=$reason
}

echo "== deploy: default isolation (gvisor)"
start=$(date +%s.%N)
deploy gv '' || fail=1
case "$CONFIRM_REASON" in
    *"isolation: gVisor sandbox (its own kernel)"*) pass "confirmation shows the isolation (gVisor)" ;;
    *) bad "confirmation does not show gVisor: $CONFIRM_REASON" ;;
esac
wait_for 240 "app gv ready" kc -n app-gv rollout status deploy/gv --timeout=5s || { kc -n app-gv describe pods | tail -n 30; fail=1; }
gv_ready=$(date +%s.%N)

echo "== deploy: isolation standard"
start_std=$(date +%s.%N)
deploy std standard || fail=1
case "$CONFIRM_REASON" in
    *"isolation: standard (shares the device's kernel)"*) pass "confirmation shows the isolation (standard)" ;;
    *) bad "confirmation does not show standard: $CONFIRM_REASON" ;;
esac
wait_for 240 "app std ready" kc -n app-std rollout status deploy/std --timeout=5s || fail=1
std_ready=$(date +%s.%N)

# ---- which kernel ---------------------------------------------------------------------------
echo "== which kernel"
gvpod=$(kc -n app-gv get pod -l melissa.io/app=gv -o jsonpath='{.items[0].metadata.name}')
stdpod=$(kc -n app-std get pod -l melissa.io/app=std -o jsonpath='{.items[0].metadata.name}')
[ "$(kc -n app-gv get pod "$gvpod" -o jsonpath='{.spec.runtimeClassName}')" = gvisor ] \
    && pass "gv pod: runtimeClassName gvisor" || bad "gv pod has no runtimeClassName gvisor"
[ -z "$(kc -n app-std get pod "$stdpod" -o jsonpath='{.spec.runtimeClassName}')" ] \
    && pass "std pod: default runtime" || bad "std pod has a runtimeClassName"
gv_version=$(kc -n app-gv exec "$gvpod" -- cat /proc/version)
std_version=$(kc -n app-std exec "$stdpod" -- cat /proc/version)
host_version=$(in_ctr cat /proc/version)
echo "   gv  /proc/version: $gv_version"
echo "   std /proc/version: $std_version"
# gVisor reports its own kernel: `4.19.0-gvisor` (release-20261116.0; `4.4.0` in older releases), never the host's.
if [ "$gv_version" != "$host_version" ] && grep -Eq "gvisor|15:06:54 PST 2016" <<<"$gv_version"; then
    pass "gv pod sees gVisor's kernel"
else bad "gv pod sees $gv_version"; fi
[ "$std_version" = "$host_version" ] && pass "std pod sees the device's kernel" || bad "std pod: $std_version"
gv_dmesg=$(kc -n app-gv exec "$gvpod" -- dmesg 2>&1 | head -n3 || true)
echo "   gv dmesg: $(echo "$gv_dmesg" | paste -sd'|' -)"
grep -q 'Starting gVisor' <<<"$gv_dmesg" && pass "gv pod's dmesg is gVisor's banner" || bad "gv dmesg: $gv_dmesg"
stored=$(kc get ns app-gv -o jsonpath='{.metadata.annotations.melissa\.io/spec}')
grep -q '"isolation":"gvisor"' <<<"$stored" && pass "the level is stored with the spec" || bad "stored spec: $stored"

# ---- cheap escape probes in the gVisor pod --------------------------------------------------
echo "== probes in the gVisor pod"
probe() { # description, shell snippet that must FAIL inside the pod
    if kc -n app-gv exec "$gvpod" -- sh -c "$2" >/dev/null 2>&1; then bad "gv pod: $1 works"
    else pass "gv pod: $1 refused"; fi
}
probe "/dev/kvm" 'test -e /dev/kvm'
probe "/dev/mem" 'test -e /dev/mem'
# /dev/net/tun exists under gVisor: the Sentry emulates it on its own netstack (it never reaches the
# host's tun driver), and creating an interface still needs CAP_NET_ADMIN, which app pods don't get.
if kc -n app-gv exec "$gvpod" -- test -e /dev/net/tun 2>/dev/null; then
    echo "   note: /dev/net/tun is gVisor's own emulated device (no host tun driver)"
fi
probe "create a tun interface" 'ip tuntap add mode tun dev melissa0'
probe "mount tmpfs" 'mkdir -p /tmp/m && mount -t tmpfs x /tmp/m'
probe "write /proc/sys" 'echo 1 > /proc/sys/kernel/sysrq'
probe "write /sys" 'echo x > /sys/kernel/uevent_helper'
node=$(kc get node "$NAME" -o jsonpath='{.status.addresses[?(@.type=="InternalIP")].address}')
for a in "$node:8080" "$node:8081"; do
    probe "the server UI at $a" "wget -q -T 3 -O /dev/null http://$a/"
done

# ---- fail closed ----------------------------------------------------------------------------
echo "== fail closed"
out=$(kc -n app-std run probe --image="$BUSYBOX" --labels=melissa.io/isolation=gvisor --restart=Never \
    --command -- sleep 1 2>&1 || true)
grep -q 'only under the gvisor RuntimeClass' <<<"$out" \
    && pass "a pod labelled gvisor without the RuntimeClass is refused" || bad "pod not refused: $out"
out=$(kc -n app-std run probe2 --image="$BUSYBOX" --restart=Never \
    --overrides='{"spec":{"runtimeClassName":"runc-ish"}}' --command -- sleep 1 2>&1 || true)
grep -Eq 'nothing else|not found' <<<"$out" \
    && pass "another RuntimeClass is refused ($(grep -o 'nothing else\|not found' <<<"$out" | head -n1))" \
    || bad "other RuntimeClass not refused: $out"
# The write identity's own Deployment: labelled gvisor, no class.
k=(kubectl --kubeconfig /run/melissa-kube/apps.kubeconfig)
patch='{"spec":{"template":{"spec":{"runtimeClassName":null}}}}'
out=$(in_ctr "${k[@]}" -n app-gv patch deploy gv --type=merge -p "$patch" --dry-run=server 2>&1 || true)
grep -q 'only under the gvisor RuntimeClass' <<<"$out" \
    && pass "melissa-apps can't drop the RuntimeClass of a gvisor app" || bad "Deployment patch not refused: $out"
# A RuntimeClass's handler is immutable; the policy guards a re-created one (checked below).
out=$(kc patch runtimeclass gvisor --type=merge -p '{"handler":"runc"}' --dry-run=server 2>&1 || true)
grep -Eq 'must use the runsc handler|field is immutable' <<<"$out" \
    && pass "the gvisor RuntimeClass's handler can't be changed" || bad "RuntimeClass handler change not refused: $out"
# Without the RuntimeClass, a gVisor deploy fails with the reason and nothing runs on runc.
kc get runtimeclass gvisor -o yaml > /dev/null
in_ctr sh -c 'mv /var/lib/rancher/k3s/server/manifests/melissa-gvisor-runtimeclass.yaml /root/rc.yaml'
kc delete runtimeclass gvisor --wait=true >/dev/null
out=$(printf 'apiVersion: node.k8s.io/v1\nkind: RuntimeClass\nmetadata:\n  name: gvisor\nhandler: runc\n' \
    | sudo podman exec -i "$NAME" kubectl create --dry-run=server -f - 2>&1 || true)
grep -q 'must use the runsc handler' <<<"$out" \
    && pass "a gvisor RuntimeClass with another handler is refused" || bad "gvisor RuntimeClass with runc: $out"
in_ctr rm -f /tmp/gone.out /tmp/gone.err
spec='{"description":"gVisor test app","containers":[{"name":"main","image":"'$BUSYBOX'","command":["sleep","86400"]}]}'
sudo podman exec -d "$NAME" sh -c "melissa-server tool --wait deploy_app '{\"app\":\"gone\",\"spec\":$spec}' >/tmp/gone.out 2>/tmp/gone.err" >/dev/null
if wait_for 60 "confirmation for gone" in_ctr grep -q 'waiting for confirmation #' /tmp/gone.err; then
    id=$(in_ctr sed -n 's/.*waiting for confirmation #\([0-9]*\).*/\1/p' /tmp/gone.err | head -n1)
    in_ctr melissa-server confirm approve "$id" >/dev/null
fi
wait_for 60 "gone deploy ends" in_ctr grep -q 'not deployed' /tmp/gone.err || true
err=$(in_ctr cat /tmp/gone.err)
if grep -q 'gVisor is not available on this device' <<<"$err" && ! kc get ns app-gone >/dev/null 2>&1; then
    pass "without the RuntimeClass a gVisor deploy fails closed: $(grep -o 'not deployed: [^(]*' <<<"$err" | head -n1)"
else bad "gVisor deploy without the RuntimeClass: $err"; fi
in_ctr sh -c 'mv /root/rc.yaml /var/lib/rancher/k3s/server/manifests/melissa-gvisor-runtimeclass.yaml'
# k3s re-applies a manifest only when it changes; put the RuntimeClass back by hand.
kc apply -f /usr/lib/melissa/k3s/gvisor/runtimeclass.yaml >/dev/null

# ---- cost -----------------------------------------------------------------------------------
echo "== cost (indicative)"
mem() { # namespace pod -> the pod cgroup's memory.current and memory.max
    local uid cg
    uid=$(kc -n "$1" get pod "$2" -o jsonpath='{.metadata.uid}')
    cg=$(in_ctr find /sys/fs/cgroup -maxdepth 4 -type d \( -name "*pod${uid//-/_}.slice" -o -name "pod$uid" \) | head -n1)
    [ -n "$cg" ] || { echo "?"; return; }
    in_ctr sh -c "awk '{ printf \"%.1f MiB\", \$1 / 1048576 }' $cg/memory.current; printf ' (limit '; \
        awk '{ printf \"%.0f MiB)\", \$1 / 1048576 }' $cg/memory.max"
}
echo "   deploy to Ready: gvisor $(secs "$gv_ready" "$start") s, standard $(secs "$std_ready" "$start_std") s (image pull in the first)"
echo "   pod memory.current: gvisor $(mem app-gv "$gvpod"), standard $(mem app-std "$stdpod")"
# The same pod again, image cached: just the sandbox start.
ready_other() { # namespace app old-pod: a pod other than old-pod is Ready
    kc -n "$1" get pod -l "melissa.io/app=$2" \
        -o jsonpath='{range .items[*]}{.metadata.name} {.status.conditions[?(@.type=="Ready")].status}{"\n"}{end}' \
        | grep -v "^$3 " | grep -q ' True$'
}
for app in gv std; do
    old=$(kc -n "app-$app" get pod -l "melissa.io/app=$app" -o jsonpath='{.items[0].metadata.name}')
    t0=$(date +%s.%N)
    kc -n "app-$app" delete pod "$old" --wait=false >/dev/null
    end=$(( $(date +%s) + 120 ))
    until ready_other "app-$app" "$app" "$old" || [ "$(date +%s)" -ge "$end" ]; do sleep 0.2; done
    echo "   pod replaced to Ready ($app, image cached): $(secs "$(date +%s.%N)" "$t0") s"
done

if [ "$fail" = 0 ]; then echo "PASS: gVisor end to end in $IMAGE (log: $LOG)"
else echo "FAIL: see above (log: $LOG)"; fi
exit "$fail"
