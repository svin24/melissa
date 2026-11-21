#!/bin/bash
# Install Melissa as a rootful podman container started by systemd (Quadlet). Run as root.
# Docs: packaging/container/README.md. Undo: packaging/container/uninstall.sh [--purge].
#
#   sudo packaging/container/install.sh               preflight, install, start, print the UI URL
#   sudo packaging/container/install.sh --preflight   only check the host
#
# Environment (all optional):
#   IMAGE=localhost/melissa:latest  image to run. A registry image is pulled; a localhost/ image must
#                                   be in root's storage (podman load) or come from IMAGE_ARCHIVE.
#   IMAGE_ARCHIVE=path.tar          `podman save` archive to load first (tagged as IMAGE)
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
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
IMAGE=${IMAGE:-localhost/melissa:latest}
UI_PORT=${UI_PORT:-8080}
UI_BIND=${UI_BIND:-127.0.0.1}
APP_PORT_BASE=${APP_PORT_BASE:-30000}
BIND=${BIND:-0.0.0.0}
SET_SYSCTLS=${SET_SYSCTLS:-yes}
ALLOW_ADDRESS_MISMATCH=${ALLOW_ADDRESS_MISMATCH:-no}
HOST_TAILSCALE=${HOST_TAILSCALE:-auto}
SYS_NET=${SYS_NET:-/sys/class/net}
case "$IMAGE" in localhost/*) AUTO_UPDATE=${AUTO_UPDATE:-local} ;; *) AUTO_UPDATE=${AUTO_UPDATE:-registry} ;; esac
APP_PORT_LAST=$((APP_PORT_BASE + 79))

STATE=/var/lib/melissa-container
QUADLET=/etc/containers/systemd/melissa.container
UNITDIR=/etc/systemd/system
LIBEXEC=/usr/local/libexec/melissa
SHARE=/usr/local/share/melissa
SYSCTL_FILE=/etc/sysctl.d/90-melissa-kubelet.conf
MODULES_FILE=/etc/modules-load.d/melissa.conf
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
    if [ -z "$bad" ]; then echo "ok: app addresses ${1:-(found at run time)} are covered by BIND=$BIND"; return 0; fi
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
    local auto_line=
    [ "$AUTO_UPDATE" = off ] || auto_line="AutoUpdate=$AUTO_UPDATE"
    mkdir -p "$STATE/status" "$STATE/request" /etc/containers/systemd "$LIBEXEC" "$SHARE"
    chmod 0755 "$STATE" "$STATE/status"; chmod 0700 "$STATE/request"
    printf '%s\n' br_netfilter overlay > "$MODULES_FILE"
    sed -e "s#@IMAGE@#$IMAGE#" -e "s#@AUTO_UPDATE_LINE@#$auto_line#" -e "s#@UI_BIND@#$UI_BIND#" \
        -e "s#@UI_PORT@#$UI_PORT#" -e "s#@BIND@#$BIND#" -e "s#@APP_PORT_BASE@#$APP_PORT_BASE#" \
        -e "s#@APP_PORT_LAST@#$APP_PORT_LAST#" -e "s#@PUBLISH_BIND@#$(publish_bind)#" \
        -e "s#@HOST_TAILSCALE@#$(host_tailscale)#" -e "s#@TAILSCALE_NAME@#$(tailscale_name "$(hostname)")#" \
        "$here/melissa.container" > "$QUADLET"
    install -m 0755 "$here/host/melissa-host-update" "$LIBEXEC/melissa-host-update"
    install -m 0644 "$here/host/melissa-update.path" "$here/host/melissa-update.service" "$UNITDIR/"
    install -m 0644 "$here/README.md" "$SHARE/README.md"
    local timer_ours=${TIMER_OURS:-no}
    [ -r "$STATE/install.env" ] && timer_ours=$(sed -n 's/^TIMER_OURS=//p' "$STATE/install.env")
    if [ "$AUTO_UPDATE" != off ] && ! systemctl is-enabled -q podman-auto-update.timer 2>/dev/null; then
        systemctl enable -q --now podman-auto-update.timer
        timer_ours=yes
        say "enabled podman-auto-update.timer (daily; it updates containers labelled for auto-update)"
    fi
    cat > "$STATE/install.env" <<EOF
# Written by packaging/container/install.sh; read by melissa-host-update and uninstall.sh.
IMAGE=$IMAGE
AUTO_UPDATE=$AUTO_UPDATE
UI_PORT=$UI_PORT
UI_BIND=$UI_BIND
BIND=$BIND
APP_PORT_BASE=$APP_PORT_BASE
TIMER_OURS=${timer_ours:-no}
EOF
    systemctl daemon-reload
    [ -e /run/systemd/generator/melissa.service ] \
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
        return 0
    fi
    for a in ${list//,/ }; do toml+="${toml:+, }\"$a\""; done
    [ -z "$toml" ] || body+="addresses = [$toml]"$'\n'
    [ "$offset" = 0 ] || body+="port_offset = $offset"$'\n'
    [ -n "$body" ] || return 0
    printf '# Written by install.sh: app links use these.\n[apps]\n%s' "$body" \
        | podman exec -i melissa sh -c 'umask 022; cat > /etc/melissa/server.toml'
    podman exec melissa systemctl try-restart melissa-server
    say "server config: [apps] $(printf '%s' "$body" | paste -sd ' ' -)"
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
*) sed -n '2,33p' "$0"; exit 2 ;;
esac

preflight || die "preflight failed (see above)"
mkdir -p "$STATE"
image
sysctls
install_files
say "starting melissa.service"
systemctl restart melissa.service
wait_for 120 "systemd in the container" podman exec melissa test -d /var/lib/melissa-container/etc-melissa
configure
wait_for 120 "the web UI" curl -fsS -o /dev/null "http://127.0.0.1:$UI_PORT/"
claim=$(claim_line "$(setup_code_output)")
cat <<EOF

Melissa is running (systemctl status melissa). k3s needs a minute or two on the first start.
  Web UI:     http://$UI_BIND:$UI_PORT/
  Apps:       ports $APP_PORT_BASE-$APP_PORT_LAST on $BIND; who may open them: Settings → Apps
              (this device only until you change it; agents ask you first)
  Agents:     sudo podman exec -it melissa melissa-setup            (e.g. agent opencode)
  Chat:       sudo podman exec -it melissa melissa-server chat
  Updates:    Settings → Updates, or sudo $LIBEXEC/melissa-host-update check|apply|rollback
              (automatic: $AUTO_UPDATE)
  Tailscale:  $(case "$(host_tailscale)" in yes) echo "the host's (Melissa uses it)" ;; *) echo "Settings → Tailscale (Melissa's own, off until you enable it)" ;; esac)
  From another machine: ssh -L 8080:127.0.0.1:$UI_PORT <this host>, or tailscale serve
  (HTTPS; sign-in sends passwords, so not plain HTTP across the network). See $SHARE/README.md.
  --privileged rootful container: it has root on this host.

$claim
EOF
