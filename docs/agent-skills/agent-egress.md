# Agent egress (deny-by-default network for sandboxed agents)

**When:** an agent CLI fails to reach something under the sandbox, adding or changing an agent backend, changing
the egress proxy (`crates/agent/src/egress/`), the bridge (`crates/agent/src/bin/melissa-netbridge.rs`) or the
sandbox's network setup (`crates/agent/src/sandbox.rs`).

## Design (verified 2026-11-18 on the dev laptop, Kinoite 45, SELinux enforcing, bwrap 0.13.1)
```
 sandbox (bwrap --unshare-all: own netns, loopback only)                    lead agent (melissa-server)
 ┌──────────────────────────────────────────────────────────────┐
 │ melissa-netbridge (starts first, runs the CLI as its child)  │
 │   127.0.0.1:3128 ──► /run/melissa-net/egress.sock ───────────┼──► proxy: allowlist + web read, own DNS,
 │   <lead listen addr> ─► /run/melissa-net/mcp.sock ──────────┼──► public IPs only, audit ──► internet
 │   127.0.0.1:53/udp: socket handed over dns.sock ─────────────┼──► lead binds it, answers 192.88.99.53
 │ agent CLI: HTTPS_PROXY=http://127.0.0.1:3128, NO_PROXY=lo,   │    MCP only: POST /mcp/<own token>
 │   GROK_WEB_FETCH_PROXY=same; /etc/resolv.conf → 127.0.0.1    │
 └──────────────────────────────────────────────────────────────┘
```
- No root, no iptables/nftables, no pasta/slirp4netns: an unprivileged network namespace (bwrap brings up `lo`
  with 127.0.0.1 and ::1) plus Unix sockets bound in. Works where rootless podman networking does not.
- Per sandbox, the lead agent creates a lease directory `$XDG_RUNTIME_DIR/melissa/egress/<16 hex>/` (0700; headless:
  `/run/melissa/egress`, `RuntimeDirectory=melissa`) with `egress.sock` and/or `mcp.sock`, bound read-only at
  `/run/melissa-net` (connecting to a socket needs no write access to its mount). The socket identifies the
  sandbox (backend, task); nothing to authenticate or spoof. Dropping the lease (CLI exited, task cancelled,
  ACP session closed or aborted) closes both sockets and every open tunnel and removes the directory.
- The MCP port inside the sandbox is the lead agent's own `listen` address, so `mcp.json` and ACP `mcpServers` URLs
  are identical inside and outside. `mcp.sock` serves only `POST /mcp/<token>` for the tokens of that sandbox
  (404 for anything else, e.g. `/api/lead/messages` or `/api/confirmations/*/approve`, which a host-network
  agent could reach before).
- The proxy, no TLS interception. Allowlist match on the normalized name (lowercase, no trailing dot) → DNS by
  the lead agent → drop non-public addresses (loopback, RFC 1918 incl. k3s pods/services 10.42/16 and 10.43/16,
  link-local incl. 169.254.169.254, CGNAT 100.64/10 = tailnet, ULA incl. `fd7a:115c:a1e0::/48`, multicast,
  reserved, documentation, 6to4 anycast 192.88.99/24 = the DNS placeholder, and IPv4 hidden in
  mapped/NAT64/6to4/compat IPv6) → connect to a checked address (never a second lookup). A name that resolves
  only to such addresses is denied, so DNS rebinding cannot reach the LAN.
- **Web reading** (`[egress] web_read = true` in `server.toml`, the default; owner decision 2026-11-20): a host
  that is not on the backend's allowlist is still reachable as a *web read*: `CONNECT` on port 443, or a plain
  forward request (`GET http://host/path`, port 80; only `GET`/`HEAD`, no body; rewritten to origin form with
  `Connection: close`; nothing after the head is relayed). Same address rules, plus local names (`localhost`,
  single labels, `.local`, `.lan`, `.internal`, `.home.arpa`, `.ts.net`) refused before any lookup. Caps per
  web tunnel (`egress::WebRead`): 64 KiB up (read-only, approximately: TLS hides methods), 32 MiB down, 120 s,
  30 s idle. Opening one taints the sandbox's MCP sessions (source `web`, the host): the task's later changes
  need the owner's OK. Each CLI's known background hosts (`egress::default_deny`: Mixpanel, grok.com, Datadog
  intake, npm registry) stay denied even then. `web_read = false`: only the allowlists, as before.
- **DNS inside:** a sinkhole on 127.0.0.1:53 answers every A query with 192.88.99.53 and everything else with no
  records; nothing is forwarded (no DNS channel out). For CLIs that resolve before using the proxy (Grok's
  `web_fetch`). The sandbox cannot bind port 53 (bwrap's `--dev` = nested user namespace); the bridge creates
  an unbound UDP socket and hands it over `dns.sock` (SCM_RIGHTS); the lead agent, owner of the sandbox's
  network namespace, binds and serves it. The lease's `resolv.conf` is bound over `/etc/resolv.conf` (or its
  link target under `/run`, Fedora: `/run/systemd/resolve/stub-resolv.conf`).
- Limits per sandbox/tunnel (`EgressLimits`): 64 open tunnels, 10 s for the request head, 10 s DNS+connect,
  10 min idle, 256 MiB per tunnel, 2 s after one side closes (some servers never answer a half-close).
- Audit (actor `agent`, task, backend; host:port and decision only, never payload):
  `egress.allow` `api.anthropic.com:443 -> 160.79.104.10:443` (web reads: `... (web read)`), `egress.web` at the
  close of a web read `CONNECT example.com:443: 1950 B up, 6568 B down, 27.4 s[; closed: upload limit ...]`
  (plain HTTP: `GET http://neverssl.com/: ...`), `egress.deny` `10.0.0.1.nip.io:443: resolves only to
  non-public addresses (10.0.0.1); ...`, `egress.error` (connect failed). A refusal's 403 body reads
  `blocked by Melissa's network policy: <host>:<port>: <reason>`. Identical denials within 60 s are counted and written with the
  next record or when the sandbox ends: `grok.com:443: not in the allowlist (+40 repeats)`. Attempts that go
  around the proxy (direct connect, DNS) never leave the namespace and are not audited.

## Config (`server.toml`, per `[[backend]]`)
```toml
[[backend]]
name = "claude"
command = "claude"
network = "proxy"          # default. "learn" | "none" | "host"
allow = ["api.anthropic.com", "platform.claude.com", "*.example.net:8443"]   # replaces the built-in list
```
- `proxy`: `allow` (absent: built-in list for the command's file name, `egress::default_allow`) + MCP, plus
  web reading unless `[egress] web_read = false` (server-wide, not per backend).
- `learn`: every public host allowed (any port, no web caps, no taint) and audited with `(learn: in the
  allowlist)` / `(learn: NOT in the allowlist)`; private addresses still refused. For finding out what a CLI
  needs; never leave it on.
- `none`: own namespace, no egress; MCP only (no bridge at all without tools).
- `host`: the old behaviour (shares the host's network and resolver); logs a warning at startup.
- Rules: `host` or `*.domain` (subdomains only, not the apex), optional `:port` (default 443). No IP literals,
  no bare `*`. A custom backend (unknown command) has an empty list: deny-all until you add hosts.
- `network` and `allow` sit on the backend, not in `[backend.sandbox]` (that table replaces the preset's
  paths entirely; the network should be changeable without copying them).
- Fails closed: `proxy`/`learn` without the egress proxy or without `melissa-netbridge` → backend unavailable,
  and a run is refused; never a fallback to the host network.

## Adding a host / a new CLI
1. Give the backend `network = "learn"` in a scratch lead agent (temp `XDG_CONFIG_HOME`/`XDG_STATE_HOME`, a short
   `XDG_RUNTIME_DIR`: Unix socket paths max 107 bytes, the scratchpad path is too long), run one short prompt.
2. Read the audit: `grep '"egress' $XDG_STATE_HOME/melissa/audit.jsonl`. Classify each host: model API, auth,
   catalogue (allow) vs telemetry, analytics, update checks, crash reports (deny).
3. Set `allow` without the doubtful hosts and run again under `proxy`; keep a host only if the CLI breaks or
   gets clearly slower without it. Record the result (dated) in the table below and in
   [agent-cli-backends.md](agent-cli-backends.md); change `egress::default_allow` for a built-in CLI.
4. Check the CLI honours `HTTPS_PROXY`: if it does not, it gets no network at all (fails closed) and the audit
   shows nothing for it. Report that; do not switch it to `host`. Check its fetch tool separately (below):
   a CLI can use the proxy for its API and not for fetching. Hosts it contacts on its own that are not needed
   go into `egress::default_deny`, or web reading lets them through (and taints every task).

## What each CLI contacts (dev laptop, 2026-11-18, learn mode then proxy mode; per mode of the CLI)
| CLI (version) | Allowed (built-in) | Seen and denied | Breaks when denied? |
|---|---|---|---|
| claude 2.1.327 (`-p`, MCP) | `api.anthropic.com` (~25 tunnels per prompt: a new CONNECT per request), `platform.claude.com` (OAuth token endpoint, from the binary; refresh not observed) | `http-intake.logs.us5.datadoghq.com` (Datadog logs) | No: device_info + answer in ~6 s |
| grok 1.0.89 (`-p` and `agent stdio`) | `cli-chat-proxy.grok.com` (chat model, subscription login), `api.x.ai` (used by ACP mode; API-key logins), `auth.x.ai` (issuer in `auth.json`; refresh not observed) | `grok.com` (~40 retries per prompt), `api.mixpanel.com` (analytics, ~40 retries) | No: one-shot even works without `api.x.ai`; ACP + Lead agent + worker fine |
| opencode 2.3.2 (`run`, free default model) | `opencode.ai` (gateway), `models.opencode.ai` (model catalogue) | none | Without `models.opencode.ai`: works, ~3 s slower |
| claude 2.1.327 (`claude-session`, stream-json, with `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`) | `api.anthropic.com` only (3 tunnels for a device_info task) | none (no Datadog intake any more) | No: learn mode then proxy, lead agent + worker in 11.8 s |
| opencode 2.3.2 (`opencode acp`, flavor opencode) | `opencode.ai`, `models.opencode.ai` (same as `run`) | none | No: learn mode then proxy, lead agent + worker in 23.6 s |
| opencode 1.19.6 (`opencode acp`, official installer's release on 2026-11-18, in the headless VM's system service) | `opencode.ai`, `models.opencode.ai` | `registry.npmjs.org` | No: lead agent + agent_info in 13 s |

| gemini 0.70.0 (`--acp`, scratch container from the image, fake API key, 2026-11-20) | `generativelanguage.googleapis.com` (API key); `cloudcode-pa.googleapis.com`, `oauth2.googleapis.com` (Google sign-in, from the source; not observed) | `play.googleapis.com` (Clearcut usage statistics, 4 per session); `registry.npmjs.org` (update check, from the source) | API reached (400 "API key not valid"); a real turn not verified |
| codex-acp 2.8.0 + codex 0.170.0 (`authenticate api-key`, mode `read-only`, same run) | `api.openai.com` (Responses API; WebSocket first, then HTTPS), `chatgpt.com` (seen with an API key too: model list), `auth.openai.com` (ChatGPT token refresh, from the source) | `api.github.com` (update check), `files.openai.com` (seen once in learn mode, not needed), `ab.chatgpt.com` (Statsig, from the source) | API reached (401); a real turn not verified |
| muse 1.4.24 (`acp`, `-c telemetry.enabled=false -c approval_judge="off"`, same run) | `api.meta.ai` (model API and catalogue; its telemetry uses the same host, turned off by the flag), `auth.meta.com` (sign-in, from `muse login`) | `lookaside.facebook.com` (downloads, from the registry entry) | API reached (key rejected at the catalogue fetch); a real turn not verified |

All of them use `HTTPS_PROXY` for their own API traffic; the Node-based ones (gemini, codex-acp's adapter) need
`NODE_USE_ENV_PROXY=1` for Node's own `fetch`/`http`, which every proxied sandbox now gets (Gemini also installs an
undici proxy agent itself). Codex itself is Rust and reads the proxy variables. Proxy env also set: `HTTP_PROXY`, `ALL_PROXY` and
lowercase forms; `NO_PROXY=localhost,127.0.0.1,::1` keeps MCP off the proxy; `GROK_WEB_FETCH_PROXY` (below).

## Fetch tools (2026-11-20, web reading)
| CLI | Fetch tool | Routing | Live result |
|---|---|---|---|
| gemini 0.70.0 | `web_fetch` | Checks the target with its own `dns.lookup` (SSRF), then fetches through the proxy (undici `EnvHttpProxyAgent`); the DNS placeholder should pass its private-address check | Not verified (needs a real key) |
| grok 1.0.89 (`agent stdio`, container) | `web_fetch` (permission kind fetch) | Ignores `HTTPS_PROXY`; uses `GROK_WEB_FETCH_PROXY` (or `[toolset.web_fetch] proxy_endpoint`), but first resolves the name itself (SSRF check; static musl binary: `/etc/resolv.conf` + UDP 53) and refuses documentation/private answers ("SSRF blocked: ... 203.0.113.53") | Works with the DNS sinkhole: https://example.com, https://github.com/rust-lang (redirects), http://example.com |
| opencode 2.3.2 (`acp`, laptop) | `webfetch` | Bun honours `HTTPS_PROXY`/`HTTP_PROXY` | https://example.com via `CONNECT`, http://neverssl.com as a forwarded `GET` |
| claude (`claude-session`) | none (`--tools ""`) | n/a | `WebFetch` would use the proxy; `WebSearch` runs server-side (PLAN open question 73) |

Before the change, Grok failed with `DNS resolution failed for example.com: failed to lookup address
information: Try again` although Melissa approved the fetch. Fetch permissions (`session/permissions.rs`) now
refuse what cannot succeed, with the reason in the audit: `reject_once (fetch): blocked by Melissa's network
policy: 10.0.0.1 is a private or local address; web reading reaches public hosts only`.

## Tests
- Unit: `egress::{addr,rules,relay,web,dns,tests}` (address ranges, rule matching, CONNECT parsing, relay limits
  incl. upload/download caps and lifetime, forward-request rewriting and refusals, DNS sinkhole replies and the
  socket hand-over, proxy and MCP socket behaviour, web reads: public host allowed, audited, tainting, upload
  cap; SSRF answers v4/v6/mapped/NAT64/metadata/tailnet/k3s/placeholder and literals refused; local names and
  other ports refused; background hosts denied; web reading off), `session::permissions::tests` (fetch
  refusals with reasons, taint on fetch, reads allowed after taint, changes not), `sandbox::tests` (argv:
  `--unshare-all` without `--share-net`, proxy env incl. `GROK_WEB_FETCH_PROXY`, `resolv.conf` bind incl.
  link targets, `--dns`, bridge before the CLI), bridge argument parsing.
- Integration `crates/agent/tests/egress.rs` (skips without bwrap/curl): a sandboxed `sh` reaches an allowlisted
  name (a local server mapped with `with_resolver_override`), is denied another name and `example.com`, cannot
  reach the host's loopback, a LAN address or the placeholder directly, resolves every name to the placeholder
  (the hand-over worked), uses MCP through the bridge, gets 404 for `/api/*`;
  the audit has exactly the three proxy decisions. Proxy mode without the proxy fails closed.
- Live check: a backend `command = "/usr/bin/sh"` with `args = ["-c", "curl -sS https://example.com/; ...",
  "melissa", "{prompt}"]` → `curl: (7) CONNECT tunnel failed, response 403`; with `--noproxy '*'` →
  `Could not resolve host`.

## Gotchas
- A dev-shell build of `melissa-netbridge` has its ELF interpreter in `/nix/store`; the proxy reads `PT_INTERP`
  and binds `/nix/store` read-only only in that case (image builds use `/usr/lib64`). Without it bwrap says
  `execvp .../melissa-netbridge: No such file or directory`.
- `melissa-netbridge` is found next to `melissa-server` (dev: `target/<profile>/`), then `/usr/bin`, then
  `PATH`. Integration tests use `env!("CARGO_BIN_EXE_melissa-netbridge")`, which is why it is a binary of
  `melissa-agent` rather than its own crate.
- Exit codes: the bridge exits with the CLI's code (128+signal if killed); 125 = bridge setup failed (listen,
  arguments), 126/127 = CLI could not be started.
- Killing the lead agent (SIGTERM) leaves its lease directories; the next start removes those whose sockets no
  longer accept connections (a second lead agent sharing the directory keeps its own).
- The pre-commit hook's clippy does not lint test targets; run `just check` (it caught an unused variable).
- Port 53 and bwrap: with `--dev` bwrap creates a second, nested user namespace (devpts needs it), so even
  `--cap-add CAP_NET_BIND_SERVICE` cannot bind port 53 in the sandbox's network namespace (`Permission denied`);
  without `--dev` it can. Hence the hand-over to the lead agent. Dropping `--dev` is no option: Grok uses ptys.
- `getent ahosts` / `AI_ADDRCONFIG` lookups find nothing inside (only loopback addresses are configured); use
  `getent hosts`. Rust's std, musl and Python's `getaddrinfo` do not set it.
- Grok ends the whole turn when a fetch permission is refused, and its `web_fetch` shows a proxy refusal only
  as `error sending request` (the 403 body is not surfaced): read the reason in the audit.
- Live testing without touching the running Melissa: a scratch server in the container as `melissa`
  (`systemd-run --unit=... -p User=melissa` with its own `XDG_*_HOME`, a short `XDG_RUNTIME_DIR`, another
  `listen` port, `melissa-server --config <scratch.toml> ask "..."`). Dev-shell binaries run there after
  `patchelf --set-interpreter /lib64/ld-linux-x86-64.so.2 --remove-rpath` (same glibc).
