# Melissa agent model

The owner's model of how Melissa is organised (the role names were bee terms until 2026-11-19; see
docs/DECISIONS.md). It is binding for code, docs and the UI. The diagram
`agent-model.svg` shows a 4-device example; its comments are machine-readable.

## Terms (internal roles)

These are internal role names. They don't have to surface in the UI: the user can name any lead agent, agent or device
whatever they like, and a lead agent names the agents and workspaces it creates. The UI shows those display names.

| Term | Meaning |
|---|---|
| **Fleet** | Optional level above the cluster: several clusters (for example sites) together. |
| **Cluster** | Every device together (one k3s cluster). |
| **Device** | One machine (a Kubernetes node). Each device has exactly one lead agent. |
| **Primary device** | The one device whose lead agent has the final say across the cluster. |
| **Lead agent** | The agent in charge of one device. Only a lead agent can overwrite or reassign workspaces, unless the user asks. |
| **Agent** | A worker agent on a device. Each agent looks after its own workspaces. |
| **Workspace** | One agent's area of responsibility; it holds that agent's services. |
| **Service** | An actual workload running inside a workspace (for example Nextcloud's containers). |
| **User** | The human. Can talk to any lead agent or agent directly and order any reassignment, for example telling one agent to take over another agent's workspaces. |

## Rules

1. **Ownership.** Every workspace is owned by exactly one agent. An agent may act only on its own workspaces (change them,
   read their status, logs or console); `my_apps` tells it which are its own and names the others with their owner
   only. Another agent's (or an unassigned) workspace is never touched on the agent's own: any such call waits for
   the user's own yes, which covers that one call only and which no "Always allow" rule and not No permission
   prompts can give. Every cross-owner attempt (asked, approved, denied, refused) is audited with both sides.
2. **Reassignment.** Only a lead agent, or the user, can overwrite or reassign a workspace. Agents never take another agent's workspaces
   on their own. An agent may take over another agent's workspaces (also across devices) only because the user said so.
3. **Authority.** A lead agent manages the agents on its device. The primary device's lead agent coordinates the other lead agents and decides
   conflicts across the cluster. The user is above every lead agent and can override any of them.
4. **Talking.** The user can chat with any lead agent or agent directly. Agents talk to each other through their lead agent
   (brokered, audited, treated as data, never as instructions); lead agents talk to the primary lead agent.
5. **Isolation.** Each agent works in its own sandbox; each workspace is isolated from other workspaces by default. Links between
   workspaces (one app needing another) are declared, approved and audited.
6. **Policy still applies.** Ownership decides *whether* an agent may act on a workspace at all; the label policy and
   confirmations decide *how* (destructive actions always ask the user, whoever owns the workspace).
7. **Standing permissions are the user's.** The user may answer a kind of request ahead of time: "Always allow"
   saves a rule for one agent (by id) or the lead agent, one tool and one workspace; developer mode's "No
   permission prompts" approves every request except an agent's call on another agent's workspace (rule 1).
   Both only answer confirmations, never turn a refusal into a yes, and only the user creates or removes them
   (no agent tool can). Every auto-approval is audited.
8. **Schedules are the user's too.** A scheduled job is a standing order to act later while nobody watches. The user
   makes and changes schedules directly; an agent may only propose one for itself (a lead agent for any agent on its
   device), and each create or remove needs the user's yes, which no "Always allow" rule can give. A scheduled run
   acts on its own only within the user's standing permissions; everything else waits for the user.

9. **File boxes are private.** The lead agent and each agent have their own file box: what the user sends in that
   agent's chat and what the agent saves to keep or hand over. Only that agent's sandbox sees its box (no other
   agent's, not the lead agent's either); the user sees every box next to its chat and decides how long files are
   kept. Files reach another agent only through the user (send it in that agent's chat).

## Naming

- Every lead agent, agent, device and workspace has a stable internal id (never reused) and a display name.
- The user can rename anything; a lead agent names what it creates (agents, workspaces) and may suggest names for new devices.
- Ids include the device, so the model works unchanged when a cluster grows from one device to many.

## Mapping to today's code (2026-11-18; stop/start, ports and the generic role names 2026-11-19)

| Model | Code today | Still to do |
|---|---|---|
| Device | `crates/device` (`melissa-device`, per-device daemon stub); device id `d-<8 hex>` made once in `$XDG_STATE_HOME/melissa/device.json` (`melissa_agent::device`); `melissa-server` runs per device | the device daemon itself; user-chosen device names |
| Lead agent | the lead agent conversation in `melissa-server` (`melissa_agent::lead`), `Scope::Lead { device }`: any workspace of its device, destructive actions name the owning agent | primary-device lead agent coordination (M3) |
| Agent | persistent, named agent (`lead::registry`, `lead::agents`): id `<device>.a<n>` (never reused), name (the lead agent chooses, the user renames), backend, tools; its own resumable agent session, sandbox, MCP session (`Scope::Agent`), transcript; lead agent tools `create_agent`, `give_agent_job`, `agent_status`, `list_agents`, `stop_agent` | budgets per agent; agent UI (owner's design) |
| Workspace | `my_apps` (an agent: its own apps with phase, image, addresses, assignment; the others by name and owner; the lead agent: all by owner; 2026-11-20); an installed app (`app-<name>` namespace) with `melissa.io/workspace`, `melissa.io/device`, `melissa.io/owner-agent` (+ `assigned-by/at`); every install names its agent; old `melissa.io/owner` labels count as unassigned; the pre-rename keys `melissa.io/cell`, `melissa.io/comb`, `melissa.io/owner-bee` are read as aliases and moved once (2026-11-19) | more workspace kinds (agent-built apps, backups) |
| Service | the app's pods/containers; the owner agent or the lead agent stops/starts them with `stop_app` (scale to 0, always the user's confirmation, nothing deleted) and `start_app` (2026-11-19); reached at `http://<device>:<port>` (the workspace's own NodePort, `melissa.io/port`) | more services per workspace |
| Reassign | lead agent: `reassign_workspace` (always the user's confirmation); user: `melissa-server workspace reassign <workspace> --to <agent>`; admission lets the owner change only with a new `assigned-at` | cross-device takeover (ids and `WorkspaceStore::assign` already take any agent id) |
| Force delete | the user only: the app dialog's Force delete, `/api/apps/{app}/force-delete`, `melissa-server app force-delete <app> --yes`; any owner; removes the workspace and its data; the owning agent gets one brokered message, the lead agent a note with its next message (2026-11-19) | |
| Talking | user ↔ lead agent (`/api/lead`, `chat`), user ↔ any agent (`/api/agents/{agent}/…`, `chat --agent`); agent ↔ agent only via the lead agent (`message_agent`: tagged as data, audited `agent.brokered`) | lead agent ↔ primary lead agent (M3) |
| File box | `melissa_agent::files`: `<state>/files/boxes/<lead or agent id>/`, bound read-write only into that agent's sandbox; index outside every sandbox; `my_files` tool; file panel and `/api/files/{key}` (2026-11-20) | quotas for agent-written files |
| Fleet | none | later |
