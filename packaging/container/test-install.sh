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
yes_if() { if eval "$1" >/dev/null 2>&1; then echo yes; else echo no; fi; }  # a compound check

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
# --- The claim line at the end of the install -------------------------------------------------
case "$(claim_line "$(printf 'Setup code: ABCDE-FGHJK\nOpen Melissa...\n')")" in
    *"Setup code: ABCDE-FGHJK "*) eq "the setup code is printed" ok ok ;;
    *) eq "the setup code is printed" ok bad ;;
esac
case "$(claim_line 'This Melissa already has an owner. Lost the password? melissa-server reset-owner')" in
    *"already has an owner"*) eq "a claimed box says sign in" ok ok ;;
    *) eq "a claimed box says sign in" ok bad ;;
esac
case "$(claim_line '')" in
    *"melissa-server setup-code"*) eq "no answer yet: the fallback command" ok ok ;;
    *) eq "no answer yet: the fallback command" ok bad ;;
esac

# --- Docker on the same host (its FORWARD policy drops podman's traffic) ----------------------
# Stubs: iptables answers from FAKE_POLICY/FAKE_CHAIN and logs changes; systemctl from
# FAKE_DOCKER (active|enabled|no) and logs the rest; podman answers the network inspect.
dk=$(mktemp -d)
STATE="$dk/state"; UNITDIR="$dk/units"; mkdir -p "$STATE" "$UNITDIR"
calls="$dk/calls"
iptables() {
    case "$*" in
    "-S DOCKER-USER") [ "${FAKE_CHAIN:-yes}" = yes ] && echo "-N DOCKER-USER" ;;
    "-S FORWARD") printf '%s\n' "-P FORWARD ${FAKE_POLICY:-DROP}" "-A FORWARD -j DOCKER-USER" ;;
    *) echo "iptables $*" >> "$calls" ;;
    esac
}
systemctl() {
    case "$*" in
    "is-active -q docker.service") [ "${FAKE_DOCKER:-no}" = active ] ;;
    "is-enabled -q docker.socket") [ "${FAKE_DOCKER:-no}" = enabled ] ;;
    is-*) return 1 ;;
    *) echo "systemctl $*" >> "$calls" ;;
    esac
}
podman() {
    case "$*" in
    "network inspect podman --format {{range .Subnets}}{{.Subnet}} {{end}}") echo "${FAKE_SUBNETS-10.88.0.0/16 fd00:0:0:1::/64 }" ;;
    *) echo "podman $*" >> "$calls" ;;
    esac
}
reset_dk() { rm -f "$calls" "$UNITDIR"/*; printf 'IMAGE=x\nDOCKER_FORWARD_SUBNETS=""\n' > "$STATE/install.env"; }
unit="$UNITDIR/melissa-docker-forward.service"

eq "subnets: IPv4 only, from podman network inspect" 10.88.0.0/16 "$(podman_subnets)"
eq "Docker's drop policy is seen" yes "$(yes_no docker_drops_forward)"
eq "an ACCEPT policy is not Docker's drop" no "$(FAKE_POLICY=ACCEPT yes_no docker_drops_forward)"
eq "no DOCKER-USER chain: not Docker's" no "$(FAKE_CHAIN=no yes_no docker_drops_forward)"
eq "docker.service active: Docker is here" yes "$(FAKE_DOCKER=active yes_no docker_installed)"
eq "docker.socket enabled: Docker is here" yes "$(FAKE_DOCKER=enabled yes_no docker_installed)"
eq "no Docker" no "$(yes_no docker_installed)"

# The unit (subnet from the stubbed inspect, 10.88.0.0/16 is podman's documented default).
FAKE_SUBNETS="172.30.0.0/16 "
docker_forward_unit "$(podman_subnets)" > "$dk/u"
eq "unit: after Docker, restarted with it" yes \
    "$(yes_if 'grep -qx After=docker.service "$dk/u" && grep -qx PartOf=docker.service "$dk/u"')"
eq "unit: started whenever Docker starts" yes "$(yes_no grep -qx 'WantedBy=docker.service' "$dk/u")"
eq "unit: skipped, not failed, without Docker's chain" yes \
    "$(yes_no grep -qx "ExecCondition=/bin/sh -c 'iptables -S DOCKER-USER >/dev/null 2>&1'" "$dk/u")"
eq "unit: source rule, idempotent, the inspected subnet" yes \
    "$(yes_no grep -qxF "ExecStart=/bin/sh -c 'iptables -C DOCKER-USER -s 172.30.0.0/16 -j ACCEPT 2>/dev/null || iptables -I DOCKER-USER -s 172.30.0.0/16 -j ACCEPT'" "$dk/u")"
eq "unit: destination rule (replies and published app ports)" yes \
    "$(yes_no grep -qxF "ExecStart=/bin/sh -c 'iptables -C DOCKER-USER -d 172.30.0.0/16 -j ACCEPT 2>/dev/null || iptables -I DOCKER-USER -d 172.30.0.0/16 -j ACCEPT'" "$dk/u")"
eq "unit: two rules, nothing else" 2 "$(grep -c '^ExecStart=' "$dk/u")"
eq "unit: not podman's default hard-coded" no "$(yes_no grep -q 10.88 "$dk/u")"
unset FAKE_SUBNETS

# Install on a Docker host: unit written, enabled, applied, recorded, one line said.
reset_dk; DOCKER_FORWARD=auto
out=$(docker_forward); rc=$?
eq "Docker host: docker_forward says Docker is here" 0 "$rc"
eq "Docker host: one line says what and why" \
    "melissa-install: Docker's firewall drops podman traffic; added DOCKER-USER rules for 10.88.0.0/16 (melissa-docker-forward.service)" "$out"
eq "Docker host: the unit is written" yes "$(yes_no grep -q 'DOCKER-USER -s 10.88.0.0/16' "$unit")"
eq "Docker host: enabled and applied now" yes \
    "$(yes_if 'grep -qx "systemctl enable -q melissa-docker-forward.service" "$calls" && grep -qx "systemctl restart melissa-docker-forward.service" "$calls"')"
eq "Docker host: recorded for uninstall" 10.88.0.0/16 "$(env_get DOCKER_FORWARD_SUBNETS)"
eq "Docker host: install.env stays sourceable" 10.88.0.0/16 "$(. "$STATE/install.env"; echo "$DOCKER_FORWARD_SUBNETS")"
eq "Docker host: no rule deleted" no "$(yes_no grep -q 'iptables -D' "$calls")"

# Docker installed but not running yet: the unit waits for it.
reset_dk
out=$(FAKE_CHAIN=no FAKE_DOCKER=enabled docker_forward)
eq "Docker not started: unit installed for later" yes "$(yes_no test -e "$unit")"
eq "Docker not started: says so" yes "$(yes_no grep -q 'when it starts' <<<"$out")"

# A normal host: nothing written, nothing run.
reset_dk
out=$(FAKE_CHAIN=no docker_forward); rc=$?
eq "no Docker: docker_forward says so" 1 "$rc"
eq "no Docker: silent" "" "$out"
eq "no Docker: no unit" no "$(yes_no test -e "$unit")"
eq "no Docker: no systemctl or iptables changes" no "$(yes_no test -s "$calls")"
eq "no Docker: nothing recorded" "" "$(env_get DOCKER_FORWARD_SUBNETS)"

# DOCKER_FORWARD=no: the commands, nothing changed.
reset_dk; DOCKER_FORWARD=no
out=$(docker_forward)
eq "DOCKER_FORWARD=no: no unit" no "$(yes_no test -e "$unit")"
eq "DOCKER_FORWARD=no: nothing run" no "$(yes_no test -s "$calls")"
eq "DOCKER_FORWARD=no: prints both commands" yes \
    "$(yes_if 'grep -qxF "  sudo iptables -I DOCKER-USER -s 10.88.0.0/16 -j ACCEPT" <<<"$out" && grep -qxF "  sudo iptables -I DOCKER-USER -d 10.88.0.0/16 -j ACCEPT" <<<"$out"')"
# ...and on a reinstall it takes out what an earlier install added.
reset_dk; DOCKER_FORWARD=auto; docker_forward >/dev/null; rm -f "$calls"; DOCKER_FORWARD=no
docker_forward >/dev/null
eq "DOCKER_FORWARD=no after auto: unit removed" no "$(yes_no test -e "$unit")"
eq "DOCKER_FORWARD=no after auto: its rules deleted" 2 "$(grep -c '^iptables -D DOCKER-USER' "$calls")"
eq "DOCKER_FORWARD=no after auto: record cleared" "" "$(env_get DOCKER_FORWARD_SUBNETS)"
DOCKER_FORWARD=auto

# A reinstall after podman's subnet changed: the old rules go.
reset_dk; env_set DOCKER_FORWARD_SUBNETS 10.99.0.0/16
docker_forward >/dev/null
eq "subnet changed: old rules deleted" yes "$(yes_no grep -qx 'iptables -D DOCKER-USER -s 10.99.0.0/16 -j ACCEPT' "$calls")"
eq "subnet changed: new one recorded" 10.88.0.0/16 "$(env_get DOCKER_FORWARD_SUBNETS)"

# install_files keeps the record when it rewrites install.env.
eq "install_files keeps DOCKER_FORWARD_SUBNETS" yes \
    "$(yes_no grep -q 'docker_subnets=$(env_get DOCKER_FORWARD_SUBNETS)' "$here/install.sh")"

# Uninstall: removes the unit and the rules only when install.sh added them.
(
    # shellcheck source=packaging/container/uninstall.sh
    . "$here/uninstall.sh"
    set +e
    fails=0
    STATE="$dk/state"; UNITDIR="$dk/units"
    reset_dk; : > "$unit"
    out=$(remove_docker_forward "")
    eq "uninstall, not ours: nothing said" "" "$out"
    eq "uninstall, not ours: no rule deleted" no "$(yes_no test -s "$calls")"
    eq "uninstall, not ours: nothing removed" yes "$(yes_no test -e "$unit")"
    reset_dk; DOCKER_FORWARD=auto; docker_forward >/dev/null; rm -f "$calls"
    out=$(remove_docker_forward "$(env_get DOCKER_FORWARD_SUBNETS)")
    eq "uninstall: unit disabled and stopped" yes \
        "$(yes_no grep -qx 'systemctl disable -q --now melissa-docker-forward.service' "$calls")"
    eq "uninstall: unit file removed" no "$(yes_no test -e "$unit")"
    eq "uninstall: both rules deleted" "iptables -D DOCKER-USER -s 10.88.0.0/16 -j ACCEPT
iptables -D DOCKER-USER -d 10.88.0.0/16 -j ACCEPT" "$(grep '^iptables' "$calls")"
    eq "uninstall: record cleared" "" "$(env_get DOCKER_FORWARD_SUBNETS)"
    eq "uninstall: says so" yes "$(yes_no grep -q 'removed melissa-docker-forward.service' <<<"$out")"
    exit "$fails"
) || fails=$((fails + $?))
eq "uninstall.sh reads the record" yes \
    "$(yes_no grep -qx 'remove_docker_forward "$DOCKER_FORWARD_SUBNETS"' "$here/uninstall.sh")"

# --- the network check after the start (never fails the install, never hangs) -----------------
NET_CHECK=yes
podman() {
    case "$*" in
    "exec melissa timeout 5 getent ahosts example.com") [ "${FAKE_ONLINE:-yes}" = yes ] ;;
    network*) echo "10.88.0.0/16" ;;
    esac
}
timeout() { shift; "$@"; }
sysctl() { echo "${FAKE_FORWARD:-1}"; }
eq "online: no warning" "" "$(network_check no)"
out=$(FAKE_ONLINE=no network_check no); rc=$?
eq "offline: the install goes on" 0 "$rc"
eq "offline: says the container has no network" yes "$(yes_no grep -q 'the container has no network access' <<<"$out")"
eq "offline: names the likely causes" yes "$(yes_no grep -q "another firewall's forward policy" <<<"$out")"
eq "offline: points to the README" yes "$(yes_no grep -q 'No network in the container' <<<"$out")"
eq "offline, forwarding off: says it" yes \
    "$(yes_if 'FAKE_ONLINE=no FAKE_FORWARD=0 network_check no | grep "net.ipv4.ip_forward = 0 here"')"
eq "offline on a Docker host: the rules to check" yes \
    "$(yes_if 'FAKE_ONLINE=no network_check yes | grep "iptables -I DOCKER-USER -d 10.88.0.0/16"')"
eq "NET_CHECK=no: skipped" "" "$(NET_CHECK=no FAKE_ONLINE=no network_check no)"
eq "the check is a lookup with short timeouts" yes \
    "$(yes_no grep -q 'timeout 15 podman exec melissa timeout 5 getent ahosts' "$here/install.sh")"
eq "the README has the sections the messages name" yes \
    "$(yes_if 'grep -q "^### Docker on the same host" "$here/README.md" && grep -q "^### No network in the container" "$here/README.md"')"
unset -f iptables systemctl podman timeout sysctl
rm -rf "$dk"

# --- Ubuntu's bwrap AppArmor profile: preflight stops with instructions, never changes AppArmor ---
aa=$(mktemp -d)
APPARMOR_PROFILES="$aa/profiles"; APPARMOR_DIR="$aa/apparmor.d"; USERNS_SYSCTL="$aa/userns"
mkdir -p "$APPARMOR_DIR/disable"
printf '%s\n' 'abi <abi/4.0>,' 'profile bwrap /usr/bin/bwrap flags=(attach_disconnected,mediate_deleted) {' '}' \
    'profile unpriv_bwrap flags=(attach_disconnected,mediate_deleted) {' '  audit deny capability,' '}' \
    > "$APPARMOR_DIR/bwrap-userns-restrict"
printf '%s\n' 'profile chrome /opt/google/chrome/chrome flags=(unconfined) {' '  userns,' '}' > "$APPARMOR_DIR/chrome"
printf '%s\n' 'profile bwrap /usr/bin/bwrap flags=(unconfined) {' '  userns,' '}' > "$aa/own-bwrap"
echo 1 > "$USERNS_SYSCTL"
loaded() { printf '%s\n' "$@" > "$APPARMOR_PROFILES"; }

loaded 'chrome (unconfined)' 'unpriv_bwrap (enforce)' 'bwrap (enforce)'
eq "AppArmor: Ubuntu's bwrap profile is seen" yes "$(yes_no bwrap_profile_loaded)"
eq "AppArmor: its file is found" "$APPARMOR_DIR/bwrap-userns-restrict" "$(bwrap_profile_files)"
out=$(check_bwrap_apparmor); rc=$?
eq "AppArmor: preflight fails" 1 "$rc"
eq "AppArmor: says what is blocked" yes "$(yes_no grep -q "agents can't run as their own users" <<<"$out")"
eq "AppArmor: the disable command" yes \
    "$(yes_no grep -qxF "    sudo ln -s $APPARMOR_DIR/bwrap-userns-restrict $APPARMOR_DIR/disable/" <<<"$out")"
eq "AppArmor: the unload command" yes \
    "$(yes_no grep -qxF "    sudo apparmor_parser -R $APPARMOR_DIR/bwrap-userns-restrict" <<<"$out")"
eq "AppArmor: how to undo it" yes \
    "$(yes_no grep -qxF "    sudo rm $APPARMOR_DIR/disable/bwrap-userns-restrict && sudo apparmor_parser -r $APPARMOR_DIR/bwrap-userns-restrict" <<<"$out")"
eq "AppArmor: userns restricted: the sysctl, persisted" yes \
    "$(yes_if 'grep -qF "/etc/sysctl.d/60-melissa-userns.conf" <<<"$out" && grep -qxF "    sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0" <<<"$out"')"
eq "AppArmor: caveat, host-wide" yes "$(yes_no grep -qF "for the whole host" <<<"$out")"
eq "AppArmor: caveat, any program can use bwrap" yes \
    "$(yes_no grep -qF "Any program can then use bwrap to create user namespaces" <<<"$out")"
eq "AppArmor: caveat, privileged container" yes "$(yes_no grep -qF "rootful and --privileged anyway" <<<"$out")"
eq "AppArmor: caveat, untested" yes "$(yes_no grep -qF "Not tested by us on an Ubuntu install" <<<"$out")"
eq "AppArmor: run install.sh again" yes "$(yes_no grep -qF "Then run install.sh again" <<<"$out")"
eq "AppArmor: nothing changed" "" "$(ls -A "$APPARMOR_DIR/disable")"
eq "AppArmor: install.sh never runs apparmor_parser or writes disable/" no \
    "$(yes_no grep -qE '^[[:space:]]*(apparmor_parser|ln|aa-disable|aa-complain) ' "$here/install.sh")"
echo 0 > "$USERNS_SYSCTL"
out=$(check_bwrap_apparmor)
eq "AppArmor: userns not restricted: no sysctl step" no "$(yes_no grep -qF sysctl.d <<<"$out")"
# Only unpriv_bwrap loaded is enough.
loaded 'unpriv_bwrap (enforce)'
eq "AppArmor: unpriv_bwrap alone is seen" yes "$(yes_no bwrap_profile_loaded)"
# The unconfined bwrap profile docs/DEV-ENV.md suggests is not Ubuntu's restriction.
rm "$APPARMOR_DIR/bwrap-userns-restrict"; mv "$aa/own-bwrap" "$APPARMOR_DIR/bwrap"
loaded 'bwrap (unconfined)' 'chrome (unconfined)'
eq "AppArmor: an unconfined bwrap profile is fine" no "$(yes_no bwrap_profile_loaded)"
eq "AppArmor: ...and its file is not listed" "" "$(bwrap_profile_files)"
eq "AppArmor: ...preflight ok" "ok: no AppArmor profile restricts bwrap" "$(check_bwrap_apparmor)"
# Loaded, but no file found: a find command instead of a path.
loaded 'bwrap (enforce)'
eq "AppArmor: no file: says how to find it" yes "$(yes_no grep -qF "find it with" <<<"$(check_bwrap_apparmor)")"
# No AppArmor at all (Fedora): silent, ok.
APPARMOR_PROFILES="$aa/none"
eq "no AppArmor: preflight ok, silent" "" "$(check_bwrap_apparmor)"
eq "no AppArmor: ok status" yes "$(yes_no check_bwrap_apparmor)"
eq "preflight runs the check" yes "$(yes_no grep -qx '    check_bwrap_apparmor || ok=1' "$here/install.sh")"
eq "the README has the section the message names" yes "$(yes_no grep -q '^### Ubuntu and AppArmor' "$here/README.md")"
eq "usage text covers the header" yes "$(yes_no grep -q "sed -n '2,$(($(grep -n '^set -euo pipefail' "$here/install.sh" | cut -d: -f1) - 1))p'" "$here/install.sh")"
unset -f loaded
rm -rf "$aa"

# --- which image: published by default, a reinstall keeps the recorded one ------------------
tr_dir=$(mktemp -d)
img() { # [VAR=value ...]: IMAGE and AUTO_UPDATE as install.sh picks them
    (unset IMAGE AUTO_UPDATE; export INSTALL_ENV="$tr_dir/install.env"
     for kv in "$@"; do export "${kv?}"; done
     . "$here/install.sh"; echo "$IMAGE $AUTO_UPDATE")
}
eq "fresh install: published edge, registry updates" "ghcr.io/svin24/melissa:edge registry" "$(img)"
eq "CHANNEL=stable" "ghcr.io/svin24/melissa:stable registry" "$(img CHANNEL=stable)"
eq "CHANNEL must be edge or stable" no "$(yes_if '(export INSTALL_ENV=/nonexistent CHANNEL=nightly; . "$here/install.sh")')"
printf 'IMAGE=localhost/melissa:latest\nAUTO_UPDATE=off\n' > "$tr_dir/install.env"
eq "reinstall keeps a local build and its update mode" "localhost/melissa:latest off" "$(img)"
eq "IMAGE moves a local build to the published one" "ghcr.io/svin24/melissa:edge registry" \
    "$(img IMAGE=ghcr.io/svin24/melissa:edge)"
eq "published image is recognised" yes "$(yes_no is_published ghcr.io/svin24/melissa:edge)"
eq "by digest too" yes "$(yes_no is_published ghcr.io/svin24/melissa@sha256:abc)"
eq "a fork's image is not" no "$(yes_no is_published ghcr.io/svin24/melissa-fork:edge)"
eq "a local build is not" no "$(yes_no is_published localhost/melissa:latest)"

# --- signature policy (trust.sh): one entry added and removed, the rest untouched ---------------
for tool in jq python3; do
    command -v "$tool" >/dev/null || { echo "skip: $tool not installed"; continue; }
    (
        export POLICY_TOOL=$tool POLICY="$tr_dir/$tool/etc/policy.json" POLICY_DEFAULT="$tr_dir/default.json" \
            REGISTRIES_D="$tr_dir/$tool/registries.d" SIGNING_KEY="$tr_dir/$tool/pki/melissa.pub"
        printf '{"default":[{"type":"insecureAcceptAnything"}],"transports":{"docker-daemon":{"":[{"type":"insecureAcceptAnything"}]}}}\n' > "$POLICY_DEFAULT"
        eq "$tool: no policy yet: created from the default" created "$(trust_install "$here/melissa-signing.pub")"
        eq "$tool: entry requires our key" "sigstoreSigned $SIGNING_KEY matchRepository" \
            "$(jq -r '.transports.docker["ghcr.io/svin24/melissa"][0] | "\(.type) \(.keyPath) \(.signedIdentity.type)"' "$POLICY")"
        eq "$tool: default kept" insecureAcceptAnything "$(jq -r '.default[0].type' "$POLICY")"
        eq "$tool: key installed" yes "$(yes_no cmp -s "$here/melissa-signing.pub" "$SIGNING_KEY")"
        eq "$tool: sigstore attachments on" yes "$(yes_no grep -q 'use-sigstore-attachments: true' "$REGISTRIES_D/melissa.yaml")"
        eq "$tool: second run changes nothing new" "" "$(trust_install "$here/melissa-signing.pub")"
        trust_remove yes
        eq "$tool: created and back to default: deleted" no "$(yes_no test -e "$POLICY")"
        eq "$tool: key and registries.d file gone" no "$(yes_if 'test -e "$SIGNING_KEY" || test -e "$REGISTRIES_D/melissa.yaml"')"
        # An existing policy with the owner's own entries.
        mkdir -p "$(dirname "$POLICY")"
        printf '{"default":[{"type":"reject"}],"transports":{"docker":{"example.com/x":[{"type":"insecureAcceptAnything"}]}}}\n' > "$POLICY"
        eq "$tool: existing policy is edited, not created" "" "$(trust_install "$here/melissa-signing.pub")"
        eq "$tool: the owner's entries stay" "reject insecureAcceptAnything" \
            "$(jq -r '"\(.default[0].type) \(.transports.docker["example.com/x"][0].type)"' "$POLICY")"
        trust_remove no
        eq "$tool: removal keeps the owner's policy" '["example.com/x"]' "$(jq -c '.transports.docker | keys' "$POLICY")"
        [ "$fails" = 0 ]
    ) || fails=$((fails + 1))
done
eq "uninstall.sh removes the check" yes "$(yes_no grep -q 'trust_remove "$POLICY_CREATED"' "$here/uninstall.sh")"
eq "the public key is a cosign (PEM) key" yes "$(yes_no grep -q 'BEGIN PUBLIC KEY' "$here/melissa-signing.pub")"
rm -rf "$tr_dir"

if [ "$fails" != 0 ]; then echo "$fails failed"; exit 1; fi
echo "all install.sh tests passed"
