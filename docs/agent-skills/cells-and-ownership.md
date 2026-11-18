# Cells and ownership ("a bee changes only its own cells")

**When:** adding or changing any tool that changes or reads into a cell (apps today; later
update/restart/scale/stop, backups, agent-built apps), touching the bee tools, `reassign_cell`, the
tool registry's scope check, `TaskContext.scope`, the cell labels on app namespaces, or the
`melissa-apps-namespaces` admission policy. Binding model: [../HIVE-MODEL.md](../HIVE-MODEL.md).

Code: `crates/agent/src/tools/scope.rs` (model + the one check), `crates/agent/src/tools/mod.rs`
(`ToolRegistry::call` → `check_scope`, `visible_to`, `describe_call`), `crates/agent/src/comb.rs` (ids),
`crates/agent/src/queen/registry.rs` (bees), `crates/agent/src/queen/bees.rs` (bee tools, reassign),
`crates/hive/src/app_tools.rs` (`Cells`: owner lookup, cell list, reassign patch; `install_app`'s `bee`),
`crates/hive/src/render.rs` (`Ownership`, label names, `assign_patch`), `apps-rbac.yaml`.

## Model (2026-11-18, replaces the owner/job model)
- **Ids.** Comb `c-<8 hex>` (made once, `$XDG_STATE_HOME/melissa/comb.json`); bee `<comb>.b<n>`
  (never reused; the registry keeps `next` and never goes below the highest id); cell = app name
  (`<comb>/<name>` across combs). All fit label values.
- **Who owns a cell:** `Owner::Bee(id)` or `Owner::Unassigned`, from the label
  `melissa.io/owner-bee` on the app namespace. Also on the namespace: `melissa.io/cell=<name>`,
  `melissa.io/comb=<comb>`, annotations `melissa.io/assigned-by|at` (RFC 3339 to the ns),
  `melissa.io/installed-by|at`, `melissa.io/origin`.
- **Scope of a task** (`TaskContext.scope`, set when its MCP session opens, never by the agent):
  `User` (operator CLI `tool`, `POST /api/tools`), `Queen { comb }` (her session), `Bee { id }` (each
  bee's own conversation), `Nothing` (default; one-off UI tasks: read only).
- **Check order** in `ToolRegistry::call`: unknown/queen-only → **scope** → label policy → confirmation
  (one confirmation, reasons joined with `; `: scope note, `describe_call`, policy reasons) → call → audit.

## Rules in model terms
| who (scope) | read names/phases (`app_status`, `list_apps`, `list_catalog`) | read into a cell (`app_logs`) | create a cell | change its own cell | change another bee's cell | change an unassigned cell | reassign a cell |
|---|---|---|---|---|---|---|---|
| user (`User`) | yes | yes | yes; owner = `bee` arg or unassigned | — | yes (policy only) | yes (policy only) | `melissa-queen cell reassign <cell> --to <bee>` (no confirmation) |
| Queen (`Queen{comb}`) | yes | yes (taints) | yes; **must name the bee** (`bee`) | — | allowed; when the policy asks (destructive always, tainted always) the reason starts "`x` is looked after by bee Ada (id)" | **confirmation** ("not assigned to any bee (only you decide about it)") | `reassign_cell`: **always** the user's confirmation (`Effect::Ownership`) |
| bee (`Bee{id}`) | yes | own cells only | yes; the new cell is its own | yes (policy: unattended, so reversible asks unless pre-approved; destructive always) | **refused** "not yours: … belongs to bee Bo (id); ask the Queen" | **refused** | never (tool hidden + refused) |
| nobody (`Nothing`) | yes | refused | refused | — | refused | refused | — |
| Queen on a cell of another comb | | | | | refused "on comb c-…; ask that comb's Queen" | | |

- **Refused** = `ToolError::Refused("not yours: …")`, MCP `isError`, audit `tool.refused`, **no
  confirmation created**. Owner unknown (k3s unreachable) → fail closed (`could not check who owns …`).
- **Hidden, not just refused:** bees never see the Queen-only tools (`create_bee`, `give_bee_job`,
  `bee_status`, `list_bees`, `stop_bee`, `message_bee`, `reassign_cell`); calling one by name gets
  "unknown tool". Their cell tools' `app` argument says "only the cells you look after and new ones".
  `Nothing` sessions don't see change/scoped-read tools at all.
- **Every new cell gets a bee:** a bee's install → itself; the Queen's install needs `bee`
  ("Ada looks after Nextcloud"; unknown bee → `BadArgs`); the user may leave it out (unassigned).
  A reinstall keeps the record; naming a different `bee` on a reinstall is `BadArgs` (use reassign).

## Reassign path
- Queen: `reassign_cell {cell, to}` (Queen-only, `Effect::Ownership` → always confirmed). Reason e.g.
  "`nextcloud` is looked after by bee Ada (c-….b1); the Queen wants to move cell `nextcloud` to bee Bo
  (c-….b2); changes who looks after a cell (only you decide that)". Recorded `assigned-by` = "the
  Queen, approved by the user"; Queen event `cell_reassigned`.
- User: `melissa-queen cell reassign <cell> --to <bee>` → `POST /api/cells/{cell}/reassign {"to"}` →
  `BeePool::user_reassign` (audited `cell.reassign`, actor ui; `assigned-by` = "the user").
- Both end in `CellStore::assign` → `Cells::assign`: merge patch (`render::assign_patch`) with the
  melissa-apps identity: owner-bee, comb (kept if set), cell, fresh `assigned-at`, old
  `melissa.io/owner` removed. `app-adopt` is gone (it errors with a hint).
- Cross-comb (M3): bee ids carry their comb and `CellStore::assign` takes any bee id; today `to` must
  be a bee of this comb's registry.

## Migration (namespaces from before bees)
`melissa.io/owner=owner|job-<key>` or no owner label → **unassigned** (read-time; `cell list` shows
`legacy_owner`). Why: a default bee would get rights over the user's apps that the user never gave;
unassigned keeps exactly the old protection (only the Queen with the user's approval, or the user).
The legacy label disappears at the next install (server-side apply no longer writes it) or reassignment
(the patch nulls it). Then assign a bee: `melissa-queen cell reassign nextcloud --to Ada`.

## Admission (`melissa-apps-namespaces`)
CREATE: `melissa.io/cell` = name, `melissa.io/comb` present, no `melissa.io/owner`; owner-bee only with
assigned-by/at. Shapes: comb `^c-[0-9a-f]{8}$`, owner-bee `^c-[0-9a-f]{8}[.]b[1-9][0-9]{0,8}$`. UPDATE:
comb frozen once set; owner-bee unchanged unless `assigned-at` is new (variable `newAssignment`); an
owned cell never loses its owner-bee; the old label may only be kept or dropped. Admission can't tell
agents apart (one write identity): per-agent scope lives in the queen; agents hold no cluster creds.

## Declaring a tool's target (every new cell-changing tool)
```rust
fn target(&self) -> Option<Target> {
    Some(Target { kind: ResourceKind::Cell, arg: "app", access: Access::Change, verb: "restart" })
}
```
`access`: `Read` (anyone), `ReadScoped` (content the cell controls), `Change`. `arg` is a required string.
A creating tool reads `ctx.scope` in `call_in` (see `InstallApp::new_owner`). Never decide ownership in
a tool by name. `describe_call(args)` adds a one-line description to confirmations.

## Verified (2026-11-18)
- Unit tests: `scope.rs` (decisions, messages, presentation), `hive/src/app_tools/tests.rs` (bee A
  refused 4× on bee B's Nextcloud, zero confirmations, nothing applied; a bee owns what it installs;
  Queen uninstall reason "`nextcloud` is looked after by bee Ada (c-0a1b2c3d.b1); destructive action";
  unassigned legacy cells; every install gets a bee; legacy migration + `assign` patch with a new
  `assigned-at` each time; fail closed), `agent/src/queen/tests.rs` (through real bee conversations:
  hidden Queen tools, refusal audited on the bee's task, own cell → "destructive action"; reassign by the
  Queen denied/approved; user reassign without confirmation), `hive/tests/rbac_manifests.rs`.
- **Not yet verified live:** the new CEL rules against a real API server (VM): create without cell/comb →
  denied; patch owner-bee without a new assigned-at → denied; `cell reassign` → allowed.

## Gotchas
- `TaskContext::default()` means **no scope**: tests or call sites that change cells must set one.
- A registry without `with_owners` treats every cell as existing and unassigned (the Queen gets asked);
  without `with_bees` messages show bee ids instead of names. `melissa-queen` wires both.
- Two reassignments in the same second must still differ: `assigned-at` has nanoseconds.
- A renamed bee keeps its old name in its running agent's system prompt until its session is new.
