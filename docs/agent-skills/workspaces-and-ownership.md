# Workspaces and ownership ("an agent changes only its own workspaces")

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
  (one confirmation, reasons joined with `; `: scope note, `describe_call`, policy reasons) → call → audit.

## Rules in model terms
| who (scope) | read names/phases (`app_status`, `list_apps`) | read into a workspace (`app_logs`) | create a workspace | change its own workspace | change another agent's workspace | change an unassigned workspace | reassign a workspace |
|---|---|---|---|---|---|---|---|
| user (`User`) | yes | yes | yes; owner = `agent` arg or unassigned | — | yes (policy only) | yes (policy only) | `melissa-server workspace reassign <workspace> --to <agent>` (no confirmation) |
| Lead agent (`Lead{device}`) | yes | yes (taints) | yes; **must name the agent** (`agent`) | — | allowed; when the policy asks (destructive always, tainted always) the reason starts "`x` is looked after by agent Ada (id)" | **confirmation** ("not assigned to any agent (only you decide about it)") | `reassign_workspace`: **always** the user's confirmation (`Effect::Ownership`) |
| agent (`Agent{id}`) | yes | own workspaces only | yes; the new workspace is its own | yes (policy: unattended, so reversible asks unless pre-approved; destructive always) | **refused** "not yours: … belongs to agent Bo (id); ask the lead agent" | **refused** | never (tool hidden + refused) |
| nobody (`Nothing`) | yes | refused | refused | — | refused | refused | — |
| Lead agent on a workspace of another device | | | | | refused "on device c-…; ask that device's lead agent" | | |

- **Refused** = `ToolError::Refused("not yours: …")`, MCP `isError`, audit `tool.refused`, **no
  confirmation created**. Owner unknown (k3s unreachable) → fail closed (`could not check who owns …`).
- **Hidden, not just refused:** agents never see the lead agent-only tools (`create_agent`, `give_agent_job`,
  `agent_status`, `list_agents`, `stop_agent`, `message_agent`, `reassign_workspace`); calling one by name gets
  "unknown tool". Their workspace tools' `app` argument says "only the workspaces you look after and new ones".
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
Examples: `deploy_app` ("deploy over"), `update_app` ("update"), `stop_app` ("stop"), `start_app` ("start"), `uninstall_app`
("uninstall") are `Change`; `app_logs` is `ReadScoped`; `app_status` is `Read`. A stop by another agent is
refused ("not yours", audited `tool.refused`, no confirmation); the lead agent's stop of Ada's workspace asks the
user with "`nextcloud` is looked after by agent Ada (…); stop `nextcloud` (…)" (2026-11-19).
`access`: `Read` (anyone), `ReadScoped` (content the workspace controls), `Change`. `arg` is a required string.
A creating tool reads `ctx.scope` in `call_in` (see `InstallApp::new_owner`). Never decide ownership in
a tool by name. `describe_call(args)` adds a one-line description to confirmations.

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

## Gotchas
- `TaskContext::default()` means **no scope**: tests or call sites that change workspaces must set one.
- A registry without `with_owners` treats every workspace as existing and unassigned (the lead agent gets asked);
  without `with_agents` messages show agent ids instead of names. `melissa-server` wires both.
- Two reassignments in the same second must still differ: `assigned-at` has nanoseconds.
- A renamed agent keeps its old name in its running agent's system prompt until its session is new.
