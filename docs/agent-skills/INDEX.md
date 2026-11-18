# Agent skills index

Read this at the start of every session. One file per skill; update the skill when you learn something.

- [dev-shell-and-checks.md](dev-shell-and-checks.md) — devenv shell, `just check`, committing with hooks, host gotchas.
- [agent-cli-backends.md](agent-cli-backends.md) — driving claude/grok/opencode headlessly, safety rules, config.
- [htmx-component.md](htmx-component.md) — the fixed component library, htmx patterns, CSP/CSRF, router tests.
- [bootc-image-build.md](bootc-image-build.md) — building desktop/headless images, layer order, laptop switch.
- [bootc-vm-test.md](bootc-vm-test.md) — qcow2 with bootc-image-builder, booting in QEMU, SSH into test VMs.
- [acp-sessions.md](acp-sessions.md) — ACP sessions (`grok-acp`): session API, verified Grok facts, permission mapping, recording/replay transcripts, crate gotchas.
- [k3s-on-bootc.md](k3s-on-bootc.md) — k3s pinned + verified in the image, airgap images, SELinux, Traefik telemetry off, smoke test,
  queen's read-only API access (ServiceAccount, systemd credential, rotation).
- [kube-rs-client.md](kube-rs-client.md) — `crates/hive`: kube-rs over rustls, `ClusterRead` trait + fakes, adding cluster tools.
