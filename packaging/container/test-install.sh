#!/bin/bash
# Tests for install.sh's pure functions (app address checks, server.toml edits). No root, no
# podman, no network: `just container-test` or `bash packaging/container/test-install.sh`.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=packaging/container/install.sh
. "$here/install.sh"
set +e

fails=0
eq() { # description, want, got
    if [ "$2" = "$3" ]; then echo "ok: $1"; else echo "FAIL: $1"$'\n'"  want: $2"$'\n'"  got:  $3"; fails=$((fails + 1)); fi
}
yes_no() { if "$@" >/dev/null 2>&1; then echo yes; else echo no; fi; }

# --- BIND coverage (the incident: loopback BIND, Tailscale address in the links) ------------
BIND=127.0.0.1
eq "loopback BIND covers 127.0.0.1" yes "$(yes_no bind_covers 127.0.0.1)"
eq "loopback BIND covers localhost" yes "$(yes_no bind_covers localhost)"
eq "loopback BIND does not cover a Tailscale address" no "$(yes_no bind_covers 100.64.0.10)"
eq "uncovered lists the dead ones" "100.64.0.10,203.0.113.5" "$(uncovered 127.0.0.1,100.64.0.10,203.0.113.5)"
ALLOW_ADDRESS_MISMATCH=no
eq "preflight refuses a mismatch" no "$(yes_no check_addresses 100.64.0.10)"
ALLOW_ADDRESS_MISMATCH=yes
eq "the override lets it pass" yes "$(yes_no check_addresses 100.64.0.10)"
ALLOW_ADDRESS_MISMATCH=no
BIND=0.0.0.0
eq "0.0.0.0 covers everything" "" "$(uncovered 100.64.0.10,203.0.113.5,localhost)"
BIND=100.64.0.10
eq "one address covers itself" yes "$(yes_no bind_covers 100.64.0.10)"
eq "one address does not cover another" no "$(yes_no bind_covers 203.0.113.5)"
eq "one address does not cover loopback" no "$(yes_no bind_covers 127.0.0.1)"
eq "an unresolvable name is not covered" no "$(yes_no bind_covers nothing.invalid)"

# --- what the server is told (MELISSA_PUBLISH_BIND) -----------------------------------------
ALLOW_ADDRESS_MISMATCH=no
BIND=127.0.0.1; eq "publish bind: loopback" 127.0.0.1 "$(publish_bind)"
BIND=localhost; eq "publish bind: localhost as an address" 127.0.0.1 "$(publish_bind)"
BIND=100.64.0.10; eq "publish bind: one address" 100.64.0.10 "$(publish_bind)"
BIND=''; eq "publish bind: empty BIND is every address" 0.0.0.0 "$(publish_bind)"
BIND=::; eq "publish bind: ::" :: "$(publish_bind)"
ALLOW_ADDRESS_MISMATCH=yes
BIND=127.0.0.1; eq "publish bind: unknown when something else forwards" "" "$(publish_bind)"
ALLOW_ADDRESS_MISMATCH=no
eq "melissa.container passes it" yes \
    "$(yes_no grep -qx 'Environment=MELISSA_PUBLISH_BIND=@PUBLISH_BIND@' "$here/melissa.container")"
eq "the server's unit takes it" yes \
    "$(yes_no grep -qx 'PassEnvironment=MELISSA_PUBLISH_BIND' "$here/rootfs/usr/lib/systemd/system/melissa-server.service.d/10-container.conf")"

# --- defaults from BIND --------------------------------------------------------------------
unset APP_ADDRESSES
BIND=127.0.0.1; eq "loopback BIND: 127.0.0.1" 127.0.0.1 "$(app_addresses)"
BIND=203.0.113.5; eq "one address: that address" 203.0.113.5 "$(app_addresses)"
APP_ADDRESSES=box.example.com; eq "APP_ADDRESSES wins" box.example.com "$(app_addresses)"
unset APP_ADDRESSES

# --- every address (the default since 2026-11-21): Melissa's filter decides ----------------
eq "BIND defaults to every address" 0.0.0.0 "$(unset BIND; . "$here/install.sh"; echo "$BIND")"
eq "Tailscale range: 100.64.0.10" yes "$(yes_no is_tailscale 100.64.0.10)"
eq "Tailscale range: 100.127.255.254" yes "$(yes_no is_tailscale 100.127.255.254)"
eq "not Tailscale: 100.128.0.1" no "$(yes_no is_tailscale 100.128.0.1)"
eq "not Tailscale: 100.63.0.1" no "$(yes_no is_tailscale 100.63.0.1)"
eq "not Tailscale: 192.168.1.20" no "$(yes_no is_tailscale 192.168.1.20)"
# A host with a LAN address, a Tailscale address and a container bridge.
ip() {
    case "$*" in
    "-4 route get 203.0.113.1") echo "203.0.113.1 via 192.168.1.1 dev eth0 src 192.168.1.20 uid 0" ;;
    "-4 -o addr show") printf '%s\n' \
        "1: lo    inet 127.0.0.1/8 scope host lo" \
        "2: eth0    inet 192.168.1.20/24 brd 192.168.1.255 scope global eth0" \
        "3: podman0    inet 10.88.0.1/16 brd 10.88.255.255 scope global podman0" \
        "4: tailscale0    inet 100.64.0.10/32 scope global tailscale0" ;;
    esac
}
BIND=0.0.0.0; eq "every address: LAN, Tailscale, loopback (no bridge)" \
    192.168.1.20,100.64.0.10,127.0.0.1 "$(app_addresses)"
unset -f ip

ALLOW_ADDRESS_MISMATCH=no
BIND=0.0.0.0; eq "old loopback default is rewritten" yes "$(yes_no test -n "$(rewrite_reason 127.0.0.1)")"
BIND=0.0.0.0; eq "the owner's own addresses are kept" "" "$(rewrite_reason 192.168.1.20,127.0.0.1)"
BIND=0.0.0.0; eq "no addresses: nothing to rewrite" "" "$(rewrite_reason '')"
BIND=127.0.0.1; eq "explicit loopback BIND keeps 127.0.0.1" "" "$(rewrite_reason 127.0.0.1)"
BIND=127.0.0.1; eq "uncovered ones are rewritten" yes "$(yes_no test -n "$(rewrite_reason 100.64.0.10)")"
ALLOW_ADDRESS_MISMATCH=yes
BIND=127.0.0.1; eq "...unless something else forwards them" "" "$(rewrite_reason 100.64.0.10)"
ALLOW_ADDRESS_MISMATCH=no
APP_ADDRESSES=box.example.com; eq "APP_ADDRESSES always rewrites" "APP_ADDRESSES=box.example.com" "$(rewrite_reason 127.0.0.1)"
unset APP_ADDRESSES

units="$here/rootfs/usr/lib/systemd/system"
eq "the filter runs before k3s" yes "$(yes_no grep -qx 'Before=k3s.service' "$units/melissa-app-access.service")"
eq "the filter's unit applies the setting" yes \
    "$(yes_no grep -qx 'ExecStart=/usr/bin/melissa-server app-access apply /etc/melissa/app-access.toml' "$units/melissa-app-access.service")"
eq "k3s waits for the filter" yes "$(yes_no grep -qx 'After=melissa-app-access.service' "$units/k3s.service.d/20-app-access.conf")"
eq "k3s doesn't start without the table (fail closed)" yes \
    "$(yes_no grep -q 'nft list table inet melissa_app_access' "$units/k3s.service.d/20-app-access.conf")"
eq "the server may start the filter" yes \
    "$(yes_no grep -q '"melissa-app-access.service"' "$here/rootfs/usr/share/polkit-1/rules.d/52-melissa-app-access.rules")"
eq "the image enables the filter" yes "$(yes_no grep -q 'melissa-ui-forward.socket melissa-app-access.service' "$here/Containerfile")"

# --- server.toml: read and rewrite [apps] addresses, nothing else ---------------------------
toml='listen = "127.0.0.1:8080"

[apps]
addresses = ["100.64.0.10", "203.0.113.5"]
port_offset = 1000

[backend]
name = "x"'
eq "read addresses" "100.64.0.10,203.0.113.5" "$(printf '%s\n' "$toml" | toml_addresses)"
eq "rewrite keeps the rest" 'listen = "127.0.0.1:8080"

[apps]
addresses = ["127.0.0.1"]
port_offset = 1000

[backend]
name = "x"' "$(printf '%s\n' "$toml" | toml_set_addresses 127.0.0.1)"

multi='[apps]
addresses = [
  "100.64.0.10",
  "203.0.113.5",
]
port_offset = 0'
eq "read a multi-line array" "100.64.0.10,203.0.113.5" "$(printf '%s\n' "$multi" | toml_addresses)"
eq "rewrite a multi-line array" '[apps]
addresses = ["203.0.113.5", "100.64.0.10"]
port_offset = 0' "$(printf '%s\n' "$multi" | toml_set_addresses 203.0.113.5,100.64.0.10)"

eq "addresses outside [apps] are not ours" "" "$(printf '[other]\naddresses = ["198.51.100.1"]\n' | toml_addresses)"
eq "no key: added to [apps]" '[apps]
port_offset = 0
addresses = ["127.0.0.1"]
[backend]' "$(printf '[apps]\nport_offset = 0\n[backend]\n' | toml_set_addresses 127.0.0.1)"
eq "no [apps]: appended" 'listen = "127.0.0.1:8080"

[apps]
addresses = ["127.0.0.1"]' "$(printf 'listen = "127.0.0.1:8080"\n' | toml_set_addresses 127.0.0.1)"

# --- Tailscale on the host (Settings → Tailscale: detect first) -----------------------------
tsdir=$(mktemp -d)
SYS_NET="$tsdir"
systemctl() { [ "${FAKE_TAILSCALED:-no}" = yes ]; }  # `systemctl is-active -q tailscaled.service`
HOST_TAILSCALE=auto
eq "no interface, no service: no host Tailscale" no "$(host_tailscale)"
mkdir "$tsdir/tailscale0"
eq "tailscale0 on the host: the host's" yes "$(host_tailscale)"
rmdir "$tsdir/tailscale0"
eq "tailscaled.service active: the host's" yes "$(FAKE_TAILSCALED=yes host_tailscale)"
HOST_TAILSCALE=no
eq "HOST_TAILSCALE=no wins over the check" no "$(FAKE_TAILSCALED=yes host_tailscale)"
HOST_TAILSCALE=yes
eq "HOST_TAILSCALE=yes" yes "$(host_tailscale)"
HOST_TAILSCALE=maybe
eq "a bad HOST_TAILSCALE fails" no "$(yes_no host_tailscale)"
HOST_TAILSCALE=auto
unset -f systemctl
rm -rf "$tsdir"
eq "node name from the host name" melissa-box "$(tailscale_name box.example.com)"
eq "node name is a DNS label" melissa-my-box "$(tailscale_name My_Box)"
eq "TAILSCALE_NAME overrides it" home-server "$(TAILSCALE_NAME=Home-Server tailscale_name box)"
eq "an empty host name still gives a name" melissa "$(tailscale_name '')"

# --- factory reset: the server's request reaches one root unit (no container is reset here) -----
reset_marker=/var/lib/melissa-container/reset/factory-reset.json
eq "the path unit watches the marker" yes \
    "$(yes_no grep -qx "PathExists=$reset_marker" "$units/melissa-factory-reset.path")"
eq "the reset unit runs only with a marker" yes \
    "$(yes_no grep -qx "ConditionPathExists=$reset_marker" "$units/melissa-factory-reset.service")"
eq "a stale marker is finished at boot, before the server and k3s" yes \
    "$(yes_no grep -q '^Before=melissa-server.service k3s.service' "$units/melissa-factory-reset.service")"
eq "the reset unit runs the helper" yes \
    "$(yes_no grep -qx 'ExecStart=/usr/libexec/melissa/factory-reset --unit' "$units/melissa-factory-reset.service")"
eq "the helper is executable" yes "$(yes_no test -x "$here/rootfs/usr/libexec/melissa/factory-reset")"
eq "the helper adds no paths of its own" yes \
    "$(yes_no grep -qx 'exec /usr/bin/melissa-server factory-reset run "$@"' "$here/rootfs/usr/libexec/melissa/factory-reset")"
eq "the server may write requests" yes \
    "$(yes_no grep -qx 'ReadWritePaths=-/var/lib/melissa-container/reset' "$units/melissa-server.service.d/10-container.conf")"
eq "the marker directory belongs to root and the server's group" yes \
    "$(yes_no grep -qx 'd /var/lib/melissa-container/reset 0770 root melissa -' "$here/rootfs/usr/lib/tmpfiles.d/melissa-container.conf")"
eq "the image enables the reset units" yes \
    "$(yes_no grep -q 'melissa-factory-reset.path melissa-factory-reset.service; ' "$here/Containerfile")"
eq "uninstall.sh never runs a reset" no "$(yes_no grep -q factory-reset "$here/uninstall.sh")"

if [ "$fails" != 0 ]; then echo "$fails failed"; exit 1; fi
echo "all install.sh tests passed"
