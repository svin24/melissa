#!/bin/bash
# Remove Melissa's container installation (packaging/container/install.sh). Run as root.
#
#   sudo packaging/container/uninstall.sh           stop and remove; keep the data volume and images
#   sudo packaging/container/uninstall.sh --purge   also delete the data (melissa-var: apps, chats,
#                                                   agent logins, config) and the Melissa images
set -euo pipefail

STATE=/var/lib/melissa-container
QUADLET=/etc/containers/systemd/melissa.container
UNITDIR=/etc/systemd/system
LIBEXEC=/usr/local/libexec/melissa
SHARE=/usr/local/share/melissa
SYSCTL_FILE=/etc/sysctl.d/90-melissa-kubelet.conf
MODULES_FILE=/etc/modules-load.d/melissa.conf
PREVIOUS=localhost/melissa-rollback:previous

say() { echo "melissa-uninstall: $*"; }
PURGE=no
case "${1:-}" in --purge) PURGE=yes ;; "") ;; *) sed -n '2,8p' "$0"; exit 2 ;; esac
[ "$(id -u)" = 0 ] || { echo "melissa-uninstall: run as root" >&2; exit 1; }

IMAGE=; TIMER_OURS=no
# shellcheck source=/dev/null
[ -r "$STATE/install.env" ] && . "$STATE/install.env"

if systemctl cat melissa.service >/dev/null 2>&1; then
    say "stopping melissa.service"
    systemctl stop melissa.service || true
fi
podman rm -f -i melissa >/dev/null 2>&1 || true
systemctl disable -q --now melissa-update.path 2>/dev/null || true
rm -f "$QUADLET" "$UNITDIR/melissa-update.path" "$UNITDIR/melissa-update.service" \
    "$LIBEXEC/melissa-host-update" "$SHARE/README.md" "$MODULES_FILE"
rmdir "$LIBEXEC" "$SHARE" 2>/dev/null || true
if [ "$TIMER_OURS" = yes ]; then
    systemctl disable -q --now podman-auto-update.timer || true
    say "disabled podman-auto-update.timer (install.sh had enabled it)"
fi
systemctl daemon-reload

if [ -e "$SYSCTL_FILE" ]; then
    rm -f "$SYSCTL_FILE"
    if [ -r "$STATE/host-sysctl.before" ]; then
        sysctl -q -p "$STATE/host-sysctl.before" && say "restored host sysctls: $(paste -sd ' ' "$STATE/host-sysctl.before")"
    fi
fi
rm -f "$STATE/host-sysctl.before"

if [ "$PURGE" = yes ]; then
    podman volume rm -f melissa-var >/dev/null 2>&1 && say "deleted volume melissa-var" || true
    for i in "$PREVIOUS" ${IMAGE:+"$IMAGE"}; do
        podman rmi "$i" >/dev/null 2>&1 && say "removed image $i" || true
    done
    rm -rf "$STATE"
    say "purged"
else
    rm -rf "$STATE/request" "$STATE/status" "$STATE/running-id" "$STATE/available-for"
    say "removed. Kept: volume melissa-var (your data), images ($IMAGE${IMAGE:+, }$PREVIOUS if present),"
    say "  $STATE/install.env. Install again to continue, or run with --purge to delete them."
fi
