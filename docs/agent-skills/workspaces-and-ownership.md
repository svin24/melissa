# Workspaces and ownership ("an agent acts only on its own workspaces")

**When:** adding or changing any tool that changes or reads into a workspace (apps today; later
update/restart/scale/stop, backups, agent-built apps), touching the agent tools, `reassign_workspace`, the
tool registry's scope check, `TaskContext.scope`, the workspace labels on app namespaces, or the
`melissa-apps-namespaces` admission policy. Binding model: [../AGENT-MODEL.md](../AGENT-MODEL.md).

Code: `crates/agent/src/tools/scope.rs` (model + the one check), `crates/agent/src/tools/mod.rs`
(`ToolRegistry::call` → `check_scope`, `visible_to`, `describe_call`), `crates/agent/src/device.rs` (ids),
`crates/agent/src/lead/registry.rs` (agents), `crates/agent/src/lead/agents.rs` (agent tools, reassign),
`crates/cluster/src/app_tools.rs` (`Workspaces`: owner lookup, workspace list, reassign patch; `deploy_app`'s `agent`),
`crates/cluster/src/render.rs` (`Ownership`, label names, `assign_patch`), `apps-rbac.yaml`.

## Model (2026-11-18, replaces the owner/job model)
- **Ids.** Device `d-<8 hex>` (made once, `$XDG_STATE_HOME/melissa/device.json`); agent `<device>.a<n>`
  (never reused; the registry keeps `next` and never goes below the highest id); workspace = app name
  (`<device>/<name>` across devices). All fit label values.
- **Who owns a workspace:** `Owner::Agent(id)` or `Owner::Unassigned`, from the label
  `melissa.io/owner-agent` on the app namespace. Also on the namespace: `melissa.io/workspace=<name>`,
  `melissa.io/device=<device>`, annotations `melissa.io/assigned-by|at` (RFC 3339 to the ns),
  `melissa.io/installed-by|at`, `melissa.io/origin`.
- **Scope of a task** (`TaskContext.scope`, set when its MCP session opens, never by the agent):
  `User` (operator CLI `tool`, `POST /api/tools`), `Lead { device }` (its session), `Agent { id }` (each
  agent's own conversation), `Nothing` (default; one-off UI tasks: read only).
- **Check order** in `ToolRegistry::call`: unknown/lead-only → **scope** → label policy → confirmation
  (one confirmation, reasons joined with `; `: scope note or the user-only reason, `describe_call`, policy
  reasons) → call → audit.
- **Which apps are mine:** `my_apps` (cluster `MyApps`, no target, filters by `ctx.scope` in `call_in`). An
  agent: `yours` (essentials from its `app_status`) and `not_yours` (`app` + `owner` name, null = unassigned;
  only the namespace is read for those). Lead agent/user/UI: `by_owner`. Always in an agent's tool list
  (`lead::agents`, even with a restricted `tools` list). Prompts and skills say: call it first when a request
  mentions an app; with look-alike names act on the one in `yours`.

## Rules in model terms
| who (scope) | list names + owners (`my_apps`, `list_apps`) | read a workspace (`app_status`) | read into a workspace (`app_logs`) | create a workspace | change its own workspace | anything on another agent's or an unassigned workspace | reassign a workspace |
|---|---|---|---|---|---|---|---|
| user (`User`) | yes | yes | yes | yes; owner = `agent` arg or unassigned | — | yes (policy only) | `melissa-server workspace reassign <workspace> --to <agent>` (no confirmation) |
| Lead agent (`Lead{device}`) | yes, all by owner | yes | yes (taints) | **refused** (2026-11-20: hands it to a helper, `create_agent` with `job` or `give_agent_job`; `LEAD_HANDS_OFF`, no confirmation). Bringing back an uninstalled app (namespace kept) is a change | — | another agent's: allowed (audited `tool.cross_owner` "allowed (lead agent)"); when the policy asks (destructive always, tainted always) the reason starts "`x` is looked after by agent Ada (id)". Unassigned: **confirmation** ("not assigned to any agent (only you decide about it)") | `reassign_workspace`: **always** the user's confirmation (`Effect::Ownership`) |
| agent (`Agent{id}`) | own in full, others name + owner only | own only | own only | yes; the new workspace is its own | yes (policy: unattended, so reversible asks unless pre-approved; destructive always) | **user-only confirmation**, reads included: "Ada wants to stop Bo's `x` (not Ada's). Only you can allow this, and only this once" | never (tool hidden + refused) |
| nobody (`Nothing`, the UI's reads) | yes, all | yes | refused | refused | — | refused | — |
| Lead agent on a workspace of another device | | | | | | refused "on device c-…; ask that device's lead agent" | |

- **User-only confirmation** (`ScopeDecision::AskUser`, `Confirmation::user_only`, `user_only` in
  `/api/confirmations`): asked whatever the label policy says (its reasons are appended), no rule offered
  (`always_allow` → `NotOffered`), never matched by a rule, **not answered by No permission prompts** (skipped at
  request time and by `set_bypass(true)`); confirmations are answered only from the owner's UI/API, so the lead
  agent cannot approve it either. A yes covers that one call; the next call asks again. Why not the bypass:
  ownership is a safety boundary, see DECISIONS 2026-11-20.
- **Refused** = `ToolError::Refused("not yours: …")`, MCP `isError`, audit `tool.refused`, **no
  confirmation created**: only an unscoped session reading into or changing a workspace, the lead agent on
  another device's workspace, and the lead agent creating a new workspace (`LEAD_HANDS_OFF`: "hand this to a
  helper …"). Owner unknown (k3s unreachable) → fail closed (`could not check who owns …`).
- **Audit `tool.cross_owner`** (registry, one line per step, detail "<outcome>: <who> → <verb> `<app>` of
  <owner>", e.g. "asked: agent Ada (d-….a1) → run commands in `minecraft` of agent Bo (d-….a2)"): `asked`,
  `approved by the user (this call only)`, `denied by the user`, `not approved (<timeout/withdrawn>)`,
  `refused`, `approved` / `allowed (lead agent)` for the lead agent's override. The usual `tool.*` lines too.
- **Reassignment flips access at once:** the owner label is read at every call (`ResourceOwners`), nothing is
  cached; after `assign` the new owner has full access and the old one gets user-only confirmations.
- **Hidden, not just refused:** agents never see the lead agent-only tools (`create_agent`, `give_agent_job`,
  `agent_status`, `list_agents`, `stop_agent`, `message_agent`, `reassign_workspace`); calling one by name gets
  "unknown tool". Every targeted tool's `app` argument (reads too) tells an agent "Only the workspaces you look
  after (my_apps lists them) and new ones you create … Another agent's needs the user's yes, each time."
  `Nothing` sessions don't see change/scoped-read tools at all.
- **Every new workspace gets an agent:** an agent's install → itself; the lead agent's install needs `agent`
  ("Ada looks after Nextcloud"; unknown agent → `BadArgs`); the user may leave it out (unassigned).
  A reinstall keeps the record; naming a different `agent` on a reinstall is `BadArgs` (use reassign).

## Reassign path
- Lead agent: `reassign_workspace {workspace, to}` (Lead-only, `Effect::Ownership` → always confirmed). Reason e.g.
  "`nextcloud` is looked after by agent Ada (d-….a1); the lead agent wants to move workspace `nextcloud` to agent Bo
  (c-….b2); changes who looks after a workspace (only you decide that)". Recorded `assigned-by` = "the
  Lead agent, approved by the user"; lead agent event `workspace_reassigned`.
- User: `melissa-server workspace reassign <workspace> --to <agent>` → `POST /api/workspaces/{workspace}/reassign {"to"}` →
  `AgentPool::user_reassign` (audited `workspace.reassign`, actor ui; `assigned-by` = "the user").
- Both end in `WorkspaceStore::assign` → `Workspaces::assign`: merge patch (`render::assign_patch`) with the
  melissa-apps identity: owner-agent, device (kept if set), workspace, fresh `assigned-at`, old
  `melissa.io/owner` removed. `app-adopt` is gone (it errors with a hint).
- Cross-device (M3): agent ids carry their device and `WorkspaceStore::assign` takes any agent id; today `to` must
  be an agent of this device's registry.

## Migration (namespaces from before agents)
`melissa.io/owner=owner|job-<key>` or no owner label → **unassigned** (read-time; `workspace list` shows
`legacy_owner`). Why: a default agent would get rights over the user's apps that the user never gave;
unassigned keeps exactly the old protection (only the lead agent with the user's approval, or the user).
The legacy label disappears at the next install (server-side apply no longer writes it) or reassignment
(the patch nulls it). Then assign an agent: `melissa-server workspace reassign nextcloud --to Ada`.

## Admission (`melissa-apps-namespaces`)
CREATE: `melissa.io/workspace` = name, `melissa.io/device` present, no `melissa.io/owner`; owner-agent only with
assigned-by/at. Shapes: device `^d-[0-9a-f]{8}$`, owner-agent `^d-[0-9a-f]{8}[.]a[1-9][0-9]{0,8}$`. UPDATE:
device frozen once set; owner-agent unchanged unless `assigned-at` is new (variable `newAssignment`); an
owned workspace never loses its owner-agent; the old label may only be kept or dropped. Admission can't tell
agents apart (one write identity): per-agent scope lives in the lead agent; agents hold no cluster creds.

## Declaring a tool's target (every new workspace-changing tool)
```rust
fn target(&self) -> Option<Target> {
    Some(Target { kind: ResourceKind::Workspace, arg: "app", access: Access::Change, verb: "restart" })
}
```
Examples: `deploy_app` ("deploy over") is `Create` (2026-11-20: a new name is creating, which the lead agent may
not do; an existing one is a change like `Change`); `update_app` ("update"), `stop_app` ("stop"), `start_app` ("start"), `uninstall_app`
("uninstall"), `app_exec` ("run commands in") are `Change`; `app_logs` is `ReadScoped`; `app_status` is `Read`. A stop by another agent is
a user-only confirmation ("Ada wants to stop Bo's `nextcloud` …", audited `tool.cross_owner`); the lead agent's stop of Ada's workspace asks the
user with "`nextcloud` is looked after by agent Ada (…); stop `nextcloud` (…)" (2026-11-19).
`access`: `Read` (names, phases, addresses: everyone but another agent), `ReadScoped` (content the workspace
controls: not unscoped sessions either), `Change`. For an agent all three mean "its own, or the user's yes".
`arg` is a required string. Audit of every app tool (2026-11-20): `app_status` Read; `app_logs` ReadScoped;
`deploy_app` Create; `app_exec`, `update_app`, `stop_app`, `start_app`, `uninstall_app` Change; `list_apps` and
`my_apps` untargeted but scope-filtered; `reassign_workspace` lead-only; `my_files` is the agent's own file box
(no workspace); `transfer_file` (lead-only, `owner_approval` every call) copies or moves a file between boxes; there is no volume tool (volumes show in `app_status`).
A creating tool reads `ctx.scope` in `call_in` (see `InstallApp::new_owner`). Never decide ownership in
a tool by name. `describe_call(args)` adds a one-line description to confirmations;
`audit_detail(ctx, args, result)` adds what the `tool.call` audit line must record beyond the
argument hash (`app_exec`: who, app, argv, stdin length, pod, container, exit code; never secrets).

## Verified (2026-11-18)
- Unit tests: `scope.rs` (decisions, messages, presentation), `cluster/src/app_tools/tests.rs` (agent A
  refused 4× on agent B's Nextcloud, zero confirmations, nothing applied; an agent owns what it installs;
  Lead agent uninstall reason "`nextcloud` is looked after by agent Ada (d-0a1b2c3d.a1); destructive action";
  unassigned legacy workspaces; every install gets an agent; legacy migration + `assign` patch with a new
  `assigned-at` each time; fail closed), `agent/src/lead/tests.rs` (through real agent conversations:
  hidden lead agent tools, refusal audited on the agent's task, own workspace → "destructive action"; reassign by the
  Lead agent denied/approved; user reassign without confirmation), `cluster/tests/rbac_manifests.rs`.
- **Not yet verified live:** the new CEL rules against a real API server (VM): create without workspace/device →
  denied; patch owner-agent without a new assigned-at → denied; `workspace reassign` → allowed.

- 2026-11-20 (`my_apps`, user-only cross-owner confirmations): `scope.rs` (every access asks the user for
  another agent's workspace, `crossing`), `permission_tests.rs` (rules and bypass never answer it, refusals
  audited with both sides), `cluster/src/app_tools/tests.rs` (`my_apps` for an agent and the lead agent, two
  Minecraft servers, every app tool asks the user with no rule/bypass, one yes = one call, lead override,
  reassignment flips access), `lead/tests.rs` (through a real agent conversation).

## Gotchas
- `TaskContext::default()` means **no scope**: tests or call sites that change workspaces must set one.
- A registry without `with_owners` treats every workspace as existing and unassigned (the lead agent gets asked);
  without `with_agents` messages show agent ids instead of names. `melissa-server` wires both.
- Two reassignments in the same second must still differ: `assigned-at` has nanoseconds.
- A renamed agent keeps its old name in its running agent's system prompt until its session is new.
