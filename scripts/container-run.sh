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
#
# Rootless by default. Run it with sudo for a rootful container (its own image storage: build there).
# Environment:
#   NAME=melissa              container name (volume: $NAME-var)
#   IMAGE=localhost/melissa-headless:dev
#   CTR_IMAGE=localhost/melissa-container:dev
#   UI_PORT=18080             host port for the web UI (bound to 127.0.0.1 only)
#   APP_PORT_BASE=30000       host port for app NodePort 30000; 80 ports are mapped (30000-30079)
#   BIND=127.0.0.1            host address for the app ports (0.0.0.0 to reach apps from the LAN)
#   PASTA_DIR=                directory with a working `pasta` (on SELinux hosts where the distro
#                             one is denied; e.g. "$(nix build --no-link --print-out-paths nixpkgs#passt)/bin")
set -euo pipefail

NAME=${NAME:-melissa}
IMAGE=${IMAGE:-localhost/melissa-headless:dev}
CTR_IMAGE=${CTR_IMAGE:-localhost/melissa-container:dev}
UI_PORT=${UI_PORT:-18080}
APP_PORT_BASE=${APP_PORT_BASE:-30000}
BIND=${BIND:-127.0.0.1}
VOLUME="$NAME-var"
repo=$(cd "$(dirname "$0")/.." && pwd)

die() { echo "container-run: $*" >&2; exit 1; }
[ -n "${PASTA_DIR:-}" ] && export PATH="$PASTA_DIR:$PATH"
in_ctr() { podman exec "$NAME" "$@"; }

controllers() {
    if [ "$(id -u)" = 0 ]; then cat /sys/fs/cgroup/cgroup.controllers
    else cat "/sys/fs/cgroup/user.slice/user-$(id -u).slice/user@$(id -u).service/cgroup.controllers"; fi
}

preflight() {
    local ok=0 c
    c=$(controllers 2>/dev/null || true)
    for want in cpu cpuset io memory pids; do
        case " $c " in *" $want "*) ;; *)
            echo "MISSING cgroup controller '$want' for this user (k3s will not start)."
            echo "  Fix (one time; no reboot expected):"
            echo "    sudo mkdir -p /etc/systemd/system/user@.service.d"
            # shellcheck disable=SC2028 # the \n are meant literally: the user runs this printf
            echo "    printf '[Service]\\nDelegate=cpu cpuset io memory pids\\n' | sudo tee /etc/systemd/system/user@.service.d/delegate.conf"
            echo "    sudo systemctl daemon-reload"
            echo "  then log out and in again if it is still missing."
            ok=1; break ;;
        esac
    done
    if [ -r /proc/modules ]; then
        for m in br_netfilter overlay; do
            grep -q "^$m " /proc/modules || { echo "MISSING kernel module $m: sudo modprobe $m"; ok=1; }
        done
    fi
    if ! podman run --rm --network=pasta --entrypoint true "$IMAGE" >/dev/null 2>&1; then
        echo "pasta networking fails here. On SELinux hosts where /usr/bin/pasta is denied, point"
        echo "PASTA_DIR at another build, e.g. PASTA_DIR=\$(nix build --no-link --print-out-paths nixpkgs#passt)/bin"
        ok=1
    fi
    [ "$ok" = 0 ] && echo "preflight ok (cgroups: $c)"
    return "$ok"
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
        # --network=pasta:--host-lo-to-ns-lo: host 127.0.0.1:UI_PORT reaches the queen on the
        #   container's own 127.0.0.1:8080, so the UI stays loopback-only inside (pods cannot reach it).
        podman run -d --name "$NAME" --hostname "$NAME" \
            --privileged --systemd=always \
            --network=pasta:--host-lo-to-ns-lo \
            -p "127.0.0.1:$UI_PORT:8080" \
            -p "$BIND:$APP_PORT_BASE-$last:30000-30079" \
            -v "$VOLUME:/var" \
            --stop-timeout 60 \
            "$CTR_IMAGE" /sbin/init >/dev/null
    fi
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
*) sed -n '2,24p' "$0"; exit 2 ;;
esac
