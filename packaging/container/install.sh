#!/bin/bash
# Install Melissa as a rootful podman container started by systemd (Quadlet). Run as root.
# Docs: packaging/container/README.md. Undo: packaging/container/uninstall.sh [--purge] (also
# installed as /usr/local/libexec/melissa/uninstall.sh).
#
#   sudo packaging/container/install.sh               preflight, install, start, print the UI URL
#   sudo packaging/container/install.sh --preflight   only check the host
#
# Environment (all optional):
#   CHANNEL=edge                    which published image to follow: edge (every master commit that
#                                   passed the release gate) or stable (tagged releases)
#   IMAGE=ghcr.io/svin24/melissa:$CHANNEL
#                                   image to run. Default: the one in install.env (a reinstall keeps
#                                   it), else the published one. A registry image is pulled; a
#                                   localhost/ image (a local build) must be in root's storage
#                                   (podman load) or come from IMAGE_ARCHIVE.
#   IMAGE_ARCHIVE=path.tar          `podman save` archive to load first (tagged as IMAGE)
#   VERIFY=yes                      published images: podman refuses them unless they carry Melissa's
#                                   signature (melissa-signing.pub, trust.sh). no: skip the check.
#   UI_PORT=8080                    host port of the web UI (on UI_BIND)
#   UI_BIND=127.0.0.1               keep it on loopback: sign-in runs over plain HTTP (README, "Remote")
#   APP_PORT_BASE=30000             host port of app NodePort 30000 (80 ports: base..base+79)
#   BIND=0.0.0.0                    host address for the app ports. Default: every address, and
#                                   Melissa decides who may open apps (Settings → Apps: this device,
#                                   tailnet or home network; this device until changed). A loopback
#                                   or single address restricts them at the host as well.
#   APP_ADDRESSES=                  [apps] addresses for app links (comma separated). Default from
#                                   BIND: 127.0.0.1 for loopback, BIND itself for one address; for
#                                   0.0.0.0 the default route's address, the Tailscale addresses and
#                                   127.0.0.1 (links offer those the setting lets in). Each one must
#                                   be covered by BIND (else links are dead: install refuses, and an
#                                   existing server.toml's addresses are rewritten to match).
#   ALLOW_ADDRESS_MISMATCH=no       yes: skip that check (something else forwards those addresses)
#   AUTO_UPDATE=                    registry (default for registry images), local (default for
#                                   localhost/ images) or off. Not off: enables podman-auto-update.timer.
#   SET_SYSCTLS=yes                 write the kubelet's host sysctls to /etc/sysctl.d (see below)
#   HOST_TAILSCALE=auto             does the host run Tailscale? auto: yes when tailscale0 exists or
#                                   tailscaled.service is active. yes: Melissa uses the host's and
#                                   Settings → Tailscale says so; no: the owner may enable Melissa's
#                                   own (off by default).
#   TAILSCALE_NAME=melissa-<host>   the node name of Melissa's own Tailscale on the tailnet
#   DOCKER_FORWARD=auto             Docker on this host drops podman's forwarded traffic: auto adds
#                                   two DOCKER-USER rules (melissa-docker-forward.service); no only
#                                   prints them (README, "Docker on the same host")
#   NET_CHECK=yes                   after the start, check that the container can look up
#                                   NET_CHECK_HOST=example.com (a DNS lookup; no: skip it)
#
# AppArmor (Ubuntu): install.sh installs and loads Melissa's own profile for its container
# (/etc/apparmor.d/melissa-container, apparmor.sh) and runs the container under it, so Ubuntu's
# user-namespace restriction stays on for everything else. Only if that profile can't be loaded on a
# restricting host does it stop, with the last-resort steps (README, "Ubuntu and AppArmor").
# firewalld: checked, never changed. StrictForwardPorts=yes stops the preflight with the commands to
# change it (podman can't publish ports then; README, "firewalld").
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=packaging/container/trust.sh
. "$here/trust.sh"
# shellcheck source=packaging/container/apparmor.sh
. "$here/apparmor.sh"
STATE=/var/lib/melissa-container
INSTALL_ENV=${INSTALL_ENV:-$STATE/install.env}
CHANNEL=${CHANNEL:-edge}
case "$CHANNEL" in edge | stable) ;; *) echo "melissa-install: CHANNEL must be edge or stable" >&2; exit 2 ;; esac
# A reinstall keeps the image (and its update mode) it was installed with, unless IMAGE says otherwise.
if [ -z "${IMAGE:-}" ] && [ -r "$INSTALL_ENV" ]; then
    IMAGE=$(sed -n 's/^IMAGE=//p' "$INSTALL_ENV" | tr -d '"' | tail -n 1)
    [ -n "${AUTO_UPDATE:-}" ] || AUTO_UPDATE=$(sed -n 's/^AUTO_UPDATE=//p' "$INSTALL_ENV" | tr -d '"' | tail -n 1)
fi
IMAGE=${IMAGE:-$SIGNED_REPO:$CHANNEL}
VERIFY=${VERIFY:-yes}
UI_PORT=${UI_PORT:-8080}
UI_BIND=${UI_BIND:-127.0.0.1}
APP_PORT_BASE=${APP_PORT_BASE:-30000}
BIND=${BIND:-0.0.0.0}
SET_SYSCTLS=${SET_SYSCTLS:-yes}
ALLOW_ADDRESS_MISMATCH=${ALLOW_ADDRESS_MISMATCH:-no}
HOST_TAILSCALE=${HOST_TAILSCALE:-auto}
DOCKER_FORWARD=${DOCKER_FORWARD:-auto}
NET_CHECK=${NET_CHECK:-yes}
NET_CHECK_HOST=${NET_CHECK_HOST:-example.com}
SYS_NET=${SYS_NET:-/sys/class/net}
case "$IMAGE" in localhost/*) AUTO_UPDATE=${AUTO_UPDATE:-local} ;; *) AUTO_UPDATE=${AUTO_UPDATE:-registry} ;; esac
APP_PORT_LAST=$((APP_PORT_BASE + 79))

QUADLET=${QUADLET:-/etc/containers/systemd/melissa.container}
# What Quadlet generates from it (checked after daemon-reload).
QUADLET_GENERATED=${QUADLET_GENERATED:-/run/systemd/generator/melissa.service}
UNITDIR=/etc/systemd/system
LIBEXEC=/usr/local/libexec/melissa
SHARE=/usr/local/share/melissa
SYSCTL_FILE=/etc/sysctl.d/90-melissa-kubelet.conf
MODULES_FILE=/etc/modules-load.d/melissa.conf
DOCKER_FORWARD_UNIT=melissa-docker-forward.service
# k3s's pod and service networks inside the container; the podman network must not overlap them.
K3S_CIDRS="10.42.0.0/16 10.43.0.0/16"
# The kubelet inside sets these host-wide (they are not namespaced). Values as the kubelet wants them.
SYSCTLS="vm.overcommit_memory=1 vm.panic_on_oom=0 kernel.panic=10 kernel.panic_on_oops=1 kernel.keys.root_maxkeys=1000000 kernel.keys.root_maxbytes=25000000"

say() { echo "melissa-install: $*"; }
die() { echo "melissa-install: $*" >&2; exit 1; }

# overlaps A/len B/len (IPv4 only; anything else: no)
overlaps() {
    local a=${1%/*} al=${1#*/} b=${2%/*} bl=${2#*/} m
    case "$a$b" in *:*) return 1 ;; esac
    # shellcheck disable=SC2086 # split the address at the dots
    ip2n() { local IFS=.; set -- $1; echo $(( ($1 << 24) + ($2 << 16) + ($3 << 8) + $4 )); }
    m=$(( al < bl ? al : bl ))
    [ $(( ($(ip2n "$a") ^ $(ip2n "$b")) >> (32 - m) )) = 0 ]
}

is_loopback() { case "$1" in 127.*|::1|'[::1]'|localhost) return 0 ;; *) return 1 ;; esac; }

# is_tailscale IPV4: in Tailscale's (CGNAT) range 100.64.0.0/10?
is_tailscale() {
    local second
    case "$1" in 100.*.*.*) ;; *) return 1 ;; esac
    second=${1#100.}; second=${second%%.*}
    [ "$second" -ge 64 ] 2>/dev/null && [ "$second" -le 127 ]
}

# bind_covers ADDRESS: are app ports published on BIND reachable at ADDRESS (an IP or a name)?
# 0.0.0.0 covers everything; loopback covers loopback only; one address covers itself.
bind_covers() {
    local a=$1 ip
    case "$BIND" in 0.0.0.0|::|'') return 0 ;; esac
    if is_loopback "$BIND"; then is_loopback "$a"; return; fi
    [ "$a" = "$BIND" ] && return 0
    # A host name: covered when it resolves to BIND.
    case "$a" in *[!0-9.:]*)
        for ip in $(getent ahosts "$a" 2>/dev/null | cut -d' ' -f1 | sort -u); do
            [ "$ip" = "$BIND" ] && return 0
        done ;;
    esac
    return 1
}

# publish_bind: BIND as the server is told (MELISSA_PUBLISH_BIND in melissa.container). The server
# leaves addresses BIND doesn't cover out of app links; it can't test them by connecting (from
# inside the container the host's published address doesn't answer: no hairpin). Empty: unknown,
# the server probes and only notes failures (ALLOW_ADDRESS_MISMATCH=yes: something else forwards).
publish_bind() {
    [ "$ALLOW_ADDRESS_MISMATCH" = yes ] && { echo; return 0; }
    case "$BIND" in ''|0.0.0.0) echo 0.0.0.0 ;; localhost) echo 127.0.0.1 ;; *) echo "$BIND" ;; esac
}

# host_tailscale: yes when the host runs Tailscale (HOST_TAILSCALE=auto: its interface exists or
# tailscaled.service is active), else no. Melissa then never starts its own (Settings → Tailscale).
host_tailscale() {
    case "$HOST_TAILSCALE" in
    yes|no) echo "$HOST_TAILSCALE"; return 0 ;;
    auto) ;;
    *) echo "melissa-install: HOST_TAILSCALE must be auto, yes or no" >&2; return 1 ;;
    esac
    if [ -e "$SYS_NET/tailscale0" ] || systemctl is-active -q tailscaled.service 2>/dev/null; then
        echo yes
    else
        echo no
    fi
}

# tailscale_port_line: publish Melissa's own tailscaled's UDP port (direct connections, not only
# DERP relays) unless the host runs Tailscale, whose tailscaled has that port. Also not with
# HOST_TAILSCALE=no on a host that runs it anyway: podman reserves a published UDP port on the
# host, so the container would not start while the host's tailscaled holds it.
TAILSCALE_PORT=41641
tailscale_port_line() {
    local host; host=$(host_tailscale) || return 1
    [ "$host" = yes ] && return 0
    if [ "$(HOST_TAILSCALE=auto host_tailscale)" = yes ]; then
        echo "melissa-install: NOTICE: the host runs Tailscale too (its tailscaled holds $TAILSCALE_PORT/udp):" \
            "not published; Melissa's own Tailscale connects through other ports or DERP relays" >&2
        return 0
    fi
    echo "PublishPort=$TAILSCALE_PORT:$TAILSCALE_PORT/udp"
}

# from_elsewhere: how to open the web UI from another device. The host's own Tailscale: the one
# command to run (Melissa never changes the host's Tailscale); Melissa's own: Settings → Tailscale.
from_elsewhere() {
    if [ "$(host_tailscale 2>/dev/null)" = yes ]; then
        echo "sudo tailscale serve --bg http://127.0.0.1:$UI_PORT (HTTPS on your tailnet), or ssh -L"
    else
        echo "Settings → Tailscale → Open Melissa from your other devices (HTTPS), or ssh -L"
    fi
}

# tailscale_name HOSTNAME: the node name of Melissa's own Tailscale: melissa-<short host name>,
# lower case letters, digits and dashes, at most 63 (TAILSCALE_NAME overrides it).
tailscale_name() {
    local raw=${TAILSCALE_NAME:-melissa-${1%%.*}} name
    name=$(printf '%s' "$raw" | tr 'A-Z' 'a-z' | tr -c 'a-z0-9-' '-' | tr -s '-' | cut -c1-63)
    name=${name#-}; name=${name%-}
    echo "${name:-melissa}"
}

# uncovered LIST: the entries of the comma-separated LIST that BIND does not cover.
uncovered() {
    local a out=''
    for a in ${1//,/ }; do bind_covers "$a" || out+="${out:+,}$a"; done
    echo "$out"
}

# check_addresses LIST: app links name LIST; the app ports must be reachable there.
check_addresses() {
    local bad
    bad=$(uncovered "$1")
    if [ -z "$bad" ]; then
        local list=${1:-(found at run time)}
        case "$BIND" in
        0.0.0.0|::|'') echo "ok: app links can use ${list//,/, } (app ports are published on all addresses; Melissa's filter decides who gets in)" ;;
        *) echo "ok: app links can use ${list//,/, } (app ports are published on $BIND; Melissa's filter decides who gets in)" ;;
        esac
        return 0
    fi
    if [ "$ALLOW_ADDRESS_MISMATCH" = yes ]; then
        echo "WARNING: app addresses $bad are not covered by BIND=$BIND (ALLOW_ADDRESS_MISMATCH=yes: going on)"
        return 0
    fi
    echo "FAIL: app links would name $bad, but app ports are published on BIND=$BIND only: those links"
    echo "      would get 'connection refused'. Use a BIND that covers them (that address, or 0.0.0.0),"
    echo "      or other APP_ADDRESSES; ALLOW_ADDRESS_MISMATCH=yes if something else forwards them."
    return 1
}

# toml_addresses: [apps] addresses of the server.toml on stdin, comma separated (empty: none).
toml_addresses() {
    awk '
        /^[[:space:]]*\[/ { sect = $0; gsub(/[[:space:]]/, "", sect) }
        sect == "[apps]" && /^[[:space:]]*addresses[[:space:]]*=/ { grab = 1 }
        grab { buf = buf $0; if ($0 ~ /\]/) grab = 0 }
        END {
            n = 0
            while (match(buf, /"[^"]*"/)) {
                out = out (n++ ? "," : "") substr(buf, RSTART + 1, RLENGTH - 2)
                buf = substr(buf, RSTART + RLENGTH)
            }
            print out
        }'
}

# toml_set_addresses LIST: server.toml on stdin with [apps] addresses set to LIST (comma
# separated), everything else kept. Adds the key (or the [apps] table) when missing.
toml_set_addresses() {
    local a toml=''
    for a in ${1//,/ }; do toml+="${toml:+, }\"$a\""; done
    awk -v line="addresses = [$toml]" '
        function put() { if (!done) { print line; done = 1 } }
        /^[[:space:]]*\[/ {
            if (in_apps) put()
            sect = $0; gsub(/[[:space:]]/, "", sect); in_apps = (sect == "[apps]"); seen = seen || in_apps
            print; next
        }
        in_apps && /^[[:space:]]*addresses[[:space:]]*=/ {
            put(); skip = ($0 !~ /\]/); next
        }
        skip { if ($0 ~ /\]/) skip = 0; next }
        { print }
        END {
            if (in_apps) put()
            else if (!seen) { print ""; print "[apps]"; print line }
        }'
}

# The server believes X-Forwarded-* on its main listener only with [web] trust_forwarded, and
# install.sh sets that only for the host's own `tailscale serve` in front of a loopback UI. This
# exact line marks it as install.sh's (removed again when that no longer holds); a
# trust_forwarded line of the owner's own is never touched.
TRUST_LINE="trust_forwarded = true # install.sh: the host's tailscale serve (HOST_TAILSCALE=yes, UI_BIND on loopback)"

# want_trust: yes when the host runs Tailscale and the web UI stays on loopback (UI_BIND). With
# a wider UI_BIND every LAN client arrives through the container's forward as 127.0.0.1 and could
# pick its own address, so never then (the server ignores it as well).
want_trust() {
    if [ "$(host_tailscale 2>/dev/null)" = yes ] && is_loopback "$UI_BIND"; then echo yes; else echo no; fi
}

# toml_set_trust yes|no: server.toml on stdin, with install.sh's trust_forwarded line under [web]
# (yes, unless the file has its own) or without it (no).
toml_set_trust() {
    awk -v want="$1" -v line="$TRUST_LINE" '
        $0 == line { next }
        { buf[n++] = $0 }
        /^[[:space:]]*trust_forwarded[[:space:]]*=/ { own = 1 }
        END {
            add = (want == "yes" && !own)
            for (i = 0; i < n; i++) {
                print buf[i]
                sect = buf[i]; gsub(/[[:space:]]/, "", sect)
                if (add && sect == "[web]") { print line; add = 0 }
            }
            if (add) { if (n) print ""; print "[web]"; print line }
        }'
}

# --- AppArmor: Melissa's own profile for its container (apparmor.sh) -------------------------
# APPARMOR_PROFILE: did install.sh load it (yes: melissa.container runs the container under it,
# uninstall.sh removes it)? APPARMOR_LINE: the Quadlet line for it (empty: none).
APPARMOR_PROFILE=no
APPARMOR_LINE=

# check_apparmor: preflight step. Fails only when the host restricts and the profile can't be loaded.
check_apparmor() {
    apparmor_enabled || return 0
    if apparmor_parser_found && [ -r "$MELISSA_APPARMOR_SRC" ]; then
        echo "ok: AppArmor: install.sh loads Melissa's profile $MELISSA_APPARMOR_PROFILE for its container"
        return 0
    fi
    if apparmor_restricts; then
        apparmor_fallback_message "apparmor_parser is not installed (package apparmor)"
        return 1
    fi
    echo "ok: AppArmor without apparmor_parser; nothing restricts bwrap here (the container runs unconfined)"
}

# apparmor_setup: load the profile and set APPARMOR_PROFILE and APPARMOR_LINE. Stops the install
# when it can't be loaded and the host restricts (the container's sandbox would fail).
apparmor_setup() {
    local reason
    apparmor_enabled || return 0
    if reason=$(apparmor_load); then
        APPARMOR_PROFILE=yes
        APPARMOR_LINE="PodmanArgs=$(apparmor_podman_args)"
        say "NOTICE: installed and loaded the AppArmor profile $MELISSA_APPARMOR_PROFILE ($APPARMOR_DIR/$MELISSA_APPARMOR_PROFILE)."
        say "  Melissa's container runs under it: it lets only that container create user namespaces (agent"
        say "  sandboxes); the host's AppArmor restrictions stay on for everything else. uninstall.sh removes it."
        return 0
    fi
    if apparmor_restricts; then
        apparmor_fallback_message "$reason" >&2
        die "preflight failed (AppArmor, see above)"
    fi
    say "WARNING: could not load the AppArmor profile $MELISSA_APPARMOR_PROFILE ($reason);"
    say "  nothing restricts bwrap on this host, so the container runs unconfined as before."
}

# --- firewalld (Fedora, RHEL, openSUSE): checked, never changed ------------------------------
# podman (netavark, nftables driver) publishes Melissa's ports with its own DNAT rules. firewalld
# 2.x accepts DNAT'd traffic before any zone is looked at, unless StrictForwardPorts=yes: then it
# only lets its own forward-ports through, and netavark refuses to start a container that
# publishes ports at all. Zone ports and services play no part either way. README, "firewalld".
FIREWALL_CMD=${FIREWALL_CMD:-firewall-cmd}
FIREWALLD_CONF=${FIREWALLD_CONF:-/etc/firewalld/firewalld.conf}
BUSCTL=${BUSCTL:-busctl}

# firewalld_active: firewalld.service runs and answers.
firewalld_active() {
    systemctl is-active -q firewalld.service 2>/dev/null || return 1
    "$FIREWALL_CMD" --state >/dev/null 2>&1
}

# firewalld_strict: yes or no. The running value (what netavark reads over D-Bus), else the
# config file, else firewalld's default (no).
firewalld_strict() {
    local v
    v=$("$BUSCTL" get-property org.fedoraproject.FirewallD1 /org/fedoraproject/FirewallD1/config \
        org.fedoraproject.FirewallD1.config StrictForwardPorts 2>/dev/null | sed -n 's/^s "\(.*\)"$/\1/p')
    [ -n "$v" ] || v=$(sed -n 's/^[[:space:]]*StrictForwardPorts[[:space:]]*=[[:space:]]*//p' "$FIREWALLD_CONF" 2>/dev/null | tail -n 1)
    case "$(printf '%s' "$v" | tr 'A-Z' 'a-z' | tr -d '[:space:]')" in
    yes|true) echo yes ;;
    *) echo no ;;
    esac
}

# check_firewalld: preflight step. Silent without firewalld; fails on StrictForwardPorts=yes
# (podman would refuse to start the container) with the commands to change it.
check_firewalld() {
    firewalld_active || return 0
    local zone
    zone=$("$FIREWALL_CMD" --get-default-zone 2>/dev/null) || zone=unknown
    if [ "$(firewalld_strict)" = no ]; then
        echo "ok: firewalld (default zone $zone, StrictForwardPorts=no) lets podman's published ports through;"
        echo "    zones don't apply to them, Melissa's filter (Settings → Apps) decides who opens apps"
        return 0
    fi
    cat <<EOF
FAIL: firewalld runs with StrictForwardPorts=yes (default zone $zone). podman then refuses to start
      a container that publishes ports ("Port forwarding not possible as firewalld StrictForwardPorts
      enabled"), and Melissa publishes its web UI ($UI_BIND:$UI_PORT) and app ports ($APP_PORT_BASE-$APP_PORT_LAST).
      Opening ports or a service in a zone does not help: with this setting firewalld lets forwarded
      container traffic through only for its own forward-ports. install.sh does not change your
      firewall. To let podman publish ports again (firewalld's default):
        sudo sed -i 's/^StrictForwardPorts=.*/StrictForwardPorts=no/' $FIREWALLD_CONF
        sudo firewall-cmd --reload
      That applies to every rootful container on this host: their published ports are accepted without
      zone checks. Melissa's own filter still decides who may open apps (this device only until you
      change Settings → Apps), and the web UI stays on $UI_BIND. Then run install.sh again.
      See README, "firewalld".
EOF
    return 1
}

version_ge() { [ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -1)" = "$2" ]; }

preflight() {
    local ok=0 v c sub p
    [ "$(id -u)" = 0 ] || { echo "FAIL: run as root (rootful podman)"; return 1; }
    command -v podman >/dev/null || { echo "FAIL: podman is not installed"; return 1; }
    v=$(podman version --format '{{.Client.Version}}' 2>/dev/null || echo 0)
    if version_ge "$v" 5.0; then echo "ok: podman $v"; else echo "FAIL: podman $v: need 5.0 or newer (Quadlet)"; ok=1; fi
    [ -x /usr/libexec/podman/quadlet ] || [ -x /usr/lib/podman/quadlet ] \
        || { echo "FAIL: no Quadlet generator (podman's systemd integration)"; ok=1; }
    command -v systemctl >/dev/null && [ -d /run/systemd/system ] || { echo "FAIL: systemd is not PID 1"; ok=1; }
    if [ -r /sys/fs/cgroup/cgroup.controllers ]; then
        c=$(cat /sys/fs/cgroup/cgroup.controllers)
        for want in cpu cpuset io memory pids; do
            case " $c " in *" $want "*) ;; *) echo "FAIL: cgroup v2 controller '$want' missing"; ok=1 ;; esac
        done
        echo "ok: cgroup v2 ($c)"
    else
        echo "FAIL: not cgroup v2 (k3s in a container needs the unified hierarchy)"; ok=1
    fi
    for m in br_netfilter overlay; do
        if grep -q "^$m " /proc/modules || modprobe "$m" 2>/dev/null; then echo "ok: kernel module $m"
        else echo "FAIL: kernel module $m cannot be loaded"; ok=1; fi
    done
    if ! systemctl is-active -q melissa.service 2>/dev/null; then
        for p in "$UI_PORT" "$APP_PORT_BASE" "$APP_PORT_LAST"; do
            if ss -Hltn "sport = :$p" 2>/dev/null | grep -q .; then
                echo "FAIL: host port $p is in use (UI_PORT=$UI_PORT, APP_PORT_BASE=$APP_PORT_BASE)"; ok=1
            fi
        done
    fi
    for sub in $(podman network inspect podman --format '{{range .Subnets}}{{.Subnet}} {{end}}' 2>/dev/null); do
        for c in $K3S_CIDRS; do
            if overlaps "$sub" "$c"; then echo "FAIL: podman network 'podman' ($sub) overlaps k3s's $c"; ok=1; fi
        done
    done
    check_addresses "$(app_addresses)" || ok=1
    check_apparmor || ok=1
    check_firewalld || ok=1
    case "$(host_tailscale)" in
    yes) echo "ok: Tailscale runs on this host; Melissa uses it (Settings → Tailscale says so)" ;;
    no) echo "ok: no Tailscale on this host; Settings → Tailscale can turn on Melissa's own (off by default)" ;;
    *) ok=1 ;;
    esac
    if systemctl is-active -q k3s.service 2>/dev/null; then
        echo "note: a k3s runs on this host too; Melissa's k3s has its own network namespace (works, tested)."
    fi
    [ "$ok" = 0 ] && echo "preflight ok"
    return "$ok"
}

# POLICY_CREATED: did install.sh create /etc/containers/policy.json (uninstall.sh then deletes it
# again when it is back to the distribution's default)? Kept across reinstalls.
POLICY_CREATED=no

# signatures: before the first pull, make podman require Melissa's signature on the published image.
signatures() {
    is_published "$IMAGE" || return 0
    if [ "$VERIFY" != yes ]; then
        say "VERIFY=$VERIFY: $IMAGE is NOT signature-checked (only TLS to the registry)"
        return 0
    fi
    local out
    out=$(trust_install "$here/melissa-signing.pub") || die "could not set up the signature check (VERIFY=no skips it)"
    [ "$(env_get POLICY_CREATED)" = yes ] && POLICY_CREATED=yes
    [ "$out" = created ] && POLICY_CREATED=yes
    say "podman now requires Melissa's signature on $SIGNED_REPO ($POLICY, key $SIGNING_KEY)"
}

image() {
    if [ -n "${IMAGE_ARCHIVE:-}" ]; then
        say "loading $IMAGE_ARCHIVE"
        local loaded
        loaded=$(podman load -q -i "$IMAGE_ARCHIVE" | sed -n 's/^Loaded image[^:]*: //p' | tail -1)
        [ -n "$loaded" ] || die "podman load found no image in $IMAGE_ARCHIVE"
        [ "$loaded" = "$IMAGE" ] || podman tag "$loaded" "$IMAGE"
    elif ! podman image exists "$IMAGE"; then
        case "$IMAGE" in
        localhost/*) die "no $IMAGE in root's image storage. Build it (just container-build) and load it:
  podman save -o melissa.tar localhost/melissa:dev && sudo IMAGE_ARCHIVE=melissa.tar $0" ;;
        *) say "pulling $IMAGE"; podman pull "$IMAGE" ;;
        esac
    fi
}

sysctls() {
    [ "$SET_SYSCTLS" = yes ] || {
        say "SET_SYSCTLS=$SET_SYSCTLS: the kubelet inside will set these host sysctls itself at start: $SYSCTLS"
        return 0
    }
    local kv k
    if [ ! -e "$STATE/host-sysctl.before" ]; then
        for kv in $SYSCTLS; do k=${kv%%=*}; echo "$k=$(sysctl -n "$k")"; done > "$STATE/host-sysctl.before"
    fi
    {
        echo "# Melissa (packaging/container/install.sh): the kubelet of Melissa's k3s needs these and"
        echo "# sets them anyway (they are host-wide). uninstall.sh removes this file and restores the"
        echo "# values from $STATE/host-sysctl.before."
        for kv in $SYSCTLS; do echo "${kv%%=*} = ${kv#*=}"; done
    } > "$SYSCTL_FILE"
    sysctl -q -p "$SYSCTL_FILE"
    say "NOTICE: set host sysctls for the kubelet ($SYSCTL_FILE): $SYSCTLS"
    say "  (before: $(paste -sd ' ' "$STATE/host-sysctl.before"); uninstall.sh restores them)"
}

install_files() {
    local auto_line= host_ts ts_port_line
    # Outside the sed below: a failure there would silently drop the PublishPort line.
    host_ts=$(host_tailscale) || die "cannot tell whether the host runs Tailscale (HOST_TAILSCALE)"
    ts_port_line=$(tailscale_port_line) || die "cannot decide whether to publish Tailscale's port"
    [ "$AUTO_UPDATE" = off ] || auto_line="AutoUpdate=$AUTO_UPDATE"
    mkdir -p "$STATE/status" "$STATE/request" "$(dirname "$QUADLET")" "$LIBEXEC" "$SHARE"
    chmod 0755 "$STATE" "$STATE/status"; chmod 0700 "$STATE/request"
    printf '%s\n' br_netfilter overlay > "$MODULES_FILE"
    sed -e "s#@IMAGE@#$IMAGE#" -e "s#@AUTO_UPDATE_LINE@#$auto_line#" -e "s#@UI_BIND@#$UI_BIND#" \
        -e "s#@UI_PORT@#$UI_PORT#" -e "s#@BIND@#$BIND#" -e "s#@APP_PORT_BASE@#$APP_PORT_BASE#" \
        -e "s#@APP_PORT_LAST@#$APP_PORT_LAST#" -e "s#@PUBLISH_BIND@#$(publish_bind)#" \
        -e "s#@APPARMOR_LINE@#$APPARMOR_LINE#" -e "s#@HOST_TAILSCALE@#$host_ts#" -e "s#@TAILSCALE_NAME@#$(tailscale_name "$(hostname)")#" \
        -e "s#@TAILSCALE_PORT_LINE@#$ts_port_line#" \
        "$here/melissa.container" > "$QUADLET"
    install -m 0755 "$here/host/melissa-host-update" "$LIBEXEC/melissa-host-update"
    # uninstall.sh and what it sources, so it works without the checkout (get.sh deletes its copy).
    install -m 0755 "$here/uninstall.sh" "$here/trust.sh" "$here/apparmor.sh" "$LIBEXEC/"
    install -m 0644 "$here/host/melissa-update.path" "$here/host/melissa-update.service" "$UNITDIR/"
    install -m 0644 "$here/README.md" "$SHARE/README.md"
    local timer_ours=${TIMER_OURS:-no} docker_subnets=''
    if [ -r "$STATE/install.env" ]; then
        timer_ours=$(sed -n 's/^TIMER_OURS=//p' "$STATE/install.env")
        docker_subnets=$(env_get DOCKER_FORWARD_SUBNETS)
    fi
    if [ "$AUTO_UPDATE" != off ] && ! systemctl is-enabled -q podman-auto-update.timer 2>/dev/null; then
        systemctl enable -q --now podman-auto-update.timer
        timer_ours=yes
        say "enabled podman-auto-update.timer (daily; it updates containers labelled for auto-update)"
    fi
    cat > "$STATE/install.env" <<EOF
# Written by packaging/container/install.sh; read by melissa-host-update and uninstall.sh.
IMAGE=$IMAGE
AUTO_UPDATE=$AUTO_UPDATE
VERIFY=$VERIFY
POLICY_CREATED=$POLICY_CREATED
APPARMOR_PROFILE=$APPARMOR_PROFILE
UI_PORT=$UI_PORT
UI_BIND=$UI_BIND
BIND=$BIND
APP_PORT_BASE=$APP_PORT_BASE
TIMER_OURS=${timer_ours:-no}
DOCKER_FORWARD_SUBNETS="$docker_subnets"
EOF
    systemctl daemon-reload
    [ -e "$QUADLET_GENERATED" ] \
        || die "Quadlet did not generate melissa.service (check: /usr/libexec/podman/quadlet -dryrun)"
    systemctl enable -q --now melissa-update.path
}

# The app addresses for the server's config: what people type before :port. For every address
# (0.0.0.0): all the useful ones, most useful first; the server offers those that Settings → Apps
# lets in (this device only by default, so 127.0.0.1 last).
app_addresses() {
    if [ -n "${APP_ADDRESSES:-}" ]; then echo "$APP_ADDRESSES"; return; fi
    local out='' lan a
    case "$BIND" in
    127.*|::1|localhost) echo 127.0.0.1 ;;
    0.0.0.0|::|'')
        # The host's LAN address (route source towards a documentation address; nothing is
        # sent), its Tailscale addresses, then loopback.
        lan=$(ip -4 route get 203.0.113.1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p')
        for a in $lan $(ip -4 -o addr show 2>/dev/null | awk '{ sub(/\/.*/, "", $4); print $4 }'); do
            case ",$out," in *",$a,"*) continue ;; esac
            if [ "$a" = "$lan" ] || is_tailscale "$a"; then out+="${out:+,}$a"; fi
        done
        echo "${out:+$out,}127.0.0.1" ;;
    *) echo "$BIND" ;;
    esac
}

# rewrite_reason HAVE: why an existing server.toml's [apps] addresses HAVE should become
# app_addresses (nothing printed: keep them).
rewrite_reason() {
    local have=$1
    if [ -n "${APP_ADDRESSES:-}" ]; then echo "APP_ADDRESSES=$APP_ADDRESSES"; return 0; fi
    [ -n "$have" ] || return 0
    if [ -n "$(uncovered "$have")" ] && [ "$ALLOW_ADDRESS_MISMATCH" != yes ]; then
        echo "the old ones ($(uncovered "$have")) are not covered by BIND=$BIND: their links were refused"
        return 0
    fi
    # Before 2026-11-21 BIND defaulted to loopback and the addresses to 127.0.0.1 alone: with app
    # ports on every address, offer the others too (Settings → Apps decides which work).
    if [ "$have" = 127.0.0.1 ] && ! is_loopback "$BIND"; then
        echo "127.0.0.1 alone was the old loopback default; Settings → Apps now decides who may open apps"
    fi
}

# Write [apps] addresses and port_offset into the server's config unless one exists already.
configure() {
    local list toml='' a offset=$((APP_PORT_BASE - 30000)) body=''
    # Before the rename to generic terms (2026-11-19) the file was queen.toml: keep it.
    podman exec melissa sh -c 'cd /etc/melissa && if [ -e queen.toml ] && [ ! -e server.toml ]; then mv queen.toml server.toml; fi' || true
    list=$(app_addresses)
    if podman exec melissa test -e /etc/melissa/server.toml; then
        if [ "$offset" != 0 ] && ! podman exec melissa grep -qsx "port_offset = $offset" /etc/melissa/server.toml; then
            say "note: /etc/melissa/server.toml has no 'port_offset = $offset' under [apps]; app links name the wrong port"
        fi
        update_addresses "$list"
        update_trust
        return 0
    fi
    for a in ${list//,/ }; do toml+="${toml:+, }\"$a\""; done
    [ -z "$toml" ] || body+="addresses = [$toml]"$'\n'
    [ "$offset" = 0 ] || body+="port_offset = $offset"$'\n'
    if [ -n "$body" ]; then
        toml=$(printf '# Written by install.sh: app links use these.\n[apps]\n%s' "$body")
    else
        toml=''
    fi
    toml=$(printf '%s' "$toml" | toml_set_trust "$(want_trust)")
    [ -n "$toml" ] || return 0
    printf '%s\n' "$toml" \
        | podman exec -i melissa sh -c 'umask 022; cat > /etc/melissa/server.toml'
    podman exec melissa systemctl try-restart melissa-server
    [ -z "$body" ] || say "server config: [apps] $(printf '%s' "$body" | paste -sd ' ' -)"
    [ "$(want_trust)" = no ] || say "server config: [web] trust_forwarded (the host's tailscale serve)"
}

# An existing server.toml: add or remove install.sh's [web] trust_forwarded line as the host's
# Tailscale and UI_BIND now say (an install from before 2026-11-28 had it on by default).
update_trust() {
    local have want
    have=$(podman exec melissa cat /etc/melissa/server.toml) || return 0
    want=$(printf '%s\n' "$have" | toml_set_trust "$(want_trust)")
    [ "$have" = "$want" ] && return 0
    printf '%s\n' "$want" \
        | podman exec -i melissa sh -c 'umask 022; cat > /etc/melissa/server.toml.new && mv /etc/melissa/server.toml.new /etc/melissa/server.toml'
    podman exec melissa systemctl try-restart melissa-server
    if [ "$(want_trust)" = yes ]; then
        say "server config: [web] trust_forwarded on (the host's tailscale serve forwards to the UI)"
    else
        say "server config: [web] trust_forwarded removed (UI_BIND=$UI_BIND or the host's Tailscale changed)"
    fi
}

# The server.toml from an earlier install keeps its [apps] addresses; rewrite just that key when
# APP_ADDRESSES asks for others, or when they name addresses BIND does not cover (links that
# would be refused). The rest of the file stays as it is.
update_addresses() {
    local want=$1 have toml why
    toml=$(podman exec melissa cat /etc/melissa/server.toml) || return 0
    have=$(printf '%s\n' "$toml" | toml_addresses)
    [ "$have" = "$want" ] && return 0
    if [ -z "${APP_ADDRESSES:-}" ] && [ -n "$have" ] && [ -n "$(uncovered "$have")" ] \
        && [ "$ALLOW_ADDRESS_MISMATCH" = yes ]; then
        say "WARNING: server.toml [apps] addresses $(uncovered "$have") are not covered by BIND=$BIND (ALLOW_ADDRESS_MISMATCH=yes: kept)"
        return 0
    fi
    why=$(rewrite_reason "$have")
    [ -n "$why" ] || return 0
    [ -n "$want" ] || { say "WARNING: no app address to write; server.toml [apps] addresses kept ($have)"; return 0; }
    printf '%s\n' "$toml" | toml_set_addresses "$want" \
        | podman exec -i melissa sh -c 'umask 022; cat > /etc/melissa/server.toml.new && mv /etc/melissa/server.toml.new /etc/melissa/server.toml'
    podman exec melissa systemctl try-restart melissa-server
    say "NOTICE: server.toml [apps] addresses: ${have:-none} -> $want ($why)"
    case "$BIND" in 127.*|::1|localhost)
        say "  Apps are on loopback only (BIND=$BIND). Without BIND they are published on every address" \
            "and Settings → Apps decides who may open them." ;;
    esac
}

# The end of the install says how to claim Melissa: the setup code itself (kept by Melissa
# until it is used or expires), or where to get it when the server isn't answering yet.
claim_line() { # output of `melissa-server setup-code` (may be empty)
    local code
    code=$(printf '%s\n' "$1" | sed -n 's/^Setup code: *//p' | head -n 1)
    if [ -n "$code" ]; then
        echo "  Setup code: $code   (enter it on the web UI to become the owner)"
    elif printf '%s' "$1" | grep -q 'already has an owner'; then
        echo "  Sign in:    Melissa already has an owner; sign in on the web UI."
    else
        echo "  Setup code: not ready yet. Get it with: sudo podman exec melissa melissa-server setup-code"
    fi
}

setup_code_output() {
    local out=""
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        out=$(podman exec melissa melissa-server setup-code 2>/dev/null) && break
        out=""
        sleep 2
    done
    printf '%s' "$out"
}

# --- Docker on the same host ---------------------------------------------------------------
# Docker sets the iptables FORWARD policy to DROP and accepts only its own bridges. An nftables
# accept ends only its own hook, so a packet netavark's table accepts is still dropped by Docker's
# chain: containers on podman's network get no network at all. Docker's place for other rules is
# its DOCKER-USER chain; a small unit puts ours there whenever Docker starts.

# podman_subnets: the IPv4 subnets of podman's network `podman` (iptables is IPv4), one per line.
podman_subnets() {
    local s
    for s in $(podman network inspect podman --format '{{range .Subnets}}{{.Subnet}} {{end}}' 2>/dev/null); do
        case "$s" in *:*) ;; */*) echo "$s" ;; esac
    done
}

# docker_drops_forward: Docker's chain is there and the FORWARD policy drops (now).
docker_drops_forward() {
    command -v iptables >/dev/null 2>&1 || return 1
    iptables -S DOCKER-USER >/dev/null 2>&1 || return 1
    iptables -S FORWARD 2>/dev/null | grep -qx -- '-P FORWARD DROP'
}

# docker_installed: Docker runs or starts on its own (it sets that policy when it starts).
docker_installed() {
    local u
    for u in docker.service docker.socket; do
        systemctl is-active -q "$u" 2>/dev/null && return 0
        systemctl is-enabled -q "$u" 2>/dev/null && return 0
    done
    return 1
}

# docker_forward_rule SUBNET: the two rules (iptables arguments after the chain) for SUBNET.
docker_forward_rules() {
    echo "-s $1 -j ACCEPT"
    echo "-d $1 -j ACCEPT"
}

# docker_forward_unit SUBNETS: the unit that keeps those rules in DOCKER-USER (on stdout).
docker_forward_unit() {
    local s r
    cat <<EOF
# Written by packaging/container/install.sh; uninstall.sh removes it and its rules.
# Docker's iptables FORWARD policy (DROP) also drops podman's network ($*),
# so Melissa's container has no network. These rules in Docker's DOCKER-USER chain let it
# through. See $SHARE/README.md, "Docker on the same host".
[Unit]
Description=Melissa: let podman's network through Docker's firewall
After=docker.service
PartOf=docker.service

[Service]
Type=oneshot
RemainAfterExit=yes
# No Docker chain (Docker not running or gone): nothing to do, not a failure.
ExecCondition=/bin/sh -c 'iptables -S DOCKER-USER >/dev/null 2>&1'
EOF
    for s in "$@"; do
        while read -r r; do
            echo "ExecStart=/bin/sh -c 'iptables -C DOCKER-USER $r 2>/dev/null || iptables -I DOCKER-USER $r'"
        done < <(docker_forward_rules "$s")
    done
    cat <<EOF

[Install]
WantedBy=docker.service
EOF
}

# docker_forward_manual SUBNETS: the commands for doing it by hand.
docker_forward_manual() {
    local s r
    for s in "$@"; do
        while read -r r; do echo "  sudo iptables -I DOCKER-USER $r"; done < <(docker_forward_rules "$s")
    done
}

# docker_forward_delete SUBNETS: delete one copy of each rule (the one the unit inserted).
docker_forward_delete() {
    local s r
    for s in "$@"; do
        while read -r r; do
            # shellcheck disable=SC2086 # split the rule into arguments
            iptables -D DOCKER-USER $r 2>/dev/null || true
        done < <(docker_forward_rules "$s")
    done
}

# env_get KEY / env_set KEY VALUE: install.env (sourced by uninstall.sh: values are quoted, and
# hold no quotes). uninstall.sh reads there what install.sh changed on the host.
env_get() {
    [ -r "$STATE/install.env" ] || return 0
    sed -n "s/^$1=//p" "$STATE/install.env" | tr -d '"' | tail -n 1
}
env_set() {
    local f=$STATE/install.env
    [ -e "$f" ] || : > "$f"
    { grep -v "^$1=" "$f" || true; echo "$1=\"$2\""; } > "$f.new" && mv "$f.new" "$f"
}

# docker_forward: on a host where Docker's firewall drops podman's traffic, install and start
# melissa-docker-forward.service (DOCKER_FORWARD=no: print the commands instead). Returns 0 when
# Docker is on this host (whatever was done), 1 when it isn't.
docker_forward() {
    local unit=$UNITDIR/$DOCKER_FORWARD_UNIT old subnets drops=no
    old=$(env_get DOCKER_FORWARD_SUBNETS)
    docker_drops_forward && drops=yes
    [ "$drops" = yes ] || docker_installed || return 1
    mapfile -t subnets < <(podman_subnets)
    if [ "${#subnets[@]}" = 0 ]; then
        say "WARNING: Docker runs here, but podman's network 'podman' has no IPv4 subnet to let through"
        return 0
    fi
    if [ "$DOCKER_FORWARD" = no ]; then
        if [ -n "$old" ] || [ -e "$unit" ]; then
            systemctl disable -q --now "$DOCKER_FORWARD_UNIT" 2>/dev/null || true
            rm -f "$unit"
            # shellcheck disable=SC2086 # a list of subnets
            docker_forward_delete $old
            env_set DOCKER_FORWARD_SUBNETS ''
            systemctl daemon-reload
            say "DOCKER_FORWARD=no: removed $DOCKER_FORWARD_UNIT and its rules from an earlier install"
        fi
        if [ "$drops" = yes ]; then
            say "Docker's firewall drops podman traffic (DOCKER_FORWARD=no: not changed). To let Melissa out:"
            docker_forward_manual "${subnets[@]}"
        fi
        return 0
    fi
    # A subnet from an earlier install that's gone now: take its rules out.
    if [ -n "$old" ] && [ "$old" != "${subnets[*]}" ]; then
        # shellcheck disable=SC2086 # a list of subnets
        docker_forward_delete $old
    fi
    docker_forward_unit "${subnets[@]}" > "$unit"
    env_set DOCKER_FORWARD_SUBNETS "${subnets[*]}"
    systemctl daemon-reload
    systemctl enable -q "$DOCKER_FORWARD_UNIT"
    systemctl restart "$DOCKER_FORWARD_UNIT" || true
    if [ "$drops" = yes ]; then
        say "Docker's firewall drops podman traffic; added DOCKER-USER rules for ${subnets[*]} ($DOCKER_FORWARD_UNIT)"
    else
        say "Docker is installed; $DOCKER_FORWARD_UNIT lets ${subnets[*]} through its firewall when it starts"
    fi
    return 0
}

# container_online: can the container look up a well-known name? Short timeouts, two tries.
container_online() {
    local _
    for _ in 1 2; do
        timeout 15 podman exec melissa timeout 5 getent ahosts "$NET_CHECK_HOST" >/dev/null 2>&1 && return 0
    done
    return 1
}

# network_check DOCKER(yes|no): warn when the container has no network. Never fails the install.
network_check() {
    [ "$NET_CHECK" != no ] || return 0
    container_online && return 0
    say "WARNING: the container has no network access (looking up $NET_CHECK_HOST failed)."
    say "  Agents can't be installed and apps can't download anything until this is fixed."
    if [ "$1" = yes ]; then
        say "  Docker runs on this host; its FORWARD chain may still drop podman traffic. These rules let it out:"
        # shellcheck disable=SC2046 # a list of subnets
        docker_forward_manual $(podman_subnets)
    else
        say "  Likely causes: another firewall's forward policy drops podman's network ($(podman_subnets | paste -sd ' ' -)),"
        if [ "$(sysctl -n net.ipv4.ip_forward 2>/dev/null)" = 0 ]; then
            say "  or IP forwarding is off (net.ipv4.ip_forward = 0 here)."
        else
            say "  or IP forwarding is off (sysctl net.ipv4.ip_forward)."
        fi
    fi
    say "  See $SHARE/README.md, \"No network in the container\" (packaging/container/README.md)."
}

wait_for() { # seconds, description, command...
    local end=$(( $(date +%s) + $1 )) what=$2; shift 2
    until "$@" >/dev/null 2>&1; do
        [ "$(date +%s)" -lt "$end" ] || die "timed out waiting for $what"
        sleep 2
    done
}

# Sourced (packaging/container/test-install.sh): only the functions.
[ "${BASH_SOURCE[0]}" = "$0" ] || return 0

case "${1:-}" in
--preflight) preflight; exit $? ;;
"") ;;
*) sed -n '2,53p' "$0"; exit 2 ;;
esac

preflight || die "preflight failed (see above)"
mkdir -p "$STATE"
apparmor_setup
signatures
image
sysctls
install_files
say "starting melissa.service"
systemctl restart melissa.service
wait_for 120 "systemd in the container" podman exec melissa test -d /var/lib/melissa-container/etc-melissa
docker=no
docker_forward && docker=yes
network_check "$docker"
configure
wait_for 120 "the web UI" curl -fsS -o /dev/null "http://127.0.0.1:$UI_PORT/"
claim=$(claim_line "$(setup_code_output)")
cat <<EOF

Melissa is running (systemctl status melissa). k3s needs a minute or two on the first start.
  Web UI:     http://$UI_BIND:$UI_PORT/$(is_loopback "$UI_BIND" && echo " (this machine only)")
  Apps:       ports $APP_PORT_BASE-$APP_PORT_LAST$(case "$BIND" in 0.0.0.0|'') ;; *) echo " on $BIND" ;; esac); only this device can open them until you change
              Settings → Apps (agents ask you first)
  Agents:     sudo podman exec -it melissa melissa-setup            (e.g. agent opencode)
  Chat:       sudo podman exec -it melissa melissa-server chat
  Updates:    Settings → Updates, or sudo $LIBEXEC/melissa-host-update check|apply|rollback
              (automatic: $AUTO_UPDATE)
  Tailscale:  $(case "$(host_tailscale)" in yes) echo "the host's (Melissa uses it)" ;; *) echo "Settings → Tailscale (Melissa's own, off until you enable it)" ;; esac)
  From another machine: $(from_elsewhere)
  (sign-in sends passwords, so only HTTPS or this machine). See $SHARE/README.md.
  --privileged rootful container: it has root on this host.
  Uninstall:  sudo $LIBEXEC/uninstall.sh            (--purge also deletes the data)

$claim
EOF
