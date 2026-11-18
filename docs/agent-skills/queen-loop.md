# The Queen loop (head agent, workers, event stream, CLI)

**When:** changing the Queen's conversation loop, its worker tools, the `/api/queen` JSON API or the
`melissa-queen chat|ask|confirm|workers` client; building the chat UI on top of the event stream.
Code: `crates/agent/src/queen/` (`mod.rs` session manager, `workers.rs`, `events.rs`, `transcript.rs`,
`tests.rs`), `crates/web/src/api.rs`, `crates/queen/src/client.rs`, prompts in `crates/queen/prompts/`.

## Architecture
- `Queen` (one per daemon) holds at most one agent session (`AgentSession`: ACP or Claude stream-json). Opened
  lazily on the first message from `[queen] backends` in order (default `grok-acp`, `claude-session`,
  `opencode-acp`); a backend that fails to start (missing CLI, not logged in, handshake error) emits
  `backend_failed` and the next is tried. Workers default to the first Queen backend (`worker_backend`). Its MCP session is attended (`attended = true`: the user is in the chat) and may
  delegate (`TaskContext.delegate = true`), so it sees the worker tools.
- One turn at a time: `send` while a turn runs → `QueenError::Busy` (HTTP 409). The event pump (one task
  per session, owns the session's event receiver) ends a turn when the agent's `TurnFinished` arrives, so
  every event of a turn is published before its `turn_finished`.
- Cancel: `session/cancel`; if the turn has not ended after 10 s, Melissa aborts it and kills the session.
- Agent death: mid-turn, `prompt` fails with `AgentError::AgentExited` → `session_lost` + the turn ends with
  `error`. Between turns, the ACP client ends the connection on stdout EOF (`incoming_closed`), the pump sees
  the event stream end → `session_lost` right away. The next message opens a new session: `session/resume` of
  the saved session id (`$XDG_STATE_HOME/melissa/queen/session.json`), else `session/new` primed with the
  last messages from the transcript. The failed message is **not** re-sent automatically (it may have had
  side effects).
- System prompt (`prompts/queen.md`, embedded) goes in front of the first message of every new (not
  resumed) session, inside `<melissa-system-prompt>` tags. Grok does not advertise ACP's
  `sessionCapabilities.systemPrompt`.
- Workers (`workers.rs`): board tasks with `parent` = the Queen's task id (reserved from the board, so audit
  lines share it), dispatched with `DispatchOptions { ctx: unattended (+ the Queen's taint), allow: subset,
  timeout, parent, actor: "queen" }`. Guards: Queen-only tools are invisible to and refused for sessions
  without `delegate` (depth 1); tool list ⊆ the Queen's own minus worker tools; `check_tool_set` for an
  unattended task; `max_workers` running at once (checked under a lock with the dispatch); timeout
  ≤ `worker_timeout_max_secs`. `worker_status` taints the Queen if the worker read untrusted content.
- Scopes (2026-11-18, [ownership-and-scopes.md](ownership-and-scopes.md)): the Queen's session has
  `Scope::ForOwner { agent: "the Queen" }` (all apps; the owner's apps after a confirmation naming them).
  `start_worker {apps?, create_apps? (default true), for_owner? (default false)}` gives the worker
  `Scope::Job` with a fresh random job key; granting an owner's app or `for_owner` asks the owner once
  (confirmation on the Queen's task, tool `start_worker`). The result carries `scope`. Worker tools reach only
  the caller's own workers (`parent` check), so no agent reads another's output except the Queen via
  `worker_status`.
- Labels: `start_worker`, `cancel_worker` Reversible (attended Queen: allowed; tainted Queen: needs a
  confirmation); `worker_status`, `list_workers` read-only.

## Event model (`QueenEvent`, JSON `{"seq", "ts", "type", ...}`)
`user_message`, `text` (reply piece), `queen_message` (whole reply, at turn end), `tool_call` (id, title,
status pending/running/done/failed), `permission`, `confirmation_needed` (id, task, tool, reason),
`confirmation_resolved` (approved/denied/withdrawn), `worker_started`, `worker_finished`, `session_opened`
(backend, resumed), `backend_failed`, `session_lost`, `turn_finished` (stop_reason: end_turn, cancelled,
refusal, max_tokens, max_turn_requests, error), `error`. Agent text is untrusted: render as plain text.
- `GET /api/queen/events?since=<seq>&wait=<s≤30>&limit=<n>` → `{events, next, missed}`. Keep `next` as the
  cursor; `missed` = the client fell behind the 2000 events kept in memory.
- Transcript: `$XDG_STATE_HOME/melissa/queen/transcript.jsonl` (mode 0600), every event except `text`,
  rotated to `.1` at `transcript_max_kib` (default 4096).

## API and CLI
- `POST /api/queen/messages {"text"}` → 202 `{turn, since}` (follow events from `since`), 409 busy, 422
  empty/too long. `POST /api/queen/cancel`, `GET /api/queen/status`, `GET /api/workers`,
  `GET /api/confirmations`, `POST /api/confirmations/{id}/approve|deny` (audited `confirmation.*`, actor ui).
- Same-origin middleware applies; JSON bodies need `application/json`. The CLI sends no `Origin`.
- `melissa-queen ask "<msg>"` (exit 1 if the turn ended in error), `melissa-queen chat` (Ctrl-C cancels the
  running turn, again quits; Ctrl-C/Ctrl-D at the prompt quits; events between turns are shown while
  waiting for input), `melissa-queen confirm [list|approve <id>|deny <id>]`, `melissa-queen workers`.
  They use `listen` from the same `queen.toml` (`--config` works too).

## Config (`[queen]` in `queen.toml`)
```toml
[queen]
backends = ["grok-acp", "claude-session", "opencode-acp"]   # session backends (acp or claude), in order
# tools = ["bee_info", "cluster_health", "start_worker", "worker_status", "list_workers", "cancel_worker"]
# worker_backend = "grok-acp" # default: first Queen backend
max_workers = 2
worker_timeout_secs = 300
worker_timeout_max_secs = 900
transcript_max_kib = 4096
```
`session_store` on a session backend (built-in `grok-acp`: `.grok/sessions`, `claude-session`:
`.claude/projects`, `opencode-acp`: `.local/share/opencode`) = where the agent keeps sessions, relative to
`$HOME`; the Queen binds `$XDG_STATE_HOME/melissa/queen/agent-sessions/<backend>/` (0700) there.

## Verified facts (2026-11-18, dev laptop, grok 1.0.89 via `grok-acp`, temp XDG dirs)
- `melissa-queen ask "Check this bee with bee_info, then start a worker that reports the kernel version using
  bee_info, wait for it, and summarise both in two lines."` → correct two-line answer in 23 s. Grok finds
  Melissa's tools with its own `search_tool`, then calls each through `use_tool`; each call comes as a
  permission request titled `melissa__<tool>` (allowed once; the MCP layer applies policy). The worker
  (grok-acp, own sandbox, tools `bee_info` + `cluster_health`) took 8 s. The Queen called `worker_status`
  with a wait: the call returned 6 s later when the worker finished (so Grok's MCP tool timeout is > 6 s;
  the upper bound is still unknown, hence `wait_secs` ≤ 50).
- Audit for that run: `queen.message` (ui) → `queen.session` opened → `agent.permission` + `tool.call`
  bee_info (task 1 = the Queen) → `tool.call` start_worker → `task.dispatch` task 2 `parent: 1` actor queen →
  the worker's own `agent.permission` + `tool.call` bee_info (task 2) → `task.finish` → `tool.call`
  worker_status.
- `chat` keeps context across messages ("My favourite colour is teal" → "teal"). Ctrl-C during a long
  answer: one more line streamed, then `turn ended: cancelled`; the next message worked in the same session.
- **Resume works.** With `~/.grok/sessions` bound to `$XDG_STATE_HOME/melissa/queen/agent-sessions/grok-acp/`,
  Grok writes `<url-encoded cwd>/<session-id>/…`, `prompt_history.jsonl` and `session_search.sqlite` there.
  After a daemon restart, `session/resume` with the saved id succeeded and Grok still knew "teal". The
  same after `kill -9` of the Queen's sandbox: `session_lost` at once, next message resumed, context intact.
- Grok does not advertise ACP's `systemPrompt` capability, so the system prompt goes in as a preamble. Grok
  also reads `AGENTS.md` from its cwd (its README says so); not used, the Queen's cwd stays empty.

## Verified with Claude and OpenCode (2026-11-18, dev laptop, temp XDG dirs, `network = "proxy"`)
Queen backend forced per run with `[queen] backends = ["claude-session"]` / `["opencode-acp"]`; the same ask as
above ("Check this bee with bee_info, then start a worker …"):
- `claude-session` (claude 2.1.327): two-line answer in 11.8 s, worker on claude-session in 3 s. Every MCP call
  is a `can_use_tool` prompt → `agent.permission allow_once … mcp__melissa__<tool>` then `tool.call`. Egress:
  `api.anthropic.com` only. Context across messages ("teal"), cancel mid-answer (`turn ended: cancelled`, next
  message fine in the same process), resume after a daemon restart and after `kill -9` of its sandbox
  (`queen.session lost … opened claude-session (resumed)`).
- `opencode-acp` (opencode 2.3.2, free model): answer in 23.6 s, worker on opencode-acp in 6 s; permissions
  `allow_once … melissa_<tool>`; egress `opencode.ai`, `models.opencode.ai`. Cancel, next message and resume
  after a daemon restart verified. The Queen prompt keeps it off its own shell ("I can't run uname -r …");
  a board task asking for the shell put `agent:execute` into the inbox, `confirm deny` → `reject_once`.

## Gotchas
- The Queen's and the workers' agents run behind the egress proxy like every sandboxed agent (`network =
  "proxy"`, [agent-egress.md](agent-egress.md)); their `egress.*` audit records carry the Queen's task id or the
  worker's. Verified 2026-11-18: the bee_info + worker run above still takes ~23 s with grok.com and Mixpanel
  denied. A long-lived Queen keeps many tunnels open: the per-sandbox cap is 64.
- Tests: a polling loop must advance its cursor; `wait_since` returns at once when events exist, and a loop
  that never awaits starves the single-threaded test runtime (looked like a deadlock; `eu-stack -p` showed
  the runtime parked in epoll, i.e. idle, which pointed at the test, not the code).
- Replay scripts end by waiting for EOF: close the Queen (`queen.close()`) before awaiting the fake agents.
- The ACP crate reports a dead agent as "Incoming transport closed"; check it with
  `agent_client_protocol::is_incoming_transport_closed` / `is_connection_terminated`, not the message text.
- `pkill -f`/`pgrep -f` patterns match the calling shell's own command line (killed it twice). Find the
  Queen's sandbox with `pgrep -x bwrap -P <daemon pid>`.
- Without the `incoming_closed()` branch in the ACP command loop, a dead agent keeps the connection (and the
  event stream) open until the next request; that is how between-turn deaths went unnoticed at first.

## Not done yet / ideas
- Web UI chat (owner's design), SSE instead of long-poll (same `EventLog`), per-worker token/tool-call budgets,
  showing `thought`/`plan` events, a `confirm` that shows the full argument JSON, Claude's
  `--append-system-prompt` instead of the preamble.
