# Acknowledgments and third-party notices

Melissa stands on the work of others. This file credits projects that shaped its design and lists third-party
software that Melissa redistributes (in its binaries or OS images), with their licenses.

## Design inspiration (ideas only, no code copied)

These projects influenced how Melissa is designed. Melissa does not contain their code; their ideas were studied
and re-implemented in Melissa's own words and structure. If code is ever copied from them, its original copyright
and license notice must be kept with it and listed below under "Included third-party software".

- **T3 Code** — <https://github.com/pingdotgg/t3code>, MIT License, Copyright (c) 2026 T3 Tools Inc.
  Inspired how Melissa talks to many agent backends: one session interface over several protocols, the Agent Client
  Protocol (ACP) flow, mapping agents' permission requests onto an approval policy, and testing with recorded,
  replayed agent sessions.
- **Universal Blue** (ublue-os) — <https://github.com/ublue-os>, Apache License 2.0, by the Universal Blue
  contributors. Inspired how Melissa builds and maintains its bootc images: Containerfile structure and layer order,
  image linting, signing and update practices, and the choice to stay on Fedora's signed kernel for Secure Boot.

## Included third-party software

Redistributed **in the Melissa binaries**:

| Component | License | Notes |
|---|---|---|
| htmx 2.0.4 | BSD Zero Clause (0BSD) | Vendored in `crates/web/assets/`, embedded in the binary |
| Rust crates (tokio, axum, maud, serde, kube, k8s-openapi, agent-client-protocol, ...) | Various (mostly MIT / Apache-2.0) | Full list: `Cargo.lock`. A generated license report is planned (see below) |

Redistributed **in the Melissa OS images** (besides the Fedora base image and its packages, which carry their own
licenses under `/usr/share/licenses`):

| Component | License | Source |
|---|---|---|
| k3s (binary and airgap images) | Apache License 2.0 | <https://github.com/k3s-io/k3s> |
| k3s-selinux | Apache License 2.0 | <https://github.com/k3s-io/k3s-selinux> |
| Tailscale | BSD-3-Clause style (RPM metadata: MIT) | <https://tailscale.com> |
| bubblewrap | LGPL-2.1-or-later | Fedora package |

## To do

- Generate a full license report for the Rust dependencies at build time (e.g. `cargo-about` or `cargo-deny`) and
  ship it in the images under `/usr/share/licenses/melissa/`.
- Ship the k3s `LICENSE` file in the images (`/usr/share/licenses/k3s/`); the k3s binary has no license file of its
  own today (k3s-selinux, Tailscale and bubblewrap come as RPMs with theirs).
