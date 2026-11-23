# Agent skills index

Read this at the start of every session. One file per skill; update the skill when you learn something.

- [dev-shell-and-checks.md](dev-shell-and-checks.md) — devenv shell, `just check`, committing with hooks, host gotchas.
- [agent-cli-backends.md](agent-cli-backends.md) — driving claude/grok/opencode headlessly, safety rules, config.
- [htmx-component.md](htmx-component.md) — the fixed component library, htmx patterns, CSP/CSRF, router tests.
- [ui-craft.md](ui-craft.md) — how UI work should look and read: the CSS tokens and sizes in use, edges, one
  primary action, copy rules and tells to avoid, motion and states, icons, the browser check at 1440/1024/768/390
  in light and dark, reviewing UI, proposed checks, and what from third-party design skills does not apply.
- [acp-sessions.md](acp-sessions.md) — agent sessions: ACP (`grok-acp`, `opencode-acp`) and Claude's stream-json
  (`claude-session`): session API, verified protocol facts per agent, permission mapping, cancel, resume,
  recording/replay transcripts, gotchas.
- [agent-egress.md](agent-egress.md) — deny-by-default network for sandboxed agents: netns + `melissa-netbridge`,
  CONNECT proxy with per-backend allowlists, MCP-only socket, `network = proxy|learn|none|host`, what each CLI contacts.
- [sandbox-escape-tests.md](sandbox-escape-tests.md) — the adversarial escape suite (`just escape-test`, `just escape-test-image`, the gate for
  image updates): how probes run, verdict words, controls, seccomp-gated probes, adding a probe in a few lines.
- [lead-loop.md](lead-loop.md) — the lead agent and its persistent, named agents: the shared
  conversation machinery, agent registry, agent tools and guards, event stream + transcript, resume via a
  Melissa-owned session store, `/api/lead`, `/api/agents`, `melissa-server chat|ask [--agent]`, `agent`, `workspace`;
  switching a conversation's backend/model at runtime (`backend list|use`, models only as agents report them).
- [container-image.md](container-image.md) — Melissa as a rootful podman container (systemd as PID 1, Quadlet):
  `packaging/common` vs `packaging/container`, build/install/smoke loop, image lint, UI forward + nftables filter,
  logind/sleep masked, persistence, the host update helper, what's verified. (The bootc images, VM tests and
  their skills are archived at tag `bootc-archive-2026-11-19`.)
- [nix-service.md](nix-service.md) — Melissa from the flake: package, NixOS module `services.melissa` (options, k3s,
  nix-ld for vendor CLIs), the sandbox on NixOS (`SystemRoot`), the NixOS VM test, the non-NixOS `melissa-host`
  bundle and why not system-manager.
- [k3s-in-container.md](k3s-in-container.md) — k3s pinned + sha256-checked in the image, airgap images, iptables-nft, container config, Traefik telemetry off, smoke test,
  lead agent's read-only API access (ServiceAccount, systemd credential, rotation).
- [kube-rs-client.md](kube-rs-client.md) — `crates/cluster`: kube-rs over rustls, `ClusterRead` trait + fakes, adding cluster tools.
- [app-deploy.md](app-deploy.md) — no catalog: agents deploy any app from a JSON spec they write
  (`deploy_app`/`update_app`; images, TCP/UDP ports, volumes, secrets, files, resources, probes), validation,
  rendering, the always-confirmed deploy, app ports and links (per-port NodePorts 30000-30079), the
  `melissa-apps` write identity + admission policies (workloads, services, volumes), owner-only
  `app-credentials`, the migration of catalog installs (Nextcloud).
- [workspaces-and-ownership.md](workspaces-and-ownership.md) — the cluster model in code: device/agent ids, workspaces owned by
  agents (`melissa.io/owner-agent`), scopes (User/Lead/Agent/Nothing), the one scope check in the registry
  (another agent's workspace: user-only confirmation, no rule/bypass; `my_apps`; `tool.cross_owner` audit),
  reassign path (Lead agent + user's yes, or the user's CLI),
  migration of old owner labels, admission rules, declaring a tool's target.
- [settings-ui.md](settings-ui.md) — Settings pages: Providers (`providers.toml` over `server.toml`, secrets
  write-only, versions, disable rules, Install button via the polkit-limited `melissa-agent-install@` unit, Nix note, sign-in guidance; echo hidden) and Updates (container image status, host update helper via request files,
  restart via polkit, confirmation cards); `/api/settings/...`, `melissa-server provider|update`; adding a section.
- [e2e-lead-app-install.md](e2e-lead-app-install.md) — the owner installs/checks/uninstalls Nextcloud by
  chatting with the lead agent: laptop lead agent + VM k3s setup, verified run with timings, taint and confirmation gotchas.

## Claude Code plugins and project skills (2026-11-18)
- Project skill `.claude/skills/melissa-ui/SKILL.md`: binding UI rules; overrides third-party design skills.
  Its details live in [ui-craft.md](ui-craft.md) (2026-11-20), distilled from tasteskill and interface-design
  (both MIT; credited in ACKNOWLEDGMENTS.md). Neither is installed in the project.
- Plugins enabled at project scope (`.claude/settings.json`, skill-only, no MCP/hooks/network):
  `frontend-design` (Anthropic) and `modern-web-guidance` (Google Chrome, pinned commit). Use them for typography,
  spacing, accessibility and modern CSS inside the fixed component library, never for runtime agent UI.
- Not installed: `playwright` (runs `npx @playwright/mcp@latest`, unpinned; no Node on the host; T3 Code's preview
  tools cover browser checks), `rust-analyzer-lsp` (rust-analyzer only exists in the dev shell; no global installs).

