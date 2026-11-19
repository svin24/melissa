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

# --- bootc images (variant: desktop | headless) -------------------------------------
# Rootless build into the user's container storage.
image-build variant="headless":
    # --network=host: rootless pasta networking fails on the dev laptop (SELinux AVC on a FUSE mount).
    # SOURCE_DATE_EPOCH: the Nix dev shell exports 315532800 (1980), which podman uses as the image's
    # creation time (bootc shows it as "built"); use the last commit's time instead.
    SOURCE_DATE_EPOCH=$(git log -1 --format=%ct) podman build --layers --network=host -f image/Containerfile \
        --build-arg VARIANT={{variant}} \
        --build-arg BASE={{ if variant == "desktop" { "quay.io/fedora-ostree-desktops/kinoite:45" } else { "quay.io/fedora/fedora-bootc:45" } }} \
        --build-arg MELISSA_VERSION=$(git describe --always --dirty) \
        -t localhost/melissa-{{variant}}:dev .

# Lint the built image (the Containerfile also lints as its last step).
image-lint variant="headless":
    podman run --rm --network=none localhost/melissa-{{variant}}:dev bootc container lint

# Copy the image into root's storage (bootc-image-builder and `bootc switch` read it there).
image-to-root variant="headless":
    podman image scp localhost/melissa-{{variant}}:dev root@localhost::

# Build a qcow2 VM disk with bootc-image-builder into vm/<variant>/ (needs sudo).
vm-disk variant="headless":
    scripts/vm-prep.sh {{variant}}
    sudo podman run --rm --privileged --pull=newer --network=host \
        --security-opt label=type:unconfined_t \
        -v ./vm/{{variant}}:/output \
        -v /var/lib/containers/storage:/var/lib/containers/storage \
        quay.io/centos-bootc/bootc-image-builder:latest \
        --type qcow2 --rootfs btrfs --config /output/config.toml localhost/melissa-{{variant}}:dev
    sudo chown -R $(id -u):$(id -g) vm/{{variant}}

# --- downloadable VM disk for testers (docs/TRY-THE-VM.md) ---------------------------
# No user, password or key inside: the tester creates the account on first boot.
# Run `just image-build` and `just image-to-root` first. RELEASE_CONFIG=<file> for another config.
# Build the published qcow2 into dist/<variant>/ with bootc-image-builder (needs sudo).
release-disk variant="headless":
    mkdir -p dist/{{variant}}
    cp "${RELEASE_CONFIG:-image/release/config.toml}" dist/{{variant}}/config.toml
    sudo podman run --rm --privileged --pull=newer --network=host \
        --security-opt label=type:unconfined_t \
        -v ./dist/{{variant}}:/output \
        -v /var/lib/containers/storage:/var/lib/containers/storage \
        quay.io/centos-bootc/bootc-image-builder:latest \
        --type qcow2 --rootfs btrfs --config /output/config.toml localhost/melissa-{{variant}}:dev
    sudo chown -R $(id -u):$(id -g) dist/{{variant}}

# Compress the release qcow2 and write its sha256 (rootless): dist/melissa-<variant>-<version>-x86_64.qcow2
release-pack variant="headless":
    scripts/release-pack.sh {{variant}}

# Boot the VM disk in QEMU (UEFI, KVM). Uses a throwaway overlay so the built disk stays clean.
vm-run variant="headless":
    scripts/vm-run.sh {{variant}}

# Update + rollback test against the running VM (switch to the current local image, reboot, roll back).
vm-update-test variant="headless":
    scripts/vm-update-test.sh {{variant}}

# Fast dev loop against a running test VM (see scripts/vm-dev.sh): setup | registry | deploy | hotswap | ssh
vm-dev cmd variant="headless" *args="":
    scripts/vm-dev.sh {{cmd}} {{variant}} {{args}}

# k3s smoke test on the running headless VM (node Ready, airgap images, SELinux, ingress via 127.0.0.1:18080)
vm-k3s-smoke:
    scripts/vm-k3s-smoke.sh

# --- third-party licenses -------------------------------------------------------------
# License report for the Rust crates in melissa-queen/-comb (cargo-about, allowlist in about.toml).
# Fails if a crate's license is not on the allowlist or cannot be identified. Needs the network
# only for `cargo fetch` (crate sources); the image build runs the same command.
licenses out="target/THIRD-PARTY-LICENSES.md":
    cargo fetch --locked
    mkdir -p "$(dirname {{out}})"
    cargo about generate --workspace --frozen --fail -o {{out}} about.hbs
    @echo "wrote {{out}}"
