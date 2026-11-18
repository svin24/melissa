# Melissa

> **Pre-alpha software.** This is an early demo for people to try and give feedback on. Expect bugs, breaking
> changes and missing pieces. Don't run it on a machine or with data you care about, and don't expose it to the
> internet. Agents act on your behalf through AI subscriptions you log in to; review what they ask to do.

**A clustered, agent-run, immutable self-hosting OS.**

Melissa turns a handful of ordinary machines (a laptop, a mini PC, a NAS, a VPS) into one self-hosting
cluster that AI agents run for you. You chat with the Queen; she and her bees install apps like Nextcloud,
keep them healthy and tidy up, asking you before anything risky.

- **Hive**: all your devices together. **Comb**: one device, with one **Queen** (the lead agent).
- **Bees**: worker agents; each looks after its own **cells** (apps and their services).
- You can talk to any Queen or bee. Only a Queen, or you, can hand a cell to another bee.
  Full model: [docs/HIVE-MODEL.md](docs/HIVE-MODEL.md).

Principles: immutable base (Fedora bootc), **Rust** almost everywhere (agents work better with a safe,
strongly typed language), a boring server-rendered web UI with no AI-generated UI, a UI that works on a LAN
without internet, and open source so others can build on top.

## Layout

```
crates/agent   agent layer: backend trait, echo + CLI backends, task board, audit log
crates/queen   queen bee (coordinator + agent layer entrypoint)
crates/comb    per-device daemon (a comb = one device)
crates/web     web UI (axum + maud + embedded htmx)
image/         bootc Containerfile with desktop (Kinoite/KDE) and headless (fedora-bootc) variants
scripts/       VM helpers (QEMU)
docs/          PLAN.md (full plan), DEV-ENV.md (Nix + devenv setup)
```

## Try it

- **From source** on any Linux (x86_64): [docs/QUICKSTART.md](docs/QUICKSTART.md). In short: install
  bubblewrap and one agent CLI (Claude Code, Grok CLI or OpenCode) and log in, then
  `devenv shell -- just demo` and open http://127.0.0.1:8080. No k3s, so no app installs.
- **VM image** with k3s: coming with the pre-alpha release.

## Quick start (dev)

With Nix + devenv + direnv available (see [docs/DEV-ENV.md](docs/DEV-ENV.md)):

```
direnv allow      # or: devenv shell
just check        # fmt, clippy, tests
```

Run the queen and open http://127.0.0.1:8080: `devenv shell -- just run-web` (full log) or `just demo` (short summary). Agents found on `PATH`
(`claude`, `grok`, `opencode`) show up as backends; a built-in `echo` backend always works.

Images: `just image-build desktop|headless`, VM tests with `just vm-disk` / `just vm-run` (see docs/agent-skills).

Status: early. The multi-agent demo works locally; images are being brought up in VMs (milestone M0).
Start with [docs/PLAN.md](docs/PLAN.md). Agents must read [AGENTS.md](AGENTS.md) first.

Credits and third-party notices: [ACKNOWLEDGMENTS.md](ACKNOWLEDGMENTS.md).

License: not chosen yet (Melissa is meant to become open source). Until a license is added, no rights to reuse the code are granted.
