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

# Build the bootc image locally with the host's podman.
image-build:
    podman build -t localhost/melissa:dev -f image/Containerfile .

# Lint the built image with the host's bootc.
image-lint:
    podman run --rm localhost/melissa:dev bootc container lint
