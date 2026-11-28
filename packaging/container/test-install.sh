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
# Without systemd-sysctl inside, the Quadlet sets the container netns's keys; only net.* (namespaced).
sysctl_keys=$(sed -n 's/^Sysctl=//p' "$here/melissa.container" | tr -d '"' | tr ' ' '\n' | grep '=' | cut -d= -f1)
for k in net.ipv4.conf.default.rp_filter net.ipv4.conf.eth0.rp_filter \
    net.ipv4.conf.default.accept_source_route net.ipv4.ping_group_range; do
    eq "melissa.container sets $k in the container netns" yes "$(yes_if "printf '%s\n' '$sysctl_keys' | grep -qx '$k'")"
done
eq "melissa.container sets only net.* sysctls (never host-wide ones)" "" "$(printf '%s\n' "$sysctl_keys" | grep -v '^net\.')"
eq "ping_group_range keeps its two numbers" yes \
    "$(yes_no grep -qx 'Sysctl="net.ipv4.ping_group_range=0 2147483647"' "$here/melissa.container")"
nsconf="$here/rootfs/usr/lib/melissa/netns-sysctl.conf"
eq "netns-sysctl.conf has the Quadlet's keys and values" \
    "$(sed -n 's/^Sysctl=//p' "$here/melissa.container" | tr -d '"' | sed 's/ \(net\.\)/\n\1/g' | sort)" \
    "$(grep -v -e '^#' -e '^$' "$nsconf" | sort)"
eq "netns-sysctl.conf holds only net.* keys" "" "$(grep -v -e '^#' -e '^$' -e '^net\.' "$nsconf")"
eq "the image enables melissa-netns-sysctl" yes \
    "$(yes_no grep -q 'melissa-netns-sysctl.service melissa-factory-reset.path melissa-factory-reset.service; ' "$here/Containerfile")"
eq "melissa-netns-sysctl runs before k3s and Tailscale" yes \
    "$(yes_no grep -qE '^Before=k3s.service melissa-tailscaled.service' "$here/rootfs/usr/lib/systemd/system/melissa-netns-sysctl.service")"

# netns-sysctl with stand-ins for systemd-detect-virt, ip and sysctl (they log what they're asked).
ns=$(mktemp -d)
mkdir -p "$ns/bin" "$ns/proc/net/ipv4/conf/default" "$ns/proc/net/ipv4/conf/eth0"
for k in rp_filter accept_source_route promote_secondaries; do
    : > "$ns/proc/net/ipv4/conf/default/$k"; : > "$ns/proc/net/ipv4/conf/eth0/$k"; done
: > "$ns/proc/net/ipv4/ping_group_range"
cat > "$ns/bin/systemd-detect-virt" <<'STUB'
#!/bin/sh
[ "$NS_VIRT" = podman ]
STUB
cat > "$ns/bin/ip" <<'STUB'
#!/bin/sh
case "$*" in
    *"route show default"*) echo "$NS_ROUTE" ;;
    *"link show dev eth0"*) echo "2: eth0@if21: <UP> mtu 1500 \    link/ether 02:00:00:00:00:01 \    $NS_KIND addrgenmode eui64" ;;
esac
STUB
cat > "$ns/bin/sysctl" <<'STUB'
#!/bin/sh
echo "$3" >> "$NS_LOG"
STUB
chmod +x "$ns/bin/"*
netns_sysctl() { # virt, default route, eth0 kind, [conf] -> keys written, one per line
    : > "$ns/log"
    PATH="$ns/bin:$PATH" NS_VIRT=$1 NS_ROUTE=$2 NS_KIND=$3 NS_LOG="$ns/log" NETNS_SYSCTL_PROC="$ns/proc" \
        NETNS_SYSCTL_CONF=${4:-$nsconf} sh "$here/rootfs/usr/libexec/melissa-container/netns-sysctl" >/dev/null
    echo "rc=$?"; cat "$ns/log"
}
bridge_route="default via 10.88.0.1 dev eth0 proto static metric 100"
eq "netns-sysctl: podman bridge sets every key" "rc=0 $(grep -v -e '^#' -e '^$' "$nsconf" | tr '\n' ' ')" \
    "$(netns_sysctl podman "$bridge_route" veth | tr '\n' ' ')"
eq "netns-sysctl: not in a container, nothing" "rc=0" "$(netns_sysctl none "$bridge_route" veth)"
eq "netns-sysctl: the host's network (default via enp1s0), nothing" "rc=0" \
    "$(netns_sysctl podman "default via 192.0.2.1 dev enp1s0 proto dhcp" ether)"
eq "netns-sysctl: eth0 that is not a veth, nothing" "rc=0" "$(netns_sysctl podman "$bridge_route" ether)"
eq "netns-sysctl: no default route, nothing" "rc=0" "$(netns_sysctl podman "" veth)"
printf '%s\n' 'kernel.core_pattern=|/bin/false' 'vm.max_map_count=1' 'net.ipv4.conf.wlan9.rp_filter=2' \
    'net.ipv4.conf.eth0.rp_filter=2' > "$ns/bad.conf"
eq "netns-sysctl: refuses host-wide keys, skips missing ones, sets the rest" "rc=0 net.ipv4.conf.eth0.rp_filter=2" \
    "$(netns_sysctl podman "$bridge_route" veth "$ns/bad.conf" | tr '\n' ' ' | sed 's/ $//')"
rm -rf "$ns"

q=$(command -v /usr/libexec/podman/quadlet /usr/lib/podman/quadlet 2>/dev/null | head -n1)
if [ -n "$q" ]; then
    qd=$(mktemp -d)
    sed -e 's/@[A-Z_]*_LINE@//' -e 's/@[A-Z_]*@/1/g' "$here/melissa.container" > "$qd/melissa.container"
    gen=$(QUADLET_UNIT_DIRS="$qd" "$q" -dryrun 2>/dev/null)
    eq "Quadlet passes the sysctls to podman" 7 "$(printf '%s\n' "$gen" | grep '^ExecStart=' | grep -o -- '--sysctl' | wc -l | tr -d ' ')"
    eq "Quadlet keeps ping_group_range as one value" yes \
        "$(yes_if "printf '%s\n' \"\$gen\" | grep -qF 'net.ipv4.ping_group_range=0\\x202147483647'")"
    rm -rf "$qd"
else
    echo "skip: no quadlet here (Quadlet dry run of melissa.container)"
fi

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
    eq "install_files: the update time in install.env" daily-03 "$(. "$STATE/install.env"; echo "$SCHEDULE")"
    eq "install_files: the timer's drop-in" yes \
        "$(yes_no grep -qx 'OnCalendar=\*-\*-\* 03:00:00' "$UNITDIR/melissa-auto-update.timer.d/schedule.conf")"
    eq "install_files: Melissa's timer units installed" yes \
        "$(yes_if 'for u in melissa-auto-update.timer melissa-auto-update.service melissa-check.timer melissa-check.service; do [ -e "$UNITDIR/$u" ] || exit 1; done')"
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

# --- melissa-host-update: the owner's schedule, requests, check without download, held versions ---
eq "the Quadlet has no AutoUpdate= (Melissa's own timer updates her)" no \
    "$(yes_no grep -qE '^(AutoUpdate=|@AUTO_UPDATE_LINE@)' "$here/melissa.container")"
eq "auto-update timer: no catch-up at boot" yes "$(yes_no grep -qx 'Persistent=false' "$here/host/melissa-auto-update.timer")"
eq "auto-update timer: within 15 minutes" yes "$(yes_no grep -qx 'RandomizedDelaySec=900' "$here/host/melissa-auto-update.timer")"
eq "auto-update service runs auto" yes \
    "$(yes_no grep -qx 'ExecStart=/usr/local/libexec/melissa/melissa-host-update auto' "$here/host/melissa-auto-update.service")"
eq "check service only checks" yes \
    "$(yes_no grep -qx 'ExecStart=/usr/local/libexec/melissa/melissa-host-update check' "$here/host/melissa-check.service")"
eq "install.sh no longer enables podman-auto-update.timer" no \
    "$(yes_no grep -qE '^[^#]*enable[^#]*podman-auto-update.timer' "$here/install.sh")"
eq "uninstall.sh removes Melissa's timers" yes \
    "$(yes_if 'grep -q "melissa-auto-update.timer melissa-check.timer" "$here/uninstall.sh" && grep -q "melissa-auto-update.timer.d" "$here/uninstall.sh"')"
eq "SCHEDULE: default daily-03" "daily-03" "$(unset SCHEDULE; INSTALL_ENV=/nonexistent; . "$here/install.sh"; echo "$SCHEDULE")"
eq "SCHEDULE: refused when malformed" no "$(yes_if '(export INSTALL_ENV=/nonexistent SCHEDULE="daily-3"; . "$here/install.sh")')"
eq "AUTO_UPDATE: refused when unknown" no "$(yes_if '(export INSTALL_ENV=/nonexistent AUTO_UPDATE=sometimes; . "$here/install.sh")')"
hu_t=$(mktemp -d)
printf 'IMAGE=localhost/melissa:latest\nAUTO_UPDATE=off\nSCHEDULE=sun-05\n' > "$hu_t/env"
eq "SCHEDULE: a reinstall keeps the owner's" "sun-05" "$(unset SCHEDULE; INSTALL_ENV="$hu_t/env"; . "$here/install.sh"; echo "$SCHEDULE")"
(
    # shellcheck source=packaging/container/host/melissa-host-update
    . "$hu"
    set +e
    fails=0
    STATE="$hu_t/state" UNITDIR="$hu_t/units"
    paths
    mkdir -p "$REQUESTS" "$STATUS"
    calls="$hu_t/calls"
    systemctl() { echo "systemctl $*" >> "$calls"; [ "$1" != show ] || echo "@1764385200"; }
    # podman: Melissa runs image "cur" (pulled by digest sha256:aaa1); nothing else is stored.
    podman() {
        case "$*" in
        "container inspect melissa --format {{.Image}}") echo cur ;;
        "image inspect "*"{{.Id}}") echo cur ;;
        "image inspect cur --format {{range .RepoDigests}}{{.}} {{end}}") echo "ghcr.io/svin24/melissa@sha256:aaa1 " ;;
        "image inspect "*"--format {{index .Labels"*) echo 0.0.3 ;;
        "image inspect cur --format "*) echo "cur|sha256:aaa1|2026-11-26T10:00:00Z|0.0.3|7c41d0e" ;;
        "image exists "*) return 1 ;;
        *) echo "podman $*" >> "$calls"; return 1 ;;
        esac
    }
    skopeo() { echo "$SKOPEO_OUT"; }
    IMAGE=ghcr.io/svin24/melissa:edge AUTO_UPDATE=registry SCHEDULE=daily-03

    eq "OnCalendar: every day" '*-*-* 03:00:00' "$(on_calendar daily-03)"
    eq "OnCalendar: one weekday" 'Sun *-*-* 23:00:00' "$(on_calendar sun-23)"
    for s in daily-00 mon-09 sun-23; do eq "schedule $s is valid" yes "$(yes_no valid_schedule "$s")"; done
    for s in daily-24 daily-3 monday-03 sun-03x "daily-03;x" "../x-03" ""; do
        eq "schedule '$s' is refused" no "$(yes_no valid_schedule "$s")"; done

    # Requests: a time, auto off, and names that are not ours (deleted, never acted on).
    : > "$ENV_FILE"
    touch "$REQUESTS/schedule-sun-05" "$REQUESTS/auto-off" "$REQUESTS/schedule-xyz-99" "$REQUESTS/reboot"
    requests > /dev/null
    eq "requests: the directory is emptied" "" "$(ls -A "$REQUESTS")"
    eq "requests: install.env has the owner's choice" "AUTO_UPDATE=off SCHEDULE=sun-05" \
        "$(grep -E '^(AUTO_UPDATE|SCHEDULE)=' "$ENV_FILE" | paste -sd ' ')"
    eq "requests: the drop-in has the time" yes "$(yes_no grep -qx 'OnCalendar=Sun \*-\*-\* 05:00:00' "$DROPIN")"
    eq "requests: the drop-in resets the default first" yes "$(yes_no grep -qx 'OnCalendar=' "$DROPIN")"
    eq "requests: off disables the timer" yes "$(yes_no grep -qx 'systemctl disable -q --now melissa-auto-update.timer' "$calls")"
    eq "requests: the check timer stays on" yes "$(yes_no grep -qx 'systemctl enable -q --now melissa-check.timer' "$calls")"
    eq "requests: status.json has the schedule and no next run while off" yes \
        "$(yes_if 'grep -q "\"schedule\":\"sun-05\",\"next_run\":null" "$STATUS/status.json"')"
    : > "$calls"
    touch "$REQUESTS/auto-on"
    requests > /dev/null
    eq "auto-on: registry image → registry" "AUTO_UPDATE=registry" "$(grep '^AUTO_UPDATE=' "$ENV_FILE")"
    eq "auto-on: timer enabled and restarted (new time, no run now)" yes \
        "$(yes_if 'grep -qx "systemctl enable -q melissa-auto-update.timer" "$calls" && grep -qx "systemctl restart melissa-auto-update.timer" "$calls"')"
    eq "auto-on: next run in status.json" yes "$(yes_no grep -q '"next_run":1764385200' "$STATUS/status.json")"
    eq "status.json is valid JSON" yes "$(yes_no python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$STATUS/status.json")"

    # Check: never downloads, compares the tag's digest with the running image's.
    : > "$calls"
    SKOPEO_OUT="sha256:aaa1|0.0.3|7c41d0e|100 200 "
    check > /dev/null
    eq "check: same digest is up to date" up_to_date "$(sed -n 's/.*"kind":"\([^"]*\)".*/\1/p' "$STATUS/last.json")"
    eq "check: no available update" yes "$(yes_no grep -q '"available":null' "$STATUS/status.json")"
    eq "check: nothing pulled" no "$(yes_no grep -q 'podman pull' "$calls")"
    eq "check: no action left behind" no "$(yes_no test -e "$STATUS/action.json")"
    SKOPEO_OUT="sha256:bbb2|0.0.4|9e2b5f1|100 200 "
    check > /dev/null
    eq "check: a new digest is available, with its version and size" yes \
        "$(yes_no grep -q '"available":{"id":"","digest":"sha256:bbb2","created":"","version":"0.0.4","revision":"9e2b5f1","size":300}' "$STATUS/status.json")"
    eq "check: last says available" available "$(sed -n 's/.*"kind":"\([^"]*\)".*/\1/p' "$STATUS/last.json")"
    eq "status.json with an update is valid JSON" yes "$(yes_no python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$STATUS/status.json")"
    # A version that failed is held: not offered, and auto skips it.
    printf '{"id":"x","digests":"sha256:bbb2","version":"0.0.4","revision":"9e2b5f1","reason":"failed","at":1,"message":""}\n' > "$STATE/held.json"
    check > /dev/null
    eq "held: not available" yes "$(yes_no grep -q '"available":null' "$STATUS/status.json")"
    eq "held: shown in status.json" yes "$(yes_no grep -q '"held":{"id":"x"' "$STATUS/status.json")"
    eq "held: last says held" held "$(sed -n 's/.*"kind":"\([^"]*\)".*/\1/p' "$STATUS/last.json")"
    update auto > /dev/null
    eq "held: auto skips it without a download" yes \
        "$(yes_if '! grep -q "podman pull" "$calls" && grep -q "\"action\":\"auto\",\"ok\":true,\"at\":[0-9]*,\"kind\":\"held\"" "$STATUS/last.json"')"
    SKOPEO_OUT="sha256:ccc3|0.0.5|aaaaaaa|100 "
    check > /dev/null
    eq "held: a newer version lifts the hold" no "$(yes_no test -e "$STATE/held.json")"
    eq "held: the newer one is available" yes "$(yes_no grep -q '"version":"0.0.5"' "$STATUS/status.json")"
    AUTO_UPDATE=off
    eq "auto with automatic updates off does nothing" "melissa-host-update: automatic updates are off" "$(auto)"
    # The registry can't be reached: a failed check, the page's state stays readable.
    skopeo() { echo "pinging container registry: connection refused"; return 1; }
    check > /dev/null
    eq "check: unreachable registry is a failed check" "false check" \
        "$(sed -n 's/.*"ok":\([a-z]*\),.*"kind":"\([^"]*\)".*/\1 \2/p' "$STATUS/last.json")"
    exit "$fails"
) || fails=$((fails + $?))
# Apply: download with progress, restart, health check; a new image that doesn't start is rolled
# back and held. podman and systemctl are stand-ins that keep "what runs" in a file.
(
    # shellcheck source=packaging/container/host/melissa-host-update
    . "$hu"
    set +e
    fails=0
    STATE="$hu_t/apply" UNITDIR="$hu_t/units2"
    paths
    mkdir -p "$REQUESTS" "$STATUS"
    calls="$hu_t/acalls"; : > "$calls"
    echo cur > "$hu_t/running"; echo cur > "$hu_t/tagged"
    IMAGE=ghcr.io/svin24/melissa:edge AUTO_UPDATE=registry SCHEDULE=daily-03 START_WAIT=0
    systemctl() {
        echo "systemctl $*" >> "$calls"
        case "$1" in
        restart) cp "$hu_t/tagged" "$hu_t/running" ;;
        is-active) [ "$NEW_STARTS" = yes ] ;;
        esac
    }
    podman() {
        case "$*" in
        "container inspect melissa --format {{.Image}}") cat "$hu_t/running" ;;
        "image inspect ghcr.io/svin24/melissa:edge --format {{.Id}}") cat "$hu_t/tagged" ;;
        "image inspect "*"{{.Id}}") echo "${3}" ;;
        "image inspect cur --format {{range .RepoDigests}}{{.}} {{end}}") echo "ghcr.io/svin24/melissa@sha256:aaa1 " ;;
        "image inspect new --format {{range .RepoDigests}}{{.}} {{end}}") echo "ghcr.io/svin24/melissa@sha256:bbb2 " ;;
        "image inspect cur --format {{index .Labels \"org.opencontainers.image.version\"}}") echo 0.0.3 ;;
        "image inspect new --format {{index .Labels \"org.opencontainers.image.version\"}}") echo 0.0.4 ;;
        "image inspect "*"--format {{index .Labels"*) echo abc1234 ;;
        "image inspect "*) echo "$3|sha256:x|2026-11-26T10:00:00Z|0.0.x|abc1234" ;;
        "pull -q ghcr.io/svin24/melissa:edge") echo new > "$hu_t/tagged"; echo "podman pull" >> "$calls" ;;
        "tag cur ghcr.io/svin24/melissa:edge") echo cur > "$hu_t/tagged"; echo "podman tag back" >> "$calls" ;;
        "--events-backend=none --log-level=error exec melissa systemctl is-active -q "*) [ "$NEW_STARTS" = yes ] ;;
        "image exists "*) return 1 ;;
        *) echo "podman $*" >> "$calls"; return 1 ;;
        esac
    }
    skopeo() { echo "sha256:bbb2|0.0.4|9e2b5f1|100 200 "; }
    sleep() { :; }
    NEW_STARTS=no
    update apply > /dev/null
    eq "apply: downloaded and restarted" yes "$(yes_if 'grep -qx "podman pull" "$calls" && grep -qx "systemctl restart melissa.service" "$calls"')"
    eq "apply, no start: back on the old image" "cur" "$(cat "$hu_t/running")"
    eq "apply, no start: tagged back" yes "$(yes_no grep -qx 'podman tag back' "$calls")"
    eq "apply, no start: the new one is held as failed" yes \
        "$(yes_if 'grep -q "\"id\":\"new\",\"digests\":\"sha256:bbb2\",\"version\":\"0.0.4\"" "$STATE/held.json" && grep -q "\"reason\":\"failed\"" "$STATE/held.json"')"
    eq "apply, no start: last says no_start for 0.0.4" yes \
        "$(yes_if 'grep -q "\"ok\":false" "$STATUS/last.json" && grep -q "\"kind\":\"no_start\"" "$STATUS/last.json" && grep -q "\"version\":\"0.0.4\"" "$STATUS/last.json"')"
    eq "apply, no start: no action left behind" no "$(yes_no test -e "$STATUS/action.json")"
    # Try again (manual apply ignores the hold) and this time it starts.
    : > "$calls"; NEW_STARTS=yes START_WAIT=60
    update apply > /dev/null
    eq "apply: running the new image" new "$(cat "$hu_t/running")"
    eq "apply: the hold is lifted" no "$(yes_no test -e "$STATE/held.json")"
    eq "apply: just_updated from 0.0.3 to 0.0.4" yes \
        "$(yes_if 'grep -q "\"from\":\"0.0.3\",\"from_revision\":\"abc1234\",\"to\":\"0.0.4\"" "$STATE/updated.json"')"
    eq "apply: just_updated in status.json" yes "$(yes_no grep -q '"just_updated":{"from":"0.0.3"' "$STATUS/status.json")"
    eq "apply: last says updated" updated "$(sed -n 's/.*"kind":"\([^"]*\)".*/\1/p' "$STATUS/last.json")"
    eq "apply: no in-progress marker left" no "$(yes_no test -e "$STATE/in-progress")"
    # While images switch, a start in between doesn't move the rollback tag.
    echo cur > "$STATE/running-id"; printf 'apply\ncur\nnew\n0.0.3\n0.0.4\n' > "$STATE/in-progress"
    : > "$calls"
    note_running
    eq "in progress: running-id kept" cur "$(cat "$STATE/running-id")"
    eq "in progress: nothing tagged" no "$(yes_no grep -q 'podman tag' "$calls")"
    # Cut off after the restart (a reboot): the new image runs and answers -> finished.
    rm -f "$STATE/updated.json"
    resume > /dev/null
    eq "resume: healthy new image -> updated" updated "$(sed -n 's/.*"kind":"\([^"]*\)".*/\1/p' "$STATUS/last.json")"
    eq "resume: just_updated written" yes "$(yes_no test -s "$STATE/updated.json")"
    eq "resume: marker gone" no "$(yes_no test -e "$STATE/in-progress")"
    # Cut off, and the new image doesn't answer -> back on the old one, the new one held.
    printf 'apply\ncur\nnew\n0.0.3\n0.0.4\n' > "$STATE/in-progress"
    NEW_STARTS=no START_WAIT=0
    resume > /dev/null
    eq "resume: unhealthy -> back on the old image" cur "$(cat "$hu_t/running")"
    eq "resume: ...the new one held as failed" yes "$(yes_no grep -q '"reason":"failed"' "$STATE/held.json")"
    eq "resume: ...last says no_start" no_start "$(sed -n 's/.*"kind":"\([^"]*\)".*/\1/p' "$STATUS/last.json")"
    eq "resume: ...marker gone" no "$(yes_no test -e "$STATE/in-progress")"
    resume > "$hu_t/rout"
    eq "resume: nothing cut off does nothing" "" "$(cat "$hu_t/rout")"
    # A rollback whose previous image doesn't start: the current one is put back, not held.
    : > "$calls"
    rollback > /dev/null
    eq "rollback fails: still on the image that ran" cur "$(cat "$hu_t/running")"
    eq "rollback fails: the name points at it again" yes "$(yes_no grep -qx 'podman tag back' "$calls")"
    eq "rollback fails: no hold on the running image" no "$(yes_no test -e "$STATE/held.json")"
    eq "rollback fails: last says so" "false no_start" \
        "$(sed -n 's/.*"ok":\([a-z]*\),.*"kind":"\([^"]*\)".*/\1 \2/p' "$STATUS/last.json")"
    eq "rollback fails: marker gone" no "$(yes_no test -e "$STATE/in-progress")"
    eq "clean: ASCII only, no quotes" 'x y' "$(clean $'\xc3\xa9x "y\\')"
    exit "$fails"
) || fails=$((fails + $?))
# k3s datastore snapshots (PLAN Q23): every update copies the stopped datastore first (newest 3 kept);
# going back to an image that needs another datastore format restores the copy, keeping what it replaces.
(
    # shellcheck source=packaging/container/host/melissa-host-update
    . "$hu"
    set +e
    fails=0
    STATE="$hu_t/snap" UNITDIR="$hu_t/units3"
    paths
    mkdir -p "$REQUESTS" "$STATUS"
    calls="$hu_t/scalls"; : > "$calls"
    vol="$hu_t/vol"; srv="$vol/lib/rancher/k3s/server"
    mkdir -p "$srv/db" "$srv/cred"
    echo OLD > "$srv/db/state.db"; echo pw > "$srv/cred/passwd"; echo tok > "$srv/token"
    echo cur > "$hu_t/srunning"; echo cur > "$hu_t/stagged"
    IMAGE=ghcr.io/svin24/melissa:edge AUTO_UPDATE=registry SCHEDULE=daily-03 START_WAIT=60
    FMT_cur="v1.37.2+k3s1 plain" FMT_new="v1.38.0+k3s1 encrypted"
    systemctl() {
        echo "systemctl $*" >> "$calls"
        case "$1" in
        restart|start) cp "$hu_t/stagged" "$hu_t/srunning" ;;
        stop) echo stopped > "$hu_t/srunning" ;;
        is-active) [ "$(cat "$hu_t/srunning")" != stopped ] ;;
        esac
    }
    podman() {
        case "$*" in
        "volume inspect melissa-var --format {{.Mountpoint}}") echo "$vol" ;;
        "run --rm --pull=never --network=none --entrypoint /bin/sh "*)
            case "$7" in cur) echo "$FMT_cur" ;; new) echo "$FMT_new" ;; *) return 1 ;; esac ;;
        "container inspect melissa --format {{.Image}}") cat "$hu_t/srunning" ;;
        "image inspect ghcr.io/svin24/melissa:edge --format {{.Id}}") cat "$hu_t/stagged" ;;
        "image inspect localhost/melissa-rollback:previous --format {{.Id}}") echo cur ;;
        "image inspect "*"{{.Id}}") echo "${3}" ;;
        "image inspect cur --format {{range .RepoDigests}}{{.}} {{end}}") echo "ghcr.io/svin24/melissa@sha256:aaa1 " ;;
        "image inspect new --format {{range .RepoDigests}}{{.}} {{end}}") echo "ghcr.io/svin24/melissa@sha256:bbb2 " ;;
        "image inspect "*"--format {{index .Labels"*) echo 0.0.x ;;
        "image inspect "*) echo "$3|sha256:x|2026-11-26T10:00:00Z|0.0.x|abc1234" ;;
        "pull -q ghcr.io/svin24/melissa:edge") echo new > "$hu_t/stagged" ;;
        "tag cur ghcr.io/svin24/melissa:edge") echo cur > "$hu_t/stagged" ;;
        "tag new ghcr.io/svin24/melissa:edge") echo new > "$hu_t/stagged" ;;
        "--events-backend=none --log-level=error exec melissa systemctl is-active -q "*) [ "$(cat "$hu_t/srunning")" != stopped ] ;;
        "image exists "*) return 0 ;;
        *) echo "podman $*" >> "$calls"; return 1 ;;
        esac
    }
    skopeo() { echo "sha256:bbb2|0.0.4|9e2b5f1|100 "; }
    sleep() { :; }
    # Four older snapshots: after this update only the newest three remain (this one among them).
    for t in 20260101T000000Z 20260102T000000Z 20260103T000000Z 20260104T000000Z; do
        mkdir -p "$SNAPSHOTS/$t-old"; done
    update apply > /dev/null
    snap=$(snapshot_for cur)
    eq "snapshot: running the new image" new "$(cat "$hu_t/srunning")"
    eq "snapshot: taken for the image left" yes "$(yes_no test -n "$snap")"
    eq "snapshot: Melissa stopped for the copy, before the restart" "stop restart" \
        "$(grep -oE 'systemctl (stop|restart) melissa.service' "$calls" | cut -d' ' -f2 | paste -sd ' ')"
    eq "snapshot: the datastore, keys and token copied" "OLD pw tok" \
        "$(cat "$snap/db/state.db" "$snap/cred/passwd" "$snap/token" 2>/dev/null | paste -sd ' ')"
    eq "snapshot: its datastore format recorded" "format=1.37 plain" "$(grep '^format=' "$snap/meta")"
    eq "snapshot: root only" 700 "$(stat -c %a "$SNAPSHOTS")"
    eq "snapshot: the newest three kept" "20260103T000000Z-old 20260104T000000Z-old ${snap##*/}" \
        "$(find "$SNAPSHOTS" -mindepth 1 -maxdepth 1 -printf '%f\n' | sort | paste -sd ' ')"
    # The new image's k3s migrated the datastore and encrypted its Secrets.
    echo NEW > "$srv/db/state.db"; echo KEY > "$srv/cred/encryption-config.json"
    : > "$calls"
    rollback > /dev/null
    eq "restore: back on the previous image" cur "$(cat "$hu_t/srunning")"
    eq "restore: the datastore from before the update" OLD "$(cat "$srv/db/state.db")"
    eq "restore: its keys (no encryption key then)" no "$(yes_no test -e "$srv/cred/encryption-config.json")"
    eq "restore: the replaced datastore and key kept" "NEW KEY" \
        "$(cat "$snap"/replaced-*/db/state.db "$snap"/replaced-*/cred/encryption-config.json 2>/dev/null | paste -sd ' ')"
    eq "restore: stopped before the copy" "stop restart" \
        "$(grep -oE 'systemctl (stop|restart) melissa.service' "$calls" | cut -d' ' -f2 | paste -sd ' ')"
    eq "restore: last says so" yes "$(yes_no grep -q 'datastore restored from before the update' "$STATUS/last.json")"
    # Same format both ways (a Melissa-only update): the datastore stays as it is.
    FMT_new="v1.37.4+k3s1 plain"
    podman tag new ghcr.io/svin24/melissa:edge; systemctl restart melissa.service
    echo LATEST > "$srv/db/state.db"
    rm -f "$STATE/held.json"
    rollback > /dev/null
    eq "same format: rolled back" cur "$(cat "$hu_t/srunning")"
    eq "same format: datastore kept" LATEST "$(cat "$srv/db/state.db")"
    eq "same format: last has no restore" no "$(yes_no grep -q 'restored' "$STATUS/last.json")"
    # No room for a copy: no update, Melissa started again on the image she ran.
    df() { printf 'Filesystem 1-blocks Used Available Use%% Mounted\nx 100 100 0 100%% /\n'; }
    rm -f "$STATE/held.json"; : > "$calls"
    update apply > /dev/null
    eq "no room: still on the old image" cur "$(cat "$hu_t/srunning")"
    eq "no room: started again, never switched" "stop start" \
        "$(grep -oE 'systemctl (stop|start|restart) melissa.service' "$calls" | cut -d' ' -f2 | paste -sd ' ')"
    eq "no room: last says snapshot" "false snapshot" \
        "$(sed -n 's/.*"ok":\([a-z]*\),.*"kind":"\([^"]*\)".*/\1 \2/p' "$STATUS/last.json")"
    eq "no room: marker gone" no "$(yes_no test -e "$STATE/in-progress")"
    unset -f df
    exit "$fails"
) || fails=$((fails + $?))
# Migration: podman-auto-update.timer goes off only if install.sh had turned it on, and no other
# container uses it.
(
    set +e
    fails=0
    calls="$hu_t/mcalls"; : > "$calls"
    mkdir -p "$hu_t/q1" "$hu_t/q2"
    printf '[Container]\nAutoUpdate=registry\n' > "$hu_t/q1/melissa.container"
    QUADLET_DIRS=("$hu_t/q1" "$hu_t/q2")
    systemctl() { echo "systemctl $*" >> "$calls"; }
    podman() { printf '%s\n' $PODMAN_PS; }
    PODMAN_PS="melissa"
    eq "migrate: not ours: left alone" "no" "$(podman_timer_migrate no 2>/dev/null)"
    eq "migrate: not ours: no systemctl" "" "$(cat "$calls")"
    eq "migrate: ours, only Melissa used it: disabled" "no" "$(podman_timer_migrate yes 2>/dev/null)"
    eq "migrate: ...with systemctl disable" yes "$(yes_no grep -qx 'systemctl disable -q --now podman-auto-update.timer' "$calls")"
    : > "$calls"
    PODMAN_PS="melissa nextcloud"
    out=$(podman_timer_migrate yes 2>&1 >/dev/null)
    eq "migrate: another container uses it: kept" "" "$(cat "$calls")"
    eq "migrate: ...and says so" yes "$(yes_no grep -q 'other containers use it (nextcloud)' <<<"$out")"
    # A Quadlet with AutoUpdate= whose container doesn't exist yet counts too.
    : > "$calls"; PODMAN_PS="melissa"
    printf '[Container]\n AutoUpdate=local\n' > "$hu_t/q2/gitea.container"
    out=$(podman_timer_migrate yes 2>&1 >/dev/null)
    eq "migrate: a Quadlet with AutoUpdate=: kept" "" "$(cat "$calls")"
    eq "migrate: ...named" yes "$(yes_no grep -q 'other containers use it (gitea)' <<<"$out")"
    exit "$fails"
) || fails=$((fails + $?))
rm -rf "$hu_t"

if [ "$fails" != 0 ]; then echo "$fails failed"; exit 1; fi
echo "all install.sh tests passed"
