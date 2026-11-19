# Melissa hive model

The owner's model of how Melissa is organised. It is binding for code, docs and the UI. The diagram
`hive-model.svg` shows a 4-device example; its comments are machine-readable.

## Terms (internal roles)

These are internal role names. They don't have to surface in the UI: the user can name any queen, bee or comb
whatever they like, and a queen names the bees and cells she creates. The UI shows those display names.

| Term | Meaning |
|---|---|
| **Apiary** | Optional level above the hive: several hives (clusters or sites) together. |
| **Hive** | The whole cluster: every device together. |
| **Comb** | One device (a node). Each comb has exactly one queen. |
| **Main comb** | The one comb whose queen has the final say across the hive. |
| **Queen** | The lead agent on a comb. Only a queen can overwrite or reassign cells, unless the user asks. |
| **Bee** | A worker agent on a comb. Each bee looks after its own cells. |
| **Cell** | One bee's area of responsibility; it holds that bee's services. |
| **Service** | An actual workload running inside a cell (for example Nextcloud's containers). |
| **User** | The human. Can talk to any queen or bee directly and order any reassignment, for example telling one bee to take over another bee's cells. |

## Rules

1. **Ownership.** Every cell is owned by exactly one bee. A bee may change only its own cells; anything else is
   refused outright (no approval prompt) and recorded in the audit log as "not yours".
2. **Reassignment.** Only a queen, or the user, can overwrite or reassign a cell. Bees never take another bee's cells
   on their own. A bee may take over another bee's cells (also across combs) only because the user said so.
3. **Authority.** A queen manages the bees on her comb. The main comb's queen coordinates the other queens and decides
   conflicts across the hive. The user is above every queen and can override any of them.
4. **Talking.** The user can chat with any queen or bee directly. Agents talk to each other through their queen
   (brokered, audited, treated as data, never as instructions); queens talk to the main queen.
5. **Isolation.** Each bee works in its own sandbox; each cell is isolated from other cells by default. Links between
   cells (one app needing another) are declared, approved and audited.
6. **Policy still applies.** Ownership decides *whether* an agent may act on a cell at all; the label policy and
   confirmations decide *how* (destructive actions always ask the user, whoever owns the cell).

## Naming

- Every queen, bee, comb and cell has a stable internal id (never reused) and a display name.
- The user can rename anything; a queen names what she creates (bees, cells) and may suggest names for new combs.
- Ids include the comb, so the model works unchanged when a hive grows from one comb to many.

## Mapping to today's code (2026-11-18, after the hive-model rename; stop/start and ports 2026-11-19)

| Model | Code today | Still to do |
|---|---|---|
| Comb | `crates/comb` (`melissa-comb`, per-device daemon stub); comb id `c-<8 hex>` made once in `$XDG_STATE_HOME/melissa/comb.json` (`melissa_agent::comb`); the Queen daemon runs per device | the comb daemon itself; user-chosen comb names |
| Queen | the Queen conversation in `melissa-queen` (`melissa_agent::queen`), `Scope::Queen { comb }`: any cell of her comb, destructive actions name the owning bee | main-comb Queen coordination (M3) |
| Bee | persistent, named agent (`queen::registry`, `queen::bees`): id `<comb>.b<n>` (never reused), name (the Queen chooses, the user renames), backend, tools; its own resumable agent session, sandbox, MCP session (`Scope::Bee`), transcript; Queen tools `create_bee`, `give_bee_job`, `bee_status`, `list_bees`, `stop_bee` | budgets per bee; bee UI (owner's design) |
| Cell | an installed app (`app-<name>` namespace) with `melissa.io/cell`, `melissa.io/comb`, `melissa.io/owner-bee` (+ `assigned-by/at`); every install names its bee; old `melissa.io/owner` labels count as unassigned | more cell kinds (agent-built apps, backups) |
| Service | the app's pods/containers; the owner bee or the Queen stops/starts them with `stop_app` (scale to 0, always the user's confirmation, nothing deleted) and `start_app` (2026-11-19); reached at `http://<device>:<port>` (the cell's own NodePort, `melissa.io/port`) | more services per cell |
| Reassign | Queen: `reassign_cell` (always the user's confirmation); user: `melissa-queen cell reassign <cell> --to <bee>`; admission lets the owner change only with a new `assigned-at` | cross-comb takeover (ids and `CellStore::assign` already take any bee id) |
| Force delete | the user only: the app dialog's Force delete, `/api/apps/{app}/force-delete`, `melissa-queen app force-delete <app> --yes`; any owner; removes the cell and its data; the owning bee gets one brokered message, the Queen a note with her next message (2026-11-19) | |
| Talking | user ↔ Queen (`/api/queen`, `chat`), user ↔ any bee (`/api/bees/{bee}/…`, `chat --bee`); bee ↔ bee only via the Queen (`message_bee`: tagged as data, audited `bee.brokered`) | Queen ↔ main Queen (M3) |
| Apiary | none | later |
