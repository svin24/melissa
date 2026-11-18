# Ownership and agent scopes ("agents change only what belongs to them")

**When:** adding or changing any tool that changes or reads into a resource with an owner (apps today;
later update/restart/scale/stop, backups, agent-built apps), touching `start_worker`, the tool registry,
`TaskContext`, the app namespace labels or the `melissa-apps-namespaces` admission policy.
Code: `crates/agent/src/tools/scope.rs` (model + the one check), `crates/agent/src/tools/mod.rs`
(`ToolRegistry::call` → `check_scope`, `visible_to`), `crates/agent/src/queen/workers.rs` (`worker_scope`),
`crates/hive/src/app_tools.rs` (`AppOwners`, targets, labels on install), `crates/hive/src/render.rs`
(`Ownership`, label/annotation names), `crates/queen/src/adopt.rs`, `apps-rbac.yaml`.

Owner's requirement (2026-11-18): no agent shuts down something like Nextcloud that does not belong to it.

## Model
- **Owner of a resource:** `owner` (the human) or `job-<key>` (the worker job that created it). Apps store
  it as label `melissa.io/owner` on their namespace, plus annotations `melissa.io/installed-by`
  (`owner`, `the Queen (task 3)`, `worker task 7, job <key>`), `melissa.io/installed-at` (RFC 3339) and
  `melissa.io/origin` (`catalog`). **No label = the owner's** (apps from before 2026-11-18, and anything
  unparseable). Job keys are 12 random hex chars (task ids restart at 1 after a daemon restart; keys don't).
- **Scope of a task** (`TaskContext.scope`, set when its MCP session opens, never by the agent):
  - `Owner`: the human in person: `melissa-queen tool`, `POST /api/tools/{name}`.
  - `ForOwner { agent }`: an agent acting for the human while they watch: the Queen (`"the Queen"`), tasks
    started from the UI (`"an agent task"`). Everything; the owner's apps only after a confirmation that
    names them. What it creates belongs to the owner.
  - `Job { key, apps, create, for_owner }`: a worker. Only `apps` (granted by the Queen) and apps its job
    created; may create new apps if `create`. What it creates belongs to `job-<key>`, or to the owner if
    `for_owner` (approved by the owner at `start_worker`).
  - `Nothing`: the default for any session nobody gave a scope.
- **Check order** in `ToolRegistry::call`: unknown/queen-only → **scope** → label policy → confirmation (one
  confirmation carries both reasons, joined with `; `) → call → audit.

## Rules: who can do what to whose app
| caller (scope) | read status (`app_status`, `list_catalog`) | read logs (`app_logs`) | create new app | change/uninstall the owner's app | change a job's app |
|---|---|---|---|---|---|
| owner (`Owner`: CLI/operator API) | yes | yes | yes, owned by `owner` | yes (label policy only: uninstall still confirms) | yes |
| Queen / UI task (`ForOwner`) | yes | yes (taints) | yes, owned by `owner` | **confirmation naming the owner** ("`nextcloud` belongs to you; the Queen wants to uninstall it; destructive action") | yes (label policy) |
| worker (`Job`) | yes | own/granted apps only | if `create_apps` (default yes), owned by `job-<key>` (or `owner` with approved `for_owner`) | **refused** unless granted in `apps` (the grant asked the owner) | own job's: yes; other jobs': **refused** unless granted |
| no scope (`Nothing`) | yes | refused | refused | refused | refused |
| anyone | | | | ownership change: **only** `sudo melissa-queen app-adopt <app>` | |

- **Refused** = `ToolError::Refused("not yours: …")`, MCP `isError`, audit `tool.refused` with the reason,
  **no confirmation created**. The label policy still applies on top of an allowed scope (worker = unattended:
  reversible needs a confirmation unless pre-approved; destructive always).
- Owner unknown (k3s unreachable) → fail closed (`could not check who owns …`, audited `tool.refused`).
- `start_worker` grants: `apps` ⊆ what the Queen may change; owner's apps in `apps`, or `for_owner: true`,
  need one confirmation naming the owner ("`nextcloud` belongs to you; the Queen wants to let a worker change
  it"). Only `Owner`/`ForOwner` sessions may grant (a `Job` session is refused, besides never seeing the tool).
- **Hidden, not just refused:** a worker's `tools/list` hides change/scoped-read tools when its scope has no
  apps and no create right; otherwise the target argument's description lists the in-scope apps, and an
  `enum` pins them when it may not create.
- **Agents' own resources:** worker tools are Queen-only (workers never see them); `worker_status`,
  `cancel_worker`, `list_workers` reach only workers whose `parent` is the calling task. Agents reach Melissa
  only through their own MCP token on the sandbox's MCP-only socket (`egress/mcp.rs` serves only that
  sandbox's tokens), so one agent reads another's output only through the Queen's `worker_status`.

## Declaring a tool's target (do this for every new app-mutating tool)
```rust
fn target(&self) -> Option<Target> {
    Some(Target { kind: ResourceKind::App, arg: "app", access: Access::Change, verb: "restart" })
}
```
- `access`: `Read` (names, phases, counts: anyone), `ReadScoped` (content the resource controls, e.g. logs),
  `Change` (anything that creates, changes, stops or deletes it).
- `arg` must be a required string argument; a call without it is `BadArgs` before the tool runs.
- `verb` appears in confirmations: "the Queen wants to `<verb>` it".
- A tool that creates resources reads `ctx.scope.creator()` / `ctx.scope.actor(task)` in `call_in` and records
  them; an existing resource keeps its record (`Ownership::of`). A new resource kind adds a
  `ResourceKind` variant and a `ResourceOwners` lookup (wired with `ToolRegistry::with_owners`).
- Never decide ownership inside a tool by name; the registry does it once for all.

## Admission (`melissa-apps-namespaces`, `apps-rbac.yaml`)
- CREATE by `melissa-apps`: `melissa.io/owner` required, `^(owner|job-[a-z0-9]{1,40})$`.
- UPDATE: an existing owner label must stay the same; an unlabelled namespace may only gain `owner`.
- The policy matches only `melissa-apps`; `app-adopt` uses k3s's admin kubeconfig (root) with field manager
  `melissa-owner`, merge patch: label → `owner`, annotations `melissa.io/adopted-at`, `melissa.io/adopted-from`.
- Admission can't tell agents apart (one write identity); per-agent scope lives in the queen. That is sound
  because agents hold no cluster credentials: tools are their only path.

## Why `app-adopt` is a root CLI, not an agent tool
An agent that wants an app should ask the owner, not ask the owner to click "approve transfer" (confirmation
fatigue, persuasion). Ownership is the input to every other check; it gets the strongest gate we have (root +
admin kubeconfig), like `app-credentials`. Adopt only moves an app *to* the owner (jobs are short-lived).

## Verified (2026-11-18, unit tests with the fake cluster; no VM: it was in use)
- `crates/hive/src/app_tools/tests.rs`: a worker's uninstall (with/without data), install-over and logs of an
  unlabelled (legacy) Nextcloud → 4× `tool.refused`, zero confirmations, nothing applied, e.g.
  `{"actor":"agent","action":"tool.refused","task":7,"detail":"not yours: the app `nextcloud` belongs to the owner; ask the Queen","tool":"uninstall_app",…}`;
  a worker's own app: label `job-k1`, `installed-by` `worker task 7, job k1`, reinstall allowed, uninstall
  confirmation reason exactly `destructive action`; job `k2` refused ("belongs to worker job k1"); the Queen's
  uninstall/install-over of Nextcloud → confirmation reasons as in the table; the Queen's new install → `owner`,
  `the Queen (task 3)`, kept on an operator reinstall; k3s down → fail closed; tool visibility per scope.
- `crates/agent/src/queen/tests.rs`: default worker scope + random key, earlier job's app granted silently,
  owner's Nextcloud grant → confirmation, denial starts no worker; `for_owner` needs a yes; a `Job` session
  can't grant; another delegating session can't see/cancel the Queen's workers; workers see no worker tools.
- `crates/hive/tests/rbac_manifests.rs`: the owner rules are in the admission policy.
- **Not yet verified live:** the CEL owner rules against a real API server (run the negative checks in
  `app-catalog.md` plus: patch `melissa.io/owner` on an app namespace with `apps.kubeconfig` → denied;
  `sudo melissa-queen app-adopt <app>` → allowed).

## Gotchas
- `TaskContext::default()` now means **no scope**: tests or new call sites that change apps must set one.
- A registry without `with_owners` treats every resource as existing and the owner's (strictest): the Queen's
  installs then ask. Always wire `AppOwners` where app tools are registered (`melissa-queen` does).
- Label values allow `[A-Za-z0-9._-]`, max 63: hence `job-<key>`, not `job:<id>`.
