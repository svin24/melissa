# Long-lived agent sessions (ACP, and Claude's stream-json)

**When:** building the Queen (head agent) and bee sessions, or wiring a backend that speaks ACP or another
session protocol.
Code: `crates/agent/src/session/` (protocol-neutral API + policy permissions), `crates/agent/src/acp/` (client,
transport, replay; `grok-acp`, `opencode-acp`), `crates/agent/src/claude/` (Claude Code over stream-json,
`claude-session`), queen config `kind = "acp"` / `kind = "claude"`. Background study of T3 Code (MIT, design reference only):
`~/.local/share/melissa/research/t3code-backends-acp.md`.

## Protocol in one paragraph
Newline-delimited JSON-RPC over the agent process's stdin/stdout. Client sends `initialize` (capabilities), then
`authenticate` only if asked, then `session/new` (cwd + `mcpServers`) or `session/load`/`session/resume`; each turn is
`session/prompt`, during which the agent streams `session/update` notifications (text, thoughts, tool calls, plans)
until the prompt returns a stop reason; `session/cancel` stops a turn. The agent may call the client:
`session/request_permission` (we answer), and file/terminal methods (we don't offer them).

## Using it (verified 2026-11-18)
- Board task: dispatch to backend `grok-acp`, `opencode-acp` or `claude-session` (all built in). One task = open session, one prompt, close. Output: reply
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
| OpenCode 2.3.2 | `opencode acp` (flavor `opencode`) | 1 | yes | yes | yes (unstable) | yes | yes (no SSE) | verified here 2026-11-18 |
| Kiro 2.32.0 | `kiro-cli acp` | 2 | yes | no | yes | yes | yes | T3 fixtures (unverified) |
| fx 0.0.13 | `fx acp` | 1 | yes | yes | no | yes | yes | T3 fixtures (unverified) |
- Claude Code has no ACP mode: Melissa drives its stream-json protocol natively (`claude-session`, see below;
  decision in DECISIONS.md, 2026-11-18).

## OpenCode over ACP (verified 2026-11-18, opencode 2.3.2, free default model `opencode/big-pickle`)
- **Code Mode:** by default OpenCode exposes MCP tools only through its own `execute` tool, which runs
  JavaScript (`await tools.melissa.comb_info()`): the permission request is for `execute` (`kind: other`,
  `rawInput.code`), so Melissa could not tell a tool call from arbitrary code. With `codemode: false` on the MCP
  server each tool is its own call named `melissa_comb_info` (`<server>_<tool>`, recognised by the policy).
  `codemode` is a per-server config key; ACP's `session/new` cannot set it, so Melissa puts its MCP server into
  OpenCode's config and sends `mcpServers: []`.
- **Permissions:** by default OpenCode ran `melissa_comb_info` without asking (verified). `permission: {"*":
  "ask"}` makes every tool (MCP and built-in) ask via `session/request_permission`; options `once` /
  `always` / `reject` (allow_once / allow_always / reject_once). Shell: `name: "shell"`, `kind: "execute"`,
  title = the command (`uname -r`) → Melissa's inbox; denied → `reject_once`. **A reject ends OpenCode's turn
  with `stopReason: cancelled`** (board task "failed: turn ended: cancelled").
- **Config injection (flavor `opencode`):** `OPENCODE_CONFIG_CONTENT` = `{"permission": {"*": "ask"}, "mcp":
  {"melissa": {"type": "remote", "url": …, "headers": {}, "codemode": false}}, "model"?}` plus
  `OPENCODE_DISABLE_PROJECT_CONFIG=1`, `OPENCODE_DISABLE_AUTOUPDATE=1`, `OPENCODE_DISABLE_CLAUDE_CODE=1`. The
  sandbox (`SandboxSpec::preset_opencode_managed`) does not bind `~/.config/opencode`, so a user config with
  `allow` rules cannot merge in. (If you give your own `[backend.sandbox]` and bind your config, its permission
  rules merge with Melissa's: avoid.)
- Cancel: `session/cancel` while a permission request waited → OpenCode sent `$/cancel_request` for it, then
  the prompt returned `cancelled`; the next prompt worked. Resume: `session/resume` with the Melissa-owned
  `~/.local/share/opencode` (database `opencode.db`, logs, snapshots) after a daemon restart kept context.
- OpenCode sends `agentInfo` (`OpenCode 2.3.2`), `configOptions` (model, mode), `usage_update` with cost.

## Claude Code over stream-json (`claude-session`, verified 2026-11-18, claude 2.1.327)
Code: `crates/agent/src/claude/` (`mod.rs` spec/backend/args, `client.rs` protocol). One process per session:
`claude -p --input-format stream-json --output-format stream-json --verbose --include-partial-messages
--tools "" --permission-mode manual --permission-prompt-tool stdio --strict-mcp-config --mcp-config <file>
(--session-id <uuid> [--no-session-persistence] | --resume <id>)`.
- Handshake: `{"type":"control_request","request_id":"melissa-init","request":{"subtype":"initialize"}}` →
  `control_response` with `claude_code_version`, commands, models and **the account (e-mail, plan)**: never log
  it; the recorder scrubs e-mail addresses. `system/init` (session id, tools, `permissionMode: "default"` —
  `manual` is the CLI's name for it) comes only with the first user message.
- Turn: `{"type":"user","message":{"role":"user","content":"…"},"parent_tool_use_id":null,"session_id":""}`.
  Output: `stream_event` (Anthropic streaming events: `content_block_delta` text/thinking, `content_block_start`
  tool_use), `assistant` and `user` (whole messages; `user` carries `tool_result`s), `system` (`status`,
  `thinking_tokens`, `commands_changed`), `rate_limit_event`, and one `result` per turn (`subtype`, `is_error`,
  `stop_reason`, `terminal_reason`, usage, `modelUsage.<m>.contextWindow`).
- Permissions: with `--permission-prompt-tool stdio` every prompt is a `control_request` `can_use_tool`
  (`tool_name`, `display_name`, `input`, `tool_use_id`, `permission_suggestions`) → answer `control_response`
  `{"behavior":"allow","updatedInput":<input>}` or `{"behavior":"deny","message":…}` (+ `"interrupt":true` when
  cancelling). Melissa never sends `updatedPermissions` (the "always" equivalent). No `--allowedTools`, so even
  MCP calls ask and the policy sees each one.
- Cancel: `control_request` `interrupt` → `control_response` `{still_queued: []}`; a pending `can_use_tool` is
  withdrawn by Claude with `control_cancel_request` (no answer expected); the turn ends with `result`
  `error_during_execution`, `terminal_reason: aborted_tools` (or similar) → Melissa reports `cancelled`. The next
  user message works in the same process.
- Resume: sessions in `~/.claude/projects/<cwd with / → ->/<id>.jsonl` (plus a `memory/` dir for auto-memory).
  The Queen binds `$XDG_STATE_HOME/melissa/queen/agent-sessions/claude-session/` there; `--resume <id>` restores
  the conversation (verified after a daemon restart and after `kill -9` of the sandbox). Unknown id: Claude
  prints `No conversation found with session ID: …`, a `result` `error_during_execution` and exits 1 before the
  handshake answer → Melissa starts a new session.
- Process stays alive between turns until stdin closes; exit code 0.

## Verified facts (grok 1.0.89, agent-client-protocol 5.3.0, 2026-11-18, dev laptop)
- The crate (`Client.v1()`) negotiates fine with Grok's ACP v1. It sends UUID request ids, and adds
  `clientCapabilities.auth.terminal = false` on its own.
- Grok does not send `agentInfo`; its version is in `initialize` `_meta.agentVersion`.
- MCP: `session/new` with `{"type":"http","name":"melissa","url":"http://127.0.0.1:<port>/mcp/<token>"}` works.
  Grok loads MCP tools lazily: it calls its own `search_tool` (no permission asked), then `use_tool`, which asks
  `session/request_permission` with `kind: "other"`, `title: "melissa__comb_info"`, `rawInput.tool_name:
  "melissa__comb_info"` (no `name` field). Options offered: always-allow / allow-once / reject-once.
- `--permission-mode default` (before `agent`) overrides a user config `permission_mode = "always-approve"`:
  `run_terminal_command` still came as a permission request (`kind: "execute"`, title "Execute `uname -r`").
- Graceful cancel: `session/cancel` → the prompt returned `stopReason: cancelled` in ~40 ms; a second prompt in the
  same session then worked. Board cancel (dropping the session) kills bwrap and grok (no leftovers).
- A comb_info task through the queen took ~11 s; audit shows `agent.permission` then `tool.call`.

## Melissa design (see DECISIONS.md)
- Permission requests → `PolicyPermissions`: Melissa MCP tools → allow_once (the MCP server applies policy, waits
  for confirmations and audits the call itself); agent built-ins by ACP kind → policy labels (read/search/think
  read-only; edit/move reversible; delete/execute/other destructive; fetch untrusted+egress; switch_mode always
  rejected) → `policy::decide` → allow_once, or a confirmation in the inbox (approved → allow_once, denied →
  reject_once, timeout/cancel → `cancelled`). **Never "always" options.** Audit: `agent.permission`, actor agent.
- Claude (`can_use_tool`) uses the same `PolicyPermissions`; its tool names map to kinds by name (`mcp__…` →
  other; `Bash` → execute, `Write`/`Edit` → edit, `WebFetch` → fetch, … in case a config enables built-ins).
- No client fs/terminal capabilities. Sandbox: the `grok` preset (tmpfs `~/.grok`, rw `auth.json`, ro
  `config.toml`) works unchanged for `agent stdio`; `claude` preset unchanged for stream-json; OpenCode ACP uses
  the managed preset (no user config).
- Replay tests: transcripts in `crates/agent/tests/fixtures/acp/*.jsonl` (`grok_comb_info`, `opencode_comb_info`)
  and `crates/agent/tests/fixtures/claude/claude_comb_info.jsonl`, header line + `{"dir":"send"|"recv",
  "msg":…}`. The replay agent matches sends structurally (`"<any>"` wildcard, request ids mapped); the Claude
  client uses the same replay agent (its request ids are fixed strings, Claude's are echoed).

## Recording a transcript
1. Add `record_dir = "/some/dir"` to the ACP or Claude backend in a scratch queen config (temp `XDG_*` dirs), run
   a task.
2. The recorder scrubs home/workdir paths, the MCP token, user and host names, e-mail addresses (Claude's
   handshake answer contains the account's address), and replaces UUIDs with fixed ids.
3. Minimize (Grok sends ~50 `_x.ai/*` notifications and big `available_commands_update`s per turn) and **read every
   line** before committing: prompts, replies and tool outputs can contain anything. Wildcard the MCP URL and the
   client version with `"<any>"`. Claude: reduce the handshake answer and `system/init` to the fields Melissa
   reads (they list the user's plugins, skills, memory paths, a socket path, rate limits and the account), drop
   cost/rate-limit details, replace `msg_…`/`toolu_…`/`req_…` ids. OpenCode: shorten the model lists, replace
   `ses_…`/`msg_…` ids.

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
  `OpenedSession.resumed` says which. The Queen and every bee pass a `state_dir` (their own resumable session store); one-shot board tasks stay throw-away.
- Grok writes `<url-encoded cwd>/<session-id>/` (chat history, events, system prompt), `prompt_history.jsonl`
  and `session_search.sqlite` into the store; resume after a process restart keeps the conversation.
- `session/load` (replays history as updates) is not used.
- OpenCode (`opencode-acp`, `session_store = ".local/share/opencode"`): its whole data dir (SQLite
  `opencode.db`, logs, snapshots) is the store; `session/resume` verified after a daemon restart.
- Claude (`claude-session`, `session_store = ".claude/projects"`): `--resume <id>` at process start (no
  protocol call); verified after a daemon restart and after `kill -9` of the agent's sandbox.

## Gotchas (Claude stream-json, OpenCode ACP)
- Claude's `system/init` arrives only after the first user message; the `initialize` control request is the
  handshake. Its answer carries the account's e-mail: never log it.
- Claude puts its auto-memory under the session store (`<project>/memory/`) and its system prompt mentions it;
  with no file tools it cannot write there ("Noted, but only for this conversation").
- Claude passed `mcp__melissa__comb_info` as a tool name in `start_worker`'s (now `create_bee`'s) `tools` once; the tool's error
  listed the plain names and Claude retried correctly. The description now asks for plain names.
- OpenCode on a policy reject ends the whole turn (`cancelled`), unlike Grok and Claude, which continue.
- OpenCode's `$/cancel_request` notification (after `session/cancel`) is handled by the catch-all handler.

## Open
- Ask a logged-out agent: `authenticate` is not implemented (we rely on the CLI's cached login).
- Claude: `--append-system-prompt` exists (a real system prompt instead of the preamble; PLAN open question 30);
  `CLAUDE_CODE_DISABLE_AUTO_MEMORY` not checked.
- MCP tool-call timeouts: Claude gets `MCP_TOOL_TIMEOUT=960000` (set, not measured); OpenCode's MCP `timeout`
  default is unknown (not set).
