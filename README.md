# Melissa

> **Pre-alpha software.** This is an early demo for people to try and give feedback on. Expect bugs, breaking
> changes and missing pieces. Don't run it on a machine or with data you care about, and don't expose it to the
> internet. Agents act on your behalf through AI subscriptions you log in to; review what they ask to do.

**A clustered, agent-run, immutable self-hosting OS.**

Melissa turns a handful of ordinary machines (a laptop, a mini PC, a NAS, a VPS) into one self-hosting
cluster that AI agents run for you. You chat with the lead agent; it and its agents install apps like Nextcloud,
keep them healthy and tidy up, asking you before anything risky.

- **Cluster**: all your devices together. **Device**: one device, with one **Lead agent** (the lead agent).
- **Agents**: worker agents; each looks after its own **workspaces** (apps and their services).
- You can talk to any lead agent or agent. Only a lead agent, or you, can hand a workspace to another agent.
  Full model: [docs/AGENT-MODEL.md](docs/AGENT-MODEL.md).

Principles: runs on an existing Linux host (a rootful podman container started by systemd; a Nix service is
in progress), **Rust** almost everywhere (agents work better with a safe,
strongly typed language), a boring server-rendered web UI with no AI-generated UI, a UI that works on a LAN
without internet, and open source so others can build on top.

## Layout

```
crates/agent   agent layer: backend trait, echo + CLI backends, task board, audit log
crates/server   lead agent agent (coordinator + agent layer entrypoint)
crates/device    per-device daemon (a device = one device)
crates/web     web UI (axum + maud + embedded htmx)
packaging/     common/ (system files every packaging installs), container/ (image, Quadlet, install.sh),
               nix/ (Nix flake/module)
scripts/       container-run.sh (dev/test driver for the container)
docs/          PLAN.md (full plan), DEV-ENV.md (Nix + devenv setup)
```

## Try it

- **From source** on any Linux (x86_64): [docs/QUICKSTART.md](docs/QUICKSTART.md). In short: install
  bubblewrap and one agent CLI (Claude Code, Grok CLI or OpenCode) and log in, then
  `devenv shell -- just demo` and open http://127.0.0.1:8080. No k3s, so no app installs.
- **Full system in a container** (rootful podman, started by systemd; k3s and apps included):
  [packaging/container/README.md](packaging/container/README.md), short version in
  [docs/QUICKSTART.md](docs/QUICKSTART.md).

## Quick start (dev)

With Nix + devenv + direnv available (see [docs/DEV-ENV.md](docs/DEV-ENV.md)):

```
direnv allow      # or: devenv shell
just check        # fmt, clippy, tests
```

Run the lead agent and open http://127.0.0.1:8080: `devenv shell -- just run-web` (full log) or `just demo` (short summary). Agents found on `PATH`
(`claude`, `grok`, `opencode`) show up as backends; a built-in `echo` backend always works.

Full system (k3s, apps) as a container: `just container-build && just container-install`, then
`just container-smoke` (see [packaging/container/README.md](packaging/container/README.md)). The earlier bootc OS
images are archived at git tag `bootc-archive-2026-11-19`.

Status: early. The multi-agent demo works locally; the full system runs as a rootful container (milestone M0).
Start with [docs/PLAN.md](docs/PLAN.md). Agents must read [AGENTS.md](AGENTS.md) first.

Credits and third-party notices: [ACKNOWLEDGMENTS.md](ACKNOWLEDGMENTS.md).

License: [MIT](LICENSE).
