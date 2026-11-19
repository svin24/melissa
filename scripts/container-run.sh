#!/usr/bin/env bash
# Run the Melissa headless image as a podman container, systemd as PID 1.
# Docs: docs/QUICKSTART.md ("Try it in a container") and docs/agent-skills/container-mode.md.
#
#   scripts/container-run.sh build      layer image/container on top of $IMAGE -> $CTR_IMAGE
#   scripts/container-run.sh up         start (create on first use) the container
#   scripts/container-run.sh status     unit states, k3s node, URLs
#   scripts/container-run.sh shell      root shell inside (melissa-setup, melissa-queen chat, kubectl)
#   scripts/container-run.sh logs [unit]
#   scripts/container-run.sh down       stop it (state stays in the volume)
#   scripts/container-run.sh rm         remove the container (keeps the volume)
#   scripts/container-run.sh purge      remove the container and its /var volume
#   scripts/container-run.sh preflight  check the host (cgroup delegation, pasta, kernel modules)
#   scripts/container-run.sh smoke      k3s Ready, Traefik, admission policies, RBAC, a NodePort from the host
#
# Rootless by default (pasta network). Run it as root (sudo) for a rootful container: its own image
# storage (build there or `podman load`), the default bridge network, and the UI through a filtered
# forward inside (docs/agent-skills/container-mode.md, "Rootful").
# Environment:
#   NAME=melissa              container name (volume: $NAME-var)
#   IMAGE=localhost/melissa-headless:dev
#   CTR_IMAGE=localhost/melissa-container:dev
#   UI_PORT=18080             host port for the web UI (bound to 127.0.0.1 only)
#   APP_PORT_BASE=30000       host port for app NodePort 30000; 80 ports are mapped (30000-30079).
#                             Another base (e.g. 31000 next to a host k3s) is written to the queen's
#                             config at first `up` as `[apps] port_offset`, so app links name it.
#   BIND=127.0.0.1            host address for the app ports (0.0.0.0 to reach apps from the LAN)
#   APP_ADDRESSES=            [apps] addresses written to the queen's config at first `up`
#                             (comma separated). Default: 127.0.0.1 when BIND is loopback,
#                             BIND when it is one address, unset (found automatically) for 0.0.0.0.
#   PASTA_DIR=                directory with a working `pasta` (on SELinux hosts where the distro
#                             one is denied; e.g. "$(nix build --no-link --print-out-paths nixpkgs#passt)/bin")
#                             Rootless only; rootful uses the bridge network.
set -euo pipefail

NAME=${NAME:-melissa}
IMAGE=${IMAGE:-localhost/melissa-headless:dev}
CTR_IMAGE=${CTR_IMAGE:-localhost/melissa-container:dev}
UI_PORT=${UI_PORT:-18080}
APP_PORT_BASE=${APP_PORT_BASE:-30000}
BIND=${BIND:-127.0.0.1}
VOLUME="$NAME-var"
repo=$(cd "$(dirname "$0")/.." && pwd)
ROOTFUL=0; [ "$(id -u)" = 0 ] && ROOTFUL=1
# k3s's pod and service networks inside the container; the podman network must not overlap them.
K3S_CIDRS="10.42.0.0/16 10.43.0.0/16"

die() { echo "container-run: $*" >&2; exit 1; }
[ -n "${PASTA_DIR:-}" ] && export PATH="$PASTA_DIR:$PATH"
in_ctr() { podman exec "$NAME" "$@"; }

controllers() {
    if [ "$ROOTFUL" = 1 ]; then cat /sys/fs/cgroup/cgroup.controllers
    else cat "/sys/fs/cgroup/user.slice/user-$(id -u).slice/user@$(id -u).service/cgroup.controllers"; fi
}

preflight() {
    local ok=0 c
    c=$(controllers 2>/dev/null || true)
    for want in cpu cpuset io memory pids; do
        case " $c " in *" $want "*) ;; *)
            echo "MISSING cgroup controller '$want' for this user (k3s fails: \"failed to find cpuset cgroup (v2)\")."
            echo "  Host prerequisite (root, one time; no reboot, usually no relogin):"
            echo "    sudo mkdir -p /etc/systemd/system/user@.service.d"
            # shellcheck disable=SC2028 # the \n are meant literally: the user runs this printf
            echo "    printf '[Service]\\nDelegate=cpu cpuset io memory pids\\n' | sudo tee /etc/systemd/system/user@.service.d/delegate.conf"
            echo "    sudo systemctl daemon-reload"
            echo "  then check: cat /sys/fs/cgroup/user.slice/user-\$(id -u).slice/user@\$(id -u).service/cgroup.controllers"
            echo "  (log out and in again if cpuset is still missing)."
            ok=1; break ;;
        esac
    done
    if [ "$ROOTFUL" = 0 ] && [ ! -e /etc/systemd/system/user@.service.d/delegate.conf ] \
        && ! grep -qs cpuset /usr/lib/systemd/system/user@.service.d/*.conf; then
        echo "note: no Delegate= drop-in for user@.service found; cpuset may be gone after a reboot."
    fi
    if [ -r /proc/modules ]; then
        for m in br_netfilter overlay; do
            grep -q "^$m " /proc/modules || { echo "MISSING kernel module $m: sudo modprobe $m"; ok=1; }
        done
    fi
    podman container exists "$NAME" 2>/dev/null || for p in "$UI_PORT" "$APP_PORT_BASE" "$((APP_PORT_BASE + 79))"; do
        if ss -Hltn "sport = :$p" 2>/dev/null | grep -q .; then
            echo "host port $p is in use (UI_PORT=$UI_PORT, APP_PORT_BASE=$APP_PORT_BASE): pick others"; ok=1
        fi
    done
    if [ "$ROOTFUL" = 1 ]; then
        preflight_rootful || ok=1
    elif ! podman run --rm --network=pasta --entrypoint true "$IMAGE" >/dev/null 2>&1; then
        echo "pasta networking fails here. On SELinux hosts where /usr/bin/pasta is denied, point"
        echo "PASTA_DIR at another build, e.g. PASTA_DIR=\$(nix build --no-link --print-out-paths nixpkgs#passt)/bin"
        ok=1
    fi
    [ "$ok" = 0 ] && echo "preflight ok (cgroups: $c)"
    return "$ok"
}

# Rootful: the default bridge network must work and must not overlap k3s's networks inside.
preflight_rootful() {
    local ok=0 sub c
    podman image exists "$CTR_IMAGE" || echo "note: no $CTR_IMAGE in root's storage: build it as root or podman load it"
    if ! podman run --rm --network=bridge --entrypoint true "$IMAGE" >/dev/null 2>&1 \
        && ! podman run --rm --network=bridge --entrypoint true "$CTR_IMAGE" >/dev/null 2>&1; then
        echo "rootful bridge networking fails here (podman run --network=bridge)"; ok=1
    fi
    for sub in $(podman network inspect podman --format '{{range .Subnets}}{{.Subnet}} {{end}}' 2>/dev/null); do
        for c in $K3S_CIDRS; do
            if overlaps "$sub" "$c"; then
                echo "podman network 'podman' ($sub) overlaps k3s's $c inside the container"; ok=1
            fi
        done
    done
    echo "rootful: --privileged is root on this host; k3s inside sets host-wide sysctls (vm.overcommit_memory=1,"
    echo "  kernel.panic=10, kernel.panic_on_oops=1, as any k3s on the host does)."
    return "$ok"
}

# overlaps A/len B/len (IPv4 only; anything else: no)
overlaps() {
    local a=${1%/*} al=${1#*/} b=${2%/*} bl=${2#*/} m
    case "$a$b" in *:*) return 1 ;; esac
    # shellcheck disable=SC2086 # split the address at the dots
    ip2n() { local IFS=.; set -- $1; echo $(( ($1 << 24) + ($2 << 16) + ($3 << 8) + $4 )); }
    m=$(( al < bl ? al : bl ))
    [ $(( ($(ip2n "$a") ^ $(ip2n "$b")) >> (32 - m) )) = 0 ]
}

# The app addresses for the queen's config: what people on the host (or LAN) type before :port.
app_addresses() {
    if [ -n "${APP_ADDRESSES:-}" ]; then echo "$APP_ADDRESSES"; return; fi
    case "$BIND" in
    127.*|::1|localhost) echo 127.0.0.1 ;;
    0.0.0.0|::|'')
        # Rootless, pasta copies the host's LAN address into the container: found automatically.
        # Rootful, the container only has its bridge address: use the host's (route source address
        # towards a documentation address; nothing is sent).
        if [ "$ROOTFUL" = 1 ]; then
            ip -4 route get 203.0.113.1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p'
        fi ;;
    *) echo "$BIND" ;;
    esac
}

# Write [apps] addresses and port_offset into the queen's config unless the owner already
# has a config. /etc/melissa lives on the /var volume (see image/container/Containerfile).
configure() {
    local list toml='' a offset=$((APP_PORT_BASE - 30000)) body=''
    if in_ctr test -e /etc/melissa/queen.toml; then
        if [ "$offset" != 0 ] && ! in_ctr grep -qsx "port_offset = $offset" /etc/melissa/queen.toml; then
            echo "note: APP_PORT_BASE=$APP_PORT_BASE but /etc/melissa/queen.toml has no 'port_offset = $offset'"
            echo "  under [apps]: app links name the wrong port until you add it ('$0 shell')."
        fi
        return 0
    fi
    list=$(app_addresses)
    for a in ${list//,/ }; do toml+="${toml:+, }\"$a\""; done
    [ -z "$toml" ] || body+="addresses = [$toml]"$'\n'
    [ "$offset" = 0 ] || body+="port_offset = $offset"$'\n'
    [ -n "$body" ] || return 0
    printf '# Written by scripts/container-run.sh (container mode): app links use these.\n[apps]\n%s' "$body" \
        | podman exec -i "$NAME" sh -c 'umask 022; cat > /etc/melissa/queen.toml'
    in_ctr systemctl try-restart melissa-queen
    echo "Queen config: [apps] $(printf '%s' "$body" | paste -sd ' ' -)"
}

wait_for() { # seconds, description, command...
    local end=$(( $(date +%s) + $1 )) what=$2; shift 2
    until "$@" >/dev/null 2>&1; do
        [ "$(date +%s)" -lt "$end" ] || { echo "FAIL: $what (timed out)"; return 1; }
        sleep 3
    done
}

smoke() {
    local fail=0 last=$((APP_PORT_BASE + 79)) code
    pass() { echo "PASS: $*"; }
    bad() { echo "FAIL: $*"; fail=1; }
    wait_for 240 "k3s.service active" in_ctr systemctl is-active -q k3s || return 1
    pass "k3s.service active ($(in_ctr k3s --version | head -1))"
    wait_for 300 "node Ready" sh -c "podman exec $NAME kubectl get nodes --no-headers | grep -q ' Ready '" || return 1
    pass "node Ready"
    sys_ready() {
        local pods; pods=$(in_ctr kubectl -n kube-system get pods --no-headers 2>/dev/null) || return 1
        echo "$pods" | grep -q '^traefik-.* Running ' && ! echo "$pods" | grep -vqE ' (Running|Completed) '
    }
    if wait_for 300 "system pods running" sys_ready; then pass "system pods running (Traefik too)"
    else in_ctr kubectl -n kube-system get pods; return 1; fi
    in_ctr grep -q 'snapshotter = "overlayfs"' /var/lib/rancher/k3s/agent/etc/containerd/config.toml \
        && pass "containerd snapshotter: overlayfs" || echo "note: containerd is not on overlayfs"
    targs() { in_ctr kubectl -n kube-system get deploy traefik -o jsonpath='{.spec.template.spec.containers[0].args}' | grep -q checkNewVersion=false; }
    wait_for 300 "Traefik update check disabled" targs && pass "Traefik update check disabled" || fail=1
    for p in melissa-apps-namespaces melissa-apps-rolebindings melissa-apps-services; do
        in_ctr kubectl get validatingadmissionpolicybinding "$p" >/dev/null 2>&1 \
            && pass "admission policy $p bound" || bad "admission policy $p missing"
    done
    wait_for 120 "melissa-kubeconfig" in_ctr test -s /run/melissa-kube/apps.kubeconfig || fail=1
    local k=(kubectl --kubeconfig /run/melissa-kube/apps.kubeconfig)
    [ "$(in_ctr "${k[@]}" auth can-i create pods -n kube-system 2>/dev/null)" = no ] \
        && pass "RBAC: melissa-apps cannot touch kube-system" || bad "RBAC: melissa-apps can create pods in kube-system"
    # Policies and Traefik's service load balancer take a little while after the pods run.
    denied() { # kubectl fails when denied: check its output, not its status (pipefail)
        local out; out=$(in_ctr "${k[@]}" create namespace smoke-not-an-app --dry-run=server 2>&1 || true)
        grep -q 'denied request' <<<"$out"
    }
    wait_for 120 "policy denies a non-app namespace" denied \
        && pass "policy: melissa-apps may not create a non-app namespace" || fail=1
    t404() { [ "$(in_ctr curl -s -o /dev/null -w '%{http_code}' -H 'Host: smoke.invalid' http://127.0.0.1/)" = 404 ]; }
    wait_for 240 "Traefik answers on :80 inside" t404 && pass "Traefik answers on :80 inside (404 for an unknown host)" || fail=1
    # A NodePort in front of Traefik on the last app port (no image pull needed), reached from
    # the host through pasta's port forward.
    in_ctr kubectl -n kube-system create service nodeport melissa-smoke --tcp=80:8000 --node-port=30079 \
        --dry-run=client -o yaml | sed 's/app: melissa-smoke/app.kubernetes.io\/name: traefik/' \
        | podman exec -i "$NAME" kubectl apply -f - >/dev/null
    code=000
    for _ in $(seq 1 20); do
        code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 3 -H 'Host: smoke.invalid' "http://127.0.0.1:$last/" || true)
        [ "$code" = 404 ] && break; sleep 2
    done
    [ "$code" = 404 ] && pass "NodePort 30079 reachable from the host at 127.0.0.1:$last" || bad "NodePort from the host: HTTP $code"
    in_ctr kubectl -n kube-system delete service melissa-smoke --wait=false >/dev/null
    pod_isolation || fail=1
    code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 "http://127.0.0.1:$UI_PORT/" || true)
    [ "$code" = 200 ] && pass "web UI http://127.0.0.1:$UI_PORT/" || bad "web UI: HTTP $code"
    [ "$fail" = 0 ] && echo "smoke test complete" || echo "smoke test FAILED"
    return "$fail"
}

# A pod must not reach the queen's UI: try from CoreDNS's network namespace (no image pull) the
# node's address on 8080 and 8081 (the rootful forward) and, rootful, the host's UI port via the gateway.
pod_isolation() {
    local cid pid node gw t code fail=0
    cid=$(in_ctr k3s crictl ps -q --name coredns 2>/dev/null | head -1)
    pid=$(in_ctr k3s crictl inspect -o go-template --template '{{.info.pid}}' "$cid" 2>/dev/null) \
        || { echo "FAIL: no CoreDNS pod to test pod isolation from"; return 1; }
    node=$(in_ctr kubectl get node "$NAME" -o jsonpath='{.status.addresses[?(@.type=="InternalIP")].address}')
    t=("$node:8080" "$node:8081")
    if [ "$ROOTFUL" = 1 ]; then
        gw=$(in_ctr ip -4 route show default | awk '{print $3; exit}')
        t+=("$gw:$UI_PORT")
    fi
    for a in "${t[@]}"; do
        code=$(in_ctr nsenter -t "$pid" -n curl -s -o /dev/null -w '%{http_code}' --max-time 3 "http://$a/" || true)
        if [ "$code" = 000 ]; then echo "PASS: a pod cannot reach $a"
        else echo "FAIL: a pod reaches $a (HTTP $code)"; fail=1; fi
    done
    return "$fail"
}

case "${1:-}" in
build)
    podman build --network=none -f "$repo/image/container/Containerfile" \
        --build-arg IMAGE="$IMAGE" -t "$CTR_IMAGE" "$repo"
    ;;
up)
    if podman container exists "$NAME"; then podman start "$NAME" >/dev/null
    else
        podman image exists "$CTR_IMAGE" || die "no $CTR_IMAGE: run '$0 build' first"
        last=$((APP_PORT_BASE + 79))
        # --privileged: k3s needs to mount, set up netfilter and namespaces for its pods, and the
        #   agent sandbox needs nested namespaces. Rootless, "privileged" means only the
        #   capabilities of your own user namespace; rootful, it is full root on the host.
        # --systemd=always: tmpfs on /run, /tmp; cgroup writable; stop with SIGRTMIN+3.
        # Rootless --network=pasta:--host-lo-to-ns-lo: host 127.0.0.1:UI_PORT reaches the queen on
        #   the container's own 127.0.0.1:8080, so the UI stays loopback-only inside (pods cannot reach it).
        # Rootful (no pasta): the bridge network; host 127.0.0.1:UI_PORT goes to :8081 on the container's
        #   eth0, where melissa-ui-forward.socket passes it to 127.0.0.1:8080. Its nftables filter admits
        #   only the bridge gateway (the host) on eth0; pods and other containers are dropped.
        if [ "$ROOTFUL" = 1 ]; then net=(--network=bridge -p "127.0.0.1:$UI_PORT:8081")
        else net=(--network=pasta:--host-lo-to-ns-lo -p "127.0.0.1:$UI_PORT:8080"); fi
        podman run -d --name "$NAME" --hostname "$NAME" \
            --privileged --systemd=always \
            "${net[@]}" \
            -p "$BIND:$APP_PORT_BASE-$last:30000-30079" \
            -v "$VOLUME:/var" \
            --stop-timeout 60 \
            "$CTR_IMAGE" /sbin/init >/dev/null
    fi
    wait_for 60 "systemd in the container" in_ctr test -d /var/lib/melissa-container/etc-melissa
    if [ "$ROOTFUL" = 1 ]; then
        # The flag survives restarts (it is on the volume); start the forward now on first use.
        in_ctr touch /var/lib/melissa-container/ui-forward
        in_ctr systemctl start melissa-ui-forward.socket
    fi
    configure
    echo "Started '$NAME'. Web UI: http://127.0.0.1:$UI_PORT  (k3s takes a minute or two: '$0 status')"
    ;;
status)
    in_ctr systemctl is-system-running || true
    for u in melissa-queen k3s melissa-kubeconfig; do printf '  %-20s %s\n' "$u" "$(in_ctr systemctl is-active "$u" || true)"; done
    in_ctr kubectl get nodes 2>/dev/null || echo "  (k3s API not up yet)"
    echo "Web UI: http://127.0.0.1:$UI_PORT   Apps: http://$BIND:$APP_PORT_BASE ..."
    ;;
shell) podman exec -it "$NAME" bash -l ;;
logs) in_ctr journalctl --no-pager -n 200 ${2:+-u "$2"} ;;
down) podman stop "$NAME" ;;
rm) podman rm -f "$NAME" ;;
purge) podman rm -f "$NAME" 2>/dev/null || true; podman volume rm -f "$VOLUME" ;;
preflight) preflight ;;
smoke) smoke ;;
*) sed -n '2,30p' "$0"; exit 2 ;;
esac
