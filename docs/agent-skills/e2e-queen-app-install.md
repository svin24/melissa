# End-to-end: the owner installs an app by talking to the Queen

**When:** checking the whole product loop (chat → Queen → Melissa tools → k3s → app → owner login →
health → uninstall through the confirmation inbox) after changes to the Queen, the MCP layer, the
policy, the app tools or the image; trying a new Queen backend against real apps.

Note (2026-11-18, later): this run predates persistent bees. Today the Queen creates a named bee and
gives it the job (`create_bee`, `give_bee_job`, `bee_status`) instead of `start_worker`/`worker_status`,
and `install_app` from the Queen names the bee that looks after the new cell
([cells-and-ownership.md](cells-and-ownership.md)). Not re-run against the VM yet.

## Setup (laptop Queen, VM cluster)
The headless test VM has k3s and the app RBAC but no agent CLI logins; the dev laptop has them. So
the Queen daemon runs on the laptop (temp XDG dirs, egress proxy, default sandbox) and its cluster
tools talk to the VM's k3s through the forwarded API port (127.0.0.1:16443 → VM 6443). The k3s
serving cert has `IP:127.0.0.1` in its SANs, so no `tls-server-name` is needed. No code change was
needed: `kubeconfig` (read-only identity) and `[apps] kubeconfig` (write identity) are config keys.

```
# 1. VM, from the MAIN checkout (owns vm/), detached so tool time limits don't kill it
cd <main checkout>
setsid -f nohup devenv shell -- scripts/vm-run.sh headless > <scratch>/vm-run.log 2>&1 < /dev/null
# 2. image from the worktree, deploy from the main checkout
(cd <worktree> && devenv shell -- just image-build headless)
scripts/vm-dev.sh deploy headless                       # ~47 s incl. reboot
scripts/vm-dev.sh ssh headless 'df -h /var'             # want > 2.5 GB free for Nextcloud
# 3. kubeconfigs (contain tokens: 0600, scratch dir only, never commit/print, delete afterwards)
umask 077; for f in queen apps; do scripts/vm-dev.sh ssh headless "sudo cat /run/melissa-kube/$f.kubeconfig" \
  | sed 's#https://127.0.0.1:6443#https://127.0.0.1:16443#' > <scratch>/kube/$f.kubeconfig; done
```
`<scratch>/config/melissa/queen.toml`:
```toml
listen = "127.0.0.1:18090"          # 8080 may be taken on the laptop
kubeconfig = "<scratch>/kube/queen.kubeconfig"
[apps]
catalog = "<worktree>/manifests"
kubeconfig = "<scratch>/kube/apps.kubeconfig"
# [queen]
# backends = ["claude-session"]     # to force a backend; default grok-acp, claude-session, opencode-acp
```
```
export XDG_CONFIG_HOME=<scratch>/config XDG_STATE_HOME=<scratch>/state XDG_RUNTIME_DIR=/tmp/mq-rt  # short: socket paths
Q=<worktree>/target/debug/melissa-queen      # devenv shell -- cargo build -p melissa-queen -p melissa-agent --bins
$Q tool cluster_health                        # proves both the API path and the kubeconfig
setsid -f nohup $Q > <scratch>/daemon.log 2>&1 < /dev/null
$Q ask "What apps can you install here?"
```
- **Owner credentials with a remote cluster:** `app-credentials` reads the Secret with k3s's admin
  kubeconfig, which never leaves the bee. Run it on the bee:
  `scripts/vm-dev.sh ssh headless 'sudo melissa-queen app-credentials nextcloud'` (write it to a
  0600 file, not the terminal log). Then `curl -K <file with user = "admin:<pw>">` against
  `--resolve nextcloud.melissa.home.arpa:18080:127.0.0.1 .../ocs/v2.php/cloud/user?format=json`.
- **Answer confirmations** from a second shell: `$Q confirm list`, `$Q confirm approve <id>`. In a
  script: run `ask` in the background, poll `confirm list` with an until-loop.
- **Afterwards:** `pkill -x melissa-queen`, `shred -u <scratch>/kube/*`, stop the VM
  (`echo system_powerdown | socat - unix-connect:vm/headless/monitor.sock`), `podman stop melissa-registry`.
- Audit summary: `python3` over `<scratch>/state/melissa/audit.jsonl` (`action`, `task`, `tool`,
  `detail`; `egress.*` lines collapse repeats as `(+N repeats)`).

## Verified run (2026-11-18, image f1b3f01 + fixes, VM 3 GiB / 8.6 GiB /var, k3s v1.37.2)
Fresh start: old `app-nextcloud` namespace deleted, unused containerd images pruned (`k3s crictl rmi
--prune`: removed nextcloud and traefik/whoami; /var 2.0 → 4.6 GB free).

| step | owner says | Queen did | time |
|---|---|---|---|
| 1 | "What apps can you install here?" (grok-acp) | `list_catalog`; listed Nextcloud, its config fields, "password only via `sudo melissa-queen app-credentials`" | 10 s incl. session open |
| 2 | "Install Nextcloud. Tell me when it is ready to log in." | `install_app` (no confirmation: attended, untainted), `app_status`, `start_worker` (grok-acp worker polling `app_status` 12×), `worker_status` waits | 6 m 18 s (cold pull 513 MB) |
| 3 | owner logs in | `app-credentials` on the bee; OCS 200 (`admin`, groups `[admin]`), wrong password 401; WebDAV upload 201 | manual |
| 4 | "Is everything healthy? Any problems? Check Nextcloud's recent logs too." | `cluster_health`, `app_status`, `app_logs` (taints); correct summary incl. the deliberate 401 | 14.5 s |
| 5 | "Clean up anything left over and uninstall Nextcloud but keep its data." | `list_workers`, `uninstall_app` → inbox `#1 uninstall_app (destructive action) args: {"app":"nextcloud","delete_data":false}` → approved → `app_status` `uninstalled_data_kept` | 24 s (8 s of it the human) |
| 5b | "Install Nextcloud again." (same, tainted session) | `install_app` → inbox "this task has read untrusted content" → **denied** → reported plainly, no retry | 10 s |
| 2' | restart onto `claude-session`: "Install Nextcloud again, reusing its kept data …" | `install_app` ran **without** confirmation (bug, fixed), Secret "kept existing", ready; Claude called `app_logs` itself, then `start_worker` waited for an unseen confirmation until Claude gave up at 300 s (bug, fixed) | 5 m 22 s |
| 5' | (claude-session, after fixes) same as 5 | `list_workers`, `list_apps`, `uninstall_app` (approved), `app_status` ×2 | 13.6 s |

Data kept: PVC + Secret stayed, the WebDAV marker file was on the volume after two uninstall/
reinstall cycles, the old password kept working; ingress 404 while uninstalled. The password
appeared 0 times in the audit log, the transcript and the daemon log. k3s `NRestarts=0`, 0 AVC denials.

Egress (whole session, proxy mode): claude-session → `api.anthropic.com` only (14 tunnels).
grok-acp (Queen + worker) → `cli-chat-proxy.grok.com` 54, `api.x.ai` 8 allowed; `grok.com` 266 and
`api.mixpanel.com` 270 denied (repeats collapsed in the audit), no visible effect.

Agent behaviour: no hallucinated actions; tool errors and denials reported with the error text; "clean
up anything left over" did not invent cleanup (before `list_apps` it could only check workers and
said so). Grok's Queen polled `app_status` itself between `worker_status` waits (duplicate work, ~8
extra calls). Claude's Queen read logs on its own initiative while waiting, which tainted its session.

## Fixed in this run
- **Taint lost on a session reopen** (Queen restart, agent death, backend switch): a fresh untainted MCP
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
- **`list_apps`** (read-only): what is on the bee incl. `uninstalled_data_kept` leftovers.

## Gotchas
- Policy for `install_app` from the chat: attended + untainted → no confirmation (the owner asked;
  Reversible; uninstall keeps data). Tainted (any `app_logs` earlier in the conversation, now also
  across restarts) → confirmation. `uninstall_app` → always a confirmation.
- Taint is sticky with no reset yet: after one `app_logs`, every action of that Queen conversation
  needs approval. Deleting `session.json` + the transcript starts clean (dev only).
- With nobody watching the inbox, a waiting call blocks the Queen's turn for up to
  `confirm_timeout_secs`. In scripted runs, answer confirmations from a second shell.
- `ask` blocks for the whole turn (an install with a cold pull is 6+ minutes): run it in the background.
- `vm-dev.sh deploy` starts the dev registry; stop it afterwards. `crictl rmi --prune` also removes the
  smoke test's `traefik/whoami` (re-pulled by `vm-k3s-smoke.sh`).
- `sudo cat /path/*` over ssh: the glob is expanded by the unprivileged shell; use `sudo sh -c '…'`.
