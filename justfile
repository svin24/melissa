default:
    @just --list

fmt:
    cargo fmt --all

lint:
    cargo clippy --all-targets -- -D warnings

test:
    cargo nextest run

check: fmt lint test

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
container-build version="dev":
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
