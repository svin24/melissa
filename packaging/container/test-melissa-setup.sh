#!/bin/bash
# Tests for melissa-setup's network check (no root, no network): `just container-test`.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=packaging/common/usr/bin/melissa-setup
. "$here/../common/usr/bin/melissa-setup"
set +e

fails=0
eq() { # description, want, got
    if [ "$2" = "$3" ]; then echo "ok: $1"; else echo "FAIL: $1"$'\n'"  want: $2"$'\n'"  got:  $3"; fails=$((fails + 1)); fi
}

dir=$(mktemp -d)
looked="$dir/looked"
timeout() { echo "$*" >> "$looked"; shift; "$@"; }
getent() { [ "${FAKE_DNS:-yes}" = yes ]; }
release="$dir/release"
echo MELISSA_RUNTIME=container > "$release"

eq "name resolves: goes on" ok "$(net_check https://x.ai/cli/install.sh 2>&1 && echo ok)"
eq "the lookup has a short timeout and the bare host" "5 getent ahosts x.ai" "$(tail -n 1 "$looked")"
net_check "https://lookaside.facebook.com/lookaside/muse/download/?channel=muse" >/dev/null 2>&1
eq "host from a URL with a query" "5 getent ahosts lookaside.facebook.com" "$(tail -n 1 "$looked")"
out=$( (FAKE_DNS=no net_check https://x.ai/cli/install.sh) 2>&1); rc=$?
eq "no DNS in the container: stops" 1 "$rc"
eq "no DNS in the container: the message" \
    "melissa-setup: This container can't reach the internet (name lookup failed). If Docker runs on this host, see packaging/container/README.md → Docker." \
    "$out"
echo MELISSA_RUNTIME=nixos > "$release"
out=$( (FAKE_DNS=no net_check https://opencode.ai/install) 2>&1)
eq "no DNS elsewhere: no container words" \
    "melissa-setup: This machine can't reach the internet (name lookup for opencode.ai failed). Check its network and DNS." "$out"
eq "downloads time out on connect and don't retry four times" yes \
    "$(grep -q -- "--connect-timeout 10 --retry 2 " "$here/../common/usr/bin/melissa-setup" && echo yes)"
eq "no download without the check first" 3 "$(grep -c '^    net_check ' "$here/../common/usr/bin/melissa-setup")"
eq "one curl, in download()" 1 "$(grep -c '^ *as_melissa curl ' "$here/../common/usr/bin/melissa-setup")"
rm -rf "$dir"

if [ "$fails" != 0 ]; then echo "$fails failed"; exit 1; fi
echo "all melissa-setup tests passed"
