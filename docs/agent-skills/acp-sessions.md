# Long-lived agent sessions over ACP (Agent Client Protocol)

**When:** building the Queen (head agent) and worker sessions, or wiring a backend that speaks ACP.
Code: `crates/agent/src/session/` (protocol-neutral API + policy permissions), `crates/agent/src/acp/` (client,
transport, replay), queen config `kind = "acp"`. Background study of T3 Code (MIT, design reference only):
`~/.local/share/melissa/research/t3code-backends-acp.md`.

## Protocol in one paragraph
Newline-delimited JSON-RPC over the agent process's stdin/stdout. Client sends `initialize` (capabilities), then
`authenticate` only if asked, then `session/new` (cwd + `mcpServers`) or `session/load`/`session/resume`; each turn is
`session/prompt`, during which the agent streams `session/update` notifications (text, thoughts, tool calls, plans)
until the prompt returns a stop reason; `session/cancel` stops a turn. The agent may call the client:
`session/request_permission` (we answer), and file/terminal methods (we don't offer them).

## Using it (verified 2026-11-18)
- Board task: dispatch to backend `grok-acp` (built in). One task = open session, one prompt, close. Output: reply
  text on stdout lines; `[tool] <title> → pending|running|done|failed`, `[permission] <title> → allow_once|…`,
  `[plan] a | b` on stderr.
- Queen loop: built, see [queen-loop.md](queen-loop.md). `SessionBackend::open(OpenSession::new(cwd,
  mcp_servers, permissions))` → `AgentSession` + event receiver; `prompt()` per user message (one turn at a
  time, `Busy` otherwise), `cancel()` from another task, `close()`. Hold it as `Arc<dyn AgentSession>`.
- Config: `[[backend]] kind = "acp"`, `name`, `command`, `args`, `tools` (default true), `[backend.sandbox]`,
  `record_dir` (debug transcripts, see below).
- Live test (needs logged-in grok): `cargo nextest run -p melissa-agent --run-ignored only -E 'binary(acp_live)'`.

## Which agents
| Agent | Start command | ACP ver | load | resume | fork | close | HTTP MCP | Source |
|---|---|---|---|---|---|---|---|---|
| Grok 1.0.89 | `grok --permission-mode default agent --no-leader stdio` | 1 | yes | yes | no | yes | yes | verified here 2026-11-18 |
| OpenCode 2.3.2 | `opencode acp` | 1 | yes | yes | yes (unstable) | yes | yes (no SSE) | `initialize` only, 2026-11-18 |
| Kiro 2.32.0 | `kiro-cli acp` | 2 | yes | no | yes | yes | yes | T3 fixtures (unverified) |
| fx 0.0.13 | `fx acp` | 1 | yes | yes | no | yes | yes | T3 fixtures (unverified) |
- Claude Code has no ACP mode: `claude -p --input-format stream-json --output-format stream-json` is its own
  streaming protocol (one process, many turns). Options: an ACP adapter from the public registry (Node; not
  auto-installed) or a native stream-json `SessionBackend`. Undecided (PLAN open question).
- OpenCode: next candidate for `kind = "acp"` (needs a sandbox check and a session through the queen).

## Verified facts (grok 1.0.89, agent-client-protocol 5.3.0, 2026-11-18, dev laptop)
- The crate (`Client.v1()`) negotiates fine with Grok's ACP v1. It sends UUID request ids, and adds
  `clientCapabilities.auth.terminal = false` on its own.
- Grok does not send `agentInfo`; its version is in `initialize` `_meta.agentVersion`.
- MCP: `session/new` with `{"type":"http","name":"melissa","url":"http://127.0.0.1:<port>/mcp/<token>"}` works.
  Grok loads MCP tools lazily: it calls its own `search_tool` (no permission asked), then `use_tool`, which asks
  `session/request_permission` with `kind: "other"`, `title: "melissa__bee_info"`, `rawInput.tool_name:
  "melissa__bee_info"` (no `name` field). Options offered: always-allow / allow-once / reject-once.
- `--permission-mode default` (before `agent`) overrides a user config `permission_mode = "always-approve"`:
  `run_terminal_command` still came as a permission request (`kind: "execute"`, title "Execute `uname -r`").
- Graceful cancel: `session/cancel` → the prompt returned `stopReason: cancelled` in ~40 ms; a second prompt in the
  same session then worked. Board cancel (dropping the session) kills bwrap and grok (no leftovers).
- A bee_info task through the queen took ~11 s; audit shows `agent.permission` then `tool.call`.

## Melissa design (see DECISIONS.md)
- Permission requests → `PolicyPermissions`: Melissa MCP tools → allow_once (the MCP server applies policy, waits
  for confirmations and audits the call itself); agent built-ins by ACP kind → policy labels (read/search/think
  read-only; edit/move reversible; delete/execute/other destructive; fetch untrusted+egress; switch_mode always
  rejected) → `policy::decide` → allow_once, or a confirmation in the inbox (approved → allow_once, denied →
  reject_once, timeout/cancel → `cancelled`). **Never "always" options.** Audit: `agent.permission`, actor agent.
- No client fs/terminal capabilities. Sandbox: the `grok` preset (tmpfs `~/.grok`, rw `auth.json`, ro
  `config.toml`) works unchanged for `agent stdio`.
- Replay tests: transcripts in `crates/agent/tests/fixtures/acp/*.jsonl`, header line + `{"dir":"send"|"recv",
  "msg":…}`. The replay agent matches sends structurally (`"<any>"` wildcard, request ids mapped).

## Recording a transcript
1. Add `record_dir = "/some/dir"` to the ACP backend in a scratch queen config (temp `XDG_*` dirs), run a task.
2. The recorder scrubs home/workdir paths, the MCP token, user and host names, and replaces UUIDs with fixed ids.
3. Minimize (Grok sends ~50 `_x.ai/*` notifications and big `available_commands_update`s per turn) and **read every
   line** before committing: prompts, replies and tool outputs can contain anything. Wildcard the MCP URL and the
   client version with `"<any>"`.

## Gotchas
- **serde_json `preserve_order`:** the ACP crate turns it on, which flips `serde_json::Map` to insertion order for
  the whole workspace. Anything that relied on sorted keys (our audit argument hash did) must sort explicitly.
- Do not answer permission requests inside the crate's `on_receive_request` callback if the answer can wait:
  callbacks block the dispatch loop (no updates, no prompt response). We `cx.spawn` the decision.
- Unhandled notifications that carry a `sessionId` are queued by the crate (up to 1024) waiting for a handler;
  register a catch-all `UntypedMessage` notification handler (we do) or Grok's `_x.ai/*` chatter fills it.
- Grok sometimes sends a stray JSON-RPC *response* with id `"skills-reload"`; the crate logs a WARN
  ("response for unknown id") and continues. Harmless.
- Dropping a session while a permission decision runs makes the crate answer it with an internal error (WARN
  "Responder dropped"); the agent is killed right after anyway.
- Typed `SessionNotification` parsing fails on unknown `sessionUpdate` kinds (vendor extensions); we fall back to
  `SessionEvent::Other(kind)` instead of erroring.
- `fork` is behind the crate's unstable `unstable_session_fork` feature; we report `fork: false` on v1.

- A dead agent does not end the crate's connection while our `connect_with` closure waits for commands; the
  command loop also selects on `cx.incoming_closed()` (stdout EOF) so the event stream ends and the owner
  notices between turns. Pending requests fail with an error for which
  `agent_client_protocol::is_incoming_transport_closed` is true → `AgentError::AgentExited`.

## Resume (verified 2026-11-18, grok 1.0.89)
- `OpenSession { state_dir, resume }`: with `AcpSpec.session_store = ".grok/sessions"` (built-in grok-acp),
  the Melissa-owned `state_dir` (0700) is bound read-write there inside the sandbox (rest of `~/.grok` stays
  tmpfs). `resume` → `session/resume` (if `sessionCapabilities.resume`), falling back to `session/new`;
  `OpenedSession.resumed` says which. Only the Queen passes a `state_dir`; workers stay throw-away.
- Grok writes `<url-encoded cwd>/<session-id>/` (chat history, events, system prompt), `prompt_history.jsonl`
  and `session_search.sqlite` into the store; resume after a process restart keeps the conversation.
- `session/load` (replays history as updates) is not used.

## Open
- Ask a logged-out agent: `authenticate` is not implemented (we rely on the CLI's cached login).
