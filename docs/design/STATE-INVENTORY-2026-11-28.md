# State inventory (2026-11-28)

Every file and directory Melissa keeps between runs: where it lives, its format, who writes and who reads
it, how it is versioned and written, and whether a backup needs it. Written for "chats and state survive
upgrades" (DECISIONS 2026-11-28) and as the input for the backup feature (PLAN M4). Keep it current when a
store is added or moved; `melissa_settings::factory_reset::WIPE`/`SWEEP` is the other list that must match.

## Where

| Name here | Service / container | From source (XDG) |
|---|---|---|
| `<config>` | `/etc/melissa` (container: symlink to `/var/lib/melissa-container/etc-melissa` on the `melissa-var` volume) | `~/.config/melissa` |
| `<state>` | `/var/lib/melissa` (`StateDirectory=`) | `~/.local/state/melissa` |
| `<data>` | `/var/lib/melissa` (same directory as `<state>`) | `~/.local/share/melissa` |
| `<home>` | `/var/lib/melissa/home` (the server user's `$HOME`) | the developer's `$HOME` |
| `<run>` | `/run/melissa` and other `/run/melissa-*` (tmpfs) | `$XDG_RUNTIME_DIR/melissa`, else `<state>/tmp` |

In the container everything persistent is on the `melissa-var` volume (`/var`); an image update or rollback
replaces only the image. The Nix service uses the same paths on the host.

## How files are versioned and written (melissa-statefile)

- **Format.** Whole files carry a top-level `format` number; a file without one is format 1 (everything
  written before 2026-11-28). Log lines (JSON Lines) carry `format` per line. All formats are 1 today.
  Format lists in code: `melissa_agent::formats`, `melissa_settings::formats`,
  `melissa_auth::{AUTH_FORMAT, SETUP_CODE_FORMAT}`.
- **Older format:** migrated forward in memory (`Format::migrations`), written in the current format at
  the next change.
- **Newer format** (an older image after a rollback): never written. Every save checks the file on disk
  first and refuses with "written by a newer version of Melissa ... changes are not saved until Melissa is
  updated again" (log and the action's error; no UI change). Per store, the "Newer" column below says what
  the older version does meanwhile. Log lines of a newer format are skipped, never removed; the older
  version keeps appending its own.
- **Marker at format 1** ("v1 key" column): written as `"format": 1` where the images from before formats
  existed ignore an extra key; left out where they would reject it (TOML read with `deny_unknown_fields`,
  JSON arrays, maps keyed by data). From format 2 on every file is a JSON object / TOML table with the key.
  In a map keyed by data a `format` entry whose value is not a whole number is data (a backend called
  "format"); those maps' values are never whole numbers.
- **Writes** ("atomic" below): a fresh temporary file in the same directory (`.<name>.<pid>-<n>.tmp`),
  fsync, rename over the file, fsync of the directory; a failed write removes its temporary file. A crash
  leaves the old file or the new one. Leftover `*.tmp` files are never read, and the next save of the same
  file removes them (process gone and older than 2 s, or older than 60 s): they may hold a copy of a secret
  (`auth.json`, a join token). Appends are single
  `write` calls in append mode; a last line a crash cut short is ended first, so it stays one bad line.

## Inventory

Backup: **must** (losing it loses the owner's data or settings), **should** (rebuildable but costly or
noticeable: history, counters, logins to providers), **cache** (rebuilt or re-fetched automatically),
**no** (runtime, secrets that must not leave the box unencrypted, or regenerated).

### `<state>`: the server's state (owner `melissa`, 0750; files 0600 unless noted)

| Path | Format | Written by | Read by | Versioned (v1 key) | Write | Newer (after rollback) | Backup |
|---|---|---|---|---|---|---|---|
| `device.json` | JSON `{id, name}` | server, once | server, CLI | yes (key) | atomic | read; never rewritten | **must** (agent ids, workspace labels hang on the id) |
| `agents/registry.json` | JSON: device, next number, agents, lead name, deleted | server (agent create/rename/stop/delete) | server, UI | yes (key) | atomic | read-only: agents shown, every change fails with the message | **must** |
| `agents/<id>/transcript.jsonl` (+ `.1` rotated) | JSON Lines, one event per line | server (each event) | server (chat view, recap), UI | per line (key) | append | newer lines skipped, own lines appended | **must** (the chats) |
| `agents/<id>/session.json` | JSON `{backend, session_id, tainted}` | server (session opened) | server (resume, taint) | yes (key) | atomic (was a plain write until 2026-11-28) | read leniently for resume; counts as **tainted** (so does an unreadable one) | **should** (resume + sticky taint) |
| `agents/<id>/tainted` | empty marker: the conversation read untrusted content | server (when tainted) | server (start) | no (its presence is the data) | atomic | n/a (any version writes it) | **should** (sticky taint; removed only by a fresh start) |
| `agents/<id>/usage.json` | JSON counters | server (each turn) | UI | yes (key) | atomic (no fsync until 2026-11-28) | read; saves refused (counters stop) | should |
| `agents/<id>/agent-sessions/<backend>/` | the provider CLI's own session store (opaque, e.g. Codex rollouts) | the agent CLI | the agent CLI (resume), usage collector | not ours | the CLI's | n/a | should (resume; without it the chat is primed from the transcript) |
| `agents/<id>/chats/*.jsonl` | JSON Lines (chats archived by "New chat" before 2026-11-20, read-only) | nothing now | server (past chats) | per line | — | — | **must** |
| `lead/` | same files as `agents/<id>/` for the lead agent (`transcript.jsonl`, `session.json`, `usage.json`, `agent-sessions/`, `chats/`) | server | server, UI | as above | as above | as above | as above |
| `lead/backend.json` | JSON `{backend, model, effort, fast}`: the lead agent's model choice (a helper's is in the registry) | server (model picker) | server | yes (key) | atomic | read; saves refused | **must** (owner's choice) |
| `lead/models.json` | JSON map backend → `{models, updated}` (older: bare list) | server (sessions, probes) | server, UI | yes (no key: data-keyed map) | atomic | treated as empty; not saved over | cache |
| `lead/provider-usage.json` | JSON map provider → counts | server | UI (Usage) | yes (no key: data-keyed map) | atomic (no fsync until 2026-11-28) | treated as empty; not saved over | should |
| `lead/attachments/` | old shared attachment store | nothing (moved into file boxes once at start) | server (migration) | — | — | — | must while it exists |
| `threads/<a>__<b>.jsonl` (+ `.1`) | JSON Lines: agent-to-agent messages | server | UI (owner only) | per line (key) | append | newer lines skipped | should |
| `schedules.json` | JSON: next id, schedules (cron, last run, handled-until), approval wait | server (owner, agents' `schedule_job`, the scheduler) | server, UI | yes (key) | atomic | shown, **none run**, changes fail | **must** |
| `permissions.json` | JSON: standing rules, bypass, full access | server (owner) | server (every confirmation), UI | yes (key) | atomic | **not read** (start with none: fails closed); never saved over | **must** |
| `files/boxes/<box>/` | the file boxes' files (agents' and the owner's uploads) | server, agents (bound into their sandbox) | server, agents, UI | not ours (user files) | the writer's | — | **must** |
| `files/index/<box>.json` | JSON array of file records | server | server, UI | yes (no key: array) | atomic | re-indexed from the files (counted as added now, nothing expires early); not saved over | should (rebuilt from the files, but "added" dates restart) |
| `files/retention.json` | JSON `{default, overrides}` | server (Settings → Files) | server (pruning), UI | yes (key) | atomic | **keep forever** meanwhile (nothing pruned); not saved over | **must** |
| `audit.jsonl` | JSON Lines, one record per action (0640) | server; root's factory reset (one line) | owner, support; nothing parses it in code | per line (key) | append | lines kept as they are | **must** (history; never rewritten) |
| `agent-users.json` | JSON: id ranges, slot per agent key | server | server (sandbox users) | yes (key) | atomic | **refused** (agent users not set up; slots decide isolation) | **must** if the files agents made are restored (their owners are these ids) |
| `auth/auth.json` (dir 0700) | JSON: accounts (Argon2id hashes), invites, sessions (token hashes) | server | server | yes (key) | atomic | read-only: existing sessions work, sign-ins and account changes fail | **must**, encrypted (password hashes) |
| `auth/revoked.json` | JSON `{sessions: [token hash], invites: [id]}`: sign-outs and withdrawn invites made while `auth.json` was read-only | server (only while `auth.json` is a newer Melissa's) | server (applied at every start; removed after the next save of `auth.json`) | yes (key) | atomic | read; not saved over (the sign-out then lasts until a restart, logged as an error) | **must** with `auth.json` |
| `auth/setup-code.json` | JSON: unclaimed box's code | server | server | yes (key) | atomic | read; not saved over | no |
| `app-secret-prints/<app>.json` | JSON map field → fingerprints of generated app secrets (no values) | cluster crate on install | redaction of app logs | no (data-keyed map; a change must rename the directory) | atomic (no dir fsync) | — | should (without it generated secrets of existing apps are not redacted in logs) |
| `work/lead/`, `work/agents/<id>/` | the agents' working directories (their files) | agents | agents | not ours | — | — | **must** (work products) |
| `work/lead/task-<n>/mcp.json` | per-task MCP config with a session token | server | the agent CLI | no | written once per task | — | no |
| `acp-registry.json` | cached public ACP registry | server (Settings → Providers → Add) | server | no | atomic | — | cache |
| `egress/`, `tmp/` | sockets, scratch (only when there is no `$XDG_RUNTIME_DIR`) | server | server | — | — | — | no |
| `comb.json`, `bees/`, `queen/` | old names, moved once at start (`melissa_agent::migrate`) | — | migration | — | — | — | must while they exist |

### `<data>`

| Path | Format | Written by | Read by | Versioned | Backup |
|---|---|---|---|---|---|
| `models/` | the speech model (`whisper.cpp` ggml, checksum-verified) | server (Settings → Voice download) | server | file name + checksum | cache (re-downloaded) |

### `<home>` (`/var/lib/melissa/home`)

| Path | What | Backup |
|---|---|---|
| `.claude/`, `.grok/`, `.gemini/`, `.codex/`, `.config/muse/`, `.local/share/opencode/`, ... | provider CLIs, their sign-ins and own caches (third-party formats) | should, encrypted (sign-ins hold tokens); a restore may need fresh sign-ins anyway |
| `.local/bin`, `.grok/bin`, `.opencode/bin`, `.local/share/melissa-agents/` | installed CLIs and registry agents | cache (reinstallable) |

### `<config>` (owner `melissa`, 0750; files 0600)

| Path | Format | Written by | Read by | Versioned (v1 key) | Write | Newer | Backup |
|---|---|---|---|---|---|---|---|
| `server.toml` | TOML, hand-written | the owner, install.sh (`[apps] addresses`); renamed from `queen.toml` once | server | no: owner-edited; renamed keys are handled by `config::legacy_keys` warnings | — | — | **must** |
| `providers.toml` | TOML (`deny_unknown_fields`) | UI, `melissa-server provider` | server (startup), UI | yes (no key) | atomic | server starts without it and **no longer moves it aside** as `.toml.broken`; saves refused | **must**, encrypted (API keys) |
| `providers.toml.broken` | a broken providers.toml moved aside | server | owner | — | — | — | should |
| `ui.toml` | TOML (`deny_unknown_fields`) | UI | UI, root's cluster prepare (device name) | yes (no key) | atomic | defaults shown, saves refused | **must** |
| `tailscale.toml` | TOML (`deny_unknown_fields`) | UI | root (`melissa-tailscaled` start), UI | yes (no key) | atomic | treated as unreadable (Tailscale stays off); saves refused | **must** |
| `app-access.toml` | TOML (`deny_unknown_fields`) | UI, agents' `set_app_access` (owner approved) | root (`app-access apply`: nftables) | yes (no key) | atomic | unreadable: the filter fails closed (this device only); saves refused | **must** |
| `cluster-join.token`, `cluster-invitation.token`, `cluster-request.json`, `cluster-answer.json` | multi-device hand-off files | server / root | root / server | no (short-lived requests) | atomic | — | no (tokens) |

### Root-owned (container)

| Path | Format | Written by | Read by | Versioned | Backup |
|---|---|---|---|---|---|
| `/var/lib/melissa-container/cluster/setting.toml` (0600, dir 0700) | TOML (`deny_unknown_fields`) | root (`melissa-server cluster ...`) | root | yes (no key); a newer one is refused like an unreadable one, never saved over | **must** (multi-device) |
| `/var/lib/melissa-container/cluster/` other files (node name, agent token, has-peers marker) | plain text | root | root, k3s | no | must (token: encrypted) |
| `/var/lib/melissa-container/reset/factory-reset.json` | JSON marker, transient | server | root reset unit | no (a stale marker always means "finish the wipe") | no |
| `/var/lib/melissa-container/install.env`, `status/` (`status.json`, `held.json`, `action.json`), `request/` | install settings, the update helper's status and requests | install.sh, `melissa-host-update` (shell) | helper, server (read-only `/run/melissa-update`) | no (shell-written; outside this crate) | install.env: should; the rest no |
| `/var/lib/melissa-container/rancher-node` (`/etc/rancher/node`) | k3s node password | k3s | k3s | k3s's | must (a node rejoining needs it) |
| `/var/lib/melissa-tailscale/` | Melissa's own Tailscale state | tailscaled | tailscaled | tailscaled's | should (else sign in again) |
| `/var/lib/rancher/k3s/` (+ `.single-node-backup`, `.agent-backup`), `/var/lib/kubelet`, `/var/lib/cni` | k3s datastore, app volumes (local-path), images | k3s | k3s | k3s's | **must**: the datastore and app volumes are the apps' data (backups of app data are the backup feature's own design: per-app, quiesced) |
| `/var/log/journal` | the journal | journald | owner | — | no |

### Runtime only (tmpfs, never persisted)

`/run/melissa/api.sock`, `/run/melissa-kube/*.kubeconfig`, `/run/melissa-net`, `/run/melissa-cluster/`,
`/run/melissa-app-access/status.json`, `/run/melissa-agent-install/*.log`, `/run/melissa-update(-request)`,
`$XDG_RUNTIME_DIR/melissa/egress`, `/voice` scratch. Rebuilt at every start; no backup.

## For the backup feature

- **Must** set, in one consistent snapshot: `<config>` (except tokens), `<state>` without the cache rows,
  `agents/*/agent-sessions` optional, `files/boxes`, `work/`, root's cluster setting, and the k3s data.
  Stop the server (or hold its store locks) for the snapshot: the stores write whole files atomically, so a
  copy taken without stopping can mix files of different moments but never holds a half-written file.
- **Encrypt**: `auth/auth.json`, `providers.toml`, `<home>` sign-ins, cluster tokens.
- **Restore into an older image** is the rollback case: files of a newer format are refused as above, so
  a restore must not go into an image older than the one that made the backup (record the release in
  the backup).
- **Cache** rows can be left out and are rebuilt: `models.json`, `acp-registry.json`, `<data>/models`,
  installed CLIs.
