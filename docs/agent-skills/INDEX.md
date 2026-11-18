# Agent skills index

Read this at the start of every session. One file per skill; update the skill when you learn something.

- [dev-shell-and-checks.md](dev-shell-and-checks.md) — devenv shell, `just check`, committing with hooks, host gotchas.
- [agent-cli-backends.md](agent-cli-backends.md) — driving claude/grok/opencode headlessly, safety rules, config.
- [htmx-component.md](htmx-component.md) — the fixed component library, htmx patterns, CSP/CSRF, router tests.
- [bootc-image-build.md](bootc-image-build.md) — building desktop/headless images, layer order, laptop switch.
- [bootc-vm-test.md](bootc-vm-test.md) — qcow2 with bootc-image-builder, booting in QEMU, SSH into test VMs.
- [acp-sessions.md](acp-sessions.md) — agent sessions: ACP (`grok-acp`, `opencode-acp`) and Claude's stream-json
  (`claude-session`): session API, verified protocol facts per agent, permission mapping, cancel, resume,
  recording/replay transcripts, gotchas.
- [agent-egress.md](agent-egress.md) — deny-by-default network for sandboxed agents: netns + `melissa-netbridge`,
  CONNECT proxy with per-backend allowlists, MCP-only socket, `network = proxy|learn|none|host`, what each CLI contacts.
- [queen-loop.md](queen-loop.md) — the Queen (head agent): session manager, worker tools and guards, event
  stream + transcript, resume via a Melissa-owned Grok session store, `/api/queen`, `melissa-queen chat|ask`.
- [k3s-on-bootc.md](k3s-on-bootc.md) — k3s pinned + verified in the image, airgap images, SELinux, Traefik telemetry off, smoke test,
  queen's read-only API access (ServiceAccount, systemd credential, rotation).
- [kube-rs-client.md](kube-rs-client.md) — `crates/hive`: kube-rs over rustls, `ClusterRead` trait + fakes, adding cluster tools.
- [app-catalog.md](app-catalog.md) — app manifests (TOML schema, pinning digests, adding an app), rendering, the
  app tools and their labels, the `melissa-apps` write identity + admission policies, owner-only
  `app-credentials`, Nextcloud verified in the VM.

## Claude Code plugins and project skills (2026-11-18)
- Project skill `.claude/skills/melissa-ui/SKILL.md`: binding UI rules; overrides third-party design skills.
- Plugins enabled at project scope (`.claude/settings.json`, skill-only, no MCP/hooks/network):
  `frontend-design` (Anthropic) and `modern-web-guidance` (Google Chrome, pinned commit). Use them for typography,
  spacing, accessibility and modern CSS inside the fixed component library, never for runtime agent UI.
- Not installed: `playwright` (runs `npx @playwright/mcp@latest`, unpinned; no Node on the host; T3 Code's preview
  tools cover browser checks), `rust-analyzer-lsp` (rust-analyzer only exists in the dev shell; no global installs).

