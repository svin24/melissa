default:
    @just --list

fmt:
    cargo fmt --all

lint:
    cargo clippy --all-targets -- -D warnings

test:
    cargo nextest run

check: fmt lint test

run-web:
    cargo run -p melissa-queen

# --- bootc images (variant: desktop | headless) -------------------------------------
# Rootless build into the user's container storage.
image-build variant="headless":
    # --network=host: rootless pasta networking fails on the dev laptop (SELinux AVC on a FUSE mount).
    podman build --layers --network=host -f image/Containerfile \
        --build-arg VARIANT={{variant}} \
        --build-arg BASE={{ if variant == "desktop" { "quay.io/fedora-ostree-desktops/kinoite:45" } else { "quay.io/fedora/fedora-bootc:45" } }} \
        --build-arg MELISSA_VERSION=$(git describe --always --dirty) \
        -t localhost/melissa-{{variant}}:dev .

# Lint the built image (the Containerfile also lints as its last step).
image-lint variant="headless":
    podman run --rm localhost/melissa-{{variant}}:dev bootc container lint

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
# License report for the Rust crates in melissa-queen/-bee (cargo-about, allowlist in about.toml).
# Fails if a crate's license is not on the allowlist or cannot be identified. Needs the network
# only for `cargo fetch` (crate sources); the image build runs the same command.
licenses out="target/THIRD-PARTY-LICENSES.md":
    cargo fetch --locked
    mkdir -p "$(dirname {{out}})"
    cargo about generate --workspace --frozen --fail -o {{out}} about.hbs
    @echo "wrote {{out}}"
