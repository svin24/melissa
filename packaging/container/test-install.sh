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
eq "preflight ok line names the links, not 0.0.0.0" \
    "ok: app links can use 192.168.1.20, 127.0.0.1 (app ports are published on all addresses; Melissa's filter decides who gets in)" \
    "$(check_addresses 192.168.1.20,127.0.0.1)"
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

# --- host devices: no udevd inside (it shares the host's /dev; uevent replay at every start) ----
eq "packages.txt does not install systemd-udev" no \
    "$(yes_no grep -qE '^systemd-udev([[:space:]]|$)' "$here/packages.txt")"
for u in systemd-udevd.service systemd-udevd-kernel.socket systemd-udevd-control.socket \
    systemd-udev-trigger.service systemd-udev-settle.service systemd-sysctl.service; do
    # Masked in the build step and checked by the image lint (is-enabled = masked).
    eq "the image masks $u" yes "$(yes_if "[ \$(grep -c -- '$u' '$here/Containerfile') -ge 2 ]")"
done
eq "the image lint refuses systemd-udev" yes "$(yes_no grep -qE 'for p in .* systemd-udev; do' "$here/Containerfile")"

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
eq "the host's Tailscale: no port published" "" "$(tailscale_port_line)"
HOST_TAILSCALE=no
eq "Melissa's own: tailscaled's port published" "PublishPort=41641:41641/udp" "$(tailscale_port_line)"
eq "HOST_TAILSCALE=no but the host runs tailscaled: not published" "" \
    "$(FAKE_TAILSCALED=yes tailscale_port_line 2>/dev/null)"
mkdir "$tsdir/tailscale0"
eq "HOST_TAILSCALE=no but tailscale0 on the host: not published" "" "$(tailscale_port_line 2>/dev/null)"
rmdir "$tsdir/tailscale0"
eq "the same port as melissa-tailscaled.service" yes \
    "$(yes_no grep -q -- '--port=41641' "$here/rootfs/usr/lib/systemd/system/melissa-tailscaled.service")"
eq "melissa.container has the port placeholder" yes \
    "$(yes_no grep -qx '@TAILSCALE_PORT_LINE@' "$here/melissa.container")"
eq "Melissa's own: open from other devices in Settings" yes \
    "$(yes_if 'from_elsewhere | grep -q "Settings → Tailscale"')"
HOST_TAILSCALE=yes
eq "the host's Tailscale: the one serve command, with the real UI port" \
    "sudo tailscale serve --bg http://127.0.0.1:9090 (HTTPS on your tailnet), or ssh -L" \
    "$(UI_PORT=9090 from_elsewhere)"
eq "melissa.container tells the server the UI port" yes \
    "$(yes_no grep -qx 'Environment=MELISSA_UI_PORT=@UI_PORT@' "$here/melissa.container")"
eq "the server's unit takes the UI port" yes \
    "$(yes_no grep -qx 'PassEnvironment=MELISSA_UI_PORT' "$here/rootfs/usr/lib/systemd/system/melissa-server.service.d/10-container.conf")"
eq "melissa.container tells the server the UI bind" yes \
    "$(yes_no grep -qx 'Environment=MELISSA_UI_BIND=@UI_BIND@' "$here/melissa.container")"
eq "the server's unit takes the UI bind" yes \
    "$(yes_no grep -qx 'PassEnvironment=MELISSA_UI_BIND' "$here/rootfs/usr/lib/systemd/system/melissa-server.service.d/10-container.conf")"
# [web] trust_forwarded: only for the host's tailscale serve in front of a loopback UI.
UI_BIND=127.0.0.1; eq "host Tailscale, loopback UI: trust the host's serve" yes "$(want_trust)"
UI_BIND=0.0.0.0; eq "host Tailscale, UI on the LAN: never" no "$(want_trust)"
UI_BIND=192.0.2.7; eq "host Tailscale, UI on one LAN address: never" no "$(want_trust)"
UI_BIND=127.0.0.1
eq "trust: a new [web] section" "[web]
$TRUST_LINE" "$(printf '' | toml_set_trust yes)"
eq "trust: appended after [apps]" "[apps]
port_offset = 0

[web]
$TRUST_LINE" "$(printf '[apps]\nport_offset = 0\n' | toml_set_trust yes)"
eq "trust: under an existing [web]" "[web]
$TRUST_LINE
hosts = [\"box.example.com\"]" "$(printf '[web]\nhosts = ["box.example.com"]\n' | toml_set_trust yes)"
eq "trust: the owner's own line is kept" "[web]
trust_forwarded = false" "$(printf '[web]\ntrust_forwarded = false\n' | toml_set_trust yes)"
eq "trust: added once" "[web]
$TRUST_LINE" "$(printf '[web]\n%s\n' "$TRUST_LINE" | toml_set_trust yes)"
eq "trust: install.sh's line removed" "[web]
hosts = []" "$(printf '[web]\n%s\nhosts = []\n' "$TRUST_LINE" | toml_set_trust no)"
eq "trust: no, nothing else changes" "[apps]
port_offset = 0" "$(printf '[apps]\nport_offset = 0\n' | toml_set_trust no)"
HOST_TAILSCALE=no
eq "Melissa's own Tailscale: never trust the main listener" no "$(want_trust)"
HOST_TAILSCALE=maybe
eq "a bad HOST_TAILSCALE fails" no "$(yes_no host_tailscale)"
eq "a bad HOST_TAILSCALE: no trust" no "$(want_trust)"
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
eq "uninstall.sh removes its LIBEXEC copies" yes "$(yes_no grep -qF '"$LIBEXEC/uninstall.sh" "$LIBEXEC/trust.sh" "$LIBEXEC/apparmor.sh"' "$here/uninstall.sh")"
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

# --- AppArmor (Ubuntu): Melissa's own profile for its container; fallback steps only if it fails ---
# Stubs: APPARMOR_SYS is a directory (AppArmor on), the profiles list a file, apparmor_parser a
# function that loads/unloads by writing that file (FAKE_PARSE=fail: the parser refuses).
aa=$(mktemp -d)
APPARMOR_SYS="$aa/sys"; APPARMOR_PROFILES="$aa/sys/profiles"; APPARMOR_DIR="$aa/apparmor.d"; USERNS_SYSCTL="$aa/userns"
mkdir -p "$APPARMOR_SYS" "$APPARMOR_DIR/disable"
fake_parser() {
    echo "apparmor_parser $*" >> "$aa/calls"
    case "$1" in
    -r) [ "${FAKE_PARSE:-ok}" = ok ] || { echo "AppArmor parser error at line 1"; return 1; }
        grep -q '^profile melissa-container flags=(unconfined)' "$2" && echo 'melissa-container (unconfined)' >> "$APPARMOR_PROFILES" ;;
    -R) sed -i '/^melissa-container /d' "$APPARMOR_PROFILES" ;;
    esac
}
APPARMOR_PARSER=fake_parser
printf '%s\n' 'abi <abi/4.0>,' 'profile bwrap /usr/bin/bwrap flags=(attach_disconnected,mediate_deleted) {' '}' \
    'profile unpriv_bwrap flags=(attach_disconnected,mediate_deleted) {' '  audit deny capability,' '}' \
    > "$APPARMOR_DIR/bwrap-userns-restrict"
printf '%s\n' 'profile chrome /opt/google/chrome/chrome flags=(unconfined) {' '  userns,' '}' > "$APPARMOR_DIR/chrome"
echo 1 > "$USERNS_SYSCTL"
loaded() { printf '%s\n' "$@" > "$APPARMOR_PROFILES"; }
ubuntu() { loaded 'chrome (unconfined)' 'unpriv_bwrap (enforce)' 'bwrap (enforce)'; rm -f "$aa/calls" "$APPARMOR_DIR/melissa-container"; }
# setup: apparmor_setup's output, then APPARMOR_PROFILE|APPARMOR_LINE (unless it stopped), then rc=N.
setup() { local o rc; o=$( (apparmor_setup && echo "$APPARMOR_PROFILE|$APPARMOR_LINE") 2>&1 ); rc=$?; printf '%s\nrc=%s\n' "$o" "$rc"; }

# The profile itself: whole container, unconfined apart from Ubuntu's userns mediation, local overrides.
prof="$here/host/apparmor/melissa-container"
eq "profile: abi 4.0 (userns rule)" yes "$(yes_no grep -qx 'abi <abi/4.0>,' "$prof")"
eq "profile: by name, no path attachment" yes "$(yes_no grep -qx 'profile melissa-container flags=(unconfined) {' "$prof")"
eq "profile: allows user namespaces" yes "$(yes_no grep -qx '  userns,' "$prof")"
eq "profile: local additions" yes "$(yes_no grep -qx '  include if exists <local/melissa-container>' "$prof")"
eq "profile: the only rules" 2 "$(grep -cE '^  [a-z]' "$prof")"

# Ubuntu (restriction and bwrap profile on): preflight ok, the profile is installed and loaded.
ubuntu
eq "AppArmor: Ubuntu's restriction is seen" yes "$(yes_no apparmor_restricts)"
eq "AppArmor: preflight ok (the profile will be loaded)" \
    "ok: AppArmor: install.sh loads Melissa's profile melissa-container for its container" "$(check_apparmor)"
out=$(setup)
eq "AppArmor: profile file installed" yes "$(yes_no cmp -s "$prof" "$APPARMOR_DIR/melissa-container")"
eq "AppArmor: loaded with apparmor_parser -r" yes "$(yes_no grep -qx "apparmor_parser -r $APPARMOR_DIR/melissa-container" "$aa/calls")"
eq "AppArmor: verified loaded" yes "$(yes_no apparmor_profile_loaded)"
eq "AppArmor: recorded, Quadlet line set" "yes|PodmanArgs=--security-opt apparmor=melissa-container" "$(tail -n 2 <<<"$out" | head -n 1)"
eq "AppArmor: NOTICE says what and why" yes \
    "$(yes_if 'grep -qF "NOTICE: installed and loaded the AppArmor profile melissa-container" <<<"$out" && grep -qF "lets only that container create user namespaces" <<<"$out" && grep -qF "stay on for everything else" <<<"$out"')"
eq "AppArmor: the host's restriction is left alone" yes \
    "$(yes_if '[ "$(cat "$USERNS_SYSCTL")" = 1 ] && [ -z "$(ls -A "$APPARMOR_DIR/disable")" ] && grep -qx "bwrap (enforce)" "$APPARMOR_PROFILES"')"
eq "AppArmor: nothing else unloaded" "apparmor_parser -r $APPARMOR_DIR/melissa-container" "$(sort -u "$aa/calls")"

# install_files: the Quadlet line and install.env (paths moved to the temp dir, systemctl stubbed).
(
    set +e
    fails=0
    STATE="$aa/state"; QUADLET="$aa/quadlet/melissa.container"; QUADLET_GENERATED="$aa/generated"
    UNITDIR="$aa/units"; LIBEXEC="$aa/libexec"; SHARE="$aa/share"; MODULES_FILE="$aa/modules"
    AUTO_UPDATE=off HOST_TAILSCALE=no APPARMOR_PROFILE=yes
    APPARMOR_LINE="PodmanArgs=$(apparmor_podman_args)"
    systemctl() { :; }
    mkdir -p "$UNITDIR"; : > "$QUADLET_GENERATED"
    install_files
    eq "install_files: Quadlet runs the container under the profile" yes \
        "$(yes_no grep -qx 'PodmanArgs=--security-opt apparmor=melissa-container' "$QUADLET")"
    eq "install_files: no placeholder left" no "$(yes_no grep -qE '^[^#]*@[A-Z_]+@' "$QUADLET")"
    eq "install_files: recorded in install.env" yes "$(. "$STATE/install.env"; [ "$APPARMOR_PROFILE" = yes ] && echo yes)"
    APPARMOR_PROFILE=no APPARMOR_LINE=
    install_files
    eq "install_files, no AppArmor: an empty line" "" "$(grep -n 'apparmor\|^@' "$QUADLET" | grep -v '^[0-9]*:#')"
    eq "install_files, no AppArmor: recorded no" yes "$(. "$STATE/install.env"; [ "$APPARMOR_PROFILE" = no ] && echo yes)"
    # get.sh deletes its checkout: uninstall.sh and what it sources stay on the host.
    eq "install_files: uninstall.sh, trust.sh, apparmor.sh in LIBEXEC" yes \
        "$(yes_if 'for f in uninstall.sh trust.sh apparmor.sh; do [ -x "$LIBEXEC/$f" ] && cmp -s "$here/$f" "$LIBEXEC/$f" || exit 1; done')"
    eq "installed uninstall.sh loads from LIBEXEC" yes \
        "$(yes_if 'bash -c ". \"\$1/uninstall.sh\"; declare -F remove_apparmor && declare -F trust_remove && declare -F apparmor_unload" _ "$LIBEXEC"')"
    eq "installed uninstall.sh: usage works" yes "$(yes_if 'grep -q "uninstall.sh --purge" <<<"$(bash "$LIBEXEC/uninstall.sh" --bad)"')"
    exit "$fails"
) || fails=$((fails + $?))

# Uninstall: unloads and deletes it when install.sh recorded it, else touches nothing.
(
    # shellcheck source=packaging/container/uninstall.sh
    . "$here/uninstall.sh"
    set +e
    fails=0
    APPARMOR_PARSER=fake_parser
    rm -f "$aa/calls"
    out=$(remove_apparmor no)
    eq "uninstall, not recorded: nothing done" "" "$out$(cat "$aa/calls" 2>/dev/null)"
    eq "uninstall, not recorded: profile kept" yes "$(yes_no test -e "$APPARMOR_DIR/melissa-container")"
    out=$(remove_apparmor yes)
    eq "uninstall: apparmor_parser -R" yes "$(yes_no grep -qx "apparmor_parser -R $APPARMOR_DIR/melissa-container" "$aa/calls")"
    eq "uninstall: profile unloaded" no "$(yes_no apparmor_profile_loaded)"
    eq "uninstall: file deleted" no "$(yes_no test -e "$APPARMOR_DIR/melissa-container")"
    eq "uninstall: says so" yes "$(yes_no grep -qF 'unloaded and removed the AppArmor profile melissa-container' <<<"$out")"
    eq "uninstall: again: silent" "" "$(remove_apparmor yes)"
    eq "uninstall: Ubuntu's profiles stay" yes "$(yes_no grep -qx 'bwrap (enforce)' "$APPARMOR_PROFILES")"
    exit "$fails"
) || fails=$((fails + $?))
eq "uninstall.sh reads the record" yes "$(yes_no grep -qx 'remove_apparmor "$APPARMOR_PROFILE"' "$here/uninstall.sh")"

# The load fails on a restricting host: stop with the fallback steps and caveats.
ubuntu; APPARMOR_PROFILE=no; APPARMOR_LINE=
out=$(FAKE_PARSE=fail setup)
eq "load fails: install stops" yes "$(yes_no grep -qx 'rc=1' <<<"$out")"
eq "load fails: nothing recorded" no "$(yes_no grep -q '^yes|' <<<"$out")"
eq "load fails: says why" yes "$(yes_no grep -qF "profile could not be loaded: apparmor_parser -r $APPARMOR_DIR/melissa-container failed: AppArmor parser error" <<<"$out")"
eq "load fails: says what is blocked" yes "$(yes_no grep -qF "agents can't run in their sandbox as their own users" <<<"$out")"
eq "load fails: the fix first" yes "$(yes_no grep -qF "Fix that (the profile needs AppArmor 4" <<<"$out")"
eq "load fails: last resort, disable command" yes \
    "$(yes_no grep -qxF "    sudo ln -s $APPARMOR_DIR/bwrap-userns-restrict $APPARMOR_DIR/disable/" <<<"$out")"
eq "load fails: last resort, unload command" yes \
    "$(yes_no grep -qxF "    sudo apparmor_parser -R $APPARMOR_DIR/bwrap-userns-restrict" <<<"$out")"
eq "load fails: how to undo it" yes \
    "$(yes_no grep -qxF "    sudo rm $APPARMOR_DIR/disable/bwrap-userns-restrict && sudo apparmor_parser -r $APPARMOR_DIR/bwrap-userns-restrict" <<<"$out")"
eq "load fails: userns restricted: the sysctl, persisted" yes \
    "$(yes_if 'grep -qF "/etc/sysctl.d/60-melissa-userns.conf" <<<"$out" && grep -qxF "    sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0" <<<"$out"')"
eq "load fails: caveat, host-wide" yes "$(yes_no grep -qF "for the whole host" <<<"$out")"
eq "load fails: caveat, any program can use bwrap" yes \
    "$(yes_no grep -qF "Any program can then use bwrap to create user namespaces" <<<"$out")"
eq "load fails: caveat, privileged container" yes "$(yes_no grep -qF "rootful and --privileged anyway" <<<"$out")"
eq "load fails: run install.sh again" yes "$(yes_no grep -qF "Then run install.sh again" <<<"$out")"
eq "load fails: the host's AppArmor is not changed" "" "$(ls -A "$APPARMOR_DIR/disable")"
# No apparmor_parser on a restricting host: preflight already stops, with the same steps.
out=$(APPARMOR_PARSER=/nonexistent/apparmor_parser check_apparmor); rc=$?
eq "no apparmor_parser: preflight fails" 1 "$rc"
eq "no apparmor_parser: says so" yes "$(yes_no grep -qF "could not be loaded: apparmor_parser is not installed" <<<"$out")"
eq "no apparmor_parser: how to get it" yes "$(yes_no grep -qF "sudo apt install apparmor" <<<"$out")"
# Restricted only by the sysctl (no bwrap profile): no bwrap commands, only the sysctl.
loaded 'chrome (unconfined)'
out=$(APPARMOR_PARSER=/nonexistent/apparmor_parser check_apparmor)
eq "sysctl only: no bwrap step" no "$(yes_no grep -qF 'sudo ln -s' <<<"$out")"
eq "sysctl only: the sysctl step" yes "$(yes_no grep -qF 'apparmor_restrict_unprivileged_userns=0' <<<"$out")"
# Only bwrap's profile (sysctl 0): no sysctl step.
echo 0 > "$USERNS_SYSCTL"; loaded 'unpriv_bwrap (enforce)'
out=$(APPARMOR_PARSER=/nonexistent/apparmor_parser check_apparmor)
eq "unpriv_bwrap alone restricts" yes "$(yes_no apparmor_restricts)"
eq "bwrap profile only: no sysctl step" no "$(yes_no grep -qF sysctl.d <<<"$out")"
# Loaded, but no file found: a find command instead of a path.
rm "$APPARMOR_DIR/bwrap-userns-restrict"; loaded 'bwrap (enforce)'
eq "no file: says how to find it" yes "$(yes_no grep -qF "find it with" <<<"$(APPARMOR_PARSER=/nonexistent/x check_apparmor)")"
# AppArmor without restrictions (an unconfined bwrap profile, sysctl 0) and no parser: ok, unconfined.
loaded 'bwrap (unconfined)' 'chrome (unconfined)'
eq "unconfined bwrap profile: no restriction" no "$(yes_no apparmor_restricts)"
eq "AppArmor, not restricting, no parser: preflight ok" yes "$(APPARMOR_PARSER=/nonexistent/x yes_no check_apparmor)"
out=$(FAKE_PARSE=fail setup)
eq "not restricting, load fails: install goes on" no "$(yes_no grep -qx 'rc=1' <<<"$out")"
eq "not restricting, load fails: a warning" yes "$(yes_no grep -qF 'WARNING: could not load the AppArmor profile' <<<"$out")"
eq "not restricting, load fails: no Quadlet line" "no|" "$(tail -n 2 <<<"$out" | head -n 1)"
# No AppArmor at all (Fedora): silent, nothing loaded, no Quadlet line.
APPARMOR_SYS="$aa/none"; rm -f "$aa/calls"
eq "no AppArmor: preflight silent" "" "$(check_apparmor)"
eq "no AppArmor: preflight ok" yes "$(yes_no check_apparmor)"
eq "no AppArmor: nothing loaded, no line" "no|" "$(setup | tail -n 2 | head -n 1)"
eq "no AppArmor: apparmor_parser not run" no "$(yes_no test -e "$aa/calls")"
eq "preflight runs the check" yes "$(yes_no grep -qx '    check_apparmor || ok=1' "$here/install.sh")"
eq "install runs the setup after the preflight" yes "$(yes_no grep -qx 'apparmor_setup' "$here/install.sh")"
eq "melissa.container has the placeholder" yes "$(yes_no grep -qx '@APPARMOR_LINE@' "$here/melissa.container")"
eq "the README has the section the message names" yes "$(yes_no grep -q '^### Ubuntu and AppArmor' "$here/README.md")"
eq "usage text covers the header" yes "$(yes_no grep -q "sed -n '2,$(($(grep -n '^set -euo pipefail' "$here/install.sh" | cut -d: -f1) - 1))p'" "$here/install.sh")"
eq "the test scripts run under the profile" yes \
    "$(yes_if 'grep -qF "\"\${aa_args[@]}\"" "$here/escape-test.sh" && grep -qF "\"\${aa_args[@]}\"" "$here/gvisor-test.sh"')"
unset -f loaded ubuntu setup fake_parser
APPARMOR_SYS=/sys/kernel/security/apparmor; APPARMOR_PARSER=apparmor_parser
rm -rf "$aa"

# --- firewalld: checked, never changed ----------------------------------------------------------
# Stubs: systemctl (is firewalld.service active), firewall-cmd (logs its calls), busctl (the running
# StrictForwardPorts), a firewalld.conf in a temp dir.
fw=$(mktemp -d)
FIREWALLD_CONF="$fw/firewalld.conf"
systemctl() { [ "${FAKE_FIREWALLD:-no}" = yes ]; }
fake_fwcmd() {
    echo "firewall-cmd $*" >> "$fw/calls"
    case "$1" in
    --state) [ "${FAKE_FIREWALLD:-no}" = yes ] && echo running ;;
    --get-default-zone) echo FedoraWorkstation ;;
    esac
}
fake_busctl() { [ -z "${FAKE_BUS:-}" ] || echo "s \"$FAKE_BUS\""; }
FIREWALL_CMD=fake_fwcmd BUSCTL=fake_busctl UI_BIND=127.0.0.1 UI_PORT=8080 APP_PORT_BASE=30000 APP_PORT_LAST=30079

FAKE_FIREWALLD=no
eq "no firewalld: preflight silent" "" "$(check_firewalld)"
eq "no firewalld: preflight ok" yes "$(yes_no check_firewalld)"
FAKE_FIREWALLD=yes
printf '%s\n' 'DefaultZone=FedoraWorkstation' '# StrictForwardPorts=yes' 'StrictForwardPorts=no' > "$FIREWALLD_CONF"
eq "Strict: from the config file" no "$(firewalld_strict)"
eq "Strict: the running value wins" yes "$(FAKE_BUS=yes firewalld_strict)"
eq "Strict: no file, no bus: firewalld's default" no "$(FIREWALLD_CONF=/nonexistent firewalld_strict)"
out=$(check_firewalld); rc=$?
eq "Strict=no: preflight ok" 0 "$rc"
eq "Strict=no: names the zone and the setting" yes \
    "$(yes_no grep -qF "ok: firewalld (default zone FedoraWorkstation, StrictForwardPorts=no) lets podman's published ports through;" <<<"$out")"
eq "Strict=no: Melissa's filter decides" yes "$(yes_no grep -qF "Melissa's filter (Settings → Apps) decides" <<<"$out")"
sed -i 's/^StrictForwardPorts=no/StrictForwardPorts=yes/' "$FIREWALLD_CONF"
out=$(check_firewalld); rc=$?
eq "Strict=yes: preflight fails" 1 "$rc"
eq "Strict=yes: says podman refuses" yes "$(yes_no grep -qF "podman then refuses to start" <<<"$out")"
eq "Strict=yes: names the ports" yes "$(yes_no grep -qF "web UI (127.0.0.1:8080) and app ports (30000-30079)" <<<"$out")"
eq "Strict=yes: a zone service would not help" yes "$(yes_no grep -qF "Opening ports or a service in a zone does not help" <<<"$out")"
eq "Strict=yes: the command" yes \
    "$(yes_no grep -qxF "        sudo sed -i 's/^StrictForwardPorts=.*/StrictForwardPorts=no/' $FIREWALLD_CONF" <<<"$out")"
eq "Strict=yes: then reload" yes "$(yes_no grep -qxF "        sudo firewall-cmd --reload" <<<"$out")"
eq "Strict=yes: what it means" yes \
    "$(yes_if 'grep -qF "every rootful container on this host" <<<"$out" && grep -qF "Melissa'"'"'s own filter still decides" <<<"$out"')"
eq "Strict=yes: the command really changes it" no \
    "$(cp "$FIREWALLD_CONF" "$fw/c2"; sed -i 's/^StrictForwardPorts=.*/StrictForwardPorts=no/' "$fw/c2"; FIREWALLD_CONF="$fw/c2" firewalld_strict)"
eq "firewalld: only read, never changed" "" "$(grep -v -e '--state$' -e '--get-default-zone$' "$fw/calls")"
eq "firewalld: config file untouched" yes "$(yes_no grep -qx 'StrictForwardPorts=yes' "$FIREWALLD_CONF")"
eq "preflight runs the check" yes "$(yes_no grep -qx '    check_firewalld || ok=1' "$here/install.sh")"
eq "the README has the section the message names" yes "$(yes_no grep -q '^### firewalld' "$here/README.md")"
unset -f systemctl fake_fwcmd fake_busctl
unset FAKE_FIREWALLD
FIREWALL_CMD=firewall-cmd BUSCTL=busctl FIREWALLD_CONF=/etc/firewalld/firewalld.conf
rm -rf "$fw"

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

# --- melissa-host-update: health probes stay out of the update unit's journal ---------------
hu="$here/host/melissa-host-update"
eq "host-update probes without podman events" yes \
    "$(yes_no grep -q 'podman --events-backend=none --log-level=error exec' "$hu")"
eq "host-update has no other podman exec" no "$(yes_no grep -qE '^[^#]*podman exec' "$hu")"
eq "host-update parses" yes "$(yes_no bash -n "$hu")"

if [ "$fails" != 0 ]; then echo "$fails failed"; exit 1; fi
echo "all install.sh tests passed"
