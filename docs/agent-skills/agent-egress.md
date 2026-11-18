# Agent egress (deny-by-default network for sandboxed agents)

**When:** an agent CLI fails to reach something under the sandbox, adding or changing an agent backend, changing
the egress proxy (`crates/agent/src/egress/`), the bridge (`crates/agent/src/bin/melissa-netbridge.rs`) or the
sandbox's network setup (`crates/agent/src/sandbox.rs`).

## Design (verified 2026-11-18 on the dev laptop, Kinoite 45, SELinux enforcing, bwrap 0.13.1)
```
 sandbox (bwrap --unshare-all: own netns, loopback only, no resolver)        queen (melissa-queen)
 ┌──────────────────────────────────────────────────────────────┐
 │ melissa-netbridge (starts first, runs the CLI as its child)  │
 │   127.0.0.1:3128 ──► /run/melissa-net/egress.sock ───────────┼──► CONNECT proxy: allowlist, own DNS,
 │   <queen listen addr> ─► /run/melissa-net/mcp.sock ──────────┼──► public IPs only, audit ──► internet
 │ agent CLI: HTTPS_PROXY=http://127.0.0.1:3128, NO_PROXY=lo    │    MCP only: POST /mcp/<own token>
 └──────────────────────────────────────────────────────────────┘
```
- No root, no iptables/nftables, no pasta/slirp4netns: an unprivileged network namespace (bwrap brings up `lo`
  with 127.0.0.1 and ::1) plus Unix sockets bound in. Works where rootless podman networking does not.
- Per sandbox, the queen creates a lease directory `$XDG_RUNTIME_DIR/melissa/egress/<16 hex>/` (0700; headless:
  `/run/melissa/egress`, `RuntimeDirectory=melissa`) with `egress.sock` and/or `mcp.sock`, bound read-only at
  `/run/melissa-net` (connecting to a socket needs no write access to its mount). The socket identifies the
  sandbox (backend, task); nothing to authenticate or spoof. Dropping the lease (CLI exited, task cancelled,
  ACP session closed or aborted) closes both sockets and every open tunnel and removes the directory.
- The MCP port inside the sandbox is the queen's own `listen` address, so `mcp.json` and ACP `mcpServers` URLs
  are identical inside and outside. `mcp.sock` serves only `POST /mcp/<token>` for the tokens of that sandbox
  (404 for anything else, e.g. `/api/queen/messages` or `/api/confirmations/*/approve`, which a host-network
  agent could reach before).
- The proxy: `CONNECT host:port` only (plain `GET http://...` is refused), no TLS interception. Allowlist match
  on the normalized name (lowercase, no trailing dot) → DNS by the queen → drop non-public addresses (loopback,
  RFC 1918, link-local incl. 169.254.169.254, CGNAT 100.64/10 = tailnet, ULA incl. `fd7a:115c:a1e0::/48`,
  multicast, reserved, documentation, and IPv4 hidden in mapped/NAT64/6to4/compat IPv6) → connect. A name
  that resolves only to such addresses is denied, so DNS rebinding cannot reach the LAN.
- Limits per sandbox/tunnel (`EgressLimits`): 64 open tunnels, 10 s for the request head, 10 s DNS+connect,
  10 min idle, 256 MiB per tunnel, 2 s after one side closes (some servers never answer a half-close).
- Audit (actor `agent`, task, backend; host:port and decision only, never payload):
  `egress.allow` `api.anthropic.com:443 -> 160.79.104.10:443`, `egress.deny` `example.com:443: not in the
  allowlist`, `egress.error` (connect failed). Identical denials within 60 s are counted and written with the
  next record or when the sandbox ends: `grok.com:443: not in the allowlist (+40 repeats)`. Attempts that go
  around the proxy (direct connect, DNS) never leave the namespace and are not audited.

## Config (`queen.toml`, per `[[backend]]`)
```toml
[[backend]]
name = "claude"
command = "claude"
network = "proxy"          # default. "learn" | "none" | "host"
allow = ["api.anthropic.com", "platform.claude.com", "*.example.net:8443"]   # replaces the built-in list
```
- `proxy`: only `allow` (absent: built-in list for the command's file name, `egress::default_allow`) + MCP.
- `learn`: every public host allowed and audited with `(learn: in the allowlist)` / `(learn: NOT in the
  allowlist)`; private addresses still refused. For finding out what a CLI needs; never leave it on.
- `none`: own namespace, no egress; MCP only (no bridge at all without tools).
- `host`: the old behaviour (shares the host's network and resolver); logs a warning at startup.
- Rules: `host` or `*.domain` (subdomains only, not the apex), optional `:port` (default 443). No IP literals,
  no bare `*`. A custom backend (unknown command) has an empty list: deny-all until you add hosts.
- `network` and `allow` sit on the backend, not in `[backend.sandbox]` (that table replaces the preset's
  paths entirely; the network should be changeable without copying them).
- Fails closed: `proxy`/`learn` without the egress proxy or without `melissa-netbridge` → backend unavailable,
  and a run is refused; never a fallback to the host network.

## Adding a host / a new CLI
1. Give the backend `network = "learn"` in a scratch queen (temp `XDG_CONFIG_HOME`/`XDG_STATE_HOME`, a short
   `XDG_RUNTIME_DIR`: Unix socket paths max 107 bytes, the scratchpad path is too long), run one short prompt.
2. Read the audit: `grep '"egress' $XDG_STATE_HOME/melissa/audit.jsonl`. Classify each host: model API, auth,
   catalogue (allow) vs telemetry, analytics, update checks, crash reports (deny).
3. Set `allow` without the doubtful hosts and run again under `proxy`; keep a host only if the CLI breaks or
   gets clearly slower without it. Record the result (dated) in the table below and in
   [agent-cli-backends.md](agent-cli-backends.md); change `egress::default_allow` for a built-in CLI.
4. Check the CLI honours `HTTPS_PROXY`: if it does not, it gets no network at all (fails closed) and the audit
   shows nothing for it. Report that; do not switch it to `host`.

## What each CLI contacts (dev laptop, 2026-11-18, learn mode then proxy mode; per mode of the CLI)
| CLI (version) | Allowed (built-in) | Seen and denied | Breaks when denied? |
|---|---|---|---|
| claude 2.1.327 (`-p`, MCP) | `api.anthropic.com` (~25 tunnels per prompt: a new CONNECT per request), `platform.claude.com` (OAuth token endpoint, from the binary; refresh not observed) | `http-intake.logs.us5.datadoghq.com` (Datadog logs) | No: comb_info + answer in ~6 s |
| grok 1.0.89 (`-p` and `agent stdio`) | `cli-chat-proxy.grok.com` (chat model, subscription login), `api.x.ai` (used by ACP mode; API-key logins), `auth.x.ai` (issuer in `auth.json`; refresh not observed) | `grok.com` (~40 retries per prompt), `api.mixpanel.com` (analytics, ~40 retries) | No: one-shot even works without `api.x.ai`; ACP + Queen + worker fine |
| opencode 2.3.2 (`run`, free default model) | `opencode.ai` (gateway), `models.opencode.ai` (model catalogue) | none | Without `models.opencode.ai`: works, ~3 s slower |
| claude 2.1.327 (`claude-session`, stream-json, with `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`) | `api.anthropic.com` only (3 tunnels for a comb_info task) | none (no Datadog intake any more) | No: learn mode then proxy, Queen + worker in 11.8 s |
| opencode 2.3.2 (`opencode acp`, flavor opencode) | `opencode.ai`, `models.opencode.ai` (same as `run`) | none | No: learn mode then proxy, Queen + worker in 23.6 s |

All three honour `HTTPS_PROXY` (no CLI needed DNS inside the sandbox). Proxy env also set: `HTTP_PROXY`,
`ALL_PROXY` and lowercase forms; `NO_PROXY=localhost,127.0.0.1,::1` keeps MCP off the proxy.

## Tests
- Unit: `egress::{addr,rules,relay,tests}` (address ranges, rule matching, CONNECT parsing, relay limits, proxy
  and MCP socket behaviour, deny collapsing, stale leases), `sandbox::tests` (argv: `--unshare-all` without
  `--share-net`, no resolver bind, bridge before the CLI, proxy env), bridge argument parsing.
- Integration `crates/agent/tests/egress.rs` (skips without bwrap/curl): a sandboxed `sh` reaches an allowlisted
  name (a local server mapped with `with_resolver_override`), is denied another name and `example.com`, cannot
  reach the host's loopback, a LAN address or DNS directly, uses MCP through the bridge, gets 404 for `/api/*`;
  the audit has exactly the three proxy decisions. Proxy mode without the proxy fails closed.
- Live check: a backend `command = "/usr/bin/sh"` with `args = ["-c", "curl -sS https://example.com/; ...",
  "melissa", "{prompt}"]` → `curl: (7) CONNECT tunnel failed, response 403`; with `--noproxy '*'` →
  `Could not resolve host`.

## Gotchas
- A dev-shell build of `melissa-netbridge` has its ELF interpreter in `/nix/store`; the proxy reads `PT_INTERP`
  and binds `/nix/store` read-only only in that case (image builds use `/usr/lib64`). Without it bwrap says
  `execvp .../melissa-netbridge: No such file or directory`.
- `melissa-netbridge` is found next to `melissa-queen` (dev: `target/<profile>/`), then `/usr/bin`, then
  `PATH`. Integration tests use `env!("CARGO_BIN_EXE_melissa-netbridge")`, which is why it is a binary of
  `melissa-agent` rather than its own crate.
- Exit codes: the bridge exits with the CLI's code (128+signal if killed); 125 = bridge setup failed (listen,
  arguments), 126/127 = CLI could not be started.
- Killing the queen (SIGTERM) leaves its lease directories; the next start removes those whose sockets no
  longer accept connections (a second queen sharing the directory keeps its own).
- The pre-commit hook's clippy does not lint test targets; run `just check` (it caught an unused variable).
