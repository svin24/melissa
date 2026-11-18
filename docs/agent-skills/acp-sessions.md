# Long-lived agent sessions over ACP (Agent Client Protocol)

**When:** building the Queen (head agent) and worker sessions, or wiring a backend that speaks ACP.
Source: study of T3 Code (MIT, design reference only, `~/dev/reference/t3code` @ 044ba3a, 2026-11-18); full notes in
`~/.local/share/melissa/research/t3code-backends-acp.md`. Unverified in Melissa until we build it.

## Protocol in one paragraph
Newline-delimited JSON-RPC over the agent process's stdin/stdout. Client sends `initialize` (capabilities), then
`authenticate` only if asked, then `session/new` (cwd + `mcpServers`) or `session/load`/`session/resume`; each turn is
`session/prompt`, during which the agent streams `session/update` notifications (text, thoughts, tool calls, plans)
until the prompt returns a stop reason; `session/cancel` stops a turn. The agent may call the client:
`session/request_permission` (we answer), and file/terminal methods (we don't offer them).

## Which agents (from T3 Code's recorded fixtures)
| Agent | Start command | ACP ver | load | resume | fork | HTTP MCP |
|---|---|---|---|---|---|---|
| Grok 1.0.41 | `grok agent stdio` | 1 | yes | yes | no | yes |
| Kiro 2.32.0 | `kiro-cli acp` | 2 | yes | no | yes | yes |
| fx 0.0.13 | `fx acp` | 1 | yes | yes | no | yes |
OpenCode/Claude/others have ACP adapters in the public ACP registry (not auto-installed by Melissa). Claude: compare
CLI `--output-format stream-json` with an ACP adapter before choosing.

## Melissa design (decided 2026-11-18, see DECISIONS.md)
- Rust crate `agent-client-protocol` (5.3.0, Apache-2.0, official org; types in `agent-client-protocol-schema`).
  First check: works against ACP v1 agents (Grok, fx).
- One session trait over both ACP sessions and one-shot CLI runs: open, prompt, cancel, answer permission, close,
  event stream; capability flags (load/resume/fork, HTTP MCP).
- Tools: pass Melissa's MCP endpoint in `session/new` `mcpServers` (HTTP + per-task token header). Fallback: a tiny
  stdio MCP bridge binary for agents without HTTP MCP. Replaces `grok mcp add` (persistent user config).
- Permission requests → Melissa policy/confirmation store: allowed → `allow_once`, denied → `reject_once`, needs a
  human → confirmation item, cancelled task/timeout → `cancelled`. **Never pick "always" options** (they persist
  grants in the agent's own config, outside our audit log).
- Sandbox: `bwrap ... -- grok agent stdio` with piped stdio; keep PID namespace + `--die-with-parent`. Declare no
  fs/terminal client capabilities; pass the agent's permission-mode flag explicitly so user config can't enable
  auto-approve. Resume needs the agent's session store to persist (currently tmpfs in our sandbox preset).
- Tests: record real sessions (both directions, JSONL, scrub ids/home paths/prompts), replay with an in-memory fake
  agent under nextest. Review fixtures for personal data before committing.
- Skip: registry auto-install (network rule), MCP-over-ACP, client fs/terminal methods, vendor JS SDKs.
