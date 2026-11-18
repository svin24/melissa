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

## Mapping to today's code (2026-11-18)

| Model | Code today | Planned |
|---|---|---|
| Comb | `crates/bee` (node daemon stub); the queen daemon runs per device | rename to comb |
| Queen | the Queen session in `melissa-queen` (one per daemon) | one per comb; main-comb queen coordination in M3 |
| Bee | "worker": a short-lived task started with `start_worker` | persistent, named bees that own cells |
| Cell | an installed app (`app-<name>` namespace) with `melissa.io/owner` = `owner` or `job-<key>` | owned by a bee: `melissa.io/owner-bee`, `melissa.io/comb` |
| Service | the app's pods/containers | same |
| Reassign | `sudo melissa-queen app-adopt` (moves an app to the user) | reassign a cell between bees: queen (with the user's approval) or user only |
| Apiary | none | later |
