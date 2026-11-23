#!/usr/bin/env bash
# Run the sandbox escape suite (crates/agent/tests/escape.rs) inside a built Melissa image, the way the
# installed container runs: rootful podman, --privileged, systemd as PID 1, and the suite as the
# `melissa` user under the server unit's hardening (NoNewPrivileges, ProtectSystem=strict, ProtectHome,
# PrivateTmp). So bwrap, melissa-netbridge, /usr and the kernel view are the shipped image's.
#
#   packaging/container/escape-test.sh [IMAGE]      (default localhost/melissa:dev; `just escape-test-image`)
#
# 1. Builds the Containerfile's `escape-build` stage (test binary + melissa-escape-probe, same builder as
#    the image's binaries) and a throwaway image: IMAGE + /opt/melissa-escape (Containerfile.escape-test).
# 2. Copies it into root's storage, boots it under a separate name (no published ports), runs the suite.
# 3. Release gate: only a pass, with the test binary built from the image's own revision and no local
#    changes under crates/, tags IMAGE as localhost/melissa:escape-tested (rootless storage, where
#    `just container-build` puts it). Any run on that image first removes the tag, so a failing run
#    revokes it. Publish only that tag (README.md, "Release gate").
#
# Needs: podman, sudo (rootful podman), the image built (`just container-build`). Log and table:
# target/escape-test-image.log. Exit status: the suite's.
set -euo pipefail

IMAGE=${1:-localhost/melissa:dev}
NAME=melissa-escape-test
BUILD_IMAGE=localhost/melissa-escape-build:dev
TEST_IMAGE=localhost/melissa-escape-test:run
TESTED_TAG=localhost/melissa:escape-tested
cd "$(dirname "$0")/../.."
mkdir -p target
LOG=target/escape-test-image.log

id=$(podman image inspect --format '{{.Id}}' "$IMAGE")
image_rev=$(podman image inspect --format '{{index .Labels "org.opencontainers.image.revision"}}' "$IMAGE")
src_rev=$(git rev-parse --short HEAD)
dirty=$(git status --porcelain -- crates Cargo.toml Cargo.lock | head -n1)

# A new run on this image revokes an earlier pass until it passes again.
if [ "$(podman image inspect --format '{{.Id}}' "$TESTED_TAG" 2>/dev/null || true)" = "$id" ]; then
    podman untag "$id" "$TESTED_TAG"
fi

echo "== building the suite (Containerfile, target escape-build)"
SOURCE_DATE_EPOCH=$(git log -1 --format=%ct) podman build --layers --network=host \
    -f packaging/container/Containerfile --target escape-build -t "$BUILD_IMAGE" .
podman build --network=host -f packaging/container/Containerfile.escape-test \
    --build-arg IMAGE="$IMAGE" --build-arg TESTS="$BUILD_IMAGE" -t "$TEST_IMAGE" .

echo "== copying the test image into root's storage"
archive=target/melissa-escape-test.tar
rm -f "$archive"
podman save -o "$archive" "$TEST_IMAGE"
sudo podman load -q -i "$archive" >/dev/null
rm -f "$archive"

cleanup() {
    sudo podman rm -f -t 5 "$NAME" >/dev/null 2>&1 || true
    sudo podman rmi -f "$TEST_IMAGE" >/dev/null 2>&1 || true
    podman rmi -f "$TEST_IMAGE" >/dev/null 2>&1 || true
}
trap cleanup EXIT
sudo podman rm -f -t 5 "$NAME" >/dev/null 2>&1 || true

echo "== booting $IMAGE ($NAME: rootful, --privileged, systemd)"
# As the Quadlet (melissa.container): --privileged --systemd=always, podman's network. No ports.
sudo podman run -d --name "$NAME" --hostname melissa --privileged --systemd=always \
    --network podman "$TEST_IMAGE" >/dev/null
# "degraded" is fine (masked units, no k3s); only the boot has to be over. systemctl answers nothing
# until systemd inside is up, so poll (2 minutes at most).
state=
for _ in $(seq 120); do
    state=$(sudo podman exec "$NAME" systemctl is-system-running 2>/dev/null || true)
    case "$state" in running | degraded) break ;; esac
    sleep 1
done
echo "   systemd: ${state:-no answer}"
case "$state" in
    running | degraded) ;;
    *) echo "FAIL: the image did not boot (systemd: ${state:-no answer})"; exit 1 ;;
esac
sudo podman exec "$NAME" id melissa >/dev/null

echo "== running the escape suite as melissa"
set +e
# The server unit's user, hardening and cgroup delegation
# (packaging/common/usr/lib/systemd/system/melissa-server.service). As in `just escape-test`, the shell moves
# itself into a `runner` leaf so the delegated cgroup holds no process, and hands that cgroup to the suite
# (MELISSA_TEST_CGROUP): the `limits.*` probes then run, or skip with the reason if delegation fails here.
# `printf 0` (the builtin: 0 is the writing process) moves the shell.
suite='cg=/sys/fs/cgroup$(sed -n "s/^0:://p" /proc/self/cgroup)
if mkdir -p "$cg/runner" 2>/dev/null && printf 0 > "$cg/runner/cgroup.procs" 2>/dev/null; then
    export MELISSA_TEST_CGROUP="$cg"
fi
exec /opt/melissa-escape/escape --nocapture --test-threads=1 \
    sandbox_escape_suite seccomp_off_backend_is_reported_not_passed'
sudo podman exec "$NAME" systemd-run --quiet --wait --pipe --collect --service-type=exec \
    -p User=melissa -p Group=melissa -p NoNewPrivileges=yes -p ProtectSystem=strict \
    -p ProtectHome=yes -p PrivateTmp=yes -p StateDirectory=melissa-escape \
    -p "Delegate=memory pids cpu io" \
    -E HOME=/var/lib/melissa-escape -E TMPDIR=/tmp -E PATH=/usr/bin \
    -E MELISSA_ESCAPE_REQUIRED=1 -E MELISSA_ESCAPE_DIR=/var/lib/melissa-escape \
    -E MELISSA_ESCAPE_PROBE=/opt/melissa-escape/melissa-escape-probe \
    -E MELISSA_NETBRIDGE=/usr/bin/melissa-netbridge \
    /usr/bin/bash -c "$suite" 2>&1 | tee "$LOG"
rc=${PIPESTATUS[0]}
set -e
grep -E '^Resource limits:' "$LOG" | head -n1 || true

if [ "$rc" -ne 0 ] || ! grep -q '^test result: ok. 2 passed' "$LOG"; then
    echo "FAIL: escape suite failed in $IMAGE (log: $LOG); not tagged $TESTED_TAG"
    exit 1
fi
if [ -n "$dirty" ] || [ "$image_rev" != "$src_rev" ]; then
    echo "PASS, but not tagged $TESTED_TAG: the image is revision '${image_rev:-?}', the suite was built" \
         "from $src_rev${dirty:+ with local changes}. Rebuild the image from this commit and run again."
    exit 0
fi
podman tag "$id" "$TESTED_TAG"
echo "PASS: tagged $TESTED_TAG (${id:0:12}, revision $image_rev)"
