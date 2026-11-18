# The Queen loop (head agent, bees, event stream, CLI)

**When:** changing the Queen's or a bee's conversation loop, the bee tools, the bee registry, the
`/api/queen` / `/api/bees` JSON API or the `melissa-queen chat|ask|confirm|bee|cell` client; building the
chat UI on top of the event stream. Model: [../HIVE-MODEL.md](../HIVE-MODEL.md); ownership rules:
[cells-and-ownership.md](cells-and-ownership.md).
Code: `crates/agent/src/queen/` (`conversation.rs` the session machinery shared by the Queen and every
bee, `mod.rs` the Queen, `bees.rs` bee pool + Queen-only tools, `registry.rs` persistent bees,
`events.rs`, `transcript.rs`, `tests.rs`), `crates/agent/src/comb.rs`, `crates/web/src/api.rs`,
`crates/queen/src/client.rs`, prompts in `crates/queen/prompts/` (`queen.md`, `bee.md`).

## Architecture
- `Conversation` (`conversation.rs`) is everything below, for the Queen and for each bee (own
  `ConversationSpec`: who, audit prefix `queen.*`/`bee.*`, backends, system prompt, state dir, workdir,
  task id, MCP policy context, tool allowlist). `Queen` = one conversation + the bee pool.
- `Queen` (one per daemon) holds at most one agent session (`AgentSession`: ACP or Claude stream-json). Opened
  lazily on the first message from `[queen] backends` in order (default `grok-acp`, `claude-session`,
  `opencode-acp`); a backend that fails to start (missing CLI, not logged in, handshake error) emits
  `backend_failed` and the next is tried. New bees default to the first Queen backend (`bee_backend`). Her
  MCP session is attended (`attended = true`: the user is in the chat), may delegate
  (`TaskContext.delegate = true`, so she sees the bee tools) and has `Scope::Queen { comb }`.
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
- **Bees** (`bees.rs`, `registry.rs`, since 2026-11-18): persistent, named agents. The registry
  (`$XDG_STATE_HOME/melissa/bees/registry.json`, 0600, atomic) keeps id (`<comb>.b<n>`, never reused), name
  (unique per comb, the Queen chooses, the user renames), backend, tools, created-by/at, stopped. Each bee
  has `bees/<id>/` with its `session.json`, `agent-sessions/<backend>/` (bound as the agent's session
  store, so its session resumes like the Queen's) and `transcript.jsonl`; it works in `work/bees/<id>`.
  Its conversation is made on first use (task id reserved from the board per daemon run) with
  `Scope::Bee { id }`, `attended = false`, `delegate = false`, allowlist = its tools (⊆ the Queen's minus
  Queen-only). Cells it owns are **not** in the registry: they are on the cells (label), see
  [cells-and-ownership.md](cells-and-ownership.md).
- Bee tools (Queen-only, hidden from bees; each also refuses any scope but `Queen`): `create_bee {name,
  backend?, tools?}`; `give_bee_job {bee, goal, timeout_secs?}` sends "A job from the Queen:\n\n<goal>"
  into the bee's conversation (409-like error if it is busy; at most `max_bees` bees working at once,
  checked under a lock; after `timeout_secs` the turn is cancelled; a watcher publishes `bee_finished`);
  `bee_status {bee, wait_secs≤50}` (status idle/working/stopped, job, activity lines since the job, last
  reply as `result`, `read_untrusted_content`); `list_bees` (with cells per bee and unassigned cells);
  `stop_bee` (cancel + close session, `stopped` until the next job or message); `message_bee {bee, text,
  from_bee?}` (bee-to-bee only via the Queen: text wrapped in `<melissa-data from="…">`, a closing tag
  inside is escaped, audited `bee.brokered`); `reassign_cell {cell, to}` (always the user's confirmation).
  Labels: create/job/stop/message Reversible (attended Queen: allowed; tainted Queen: confirmation),
  status/list read-only, reassign `Effect::Ownership`.
- Taint: a job or message from a tainted Queen (or from a tainted `from_bee`) taints the bee
  (`McpServer::taint_session` + its `session.json`); `bee_status` of a tainted bee taints the Queen.
- The user talks to any bee directly (`/api/bees/{bee}/…`, `chat --bee`): same conversation, same
  one-turn-at-a-time rule (a Queen job and a user message never overlap). The bee's MCP session stays
  unattended even then (one MCP session per agent session).

## Event model (`QueenEvent`, JSON `{"seq", "ts", "type", ...}`)
Same shape for the Queen and every bee. `user_message`, `text` (reply piece), `agent_message` (whole reply,
at turn end; was `queen_message`, still read from old transcripts), `tool_call` (id, title, status
pending/running/done/failed), `permission`, `confirmation_needed` (id, task, tool, reason),
`confirmation_resolved` (approved/denied/withdrawn), `session_opened` (backend, resumed), `backend_failed`,
`session_lost`, `turn_finished` (stop_reason: end_turn, cancelled, refusal, max_tokens, max_turn_requests,
error), `error`. Only in the Queen's log: `bee_created` (bee, name, backend), `bee_started` (bee, name,
goal), `bee_finished` (bee, name, status succeeded/failed/cancelled, error), `cell_reassigned` (cell, from,
to, by). Agent text is untrusted: render as plain text.
- `GET /api/queen/events?since=<seq>&wait=<s≤30>&limit=<n>` → `{events, next, missed}`. Keep `next` as the
  cursor; `missed` = the client fell behind the 2000 events kept in memory.
- Transcript: `$XDG_STATE_HOME/melissa/queen/transcript.jsonl` and `bees/<id>/transcript.jsonl` (mode
  0600), every event except `text`, rotated to `.1` at `transcript_max_kib` (default 4096).

## API and CLI
- `POST /api/queen/messages {"text"}` → 202 `{turn, since}` (follow events from `since`), 409 busy, 422
  empty/too long. `POST /api/queen/cancel`, `GET /api/queen/status`,
  `GET /api/confirmations`, `POST /api/confirmations/{id}/approve|deny` (audited `confirmation.*`, actor ui).
- Bees (`{bee}` = name, any case, or id; 404 unknown): `GET /api/bees`, `GET /api/bees/{bee}`,
  `POST /api/bees/{bee}/messages|cancel`, `GET /api/bees/{bee}/events|status` (same shapes as the Queen's),
  `POST /api/bees/{bee}/rename {"name"}` (422 bad/taken). Cells: `GET /api/cells` (502 without k3s),
  `POST /api/cells/{cell}/reassign {"to"}` (the user's own reassignment, no confirmation).
- Same-origin middleware applies; JSON bodies need `application/json`. The CLI sends no `Origin`.
- `melissa-queen ask "<msg>"` (exit 1 if the turn ended in error), `melissa-queen chat` (Ctrl-C cancels the
  running turn, again quits; Ctrl-C/Ctrl-D at the prompt quits; events between turns are shown while
  waiting for input), `--bee <bee>` on both talks to a bee instead, `melissa-queen confirm [list|approve
  <id>|deny <id>]`, `melissa-queen bee list` (also `bees`; `workers` is a hidden alias for one release),
  `bee rename <bee> <new name>`, `cell list`, `cell reassign <cell> --to <bee>`.
  They use `listen` from the same `queen.toml` (`--config` works too).

## Config (`[queen]` in `queen.toml`)
```toml
[queen]
backends = ["grok-acp", "claude-session", "opencode-acp"]   # session backends (acp or claude), in order
# tools = ["bee_info", "cluster_health", "create_bee", "give_bee_job", "bee_status", "list_bees", "stop_bee"]
# bee_backend = "grok-acp"   # session backend for new bees; default: first Queen backend
max_bees = 2                 # bees working on a job at once
bee_timeout_secs = 300       # job time limit when the Queen gives none
bee_timeout_max_secs = 900
transcript_max_kib = 4096    # Queen's and each bee's transcript
```
The old keys `worker_backend`, `max_workers`, `worker_timeout_secs`, `worker_timeout_max_secs` are still
read (aliases, one release).
`session_store` on a session backend (built-in `grok-acp`: `.grok/sessions`, `claude-session`:
`.claude/projects`, `opencode-acp`: `.local/share/opencode`) = where the agent keeps sessions, relative to
`$HOME`; the Queen binds `$XDG_STATE_HOME/melissa/queen/agent-sessions/<backend>/` (0700) there.

## Verified with persistent bees (2026-11-18, dev laptop, grok-acp, temp XDG dirs, no k3s)
- `ask "Create a bee named Ada with only the bee_info tool and give her a small job: check this comb with
  bee_info and report the kernel version. Wait for her result, then summarise in one line."` → 22.5 s:
  Grok found the tools via `search_tool`, `create_bee` → `[bee Ada] created (c-….b1, grok-acp)`,
  `give_bee_job`, `bee_status` (waited) → `[bee Ada] succeeded` → correct one-line answer.
- `ask --bee ada "…what was your last job and what did you find?"` → 3.7 s, same session, correct.
  `bee rename Ada "Ada L"` → listed with the same id. Daemon restart, `ask --bee "ada l" …` →
  `[session] grok-acp: resumed the previous session`, correct answer in 3.2 s.
- Audit: `queen.message` (ui) → `queen.session` → `tool.call create_bee` (task 1) → `bee.message` actor
  queen on the bee's task 2 ("A job from the Queen: …") → `bee.session opened grok-acp (new)` (task 2) →
  `tool.call bee_info` (task 2) → `tool.call bee_status` (task 1) → `bee.message` actor ui →
  `bee.rename` → after the restart `bee.session opened grok-acp (resumed)`. `cell list` without k3s:
  502 "k3s is not running on this comb". `registry.json` mode 600.

## Verified facts (2026-11-18, dev laptop, grok 1.0.89 via `grok-acp`, temp XDG dirs; worker era)
Tool names in this and the next sections are the pre-bee ones (`start_worker` = today's create +
`give_bee_job`, `worker_status` = `bee_status`).
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

## End-to-end app install through the chat (2026-11-18)
Full walkthrough, timings and transcript summary: [e2e-queen-app-install.md](e2e-queen-app-install.md).
Install → ready (cold 513 MB pull) 6 m 18 s with a grok worker polling `app_status`; health + logs
14.5 s; uninstall keeping data 13–24 s incl. the approval. Findings that changed the Queen:
- **Taint is sticky for the conversation** (`Inner.tainted`, `session.json` `tainted`): noted at every
  turn end and when a session is lost/closed, applied to every new MCP session of the Queen. Before,
  a reopened session (resume or transcript-primed) started untainted although the agent still had
  the untrusted content. No reset yet (open question).
- **Confirmation waits are capped** by `confirm_timeout_secs` (default 240): Claude Code abandons an
  MCP call at 300 s without closing or cancelling it, which left approvable "ghost" confirmations.
  The MCP server also handles `notifications/cancelled` (audited `tool.cancelled`).
- Grok's Queen started a worker to wait for readiness and still polled `app_status` itself between
  `worker_status` waits; Claude's Queen read `app_logs` on its own while waiting (tainting itself).

## Gotchas
- **Port clashes with other worktrees:** another session's test daemon may already listen on your
  chosen port; `melissa-queen ask` then talks to *that* Queen. Check `ss -ltnp | grep <port>` before
  starting yours, and stop only your own daemon by pid (never `pkill -x melissa-queen`).
- A Queen tool call that waits for a confirmation blocks the turn (up to `confirm_timeout_secs`). If
  nobody watches the inbox the agent gets a withdrawal error and should say so (verified with claude).
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
- Web UI chat (owner's design), SSE instead of long-poll (same `EventLog`), per-bee token/tool-call budgets,
  showing `thought`/`plan` events, a `confirm` that shows the full argument JSON, Claude's
  `--append-system-prompt` instead of the preamble.
