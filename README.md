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
crates/queen   queen bee (coordinator + agent layer entrypoint)
crates/bee     per-node daemon
crates/web     web UI (axum + maud + embedded htmx)
image/         bootc Containerfile(s)
docs/          PLAN.md (full plan), DEV-ENV.md (Nix + devenv setup)
```

## Quick start (dev)

With Nix + devenv + direnv available (see [docs/DEV-ENV.md](docs/DEV-ENV.md)):

```
direnv allow      # or: devenv shell
just check        # fmt, clippy, tests
```

Status: early planning; the crates are a skeleton and have not been built yet.
Start with [docs/PLAN.md](docs/PLAN.md). Agents must read [AGENTS.md](AGENTS.md) first.
