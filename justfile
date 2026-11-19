default:
    @just --list

fmt:
    cargo fmt --all

lint:
    cargo clippy --all-targets -- -D warnings

test:
    cargo nextest run

check: fmt lint test

# The queen plus melissa-netbridge (the sandbox's network bridge, a binary of melissa-agent:
# `cargo run -p melissa-queen` alone does not build it, and sandboxed agents then have no network).
build:
    cargo build --bins -p melissa-queen -p melissa-agent

run-web: build
    cargo run -q -p melissa-queen

# First run from source (docs/QUICKSTART.md): builds, prints a short summary (URL, agents found,
# sandbox, k3s) and serves http://127.0.0.1:8080 until Ctrl-C.
demo: build
    cargo run -q -p melissa-queen -- demo

# Talk to the running Queen from a terminal (second shell): `just chat`, `just ask "describe this machine"`.
chat:
    cargo run -q -p melissa-queen -- chat

ask +message:
    cargo run -q -p melissa-queen -- ask {{quote(message)}}

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
    podman save -o target/melissa-image.tar localhost/melissa:dev
    sudo --preserve-env=UI_PORT,UI_BIND,APP_PORT_BASE,BIND,APP_ADDRESSES,AUTO_UPDATE,SET_SYSCTLS \
        IMAGE_ARCHIVE=target/melissa-image.tar packaging/container/install.sh
    rm -f target/melissa-image.tar

# Remove the installation (keeps the data volume unless purge=--purge).
container-uninstall purge="":
    sudo packaging/container/uninstall.sh {{purge}}

# k3s, Traefik, policies, RBAC, NodePort and pod isolation checks against the installed container.
container-smoke:
    sudo env NAME=melissa UI_PORT=${UI_PORT:-8080} APP_PORT_BASE=${APP_PORT_BASE:-30000} scripts/container-run.sh smoke

# --- third-party licenses -------------------------------------------------------------
# License report for the Rust crates in melissa-queen/-comb (cargo-about, allowlist in about.toml).
# Fails if a crate's license is not on the allowlist or cannot be identified. Needs the network
# only for `cargo fetch` (crate sources); the image build runs the same command.
licenses out="target/THIRD-PARTY-LICENSES.md":
    cargo fetch --locked
    mkdir -p "$(dirname {{out}})"
    cargo about generate --workspace --frozen --fail -o {{out}} about.hbs
    @echo "wrote {{out}}"
