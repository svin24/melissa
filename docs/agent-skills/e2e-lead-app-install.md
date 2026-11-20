# End-to-end: the owner installs an app by talking to the lead agent

**When:** checking the whole product loop (chat → Lead agent → Melissa tools → k3s → app → owner login →
health → uninstall through the confirmation inbox) after changes to the lead agent, the MCP layer, the
policy, the app tools or the image; trying a new lead agent backend against real apps.

Note (2026-11-18, later): this run predates persistent agents. Today the lead agent creates a named agent and
gives it the job (`create_agent`, `give_agent_job`, `agent_status`) instead of `start_worker`/`worker_status`,
and `install_app` from the lead agent names the agent that looks after the new workspace
([workspaces-and-ownership.md](workspaces-and-ownership.md)). Not re-run end to end since.

Note (2026-11-20): the lead agent no longer deploys new apps at all: it hands the request to a helper
(`create_agent` with `job`, or `give_agent_job`), whose `deploy_app` makes it the owner; the lead's own
`deploy_app` of a new app is refused ([lead-loop.md](lead-loop.md), "The lead hands app work to helpers").

Note (2026-11-19): apps now have `stop_app`/`start_app` ("turn off" must never become an uninstall) and
links `http://<device>:<port>` instead of `<app>.<domain>`.

Note (2026-11-19, later): the catalog is gone. `list_catalog`/`install_app` below are history; agents now
write a spec and call `deploy_app`, which always asks the user ([app-deploy.md](app-deploy.md)).

## Setup (the installed container, 2026-11-19)
Everything runs inside the rootful container from `packaging/container/install.sh`
([container-image.md](container-image.md)): lead agent, agent sandbox, k3s. Agent logins live in the volume.
```
just container-build && just container-install      # or keep the running install
sudo podman exec melissa melissa-setup agent opencode --yes --no-login   # or: -it ... agent claude (logs in)
sudo podman exec melissa melissa-server tool cluster_health
sudo podman exec melissa melissa-server ask "What apps can you install here?"
```
- **Answer confirmations** from a second shell: `sudo podman exec melissa melissa-server confirm list|approve <id>`.
  A `tool --wait` client that is killed leaves no pending confirmation behind; start it again.
- **Owner credentials:** `sudo podman exec melissa melissa-server app-credentials nextcloud` (write it to a 0600
  file, not the terminal log), then `curl -K <file with user = "admin:<pw>">
  http://<BIND>:<port>/ocs/v2.php/cloud/user?format=json`.
- Apps answer on the host at `BIND:30000+` (the app's link). Check `df -h` of the volume's filesystem before
  installing (Nextcloud pulls ~0.5 GB).
- Audit summary: `python3` over `/var/lib/melissa/audit.jsonl` inside (`action`, `task`, `tool`, `detail`;
  `egress.*` lines collapse repeats as `(+N repeats)`).

The runs below used the archived bootc VM with the lead agent on the laptop (procedure at tag
`bootc-archive-2026-11-19`); the results about the lead agent's behaviour still hold.

## Verified run (2026-11-18, image f1b3f01 + fixes, VM 3 GiB / 8.6 GiB /var, k3s v1.37.2)
Fresh start: old `app-nextcloud` namespace deleted, unused containerd images pruned (`k3s crictl rmi
--prune`: removed nextcloud and traefik/whoami; /var 2.0 → 4.6 GB free).

| step | owner says | Lead agent did | time |
|---|---|---|---|
| 1 | "What apps can you install here?" (grok-acp) | `list_catalog`; listed Nextcloud, its config fields, "password only via `sudo melissa-server app-credentials`" | 10 s incl. session open |
| 2 | "Install Nextcloud. Tell me when it is ready to log in." | `install_app` (no confirmation: attended, untainted), `app_status`, `start_worker` (grok-acp worker polling `app_status` 12×), `worker_status` waits | 6 m 18 s (cold pull 513 MB) |
| 3 | owner logs in | `app-credentials` on the agent; OCS 200 (`admin`, groups `[admin]`), wrong password 401; WebDAV upload 201 | manual |
| 4 | "Is everything healthy? Any problems? Check Nextcloud's recent logs too." | `cluster_health`, `app_status`, `app_logs` (taints); correct summary incl. the deliberate 401 | 14.5 s |
| 5 | "Clean up anything left over and uninstall Nextcloud but keep its data." | `list_workers`, `uninstall_app` → inbox `#1 uninstall_app (destructive action) args: {"app":"nextcloud","delete_data":false}` → approved → `app_status` `uninstalled_data_kept` | 24 s (8 s of it the human) |
| 5b | "Install Nextcloud again." (same, tainted session) | `install_app` → inbox "this task has read untrusted content" → **denied** → reported plainly, no retry | 10 s |
| 2' | restart onto `claude-session`: "Install Nextcloud again, reusing its kept data …" | `install_app` ran **without** confirmation (bug, fixed), Secret "kept existing", ready; Claude called `app_logs` itself, then `start_worker` waited for an unseen confirmation until Claude gave up at 300 s (bug, fixed) | 5 m 22 s |
| 5' | (claude-session, after fixes) same as 5 | `list_workers`, `list_apps`, `uninstall_app` (approved), `app_status` ×2 | 13.6 s |

Data kept: PVC + Secret stayed, the WebDAV marker file was on the volume after two uninstall/
reinstall cycles, the old password kept working; ingress 404 while uninstalled. The password
appeared 0 times in the audit log, the transcript and the daemon log. k3s `NRestarts=0`, 0 AVC denials.

Egress (whole session, proxy mode): claude-session → `api.anthropic.com` only (14 tunnels).
grok-acp (Lead agent + worker) → `cli-chat-proxy.grok.com` 54, `api.x.ai` 8 allowed; `grok.com` 266 and
`api.mixpanel.com` 270 denied (repeats collapsed in the audit), no visible effect.

Agent behaviour: no hallucinated actions; tool errors and denials reported with the error text; "clean
up anything left over" did not invent cleanup (before `list_apps` it could only check workers and
said so). Grok's lead agent polled `app_status` itself between `worker_status` waits (duplicate work, ~8
extra calls). Claude's lead agent read logs on its own initiative while waiting, which tainted its session.

## Fixed in this run
- **Taint lost on a session reopen** (Lead agent restart, agent death, backend switch): a fresh untainted MCP
  token although the agent still had the logs (resume) or a summary (transcript primer). Now sticky for
  the conversation and saved in `session.json` (`tainted`). Verified live: logs on claude-session →
  daemon restart → resumed → `start_worker` asked for approval.
- **Ghost confirmations:** Claude Code 2.1.327 abandons an HTTP MCP call after 300 s (despite
  `MCP_TOOL_TIMEOUT=960000`) but keeps the connection open and sends no `notifications/cancelled`
  (`nsenter -t <netbridge pid> -U -n --preserve-credentials ss -tnpo`: both MCP connections
  ESTABLISHED). The confirmation stayed in the inbox; approving it would have started a worker for
  nobody. Now `confirm_timeout_secs` (default 240) withdraws it; the agent gets "confirmation timed
  out: nobody answered in time, so the request was withdrawn". Verified live with 30 s. The MCP server
  also honours `notifications/cancelled` now and withdraws on a client close (tests), for clients that
  do send/close.
- **`list_apps`** (read-only): what is on the agent incl. `uninstalled_data_kept` leftovers.

## Gotchas
- Policy for `install_app` from the chat: attended + untainted → no confirmation (the owner asked;
  Reversible; uninstall keeps data). Tainted (any `app_logs` earlier in the conversation, now also
  across restarts) → confirmation. `uninstall_app` → always a confirmation.
- Taint is sticky with no reset yet: after one `app_logs`, every action of that lead agent conversation
  needs approval. Deleting `session.json` + the transcript starts clean (dev only).
- With nobody watching the inbox, a waiting call blocks the lead agent's turn for up to
  `confirm_timeout_secs`. In scripted runs, answer confirmations from a second shell.
- `ask` blocks for the whole turn (an install with a cold pull is 6+ minutes): run it in the background.
- `k3s crictl rmi --prune` inside also removes images the smoke test or apps re-pull later.
- `sudo cat /path/*`: the glob is expanded by the unprivileged shell; use `sudo sh -c '…'`.
