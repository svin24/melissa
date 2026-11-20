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

# --- defaults from BIND --------------------------------------------------------------------
unset APP_ADDRESSES
BIND=127.0.0.1; eq "loopback BIND: 127.0.0.1" 127.0.0.1 "$(app_addresses)"
BIND=203.0.113.5; eq "one address: that address" 203.0.113.5 "$(app_addresses)"
APP_ADDRESSES=box.example.com; eq "APP_ADDRESSES wins" box.example.com "$(app_addresses)"
unset APP_ADDRESSES

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

if [ "$fails" != 0 ]; then echo "$fails failed"; exit 1; fi
echo "all install.sh tests passed"
