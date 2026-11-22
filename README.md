# Melissa

> **Pre-alpha software.** This is an early demo for people to try and give feedback on. Expect bugs, breaking
> changes and missing pieces. Don't run it on a machine or with data you care about, and don't expose it to the
> internet. Agents act on your behalf through AI subscriptions you log in to; review what they ask to do.

**A self-hosting server that AI agents run for you.**

Melissa runs on a Linux machine you already have. You chat with Melissa, the lead agent; she and her
helper agents install apps like Nextcloud, keep them healthy and tidy up, asking you before anything risky.
Agents use the AI subscriptions you sign in to: Claude, Grok, OpenCode, Gemini, Codex or Muse Code.

- **Device**: one machine, with one **lead agent** in charge. **Cluster**: all your devices together
  (one device today).
- **Agents** (helpers in the UI): each has its own chat and looks after its own apps.
- You can talk to any agent. Only the lead agent, or you, can hand an app to another agent.
  Full model: [docs/AGENT-MODEL.md](docs/AGENT-MODEL.md).

Principles: runs on an existing Linux host, **Rust** almost everywhere (agents work better with a safe,
strongly typed language), a boring server-rendered web UI with no AI-generated UI, works on a LAN without
internet, and open source so others can build on top.

## Install

1. **Container** (the main way): rootful podman started by systemd, with k3s for apps. Any Linux with
   podman 5+.
2. **NixOS** (experimental): the `services.melissa` module, with k3s. Tested in a NixOS VM, not yet on real
   hardware.

Steps for each: [docs/QUICKSTART.md](docs/QUICKSTART.md). To remove it:
[Uninstall](docs/QUICKSTART.md#uninstall). Bugs and ideas: [issues](https://github.com/svin24/melissa/issues).

## Develop

> Melissa was developed using AI. Due to the nature of AI, a lot of code is moved around daily. If you want to help co-develop it, I expect that you use AI. It is also in extremely early development, with a lot of unfinished features.

With Nix + devenv + direnv (setup, the repo layout and running from source: [docs/DEV-ENV.md](docs/DEV-ENV.md)):

```
direnv allow      # or: devenv shell
just check        # fmt, clippy, tests
just demo         # run from source at http://127.0.0.1:8080 (no app platform, so no apps)
```

`just --list` shows the rest. Start with [docs/PLAN.md](docs/PLAN.md); decisions and their reasons are in
[docs/DECISIONS.md](docs/DECISIONS.md). Agents must read [AGENTS.md](AGENTS.md) first. The earlier bootc OS
images are archived at git tag `bootc-archive-2026-11-19`.

Credits and third-party notices: [ACKNOWLEDGMENTS.md](ACKNOWLEDGMENTS.md). License: [MIT](LICENSE).
