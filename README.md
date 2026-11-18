# Melissa

**An open source, clustered, agent-run, immutable self-hosting OS.**

Melissa turns a handful of ordinary machines (a laptop, a mini PC, a NAS, a VPS) into one self-hosting
cluster that an AI agent operates for you. Install apps like Nextcloud or Immich from a fixed catalog,
and let an agent keep the hive updated, backed up and healthy.

- **Bee** = a node (any machine running Melissa).
- **Hive** = the cluster.
- **Queen bee** = the master agent that coordinates the hive. Every bee can also run its own agents.

Principles: immutable base (Fedora bootc), **Rust** almost everywhere (agents work better with a safe,
strongly typed language), a boring server-rendered web UI with no AI-generated UI, a UI that works on a LAN
without internet, and open source so others can build on top.

## Layout

```
crates/agent   agent layer: backend trait, echo + CLI backends, task board, audit log
crates/queen   queen bee (coordinator + agent layer entrypoint)
crates/bee     per-node daemon
crates/web     web UI (axum + maud + embedded htmx)
image/         bootc Containerfile with desktop (Kinoite/KDE) and headless (fedora-bootc) variants
scripts/       VM helpers (QEMU)
docs/          PLAN.md (full plan), DEV-ENV.md (Nix + devenv setup)
```

## Quick start (dev)

With Nix + devenv + direnv available (see [docs/DEV-ENV.md](docs/DEV-ENV.md)):

```
direnv allow      # or: devenv shell
just check        # fmt, clippy, tests
```

Run the queen and open http://127.0.0.1:8080: `devenv shell -- just run-web`. Agents found on `PATH`
(`claude`, `grok`, `opencode`) show up as backends; a built-in `echo` backend always works.

Images: `just image-build desktop|headless`, VM tests with `just vm-disk` / `just vm-run` (see docs/agent-skills).

Status: early. The multi-agent demo works locally; images are being brought up in VMs (milestone M0).
Start with [docs/PLAN.md](docs/PLAN.md). Agents must read [AGENTS.md](AGENTS.md) first.
