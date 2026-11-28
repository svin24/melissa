#!/bin/bash
# Remove Melissa's container installation (packaging/container/install.sh). Run as root.
# install.sh also installs it as /usr/local/libexec/melissa/uninstall.sh (with trust.sh, apparmor.sh).
#
#   sudo packaging/container/uninstall.sh           stop and remove; keep the data volume and images
#   sudo packaging/container/uninstall.sh --purge   also delete the data (melissa-var: apps, chats,
#                                                   agent logins, config) and the Melissa images
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=packaging/container/trust.sh
. "$here/trust.sh"
# shellcheck source=packaging/container/apparmor.sh
. "$here/apparmor.sh"
STATE=/var/lib/melissa-container
QUADLET=/etc/containers/systemd/melissa.container
UNITDIR=/etc/systemd/system
LIBEXEC=/usr/local/libexec/melissa
SHARE=/usr/local/share/melissa
SYSCTL_FILE=/etc/sysctl.d/90-melissa-kubelet.conf
MODULES_FILE=/etc/modules-load.d/melissa.conf
PREVIOUS=localhost/melissa-rollback:previous
DOCKER_FORWARD_UNIT=melissa-docker-forward.service

say() { echo "melissa-uninstall: $*"; }

# remove_docker_forward SUBNETS: install.sh's DOCKER-USER rules for Docker hosts (install.env
# DOCKER_FORWARD_SUBNETS; empty: install.sh added none, nothing is touched). Deletes the unit and
# one copy of each rule, the one the unit inserted.
remove_docker_forward() {
    local s f
    [ -n "$1" ] || return 0
    systemctl disable -q --now "$DOCKER_FORWARD_UNIT" 2>/dev/null || true
    rm -f "$UNITDIR/$DOCKER_FORWARD_UNIT"
    for s in $1; do
        for f in -s -d; do iptables -D DOCKER-USER "$f" "$s" -j ACCEPT 2>/dev/null || true; done
    done
    if [ -w "$STATE/install.env" ]; then
        sed -i 's/^DOCKER_FORWARD_SUBNETS=.*/DOCKER_FORWARD_SUBNETS=""/' "$STATE/install.env"
    fi
    say "removed $DOCKER_FORWARD_UNIT and its DOCKER-USER rules for $1"
}

# remove_apparmor RECORDED: install.sh's AppArmor profile for the container (install.env
# APPARMOR_PROFILE=yes; anything else: install.sh loaded none, nothing is touched).
remove_apparmor() {
    [ "$1" = yes ] && [ -e "$APPARMOR_DIR/$MELISSA_APPARMOR_PROFILE" ] || return 0
    apparmor_unload
    say "unloaded and removed the AppArmor profile $MELISSA_APPARMOR_PROFILE ($APPARMOR_DIR/$MELISSA_APPARMOR_PROFILE)"
}

# Sourced (packaging/container/test-install.sh): only the functions.
[ "${BASH_SOURCE[0]}" = "$0" ] || return 0

PURGE=no
case "${1:-}" in --purge) PURGE=yes ;; "") ;; *) sed -n '2,7p' "$0"; exit 2 ;; esac
[ "$(id -u)" = 0 ] || { echo "melissa-uninstall: run as root" >&2; exit 1; }

IMAGE=; TIMER_OURS=no; DOCKER_FORWARD_SUBNETS=; POLICY_CREATED=no; APPARMOR_PROFILE=no
# shellcheck source=/dev/null
[ -r "$STATE/install.env" ] && . "$STATE/install.env"

# --purge deletes Melissa's own Tailscale's state: log the node out first, so it leaves the
# tailnet instead of staying there as an offline machine (only while it is signed in).
if [ "$PURGE" = yes ] && podman exec melissa test -S /run/tailscale/tailscaled.sock 2>/dev/null; then
    podman exec melissa tailscale logout >/dev/null 2>&1 && say "logged Melissa's Tailscale node out" || true
fi
if systemctl cat melissa.service >/dev/null 2>&1; then
    say "stopping melissa.service"
    systemctl stop melissa.service || true
fi
podman rm -f -i melissa >/dev/null 2>&1 || true
systemctl disable -q --now melissa-update.path melissa-auto-update.timer melissa-check.timer 2>/dev/null || true
rm -rf "$UNITDIR/melissa-auto-update.timer.d"
rm -f "$QUADLET" "$UNITDIR/melissa-update.path" "$UNITDIR/melissa-update.service" \
    "$UNITDIR/melissa-auto-update.timer" "$UNITDIR/melissa-auto-update.service" \
    "$UNITDIR/melissa-check.timer" "$UNITDIR/melissa-check.service" \
    "$LIBEXEC/melissa-host-update" "$SHARE/README.md" "$MODULES_FILE" \
    "$LIBEXEC/uninstall.sh" "$LIBEXEC/trust.sh" "$LIBEXEC/apparmor.sh"
rmdir "$LIBEXEC" "$SHARE" 2>/dev/null || true
if [ "$TIMER_OURS" = yes ]; then
    systemctl disable -q --now podman-auto-update.timer || true
    say "disabled podman-auto-update.timer (install.sh had enabled it)"
fi
remove_docker_forward "$DOCKER_FORWARD_SUBNETS"
# After the container is gone: nothing runs under the profile any more.
remove_apparmor "$APPARMOR_PROFILE"
# The signature check for the published image (install.sh, trust.sh); install adds it again.
if [ -e "$REGISTRIES_D/melissa.yaml" ] || [ -e "$SIGNING_KEY" ]; then
    if trust_remove "$POLICY_CREATED"; then
        say "removed the signature check for $SIGNED_REPO from $POLICY"
    else
        say "could not edit $POLICY: remove the $SIGNED_REPO entry by hand"
    fi
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
    rm -rf "$STATE/request" "$STATE/status" "$STATE/running-id" "$STATE/available-for" \
        "$STATE/available.json" "$STATE/held.json" "$STATE/updated.json" "$STATE/lock" "$STATE/in-progress" "$STATE/pull-tmp"
    say "removed. Kept: volume melissa-var (your data), images ($IMAGE${IMAGE:+, }$PREVIOUS if present),"
    say "  $STATE/install.env. Install again to continue, or run with --purge to delete them."
fi
