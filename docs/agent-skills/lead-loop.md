# The lead agent loop (agents, event stream, CLI)

**When:** changing the lead agent's or an agent's conversation loop, the agent tools, the agent registry, the
`/api/lead` / `/api/agents` JSON API or the `melissa-server chat|ask|confirm|agent|workspace` client; building the
chat UI on top of the event stream. Model: [../AGENT-MODEL.md](../AGENT-MODEL.md); ownership rules:
[workspaces-and-ownership.md](workspaces-and-ownership.md).
Code: `crates/agent/src/lead/` (`conversation.rs` the session machinery shared by the lead agent and every
agent, `mod.rs` the lead agent, `agents.rs` agent pool + Lead-only tools, `registry.rs` persistent agents,
`events.rs`, `transcript.rs`, `tests.rs`), `crates/agent/src/device.rs`, `crates/web/src/api.rs`,
`crates/server/src/client.rs`, prompts in `crates/server/prompts/` (`lead.md`, `agent.md`).

## Architecture
- `Conversation` (`conversation.rs`) is everything below, for the lead agent and for each agent (own
  `ConversationSpec`: who, audit prefix `lead.*`/`agent.*`, backends, system prompt, state dir, workdir,
  task id, MCP policy context, tool allowlist, the file boxes and whose box it is). Sent files are stored in
  the conversation's box (`FileBoxes::store_uploads`), the session binds the box (`OpenSession::files_dir`), and
  the box is synced when a turn ends (files the agent saved show in the panel). A scheduled turn also sets
  `TaskContext::confirm_wait` (Settings → Schedules) on its MCP session for the length of the turn. `Lead` = one conversation + the agent pool.
- `Lead` (one per daemon) holds at most one agent session (`AgentSession`: ACP or Claude stream-json). Opened
  lazily on the first message from `[lead] backends` in order (default `grok-acp`, `claude-session`,
  `opencode-acp`); a backend that fails to start (missing CLI, not logged in, handshake error) emits
  `backend_failed` and the next is tried. New agents default to the first lead agent backend (`agent_backend`), or, once
  the user switched the lead agent, to the lead agent's chosen backend (q55c; `AgentPool::follow_lead`, also after a
  restart; existing agents keep theirs). Its MCP session is attended (`attended = true`: the user is in the chat), may delegate
  (`TaskContext.delegate = true`, so it sees the agent tools) and has `Scope::Lead { device }`.
- One turn at a time, with a queue (2026-11-21): `Conversation::submit` while a turn runs queues the message
  (`message_queued`; it starts when the turn ends, also after a cancel) or, with `SendMode::Steer`, puts it
  into the running turn (`AgentSession::steer`, Claude Code) or stops the turn and runs it next (ACP).
  `send` (jobs, scheduled runs) still refuses with `LeadError::Busy`. The event pump (one task
  per session, owns the session's event receiver) ends a turn when the agent's `TurnFinished` arrives, so
  every event of a turn is published before its `turn_finished`.
- Cancel: `session/cancel`; if the turn has not ended after 10 s, Melissa aborts it and kills the session.
- Agent death: mid-turn, `prompt` fails with `AgentError::AgentExited` → `session_lost` + the turn ends with
  `error`. Between turns, the ACP client ends the connection on stdout EOF (`incoming_closed`), the pump sees
  the event stream end → `session_lost` right away. The next message opens a new session: `session/resume` of
  the saved session id (`$XDG_STATE_HOME/melissa/lead/session.json`), else `session/new` primed with the
  last messages from the transcript. The failed message is **not** re-sent automatically (it may have had
  side effects).
- System prompt (`prompts/lead.md`, embedded) goes in front of the first message of every new (not
  resumed) session, inside `<melissa-system-prompt>` tags. Grok does not advertise ACP's
  `sessionCapabilities.systemPrompt`.
- **Agents** (`agents.rs`, `registry.rs`, since 2026-11-18): persistent, named agents. The registry
  (`$XDG_STATE_HOME/melissa/agents/registry.json`, 0600, atomic) keeps id (`<device>.a<n>`, never reused), name
  (unique per device, the lead agent chooses, the user renames), backend, tools, created-by/at, stopped. Each agent
  has `agents/<id>/` with its `session.json`, `agent-sessions/<backend>/` (bound as the agent's session
  store, so its session resumes like the lead agent's) and `transcript.jsonl`; it works in `work/agents/<id>`.
  Its conversation is made on first use (task id reserved from the board per daemon run) with
  `Scope::Agent { id }`, `attended = false`, `delegate = false`, allowlist = its tools (⊆ the lead agent's minus
  Lead-only). Workspaces it owns are **not** in the registry: they are on the workspaces (label), see
  [workspaces-and-ownership.md](workspaces-and-ownership.md).
- Agent tools (Lead-only, hidden from agents; each also refuses any scope but `Lead`): `create_agent {name,
  backend?, tools?, job?, user_request?, research?, timeout_secs?}` (with `job` the new agent starts on it in
  the same call, as `give_agent_job`; the brief is checked before the agent is created; if the job cannot start,
  e.g. too many agents working, the agent stays and the result has `job_error`); `give_agent_job {agent, goal,
  user_request?, research?, timeout_secs?}` sends "A job from the lead agent:\n\n<goal>" (then "The user's
  request, in their words:\n…" and "What the lead agent already found out:\n…" when given) into the agent's
  conversation (error if it is busy; at most `max_agents` agents working at once,
  checked under a lock; after `timeout_secs` the turn is cancelled; a watcher publishes `agent_finished` and
  queues the helper's report for the lead agent as a `helper_report` turn; it returns at once and the lead
  ends her turn); `agent_status {agent}` (no waiting since 2026-11-21; status idle/working/stopped, job, activity lines since the job, last
  reply as `result`, `read_untrusted_content`); `list_agents` (with workspaces per agent and unassigned workspaces);
  `stop_agent` (cancel + close session, `stopped` until the next job or message); `message_agent {agent, text,
  from_agent?}` (agent-to-agent only via the lead agent: text wrapped in `<melissa-data from="…">`, a closing tag
  inside is escaped, audited `agent.brokered`); `reassign_workspace {workspace, to}` (always the user's confirmation);
  `delete_agent {agent}` (2026-11-20; Destructive: always the user's confirmation, naming the agent; then
  `AgentPool::delete`, the same as Settings → Agents and `DELETE /api/agents/{agent}`: refused while it looks after
  apps, else its schedules, file box, transcript/session store and workdir go; event `agent_deleted`).
  Labels: create/job/stop/message Reversible (attended lead agent: allowed; tainted lead agent: confirmation),
  status/list read-only, reassign `Effect::Ownership`.
- **History after a restart:** `EventLog::with_history` starts the lead agent's and each agent's log with the
  last 100 turns (`RESTORED_TURNS`; 30 before one chat per agent) of the chat from the transcript (after the last `new_chat`; memory only, renumbered
  seqs and turn ids, between `earlier_start`/`earlier_end`); new turn ids continue after them. Each log has an
  `epoch()` so clients notice a cursor from before a restart.
- **One chat per agent, Fresh start** (2026-11-20; was "New chat"): `Lead::fresh_start` / `AgentPool::fresh_start`
  (`POST /api/lead/fresh-start`, `/api/agents/{agent}/fresh-start`, `/ui/fresh-start`, the agent menu, CLI
  `fresh-start [--agent]`, old names `new-chat`/`chat --new`): withdraws Melissa's own pending confirmations (audited
  `confirmation.withdrawn`; agents' stay), stops a running turn, closes the session and deletes `session.json`,
  clears the taint, pushes `fresh_start`. Nothing is archived; `Transcript::recap` (the priming of a new session)
  stops at the last `fresh_start`/`new_chat`. Archives from before (`lead/chats/<id>.jsonl`) stay readable:
  `Lead::past_chats/past_chat`, `GET /api/lead/chats`, `/chats` and `/chats/<id>` in the UI. Fresh start is the
  owner-approved way to clear taint.
- **Silent compaction** (`CompactPolicy` in `AgentLimits`, `[lead] compact_at_percent` 70, `compact_idle_secs`
  120; `Conversation::{plan_compaction, compact}`): after a turn with the context past the threshold (the provider's
  last `Usage`), once idle that long (at once after an overflow), `Transcript::condense` makes a summary of the
  older messages since the last fresh start (keeping the last 6 whole), `context_compacted {summary}` is pushed
  (transcript only; the feed ignores it), `session.json` keeps the backend but no session id, and `fresh` makes the
  next turn close the live session and open a new one; its preamble carries `<earlier-summary>` then the recent
  messages. Skipped for `SessionBackend::compacts_itself` (Claude). Audited `<kind>.compact`; `Lead::compact` /
  `POST /api/lead/compact` on request. Tests: `lead/tests/compaction.rs` (paused clock).
- Taint events: the first time a conversation reads untrusted content it pushes `tainted {tool, subject}`
  (the MCP server remembers the first tainting tool and its `app`/`workspace`/`agent` argument; `tool: "lead agent"` for
  a job or message from a tainted lead agent), right after the tool call.
- Taint: a job or message from a tainted lead agent (or from a tainted `from_agent`) taints the agent
  (`McpServer::taint_session` + its `session.json`); `agent_status` of a tainted agent taints the lead agent.
- The user talks to any agent directly (`/api/agents/{agent}/…`, `chat --agent`): same conversation, same
  one-turn-at-a-time rule (a lead agent job and a user message never overlap). The agent's MCP session stays
  unattended even then (one MCP session per agent session).

## The lead hands app work to helpers (2026-11-20)
Owner feedback: asked to "set up a Minecraft server", the lead deployed it itself. Now:
- The lead agent creates **no new app**: `deploy_app` targets `Access::Create`, and `scope::check` refuses
  `Scope::Lead` + `Create` on a workspace that does not exist (`LEAD_HANDS_OFF`, audited `tool.refused`, before
  any confirmation; `Deployer::new_owner` refuses the lead too, as a second guard). Every app has a helper owner
  from the start; a helper's deploy makes it the owner (`melissa.io/owner-agent` = its id).
- The lead keeps its oversight and overrides on existing apps (status, logs, stop/start, exec, update,
  uninstall, reassign, and bringing back an app uninstalled with its data kept), with the same confirmations.
- "Make / install / set up / build X": at most brief read-only research (which image, ports, settings), then
  hand it off at once: `give_agent_job` to a helper that already looks after related apps, else `create_agent`
  with `job` (one call). The brief carries the goal, the user's words (`user_request`) and the research. Then
  the lead tells the user who is on it and ends her turn; the helper's report brings her back to relay the
  outcome (2026-11-21).

## Switching backend and model (2026-11-19)
Code: `lead/backends.rs` (choice, model cache, views), `Conversation::{backends, check_choice, switch}`,
`Lead::{backends, use_backend, backend_choice}`, `AgentPool::{backends_of, user_use_backend}`.
- The user picks the backend (and model) of the lead agent or of one agent. Choices: the lead agent's `[lead] backends`; a
  agent: every session backend. Refused (`LeadError::Unavailable`, HTTP 422) when `SessionBackend::not_ready()`
  says not installed, not logged in (claude: `~/.claude/.credentials.json`, grok: `~/.grok/auth.json`, only checked
  when the preset binds that file and no API-key env var is passed) or the sandbox/netbridge is missing. Unknown
  backend or model → `Invalid` (422).
- **Saved in state, not config:** Lead agent → `$XDG_STATE_HOME/melissa/lead/backend.json` (0600; ignored with a warning
  if the backend left `[lead] backends`); agent → `backend`/`model` in `agents/registry.json`. Audited
  `lead.backend` / `agent.backend` (actor ui, `"from → to; applies at the next turn|after the running turn"`).
  Event `backend_switched {backend, model?, model_label?}` (feed: "Switched to Claude (Sonnet 5.5), picking up
  where we left off").
- **Never mid-turn:** `send()` snapshots the choice; the turn runs on it. At the start of the next turn
  `live_session` sees `live.wanted != choice`, closes the old session (like shutdown: `session.json` kept) and
  opens the new one. Order: choice first, then the rest of `[lead] backends` (failover); agents only their own.
- **Context carries over like after a lost session:** same backend, other model → the saved session id is
  resumed (ACP `session/resume` then `session/set_config_option`; Claude `--resume <id> --model <m>`); another
  backend → no matching `session.json`, so a new session primed with the transcript recap (the preamble). Switching
  back later does **not** resume that backend's old session (it would miss the turns in between).
- **Models are only what the agent reports** (`OpenedSession.models`): ACP `configOptions` with `category:
  "model"` (select; grouped options flattened) in `session/new`/`session/resume`; Claude's `initialize` answer
  `models[].value/displayName`. Kept per backend with an `updated` time in `lead/models.json` (0600, shared by
  Lead agent and agents; the older bare-list form still reads) whenever a session opens; until then a backend shows no
  models and a model can't be chosen. The agent's whole list is offered, uncurated (q55b). Claude's `--model`
  from the user replaces the config's `--model`; ids are checked with `session::valid_model_id`.
- **Reasoning levels (2026-11-20)** are only what the agent reports, per model (`ModelOption.efforts`): Claude's
  `models[].supportedEffortLevels` (worded Low/Medium/High/Extra high/Max; started with `--effort <id>`, replacing
  the config's), ACP's `thought_level` select (set with `session/set_config_option` after the model; reported for
  the session's model only, so the cache keeps levels seen earlier for the others). The choice (`effort`) is
  checked against the chosen model's levels, saved with backend and model (`lead/backend.json`, the agent
  registry), shown in `backend_switched.effort_label`, and applies from the next turn (the session reopens).
- **Fast mode (2026-11-20)**, the same way (`ModelOption.fast`, choice `fast`): Claude's `models[].supportsFastMode`
  (set with `--settings {"fastMode":true}`; the handshake then says `fast_mode_state: "on"`), an ACP option of
  category `speed` (unstable in ACP, so a custom category: a boolean, or a two-value select with a `fast` value;
  set with `session/set_config_option`). Never inferred from a model's name: a fast model (`grok-4.7-build-fast`,
  Haiku) reports none. Refused for a model that doesn't offer it; the web picker keeps it on a model change only
  when the new model offers it. Other ACP options (OpenCode's `mode` build/plan) are ignored on purpose.
- **Model probes** (q55a, `backends::probe`, `ModelCache::refresh`): open a session through the backend's own
  `open` (same sandbox/egress as real sessions; no MCP servers, every permission refused, empty cwd
  `<workdir>/../probe/<backend>`), read `OpenedSession.models`, close it; **no prompt is sent**. 15 s timeout.
  Only usable backends (`info().available`, `not_ready()` none). Automatic (chat page render, `GET
  /api/lead/backends`): only when the list is missing or older than 24 h, at most once per backend per 10 min.
  Forced: `POST /api/lead/backends/refresh` / `melissa-server backend refresh` (still one probe per backend at a
  time). A list found by a probe shows in the menu from the next render.
- Verified: grok 1.0.89's binary contains `configOptions`/`set_config_option` (strings; not yet run live); claude
  2.1.327's `initialize` answer lists `default, opus, fable, sonnet, haiku, …` with display names (live, no prompt
  sent); OpenCode sends `configOptions` (fixture `opencode_device_info.jsonl`).
- `kind = "echo"` session backends (`[[backend]] kind = "echo"`, `name = "echo-a"`; not `echo`): no LLM, repeat the
  message and say how many earlier messages Melissa handed over. For demos and trying switching.
- API: `GET /api/lead/backends`, `POST /api/lead/backend {name, model?}`, `GET /api/agents/{agent}/backends`, `POST
  /api/agents/{agent}/backend`, `POST /api/lead/backends/refresh`. CLI: `melissa-server backend [list] [--agent <agent>]`,
  `backend use <name> [--model <id>] [--agent <agent>]`, `backend refresh [--agent <agent>]`.
- Verified 2026-11-19 (dev laptop, temp XDG dirs, port 18093, echo-a/echo-b + claude-session with a seeded
  model list + a missing opencode): message on echo-a, switch in the UI → feed line, next message "Connected to
  echo-b" and the echo said it got 2 earlier messages; switch to Claude · Sonnet 5.5 shown, not sent; CLI list/use
  and 422 refusals; audit lines as above.

## Scheduled jobs (2026-11-20)
`crates/agent/src/lead/schedules/`: `Schedules` (store, `$XDG_STATE_HOME/melissa/schedules.json`, 0600; owner changes
audited `schedule.*` actor `ui`), `Scheduler` (one Tokio task, started by `Lead::start_schedules`; reach it with
`Lead::scheduler()`), `ScheduleJobTool` (`schedule_job`, registered in the server next to the pool's tools).
- A schedule = name, agent (`Principal`: lead or agent id, rename-safe), instruction, 5-field cron (croner, jiff
  time zone = the box's `TimeZone::system()`), enabled, `max_minutes` (default 60), created_by, `handled_until`,
  `last_run`. `next_run` is computed (`Schedules::next_run`), not stored.
- Firing = `Conversation::send_scheduled(name, text)`: the event is a normal `user_message` with `scheduled`, the
  prompt gets a short "Scheduled job … nobody is watching" note, and the MCP session is `attended = false` for the
  turn (`McpServer::set_attended`; restored in `finish_turn`). Agents: `AgentPool::scheduled_send` (wakes a stopped
  agent, by id). Busy conversation → the run is skipped, never queued.
- Rules: skip on overlap or busy agent; a due time found up to `DEFAULT_GRACE` (1 h) late runs once, older is
  recorded `missed` (only the latest due time counts); a change or switching on counts from now; a run past
  `max_minutes` is cancelled (`timed_out`); `running` at startup becomes `interrupted`.
- `schedule_job` create/remove always ask the owner via `Tool::owner_approval` (new hook in `ToolRegistry::call`):
  the confirmation carries no rule offer, so "Always allow" can't cover it; the developer bypass still does, an
  agent's Full access never does (owner-only, like `Tool::owner_only` tools and `Effect::Ownership`).
  Scope: an agent only for itself (else refused "not yours" before anyone is asked), the lead agent any agent.
- Tests use `FakeClock` (follows Tokio's paused clock) + a `FakeRunner`, never real sleeps
  (`schedules/tests.rs`); real conversations in `lead/tests/schedules.rs`.
- UI: Settings → Schedules (`settings/schedules.rs`), API `/api/schedules` (documented in `api.rs`); feed label via
  `components::scheduled_message`.

## Event model (`LeadEvent`, JSON `{"seq", "ts", "type", ...}`)
Same shape for the lead agent and every agent. `user_message` (`scheduled`: the schedule's name when Melissa
sent it for a scheduled job), `text` (reply piece), `agent_message` (whole reply,
at turn end; was `lead_message`, still read from old transcripts), `tool_call` (id, title, status
pending/running/done/failed, `ms` on the finishing event), `thought` (turn, text, ms: once when a thought starts,
empty with no `ms`, and once whole with its time when the agent moves on or the turn ends; kept in the
transcript, cut at 20,000 chars; shown only with Settings → Developer → "Show thinking and timings"), `permission`, `confirmation_needed` (id, task, tool, reason),
`confirmation_resolved` (approved/denied/withdrawn), `session_opened` (backend, resumed), `backend_failed`,
`session_lost`, `backend_switched` (backend, model, model_label), `turn_finished` (stop_reason: end_turn, cancelled, refusal, max_tokens, max_turn_requests,
error; tool calls and a thought still open then are shown stopped, as agents send no end for them), `error`, `tainted` (tool, subject), `fresh_start`, `context_compacted` (summary; not shown), `new_chat` (old transcripts only), and in memory only `earlier_start`/`earlier_end`. Only in the lead agent's log: `agent_created` (agent, name, backend), `agent_started` (agent, name,
goal), `agent_finished` (agent, name, status succeeded/failed/cancelled, error), `workspace_reassigned` (workspace, from,
to, by). Agent text is untrusted: the web UI renders it with the safe `components::markdown`.
- `GET /api/lead/events?since=<seq>&wait=<s≤30>&limit=<n>` → `{events, next, missed}`. Keep `next` as the
  cursor; `missed` = the client fell behind the 2000 events kept in memory.
- Transcript: `$XDG_STATE_HOME/melissa/lead/transcript.jsonl` and `agents/<id>/transcript.jsonl` (mode
  0600), every event except `text`, rotated to `.1` at `transcript_max_kib` (default 4096).

## API and CLI
- `POST /api/lead/messages {"text", "steer"?}` → 202 `{status: started|queued|steered|next, turn?, id?,
  since}` (follow events from `since`; a queued message starts its turn later), 422 empty/too long.
  `GET /api/lead/queue`, `POST /api/lead/queue/{id}/steer|remove` (same for agents). `POST /api/lead/cancel`, `GET /api/lead/status`,
  `GET /api/confirmations`, `POST /api/confirmations/{id}/approve|deny` (audited `confirmation.*`, actor ui).
- Agents (`{agent}` = name, any case, or id; 404 unknown): `GET /api/agents`, `GET /api/agents/{agent}`,
  `POST /api/agents/{agent}/messages|cancel`, `GET /api/agents/{agent}/events|status` (same shapes as the lead agent's),
  `POST /api/agents/{agent}/rename {"name"}` (422 bad/taken). Workspaces: `GET /api/workspaces` (502 without k3s),
  `POST /api/workspaces/{workspace}/reassign {"to"}` (the user's own reassignment, no confirmation).
- Same-origin middleware applies; JSON bodies need `application/json`. The CLI sends no `Origin`.
- `melissa-server ask "<msg>"` (exit 1 if the turn ended in error), `melissa-server chat` (Ctrl-C cancels the
  running turn, again quits; Ctrl-C/Ctrl-D at the prompt quits; events between turns are shown while
  waiting for input), `--agent <agent>` on both talks to an agent instead, `melissa-server confirm [list|approve
  <id>|deny <id>]`, `melissa-server agent list` (also `agents`; `workers` is a hidden alias for one release),
  `agent rename <agent> <new name>`, `workspace list`, `workspace reassign <workspace> --to <agent>`, `new-chat` (`chat --new`
  starts one, then talks).
- Agent tool results lead with the agent's name (`"agent": "Ada"`, id in `agent_id`; `reassign_workspace`: `to` name,
  `to_id`): agents echo what they read, and the prompts tell them to use names, "this device" and "apps",
  never ids or internal terms. Tool arguments still take a name or an id.
  They use `listen` from the same `server.toml` (`--config` works too).
- `melissa-server demo` (`just demo`): serve with a startup summary on stdout (URL, agents and why each is
  unusable, the lead agent's backend, bwrap namespace probe, k3s) and only warnings in the log. Code:
  `crates/server/src/preflight.rs`. `just build` also builds `melissa-netbridge` (`cargo run -p
  melissa-server` alone does not).

## Config (`[lead]` in `server.toml`)
```toml
[lead]
backends = ["grok-acp", "claude-session", "opencode-acp"]   # session backends (acp or claude), in order
# tools = ["device_info", "cluster_health", "create_agent", "give_agent_job", "agent_status", "list_agents", "stop_agent"]
# agent_backend = "grok-acp"   # session backend for new agents; default: first lead agent backend
max_agents = 2                 # agents working on a job at once
agent_timeout_secs = 300       # job time limit when the lead agent gives none
agent_timeout_max_secs = 900
transcript_max_kib = 4096    # Lead agent's and each agent's transcript
```
The old keys `worker_backend`, `max_workers`, `worker_timeout_secs`, `worker_timeout_max_secs` are still
read (aliases, one release).
`session_store` on a session backend (built-in `grok-acp`: `.grok/sessions`, `claude-session`:
`.claude/projects`, `opencode-acp`: `.local/share/opencode`) = where the agent keeps sessions, relative to
`$HOME`; the lead agent binds `$XDG_STATE_HOME/melissa/lead/agent-sessions/<backend>/` (0700) there.

## Verified: history, new chat, names (2026-11-18, dev laptop, grok-acp, temp XDG dirs, no k3s)
- "Describe this device … how much disk space is free" → `device_info` with `disks`, answer in plain words.
  "Create a helper named Ada …" → "Ada reports the kernel version is …" (no ids in the reply).
- Daemon restarted (SIGINT): the page showed both turns again between "Earlier" and "Melissa restarted";
  `last_seq` 26 before any new event.
- New chat (`POST /ui/new-chat`): 303 to an empty chat, `lead/chats/<id>.jsonl` (0600), transcript holds only
  `new_chat`, `session.json` gone, audit `lead.new_chat`. Next message: `[session] grok-acp: new session`,
  "This is the first message from you in this conversation.", `session.json` `tainted:false`.

## Verified with persistent agents (2026-11-18, dev laptop, grok-acp, temp XDG dirs, no k3s)
- `ask "Create an agent named Ada with only the device_info tool and give her a small job: check this device with
  device_info and report the kernel version. Wait for her result, then summarise in one line."` → 22.5 s:
  Grok found the tools via `search_tool`, `create_agent` → `[agent Ada] created (d-….a1, grok-acp)`,
  `give_agent_job`, `agent_status` (waited) → `[agent Ada] succeeded` → correct one-line answer.
- `ask --agent ada "…what was your last job and what did you find?"` → 3.7 s, same session, correct.
  `agent rename Ada "Ada L"` → listed with the same id. Daemon restart, `ask --agent "ada l" …` →
  `[session] grok-acp: resumed the previous session`, correct answer in 3.2 s.
- Audit: `lead.message` (ui) → `lead.session` → `tool.call create_agent` (task 1) → `agent.message` actor
  lead agent on the agent's task 2 ("A job from the lead agent: …") → `agent.session opened grok-acp (new)` (task 2) →
  `tool.call device_info` (task 2) → `tool.call agent_status` (task 1) → `agent.message` actor ui →
  `agent.rename` → after the restart `agent.session opened grok-acp (resumed)`. `workspace list` without k3s:
  502 "k3s is not running on this device". `registry.json` mode 600.

## Verified facts (2026-11-18, dev laptop, grok 1.0.89 via `grok-acp`, temp XDG dirs; worker era)
Tool names in this and the next sections are the pre-agent ones (`start_worker` = today's create +
`give_agent_job`, `worker_status` = `agent_status`).
- `melissa-server ask "Check this agent with device_info, then start a worker that reports the kernel version using
  device_info, wait for it, and summarise both in two lines."` → correct two-line answer in 23 s. Grok finds
  Melissa's tools with its own `search_tool`, then calls each through `use_tool`; each call comes as a
  permission request titled `melissa__<tool>` (allowed once; the MCP layer applies policy). The worker
  (grok-acp, own sandbox, tools `device_info` + `cluster_health`) took 8 s. The lead agent called `worker_status`
  with a wait: the call returned 6 s later when the worker finished (so Grok's MCP tool timeout is > 6 s;
  the upper bound is still unknown, hence `wait_secs` ≤ 50).
- Audit for that run: `lead.message` (ui) → `lead.session` opened → `agent.permission` + `tool.call`
  device_info (task 1 = the lead agent) → `tool.call` start_worker → `task.dispatch` task 2 `parent: 1` actor lead agent →
  the worker's own `agent.permission` + `tool.call` device_info (task 2) → `task.finish` → `tool.call`
  worker_status.
- `chat` keeps context across messages ("My favourite colour is teal" → "teal"). Ctrl-C during a long
  answer: one more line streamed, then `turn ended: cancelled`; the next message worked in the same session.
- **Resume works.** With `~/.grok/sessions` bound to `$XDG_STATE_HOME/melissa/lead/agent-sessions/grok-acp/`,
  Grok writes `<url-encoded cwd>/<session-id>/…`, `prompt_history.jsonl` and `session_search.sqlite` there.
  After a daemon restart, `session/resume` with the saved id succeeded and Grok still knew "teal". The
  same after `kill -9` of the lead agent's sandbox: `session_lost` at once, next message resumed, context intact.
- Grok does not advertise ACP's `systemPrompt` capability, so the system prompt goes in as a preamble. Grok
  also reads `AGENTS.md` from its cwd (its README says so); not used, the lead agent's cwd stays empty.

## Verified with Claude and OpenCode (2026-11-18, dev laptop, temp XDG dirs, `network = "proxy"`)
Lead agent backend forced per run with `[lead] backends = ["claude-session"]` / `["opencode-acp"]`; the same ask as
above ("Check this agent with device_info, then start a worker …"):
- `claude-session` (claude 2.1.327): two-line answer in 11.8 s, worker on claude-session in 3 s. Every MCP call
  is a `can_use_tool` prompt → `agent.permission allow_once … mcp__melissa__<tool>` then `tool.call`. Egress:
  `api.anthropic.com` only. Context across messages ("teal"), cancel mid-answer (`turn ended: cancelled`, next
  message fine in the same process), resume after a daemon restart and after `kill -9` of its sandbox
  (`lead.session lost … opened claude-session (resumed)`).
- `opencode-acp` (opencode 2.3.2, free model): answer in 23.6 s, worker on opencode-acp in 6 s; permissions
  `allow_once … melissa_<tool>`; egress `opencode.ai`, `models.opencode.ai`. Cancel, next message and resume
  after a daemon restart verified. The lead agent prompt keeps it off its own shell ("I can't run uname -r …");
  a board task asking for the shell put `agent:execute` into the inbox, `confirm deny` → `reject_once`.

## End-to-end app install through the chat (2026-11-18)
Full walkthrough, timings and transcript summary: [e2e-lead-app-install.md](e2e-lead-app-install.md).
Install → ready (cold 513 MB pull) 6 m 18 s with a grok worker polling `app_status`; health + logs
14.5 s; uninstall keeping data 13–24 s incl. the approval. Findings that changed the lead agent:
- **Taint is sticky for the conversation** (`Inner.tainted`, `session.json` `tainted`): noted at every
  turn end and when a session is lost/closed, applied to every new MCP session of the lead agent. Before,
  a reopened session (resume or transcript-primed) started untainted although the agent still had
  the untrusted content. No reset yet (open question).
- **Confirmation waits are capped** by `confirm_timeout_secs` (default 240): Claude Code abandons an
  MCP call at 300 s without closing or cancelling it, which left approvable "ghost" confirmations.
  The MCP server also handles `notifications/cancelled` (audited `tool.cancelled`). Since 2026-11-20 every
  turn end (`Conversation::finish_turn`) withdraws the conversation's still-pending confirmations
  (`confirmation.withdrawn`, "the run ended"), and scheduled runs wait at most the backend's
  `tool_call_limit` less a minute (Claude: 4 min).
- Grok's lead agent started a worker to wait for readiness and still polled `app_status` itself between
  `worker_status` waits; Claude's lead agent read `app_logs` on its own while waiting (tainting itself).

## Gotchas
- **Port clashes with other worktrees:** another session's test daemon may already listen on your
  chosen port; `melissa-server ask` then talks to *that* Lead agent. Check `ss -ltnp | grep <port>` before
  starting yours, and stop only your own daemon by pid (never `pkill -x melissa-server`).
- A Lead agent tool call that waits for a confirmation blocks the turn (up to `confirm_timeout_secs`). If
  nobody watches the inbox the agent gets a withdrawal error and should say so (verified with claude).
- The lead agent's and the workers' agents run behind the egress proxy like every sandboxed agent (`network =
  "proxy"`, [agent-egress.md](agent-egress.md)); their `egress.*` audit records carry the lead agent's task id or the
  worker's. Verified 2026-11-18: the device_info + worker run above still takes ~23 s with grok.com and Mixpanel
  denied. A long-lived lead agent keeps many tunnels open: the per-sandbox cap is 64.
- Tests: a polling loop must advance its cursor; `wait_since` returns at once when events exist, and a loop
  that never awaits starves the single-threaded test runtime (looked like a deadlock; `eu-stack -p` showed
  the runtime parked in epoll, i.e. idle, which pointed at the test, not the code).
- Replay scripts end by waiting for EOF: close the lead agent (`lead.close()`) before awaiting the fake agents.
- The ACP crate reports a dead agent as "Incoming transport closed"; check it with
  `agent_client_protocol::is_incoming_transport_closed` / `is_connection_terminated`, not the message text.
- `pkill -f`/`pgrep -f` patterns match the calling shell's own command line (killed it twice). Find the
  Lead agent's sandbox with `pgrep -x bwrap -P <daemon pid>`.
- Without the `incoming_closed()` branch in the ACP command loop, a dead agent keeps the connection (and the
  event stream) open until the next request; that is how between-turn deaths went unnoticed at first.

## Not done yet / ideas
- Web UI chat: done 2026-11-18 (SSE from the same `EventLog`, see [htmx-component.md](htmx-component.md)). Per-agent token/tool-call budgets,
  showing `thought`/`plan` events, a `confirm` that shows the full argument JSON, Claude's
  `--append-system-prompt` instead of the preamble.
