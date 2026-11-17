# Melissa

**An open source, clustered, agent-run, immutable self-hosting OS.**

Melissa turns a handful of ordinary machines (a laptop, a mini PC, a NAS, a VPS) into one self-hosting
cluster that an AI agent operates for you. Install apps like Nextcloud or Immich from a fixed catalog,
and let an agent keep the hive updated, backed up and healthy.

- **Bee** = a node (any machine running Melissa).
- **Hive** = the cluster.
- **Queen bee** = the master agent that coordinates the hive. Every bee can also run its own agents.

Principles: immutable base (Fedora bootc), Go everywhere, boring web UI with no AI-generated UI,
works on a LAN with no internet dependency for the UI, open source so others can build on top.

Status: early planning. Start with [docs/PLAN.md](docs/PLAN.md). Agents must read [AGENTS.md](AGENTS.md) first.
