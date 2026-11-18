# Decisions

Format: date — decision. Why. Alternatives considered.

- **2026-11-18 — Agent backends drive the vendors' CLIs headlessly** (`claude -p`, `grok -p`, `opencode run`) behind
  one Rust `Backend` trait, plus a built-in `echo` backend. Why: reuses the user's existing subscriptions and logins,
  no API keys in Melissa, works today. Alternatives: direct HTTP APIs (needs keys, per-vendor code), ACP/MCP
  bridges (later, once tools exist).
- **2026-11-18 — Prompts go to CLIs as a single argv element, never through a shell; prompts starting with `-` are
  rejected.** Why: prevents shell injection and flag injection (e.g. an auto-approve flag).
- **2026-11-18 — Fixed component library in `crates/web/src/components.rs`; pages only call it.** Why: AGENTS.md UI rule;
  one reviewable place for markup. Hand-written CSS instead of a vendored framework (small, no download).
- **2026-11-18 — Until login exists: loopback-only bind, same-origin check for POSTs, strict CSP.** Why: no auth yet.
  Alternative: CSRF tokens now (deferred to the session work).
- **2026-11-18 — Two bootc image variants (desktop on Kinoite, headless on fedora-bootc) from one Containerfile.**
  Why: owner wants both; the laptop moves to the desktop one. Shared build stage keeps them in sync.
- **2026-11-18 — Extra packages are baked into the images (package list files), not layered on hosts.** Why: owner
  preference, reproducible bees.
- **2026-11-18 — Desktop queen runs as a per-user systemd unit; headless as a system unit with DynamicUser.** Why: the
  agent CLIs and their logins are per user on desktops; servers need no login session.
- **2026-11-18 — VM testing with QEMU from the Nix dev shell + bootc-image-builder.** Why: no host packages needed on an
  immutable OS. Alternative: libvirt/virt-install on the host (not installed).
- **2026-11-18 — Code hosted in a private GitHub repo.** Owner approved; public release needs an explicit yes.
- **2026-11-18 — Base directly on Fedora's atomic images (Kinoite, fedora-bootc), not on Universal Blue or a custom
  kernel.** Why: Fedora's shim, kernel and modules are signed, so Secure Boot works out of the box with no MOK
  enrollment; owner requirement at least through the first demo. The build enforces it (one kernel, kernel-core
  signed by the Fedora release key, no `kmod-*`/`akmod-*`), and VM tests boot with Secure Boot on.
  Alternatives: ublue bases (own kernel/kmods signed with their key, needs MOK), sealed images with our own key (later).
- **2026-11-18 — Take ideas from Universal Blue (Apache-2.0), never code.** Notes: `~/.local/share/melissa/research/`.
- **2026-11-18 — Agents get tools only through MCP served by the queen, one session token per task.** Why: every agent
  CLI speaks MCP; per-task tokens tie each call to that task's policy context; Claude's built-in tools are removed
  (`--tools ""`) so Melissa's policy is the only path to act. Alternatives: a shell with an allowlist (rejected: too
  broad), per-CLI tool plugins (more code per vendor).
- **2026-11-18 — Tool policy keys off labels (private / untrusted / egress / effect), not tool names.** Why: new tools are
  safe by construction if labelled honestly; labels are reviewed with the tool. Destructive always confirms.
- **2026-11-18 — k3s from pinned, sha256-verified release artifacts (binary, airgap images) plus k3s-selinux 1.6 (coreos
  build), all baked into the image; SQLite datastore; bundled Traefik with telemetry off.** Why: reproducible, offline
  first boot, SELinux enforcing, version moves with the image. Alternatives: the install script (mutates /etc and
  /usr/local, unpinned), k3s-selinux 1.7 testing builds (not stable), cert-manager now (later, for ACME in M4).
- **2026-11-18 — Apps are pulled live at runtime, driven by sandboxed agents (owner).** The agent decides and calls tools;
  the queen validates against catalog manifests and applies them to k3s; containerd pulls the images. Agents never hold
  cluster credentials or general egress. Alternatives: shipping app images in the OS image or sysexts (rejected for the
  catalog: too big, slow to update), agents running containers themselves (rejected: bypasses policy and audit).
- **2026-11-18 — Image = bare necessities for web UI + agents, plus k3s; the box ships empty and agents set up the rest.**
  Owner decision. k3s stays in the image (it's the platform agents act on; installing it would need host root).
- **2026-11-18 — Agent CLIs run under bubblewrap by default, with per-CLI presets of the narrowest home paths that
  work.** Why: defence in depth; agents must not read the user's files or change the host. bwrap ships with Fedora
  Atomic desktops and fedora-bootc (listed in `packages-common.txt` so it stays), is small, rootless, and needs no
  network setup (rootless podman's pasta networking is broken on the dev laptop). Host network is shared for now;
  a deny-by-default egress proxy is the next step. Alternatives: rootless podman (network broken here, heavier),
  systemd-run/landlock (less isolation of the file tree), no sandbox (rejected).
- **2026-11-18 — The Queen is the head agent; workers are sandboxed agents it starts.** Owner decision. Queen backend is a
  configurable list of subscription backends; no local LLMs on weak hardware.
- **2026-11-18 — ACP is the primary protocol for long-lived Queen/worker sessions; one-shot CLI runs stay as fallback.**
  Why: standard JSON-RPC over stdio with streaming updates, permission requests, cancel and resume; Grok/Kiro/fx speak
  it; tools can be passed per session (`mcpServers`). Use the official Rust crate `agent-client-protocol` (Apache-2.0).
  Permission requests map onto Melissa's policy; never answer with "always". Studied in T3 Code (MIT, reference only).
- **2026-11-18 — The queen reaches k3s as ServiceAccount `melissa/melissa-queen` with a non-expiring Secret-based token,
  minted into `/run/credstore/melissa.kubeconfig` by a root oneshot and imported with `ImportCredential=`.** Why:
  systemd credentials are copied once at service start, so a short-lived bound token (`kubectl create token`) would
  force queen restarts (killing agent tasks) on every refresh; the token is root-only on disk, scoped by read-only
  RBAC, and revocable by recreating the ServiceAccount (`melissa-kubeconfig rotate`; recreating only the Secret
  yields the same token). `ImportCredential=` (not `LoadCredential=`
  with a path) because a missing credential is fine: the queen starts without k3s (desktop, k3s down) and
  `cluster_health` says so. Alternatives: bound tokens + timer + restart (rejected: restarts), reading k3s.yaml
  (rejected: cluster-admin), a group-readable token file refreshed in place (possible later).
- **2026-11-18 — Revised: the queen's kubeconfig is a group-readable file in `/run/melissa-kube`, not a systemd
  credential, and the queen does not wait for k3s.** `melissa-kubeconfig` writes `queen.kubeconfig` root:`melissa-kube`
  0640 on tmpfs (dir 0750, headless only); `melissa-queen.service` keeps `DynamicUser=yes` and gets
  `SupplementaryGroups=melissa-kube`; the queen re-reads the file on every cluster call. Why: credentials are copied
  only at service start, so ordering the queen after the 300 s oneshot delayed the web UI and agents at first boot,
  and a late or rotated kubeconfig needed a restart that kills agent tasks. Trade-off: the token is readable by group
  `melissa-kube` instead of root only; no login user is in that group and only root can put a unit in it, so the
  exposure is the same set of principals (root + the queen). Alternatives: credential + restart on change (rejected:
  kills tasks), ACL for the dynamic UID (UID changes per start), a root helper serving tokens over a socket (more code;
  revisit for short-lived tokens, PLAN Q25).
- **2026-11-18 — Cluster code lives in a new `crates/hive` (kube-rs 5.1 + k8s-openapi 0.29 `v1_37`, rustls + ring),
  behind narrow traits (`ClusterRead` now, a separate write trait later).** Why: keeps the large kube dependency tree out
  of `melissa-agent`, tools are unit-tested against a fake, and read-only tools can't reach write methods.
  Alternatives: a module in `crates/agent` (heavier builds for every agent test), shelling out to kubectl (rejected).
