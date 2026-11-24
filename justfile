default:
    @just --list

# crates/escape-probe is its own workspace (test-only helper, kept out of Cargo.lock and the license report).
probe := "--manifest-path crates/escape-probe/Cargo.toml"

fmt:
    cargo fmt --all
    cargo fmt {{probe}}

lint:
    cargo clippy --all-targets -- -D warnings
    cargo clippy {{probe}} --target-dir target --all-targets -- -D warnings

# Builds the escape suite's probe helper first, so the suite's kernel probes run here too.
test: escape-probe
    cargo nextest run
    cargo test -q {{probe}} --target-dir target

# The escape suite's probe helper (test-only) at target/debug/melissa-escape-probe, where the suite finds it.
escape-probe:
    cargo build -q {{probe}} --target-dir target

check: fmt lint test

# Sandbox escape suite (docs/agent-skills/sandbox-escape-tests.md): real escape attempts inside the real agent
# sandbox, printed as a table; fails on any escape. The gate for image updates: required here, so a host without
# bwrap or user namespaces, or without the probe helper (melissa-escape-probe, test-only), fails instead of
# skipping. Also runs the `seccomp = false` check (kernel probes reported as skipped, never passed).
# The resource-limit probes need a delegated cgroup: where systemd's user manager runs, the suite runs in a
# transient scope with Delegate=yes (the shell moves into a `runner` leaf so the scope itself holds no
# process); elsewhere they skip.
escape-test: escape-probe
    #!/usr/bin/env bash
    set -eu
    # `printf 0` (the builtin: 0 is the writing process) moves this shell; systemd-run would turn `$$` into `$`.
    suite='cg=/sys/fs/cgroup$(sed -n "s/^0:://p" /proc/self/cgroup)
        if mkdir -p "$cg/runner" 2>/dev/null && printf 0 > "$cg/runner/cgroup.procs" 2>/dev/null; then
            export MELISSA_TEST_CGROUP="$cg"
        fi
        MELISSA_ESCAPE_REQUIRED=1 exec cargo nextest run -p melissa-agent --test it --no-capture \
            escape::sandbox_escape_suite escape::seccomp_off_backend_is_reported_not_passed'
    if systemd-run --user --scope --quiet -p Delegate=yes true >/dev/null 2>&1; then
        exec systemd-run --user --scope --quiet -p Delegate=yes -- bash -c "$suite"
    fi
    exec bash -c "$suite"

# The same suite inside a built container image (default localhost/melissa:dev), run the way the installed
# container runs: rootful, --privileged, systemd as PID 1, the suite as the `melissa` user with the server
# unit's hardening, in a delegated cgroup so the limit probes run. The test binary and probe helper come from
# the Containerfile's `escape-build` stage and are added to a throwaway copy of the image (never to the image
# itself). Needs sudo (rootful podman). Only a pass, built from the image's own revision, tags the image
# `localhost/melissa:escape-tested`: the one tag a release may push (packaging/container/README.md, "Release gate").
escape-test-image image="localhost/melissa:dev":
    packaging/container/escape-test.sh {{quote(image)}}

# gVisor end to end in the built image (default localhost/melissa:dev): a throwaway rootful container with
# k3s deploys a tiny app with the default isolation and one `standard` through deploy_app, proves which
# kernel each sees, runs cheap escape probes and the fail-closed checks, prints start time and memory.
# Needs sudo and the network (busybox); leaves nothing behind. Log: target/gvisor-test-image.log.
gvisor-test-image image="localhost/melissa:dev":
    packaging/container/gvisor-test.sh {{quote(image)}}

# The server plus melissa-netbridge (the sandbox's network bridge, a binary of melissa-agent:
# `cargo run -p melissa-server` alone does not build it, and sandboxed agents then have no network).
build:
    cargo build --bins -p melissa-server -p melissa-agent

run-web: build
    cargo run -q -p melissa-server

# First run from source (docs/DEV-ENV.md, "Run from source"): builds, prints a short summary (URL, agents found,
# sandbox, k3s) and serves http://127.0.0.1:8080 until Ctrl-C.
demo: build
    cargo run -q -p melissa-server -- demo

# Talk to the running server from a terminal (second shell): `just chat`, `just ask "describe this machine"`.
chat:
    cargo run -q -p melissa-server -- chat

ask +message:
    cargo run -q -p melissa-server -- ask {{quote(message)}}

# --- UI screenshots (scripts/ui-shots, docs/DEV-ENV.md) ---------------------------------
# Every screen at 1440/1024/390 px, dark and light, from a throwaway Melissa with fixed
# content, into target/ui-shots/<revision>/ (index.html is the contact sheet).
# `just ui-shots --rev <rev>` shoots another revision; `--only <text>` a few screens.
ui-shots *args:
    python3 scripts/ui-shots/ui_shots.py shoot {{args}}

# Compare two shot sets (directories or revisions; a revision without shots is shot first):
# diff images and a summary in target/ui-shots/diff-<a>-vs-<b>/.
ui-diff a b *args:
    python3 scripts/ui-shots/ui_shots.py diff {{quote(a)}} {{quote(b)}} {{args}}

# --- design tokens (docs/DEV-ENV.md, Design tokens) ------------------------------------
# Write crates/web/design/tokens.json (Penpot's token file) from scripts/tokens/tokens_gen.py,
# then check it against melissa.css's :root. `just tokens css` prints a :root draft instead.
tokens mode="json":
    #!/usr/bin/env sh
    set -eu
    if [ "{{mode}}" = "json" ]; then
        python3 scripts/tokens/tokens_gen.py json > crates/web/design/tokens.json
        cargo nextest run -p melissa-web tokens::
    else
        python3 scripts/tokens/tokens_gen.py {{mode}}
    fi

# --- container image (packaging/container) ---------------------------------------------
# Build localhost/melissa:dev (rootless). --network=host: rootless pasta may be denied by SELinux.
# Default version: melissa-server's (crates/server/Cargo.toml).
melissa_version := `sed -n 's/^version = "\(.*\)"/\1/p' crates/server/Cargo.toml | head -1`
container-build version=melissa_version:
    # SOURCE_DATE_EPOCH: the Nix dev shell exports 315532800 (1980), which podman uses as the image's
    # creation time (Settings shows it as "built"); use the last commit's time instead.
    SOURCE_DATE_EPOCH=$(git log -1 --format=%ct) podman build --layers --network=host \
        -f packaging/container/Containerfile \
        --build-arg MELISSA_VERSION={{version}} --build-arg MELISSA_REVISION=$(git rev-parse --short HEAD) \
        -t localhost/melissa:dev .

# Install it rootful with the Quadlet (sudo): copies localhost/melissa:dev into root's storage as
# localhost/melissa:latest. Extra settings as env, e.g. `UI_PORT=8090 just container-install`.
container-install:
    mkdir -p target
    rm -f target/melissa-image.tar
    podman save -o target/melissa-image.tar localhost/melissa:dev
    sudo --preserve-env=UI_PORT,UI_BIND,APP_PORT_BASE,BIND,APP_ADDRESSES,AUTO_UPDATE,SET_SYSCTLS \
        IMAGE_ARCHIVE=target/melissa-image.tar packaging/container/install.sh
    rm -f target/melissa-image.tar

# install.sh's address checks, server.toml edits and Docker rules; melissa-setup's network check
# (no root, no podman, no network).
container-test:
    bash packaging/container/test-install.sh
    bash packaging/container/test-melissa-setup.sh

# Remove the installation (keeps the data volume unless purge=--purge).
container-uninstall purge="":
    sudo packaging/container/uninstall.sh {{purge}}

# k3s, Traefik, policies, RBAC, NodePort and pod isolation checks against the installed container.
# Uses the installation's settings (UI_PORT, BIND, APP_PORT_BASE from /var/lib/melissa-container/install.env).
container-smoke:
    sudo sh -c 'set -a; . /var/lib/melissa-container/install.env; NAME=melissa exec scripts/container-run.sh smoke'

# --- third-party licenses -------------------------------------------------------------
# License report for the Rust crates in melissa-server/-device (cargo-about, allowlist in about.toml).
# Fails if a crate's license is not on the allowlist or cannot be identified. Needs the network
# only for `cargo fetch` (crate sources); the image build runs the same command.
licenses out="target/THIRD-PARTY-LICENSES.md":
    cargo fetch --locked
    mkdir -p "$(dirname {{out}})"
    cargo about generate --workspace --frozen --fail -o {{out}} about.hbs
    @echo "wrote {{out}}"
