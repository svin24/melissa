#!/bin/sh
# Tests for get.sh (the one-line installer): argument and environment handling, the source ref,
# the host checks and the download, with stubbed id, uname, podman and curl. No root, no network.
# POSIX sh like get.sh: `just container-test`, or `sh` / `dash packaging/container/test-get.sh`.
set -u
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
GET_SH_SOURCED=yes
# shellcheck source=get.sh
. "$root/get.sh"
set +e

fails=0
eq() { # description, want, got
    if [ "$2" = "$3" ]; then echo "ok: $1"; else printf 'FAIL: %s\n  want: %s\n  got:  %s\n' "$1" "$2" "$3"; fails=$((fails + 1)); fi
}
has() { # description, needle, haystack
    case "$3" in *"$2"*) echo "ok: $1" ;; *) printf 'FAIL: %s\n  want: %s\n  in:   %s\n' "$1" "$2" "$3"; fails=$((fails + 1)) ;; esac
}

t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
bin=$t/bin
mkdir -p "$bin" "$t/only" "$t/systemd" "$t/tmp"
# Stubs, steered by FAKE_* variables. curl logs its arguments to $t/curl.log.
cat > "$bin/id" <<'EOF'
#!/bin/sh
echo "${FAKE_UID:-0}"
EOF
cat > "$bin/uname" <<'EOF'
#!/bin/sh
echo "${FAKE_OS:-Linux}"
EOF
cat > "$bin/podman" <<'EOF'
#!/bin/sh
echo "${FAKE_PODMAN-5.4.2}"
EOF
cat > "$bin/curl" <<'EOF'
#!/bin/sh
echo "$*" >> "$FAKE_LOG"
out=
while [ "$#" -gt 0 ]; do
    case "$1" in
    -I) printf 'HTTP/2 302\r\n'; [ -z "${FAKE_LOCATION:-}" ] || printf 'location: %s\r\n' "$FAKE_LOCATION"; printf '\r\n'; exit 0 ;;
    -o) out=$2; shift ;;
    esac
    shift
done
[ -n "${FAKE_TARBALL:-}" ] || exit 22
cp "$FAKE_TARBALL" "$out"
EOF
chmod +x "$bin"/*
ln -s "$bin/id" "$bin/uname" "$t/only/"
export FAKE_LOG="$t/curl.log"
PATH=$bin:$PATH
SYSTEMD_RUN=$t/systemd

# --- the host checks -------------------------------------------------------------------------
eq "host: everything there" "" "$(host_problems)"
eq "host: not root" "run as root (curl ... | sudo sh)" "$(FAKE_UID=1000; export FAKE_UID; host_problems)"
eq "host: not Linux" "Melissa's container install needs Linux" "$(FAKE_OS=Darwin; export FAKE_OS; host_problems)"
eq "host: podman 4" "podman 4.9.3 is too old: need 5.0 or newer (Quadlet)" "$(FAKE_PODMAN=4.9.3; export FAKE_PODMAN; host_problems)"
eq "host: podman 10" "" "$(FAKE_PODMAN=10.0.1; export FAKE_PODMAN; host_problems)"
eq "host: unreadable podman version" "could not read podman's version" "$(FAKE_PODMAN=; export FAKE_PODMAN; host_problems)"
eq "host: no systemd" "systemd is not running as PID 1" "$(SYSTEMD_RUN=$t/none; host_problems)"
# shellcheck disable=SC2123 # on purpose: a PATH without curl, tar, bash and podman
eq "host: tools missing" "curl is not installed
tar is not installed
bash is not installed
podman is not installed (need 5.0 or newer)" "$(PATH=$t/only; host_problems)"

# --- which source ----------------------------------------------------------------------------
eq "ref: edge is master" master "$(unset CHANNEL MELISSA_REF; pick_ref)"
eq "ref: CHANNEL=edge" master "$(CHANNEL=edge; unset MELISSA_REF; pick_ref)"
eq "ref: MELISSA_REF wins" v0.3.0 "$(CHANNEL=stable MELISSA_REF=v0.3.0; pick_ref)"
eq "ref: a branch with a slash" feature/x "$(MELISSA_REF=feature/x; pick_ref)"
# shellcheck disable=SC2016 # literal: a command substitution must be refused, not run
for bad in '-x' 'a..b' 'a b' 'a;b' '$(x)'; do
    eq "ref: refuses MELISSA_REF '$bad'" no "$(MELISSA_REF=$bad; pick_ref >/dev/null 2>&1 && echo yes || echo no)"
done
eq "ref: unknown channel" "melissa-get: CHANNEL must be edge or stable" "$(CHANNEL=nightly; unset MELISSA_REF; pick_ref 2>&1)"
: > "$FAKE_LOG"
eq "ref: stable is the latest release" v1.2.3 \
    "$(CHANNEL=stable FAKE_LOCATION=https://github.com/svin24/melissa/releases/tag/v1.2.3; export FAKE_LOCATION; unset MELISSA_REF; pick_ref)"
eq "ref: stable asks GitHub's releases/latest" "--proto =https --tlsv1.2 -fsS -I https://github.com/svin24/melissa/releases/latest" "$(cat "$FAKE_LOG")"
out=$(CHANNEL=stable FAKE_LOCATION=https://github.com/svin24/melissa/releases; export FAKE_LOCATION; unset MELISSA_REF; pick_ref 2>&1)
has "ref: no release yet: says so" "there is no stable release yet" "$out"
has "ref: no release yet: points to edge" "use the edge channel" "$out"
eq "ref: no redirect at all: refused" no "$(CHANNEL=stable; unset FAKE_LOCATION MELISSA_REF; pick_ref >/dev/null 2>&1 && echo yes || echo no)"
eq "ref: a tag that isn't v*: refused" no \
    "$(CHANNEL=stable FAKE_LOCATION=https://github.com/svin24/melissa/releases/tag/nightly; export FAKE_LOCATION; unset MELISSA_REF; pick_ref >/dev/null 2>&1 && echo yes || echo no)"

# --- download and run (a fake source tarball whose install.sh reports what it got) -----------
src=$t/src/melissa-master
mkdir -p "$src/packaging/container" "$src/crates"
cat > "$src/packaging/container/install.sh" <<'EOF'
#!/bin/bash
echo "args=$*"
echo "env=CHANNEL:${CHANNEL:-} UI_PORT:${UI_PORT:-} BIND:${BIND:-} VERIFY:${VERIFY:-}"
echo "here=$(cd "$(dirname "$0")" && pwd)"
if read -r line; then echo "stdin=$line"; else echo "stdin=empty"; fi
exit "${FAKE_INSTALL_STATUS:-0}"
EOF
echo "not needed" > "$src/crates/big"
tar -czf "$t/melissa.tar.gz" -C "$t/src" melissa-master
export FAKE_TARBALL="$t/melissa.tar.gz"

: > "$FAKE_LOG"
dir=$(fetch master "$t/tmp")
eq "fetch: packaging/container extracted" "$t/tmp/melissa-master/packaging/container" "$dir"
eq "fetch: nothing else extracted" no "$([ -e "$t/tmp/melissa-master/crates" ] && echo yes || echo no)"
eq "fetch: the codeload URL" "--proto =https --tlsv1.2 -fsS -L -o $t/tmp/melissa.tar.gz https://codeload.github.com/svin24/melissa/tar.gz/master" "$(cat "$FAKE_LOG")"
rm -rf "$t/tmp"/*
has "fetch: a failed download says so" "download of https://codeload.github.com/svin24/melissa/tar.gz/master failed" \
    "$(unset FAKE_TARBALL; fetch master "$t/tmp" 2>&1)"
echo "not a tarball" > "$t/junk"
eq "fetch: not a tarball: refused" no "$(FAKE_TARBALL=$t/junk; fetch master "$t/tmp" >/dev/null 2>&1 && echo yes || echo no)"
rm -rf "${t:?}/tmp"/*

# main in a subshell (it exits); TMPDIR shows that the temporary directory goes away.
run_main() { (TMPDIR=$t/tmp; main "$@") 2>&1; }
out=$(echo "from stdin" | CHANNEL=edge UI_PORT=8090 BIND=127.0.0.1 run_main --preflight)
status=$?
eq "main: install.sh's status" 0 "$status"
has "main: prints what it does first" "melissa-get: Melissa container install: check this host" "$out"
has "main: names the uninstall" "/usr/local/libexec/melissa/uninstall.sh" "$out"
has "main: --preflight passed on" "args=--preflight" "$out"
has "main: settings passed on" "env=CHANNEL:edge UI_PORT:8090 BIND:127.0.0.1 VERIFY:" "$out"
has "main: runs the extracted install.sh" "here=$t/tmp/melissa-get." "$out"
has "main: install.sh gets no stdin" "stdin=empty" "$out"
eq "main: temporary directory removed" "" "$(ls -A "$t/tmp")"
out=$(FAKE_INSTALL_STATUS=3 run_main)
eq "main: install.sh fails: same status" 3 "$?"
has "main: no argument: none passed" "args=
" "$out"
eq "main: temporary directory removed after a failure" "" "$(ls -A "$t/tmp")"

: > "$FAKE_LOG"
out=$(FAKE_UID=1000; export FAKE_UID; run_main)
eq "main: not root: exit 1" 1 "$?"
has "main: not root: summary first" "melissa-get: Melissa container install" "$out"
has "main: not root: refused" "melissa-get: FAIL: run as root" "$out"
eq "main: not root: nothing downloaded" "" "$(cat "$FAKE_LOG")"
: > "$FAKE_LOG"
out=$(CHANNEL=stable; unset FAKE_LOCATION; run_main)
eq "main: no stable release: exit 1" 1 "$?"
eq "main: no stable release: nothing downloaded" "" "$(grep codeload "$FAKE_LOG")"

out=$(run_main --purge)
eq "main: unknown argument: exit 2" 2 "$?"
has "main: unknown argument: usage" "usage: get.sh [--preflight]" "$out"
eq "main: two arguments: exit 2" 2 "$(run_main --preflight x >/dev/null; echo $?)"
eq "main: --help: exit 0" 0 "$(run_main --help >/dev/null; echo $?)"

# Run as a script, not sourced: the usage path, under the shell running these tests.
eq "script: unknown argument: exit 2" 2 "$(unset GET_SH_SOURCED; sh "$root/get.sh" --bad >/dev/null 2>&1; echo $?)"

if [ "$fails" != 0 ]; then echo "$fails failed"; exit 1; fi
echo "all get.sh tests passed"
