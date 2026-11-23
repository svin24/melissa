# Decisions

Format: date — decision. Why. Alternatives considered.

Entries before 2026-11-19 use the old role names: hive = cluster, comb = device, queen = lead agent
(`melissa-queen` = `melissa-server`), bee = agent, cell = workspace, apiary = fleet (see the last entry).

- **2026-11-18 — Agent backends drive the vendors' CLIs headlessly** (`claude -p`, `grok -p`, `opencode run`) behind
  one Rust `Backend` trait, plus a built-in `echo` backend. Why: reuses the user's existing subscriptions and logins,
  no API keys in Melissa, works today. Alternatives: direct HTTP APIs (needs keys, per-vendor code), ACP/MCP
  bridges (later, once tools exist).
- **2026-11-18 — Prompts go to CLIs as a single argv element, never through a shell; prompts starting with `-` are
  rejected.** Why: prevents shell injection and flag injection (e.g. an auto-approve flag).
- **2026-11-18 — Fixed component library in `crates/web/src/components.rs`; pages only call it.** Why: AGENTS.md UI rule;
  one reviewable place for markup. Hand-written CSS instead of a vendored framework (small, no download).
- **2026-11-18 — Until login exists: loopback-only bind, same-origin check for POSTs, strict CSP.** Why: no auth yet.
  Alternative: CSRF tokens now (deferred to the session work).
- **2026-11-18 — Two bootc image variants (desktop on Kinoite, headless on fedora-bootc) from one Containerfile.**
  Why: owner wants both; the laptop moves to the desktop one. Shared build stage keeps them in sync.
- **2026-11-18 — Extra packages are baked into the images (package list files), not layered on hosts.** Why: owner
  preference, reproducible bees.
- ~~**2026-11-18 — Desktop queen runs as a per-user systemd unit; headless as a system unit with DynamicUser.**~~
  Superseded the same day: both images run the system queen as the fixed `melissa` user (see below).
- **2026-11-18 — VM testing with QEMU from the Nix dev shell + bootc-image-builder.** Why: no host packages needed on an
  immutable OS. Alternative: libvirt/virt-install on the host (not installed).
- **2026-11-18 — Code hosted in a private GitHub repo.** Owner approved; public release needs an explicit yes.
- **2026-11-18 — Base directly on Fedora's atomic images (Kinoite, fedora-bootc), not on Universal Blue or a custom
  kernel.** Why: Fedora's shim, kernel and modules are signed, so Secure Boot works out of the box with no MOK
  enrollment; owner requirement at least through the first demo. The build enforces it (one kernel, kernel-core
  signed by the Fedora release key, no `kmod-*`/`akmod-*`), and VM tests boot with Secure Boot on.
  Alternatives: ublue bases (own kernel/kmods signed with their key, needs MOK), sealed images with our own key (later).
- **2026-11-18 — Take ideas from Universal Blue (Apache-2.0), never code.** Notes: `~/.local/share/melissa/research/` (the owner's local notes, not in the repo).
- **2026-11-18 — Agents get tools only through MCP served by the queen, one session token per task.** Why: every agent
  CLI speaks MCP; per-task tokens tie each call to that task's policy context; Claude's built-in tools are removed
  (`--tools ""`) so Melissa's policy is the only path to act. Alternatives: a shell with an allowlist (rejected: too
  broad), per-CLI tool plugins (more code per vendor).
- **2026-11-18 — Tool policy keys off labels (private / untrusted / egress / effect), not tool names.** Why: new tools are
  safe by construction if labelled honestly; labels are reviewed with the tool. Destructive always confirms.
- **2026-11-18 — k3s from pinned, sha256-verified release artifacts (binary, airgap images) plus k3s-selinux 1.6 (coreos
  build), all baked into the image; SQLite datastore; bundled Traefik with telemetry off.** Why: reproducible, offline
  first boot, SELinux enforcing, version moves with the image. Alternatives: the install script (mutates /etc and
  /usr/local, unpinned), k3s-selinux 1.7 testing builds (not stable), cert-manager now (later, for ACME in M4).
- **2026-11-18 — Apps are pulled live at runtime, driven by sandboxed agents (owner).** The agent decides and calls tools;
  the queen validates against catalog manifests and applies them to k3s; containerd pulls the images. Agents never hold
  cluster credentials or general egress. Alternatives: shipping app images in the OS image or sysexts (rejected for the
  catalog: too big, slow to update), agents running containers themselves (rejected: bypasses policy and audit).
- **2026-11-18 — Image = bare necessities for web UI + agents, plus k3s; the box ships empty and agents set up the rest.**
  Owner decision. k3s stays in the image (it's the platform agents act on; installing it would need host root).
- **2026-11-18 — Agent CLIs run under bubblewrap by default, with per-CLI presets of the narrowest home paths that
  work.** Why: defence in depth; agents must not read the user's files or change the host. bwrap ships with Fedora
  Atomic desktops and fedora-bootc (listed in `packages-common.txt` so it stays), is small, rootless, and needs no
  network setup (rootless podman's pasta networking is broken on the dev laptop). Host network is shared for now;
  a deny-by-default egress proxy is the next step. Alternatives: rootless podman (network broken here, heavier),
  systemd-run/landlock (less isolation of the file tree), no sandbox (rejected).
- **2026-11-18 — The Queen is the head agent; workers are sandboxed agents it starts.** Owner decision. Queen backend is a
  configurable list of subscription backends; no local LLMs on weak hardware.
- **2026-11-18 — ACP is the primary protocol for long-lived Queen/worker sessions; one-shot CLI runs stay as fallback.**
  Why: standard JSON-RPC over stdio with streaming updates, permission requests, cancel and resume; Grok/Kiro/fx speak
  it; tools can be passed per session (`mcpServers`). Use the official Rust crate `agent-client-protocol` (Apache-2.0).
  Permission requests map onto Melissa's policy; never answer with "always". Studied in T3 Code (MIT, reference only).
- **2026-11-18 — The queen reaches k3s as ServiceAccount `melissa/melissa-queen` with a non-expiring Secret-based token,
  minted into `/run/credstore/melissa.kubeconfig` by a root oneshot and imported with `ImportCredential=`.** Why:
  systemd credentials are copied once at service start, so a short-lived bound token (`kubectl create token`) would
  force queen restarts (killing agent tasks) on every refresh; the token is root-only on disk, scoped by read-only
  RBAC, and revocable by recreating the ServiceAccount (`melissa-kubeconfig rotate`; recreating only the Secret
  yields the same token). `ImportCredential=` (not `LoadCredential=`
  with a path) because a missing credential is fine: the queen starts without k3s (desktop, k3s down) and
  `cluster_health` says so. Alternatives: bound tokens + timer + restart (rejected: restarts), reading k3s.yaml
  (rejected: cluster-admin), a group-readable token file refreshed in place (possible later).
- **2026-11-18 — Revised: the queen's kubeconfig is a group-readable file in `/run/melissa-kube`, not a systemd
  credential, and the queen does not wait for k3s.** `melissa-kubeconfig` writes `queen.kubeconfig` root:`melissa-kube`
  0640 on tmpfs (dir 0750, headless only); `melissa-queen.service` keeps `DynamicUser=yes` and gets
  `SupplementaryGroups=melissa-kube`; the queen re-reads the file on every cluster call. Why: credentials are copied
  only at service start, so ordering the queen after the 300 s oneshot delayed the web UI and agents at first boot,
  and a late or rotated kubeconfig needed a restart that kills agent tasks. Trade-off: the token is readable by group
  `melissa-kube` instead of root only; no login user is in that group and only root can put a unit in it, so the
  exposure is the same set of principals (root + the queen). Alternatives: credential + restart on change (rejected:
  kills tasks), ACL for the dynamic UID (UID changes per start), a root helper serving tokens over a socket (more code;
  revisit for short-lived tokens, PLAN Q25).
- **2026-11-18 — Cluster code lives in a new `crates/hive` (kube-rs 5.1 + k8s-openapi 0.29 `v1_37`, rustls + ring),
  behind narrow traits (`ClusterRead` now, a separate write trait later).** Why: keeps the large kube dependency tree out
  of `melissa-agent`, tools are unit-tested against a fake, and read-only tools can't reach write methods.
  Alternatives: a module in `crates/agent` (heavier builds for every agent test), shelling out to kubectl (rejected).
- **2026-11-18 — Session API: `SessionBackend::open` → `AgentSession` (+ an event receiver), protocol-neutral, in
  `crates/agent/src/session`.** Methods `prompt`/`cancel`/`close` take `&self` and return `BoxFuture`, so the API
  stays object safe (`Arc<dyn AgentSession>`) and a turn can be cancelled from another task; one turn at a time.
  Events are typed (`Text`, `Thought`, `ToolStarted/Updated/Finished`, `Plan`, `Usage`, `Permission`,
  `TurnFinished`, `Other`); permission requests go to a `PermissionHandler` given at open. Why: the Queen keeps one
  session across many prompts; worker tasks reuse the same code through `AcpBackend`'s `Backend` impl (one prompt
  per session). Alternatives: a `Stream`-returning trait (not object safe without boxing anyway), callbacks per
  event (harder to test), exposing the ACP crate's types (ties the Queen to one protocol; Claude has none).
- **2026-11-18 — ACP permission mapping.** Melissa's own MCP tools: allow once at the ACP layer (the MCP server
  enforces policy, confirmations and audit on the call itself; asking twice would double prompts). The agent's
  built-in tools: ACP kind → policy labels (read/search/think read-only; edit/move reversible; delete/execute/
  unknown destructive; fetch untrusted + egress; switch_mode always rejected), then `policy::decide` and the
  confirmation inbox: allowed → `allow_once`, approved → `allow_once`, denied → `reject_once`, timeout or cancelled
  turn → `cancelled`. Never an "always" option, even if it is the only allow option offered. Every answer is
  audited as `agent.permission` (actor `agent`, input hashed). The agent's strictest permission mode is passed on
  its command line (`grok --permission-mode default`) because user configs can enable auto-approve.
- **2026-11-18 — `agent-client-protocol` 5.3.0 with default features off; we spawn the agent and frame lines
  ourselves (`Lines` transport).** Why: reuse the existing bwrap sandbox command and add a recorder tee for replay
  transcripts. Gotcha accepted: the crate enables serde_json `preserve_order` workspace-wide (canonical hashes now
  sort keys explicitly).
- **2026-11-18 — The Queen loop: one lazily opened agent session per daemon, one turn at a time (a second message
  is refused, not queued).** Code in `melissa_agent::queen`. Backends are tried in the configured order; a
  backend that fails to start is reported (`backend_failed`) and the next one tried. Why refuse instead of
  queue: a queued instruction can go stale while the Queen acts (the user may have changed their mind after
  seeing the first answer), the client already knows when a turn ends (`turn_finished`), and refusing keeps
  "nothing runs that the user did not just ask for" simple; a client may still queue on its side. Cancel sends
  `session/cancel`; after 10 s without the turn ending, Melissa kills the session. Alternatives: a server-side
  queue (rejected for the reasons above), interrupt-and-replace (surprising mid-tool-call).
- **2026-11-18 — Worker guards** (superseded the same day by persistent bees, below; the limits, depth 1,
  allowlist ⊆ the Queen's and taint rules carry over). Workers were board tasks with the Queen as `parent`, started only through the
  Queen-only tools (`start_worker`, `worker_status`, `list_workers`, `cancel_worker`; hidden from and refused
  for every session without `TaskContext.delegate`, so depth is 1). A worker runs unattended, with at most the
  Queen's own tools minus the worker tools (an allowlist must be a subset), passes `check_tool_set`, inherits
  the Queen's taint (and taints the Queen when it reads untrusted content), has a timeout ≤
  `worker_timeout_max_secs` (default 300 s, max 900 s), and at most `max_workers` (default 2) run at once.
  `start_worker`/`cancel_worker` are labelled Reversible: an attended Queen (the user is in the chat) may start
  workers without a confirmation, a tainted Queen needs one. Everything is audited (`tool.call`,
  `task.dispatch` with `parent` and actor `queen`, `task.finish`). Alternatives: workers as separate ACP
  sessions inside the Queen (no board visibility, no reuse of timeouts/cancel), letting workers delegate
  (unbounded fan-out and cost).
- **2026-11-18 — Queen context survives restarts by resuming the agent's own session, with Melissa's transcript
  as fallback.** The Queen's Grok runs with a Melissa-owned session store (`$XDG_STATE_HOME/melissa/queen/
  agent-sessions/<backend>`, 0700) bound at `~/.grok/sessions` inside the sandbox (only that directory, not
  `~/.grok`); the session id is saved and the next session tries `session/resume` (verified: context kept
  across a daemon restart and a killed agent). If resuming fails, the new session gets the last messages from
  Melissa's JSONL transcript (`queen/transcript.jsonl`, every event but text pieces, rotated at 4 MiB, 0600).
  A message whose turn died with the agent is not re-sent automatically (it may have had side effects).
  Alternatives: `session/load` (replays the whole history as events), transcript only (loses tool context),
  binding all of `~/.grok` (exposes the user's own sessions and memory).
- **2026-11-18 — The Queen's system prompt is a reviewed file shipped in the binary
  (`crates/queen/prompts/queen.md`, `worker.md` for workers), sent as a delimited preamble of the first message
  of every new (not resumed) session.** Why: ACP's `systemPrompt` field needs a capability Grok does not
  advertise; a preamble works with any agent and is visible in the protocol (testable). Trade-off: a long
  session may compact it away; revisit with an agent that supports `systemPrompt` (or Grok's `AGENTS.md`).
- **2026-11-18 — Queen API: JSON under `/api/` with a numbered event stream and long-poll (`GET
  /api/queen/events?since=&wait=`), not SSE yet.** One event model (`QueenEvent`) for the CLI now and the web UI
  later; clients keep a cursor (`next`) and learn when they fell behind (`missed`, 2000 events kept). Long-poll
  needs no streaming client in the CLI and works through the existing middleware; SSE can be added on the same
  `EventLog`. Same-origin check applies; JSON bodies need `application/json`. The CLI (`melissa-queen chat|ask|
  confirm|workers`) uses hyper's HTTP/1 client, already in the build via kube-rs (no new crates).
- **2026-11-18 — Deny-by-default egress for sandboxed agents: own network namespace + a bridge binary + a
  queen-side CONNECT proxy over per-sandbox Unix sockets.** bwrap unshares the network (loopback only, no
  resolver); `melissa-netbridge` (a binary of `melissa-agent`, safe Rust) starts first, listens on loopback and
  forwards each connection to `egress.sock` (HTTPS `CONNECT` proxy: per-backend host allowlist, port 443 by
  default, DNS by the queen, non-public addresses refused after resolution, limits, every decision audited) or
  `mcp.sock` (only `POST /mcp/<token>` for that sandbox's tokens), then runs the CLI as its child. The socket
  directory is per sandbox, so the socket identifies the agent. Config per backend: `network =
  "proxy"|"learn"|"none"|"host"`, `allow = [...]` (absent: built-in list from learn-mode runs). Fails closed.
  Why: works rootless and with SELinux enforcing on the laptop (no pasta/slirp4netns, iptables or root helper)
  and under DynamicUser; CLIs already honour `HTTPS_PROXY` (verified: claude, grok, opencode); no TLS
  interception means no CA to install and no secrets in Melissa's memory. It also closes the queen's own HTTP API
  (Queen chat, confirmation approval) and host abstract Unix sockets to agents. Alternatives: pasta/slirp4netns
  with a filtering netns (broken on the laptop, needs nftables for the policy), a SOCKS proxy (fewer CLIs
  support it), transparent interception (needs root/nftables), filtering by IP (provider IPs change; CDNs), TLS
  interception (CA trust per CLI, sees payloads), seccomp/Landlock network rules (Landlock TCP rules cannot
  name hosts; `unsafe` or a new crate). Trade-offs: host-level only (telemetry on the API host, e.g.
  Anthropic's event endpoints, cannot be separated), and a CLI that ignores the proxy gets no network at all.
- **2026-11-18 — `network` and `allow` are backend keys, not part of `[backend.sandbox]`.** Why: a sandbox table
  replaced the whole preset (paths), so changing only the network would have meant copying the preset (no longer
  so since 2026-11-23, see below). Repeated
  identical denials (grok retries telemetry ~40 times per prompt) are counted within 60 s and written as
  `(+N repeats)` with the next record or when the sandbox ends, so the audit stays readable and every attempt
  is still counted.
- **2026-11-18 — App manifests are Melissa's own small TOML format (`crates/catalog`, `manifests/*.toml`,
  shipped in `/usr/share/melissa/catalog/`), strictly parsed (`deny_unknown_fields`) and validated.**
  Images must be pinned by digest from the manifest's registry allowlist; config fields are typed
  (string/integer/boolean/choice with defaults, `secret` generated at install); every container
  declares requests and limits. Agents may only pick an app and override declared, non-secret fields.
  Why TOML: same as `queen.toml`, no YAML implicit typing, `toml` already a dependency (`serde_yaml` is
  unmaintained). Why not Helm/Compose: a tiny reviewed surface the queen fully validates (no templating,
  no privileged/hostPath knobs), rendered by a pure, golden-tested function. Alternatives: Helm charts
  (huge surface, templating agents could steer), Compose (no ingress/volume/probe semantics for k3s).
- **2026-11-18 — Apps run as a single-replica Deployment (`Recreate`) with separately managed PVCs, not
  a StatefulSet.** Why: one replica on one RWO volume needs no stable identity; separate PVCs make "keep
  data" vs "delete data" explicit in `uninstall_app`; `Recreate` never runs two pods on the data.
  Revisit for apps that scale out.
- **2026-11-18 — App writes use a second ServiceAccount, `melissa/melissa-apps`, separate from the
  read-only `melissa-queen`; admission policies pin it to `app-*` namespaces.** Cluster-wide it may only
  create/patch/delete Namespaces, create/patch RoleBindings and `bind` ClusterRole `melissa-app-workload`.
  ValidatingAdmissionPolicies (fail closed, matched on its username) allow only namespaces named
  `app-<name>` labelled `melissa.io/app=<name>` with Pod Security baseline/restricted, RoleBindings
  only there and only granting `melissa-app-workload` to itself, and ClusterIP Services without
  externalIPs. The installer binds `melissa-app-workload` per app namespace (workloads, Services,
  Ingresses, PVCs, ConfigMaps; Secrets create/delete only; pods and logs read). Why: RBAC alone can't
  say "namespaces matching app-*"; namespaced Roles give per-app scope without cluster-wide workload or
  Secret access; PSA baseline keeps the identity off the host (no privileged pods/hostPath); a separate
  identity keeps read-only tools on read-only credentials, shows up separately in the API audit and
  rotates on its own (`melissa-kubeconfig rotate apps`). Alternatives: widening `melissa-queen` (one
  token for everything), cluster-wide write ClusterRole (could touch kube-system), a root helper doing
  the applies (more code, same trust question). Verified in the VM, including the denials.
- **2026-11-18 — Generated app secrets live only in the app's Secret; neither queen identity can read
  Secrets; the owner reads them with `sudo melissa-queen app-credentials <app>` (k3s's root-only admin
  kubeconfig).** The installer creates the Secret once (POST; 409 = keep, so reinstalls keep the
  credentials that match the data) and never returns values to agents. Why: agents run as the queen's
  unprivileged user, so anything the queen can read an escaped agent could too; root + the admin
  kubeconfig is a boundary agents don't cross. Alternatives: a daemon API route (the loopback API has no
  auth yet and sandboxed agents share the host network), a copy in the queen's state dir (readable by
  the queen's user, can drift from the cluster).
- **2026-11-18 — Operator tool calls through the daemon: `POST /api/tools/{name}`, used by
  `melissa-queen tool --wait`.** Runs the tool through the daemon's registry as an unattended task 0
  with the normal policy, so confirmations land in the same inbox and the call waits for
  `confirm approve`; Queen-only tools are unreachable; audited (`operator.tool` + the usual
  `tool.*` lines). Why: the in-process `tool` command has its own inbox no one can answer. Exposure: same
  as the existing loopback API (same-origin check, no auth yet; see PLAN open question 28).
- **2026-11-18 — Claude Code as a session backend: Melissa drives Claude's own stream-json protocol natively
  (`claude-session`, `crates/agent/src/claude`), not a third-party ACP adapter.** One `claude -p
  --input-format stream-json --output-format stream-json --include-partial-messages` process per session, built-in
  tools removed (`--tools ""`), `--permission-mode manual` (the CLI's default "ask" mode, set explicitly),
  `--permission-prompt-tool stdio` so every permission prompt is a `can_use_tool` control request answered by
  Melissa's policy (never "always" rules), no `--allowedTools` (MCP calls ask too), `interrupt` control requests
  for cancel, `--session-id`/`--resume` with a Melissa-owned `~/.claude/projects` for the Queen. Why: everything
  needed (many turns per process, partial messages, permission prompts, interrupt, resume) is in the CLI and was
  verified live; it maps onto the existing `AgentSession` API with ~600 lines of Rust and no new crates; same
  sandbox and egress proxy as every agent. The ACP adapter would need Node (not on the host, not in the image),
  a network install of a third-party package (supply chain, version drift) and one more process translating
  permissions we must trust. Trade-off: the stream-json/control protocol is Claude Code's own and less
  documented than ACP; new CLI versions can change it (replay fixture + live checks catch that). Resolves PLAN
  open question 26.
- **2026-11-18 — OpenCode over ACP with a Melissa-owned config (`opencode-acp`, ACP flavor `opencode`).**
  Melissa writes OpenCode's whole config into `OPENCODE_CONFIG_CONTENT`: `permission: {"*": "ask"}` (its strictest
  setting: every built-in and MCP tool asks over `session/request_permission`; by default it ran MCP tools
  without asking), Melissa's MCP server with `codemode: false`, project config/auto-update/Claude-compat off; the
  sandbox does not bind the user's `~/.config/opencode`. Why: with Code Mode on (default) MCP tools are only
  reachable through OpenCode's JavaScript `execute` tool, so the permission request shows code, not a tool name,
  and Melissa could not tell a comb_info call from anything else; `codemode` can only be set in config, not in
  ACP's `session/new`. Hiding the user's config keeps their `allow` rules from merging into Melissa's. Trade-off:
  the user's model/provider choice does not apply (set `model = "provider/model"` on the backend); an OpenCode
  policy reject ends the turn (`cancelled`) rather than letting the agent continue. Also: the `opencode` preset
  now binds only `opencode.json{,c}` instead of the whole config directory, which held a local server password
  (`service.json`).
- **2026-11-18 — Queen backend order: `grok-acp`, then `claude-session`, then `opencode-acp`** (config
  `[queen] backends`). Each is tried when the previous one fails to start (missing CLI, login, handshake).
  Workers default to the first Queen backend and may use any session backend.
- **2026-11-18 — Web UI home is the Queen chat with a compact app-state preview (owner).** Simple, chat-bot-like; approvals
  inline. Concepts exist as mockups; building needs the owner's go.
- **2026-11-18 — Sub-agents may build apps in their own sandboxed environments; Nix is a candidate for those
  environments (owner).** Agents also handle cleanup and maintenance behind confirmations.
- **2026-11-18 — Isolation by default, communication by explicit, approved, audited links (owner).** Agent to agent via
  the Queen; app to app via manifest-declared NetworkPolicy links; agent to app via tools only.
- **2026-11-18 — Agents change only what belongs to them: resource ownership + task scopes (owner).**
  (Superseded the same day by "Cells owned by bees", below: same single check, refused-not-asked and fail-closed
  rules; owner/job labels and `app-adopt` replaced.) Every app
  records its owner on its namespace (`melissa.io/owner` = `owner` | `job-<key>`, plus installed-by/at and
  origin); no label = the owner's. Every task has a scope (`Owner` for the operator CLI/API, `ForOwner` for the
  attended Queen and UI tasks, `Job` for workers, `Nothing` by default). Tools declare their target argument;
  the tool registry checks the scope once, before the label policy, for every tool. Out of scope is **refused**
  (audited `tool.refused`), not a confirmation: a click-through prompt is not protection against an agent
  shutting down someone else's app. The Queen changing the owner's apps gets a confirmation that names the
  owner; workers get `apps` (granted by the Queen, owner apps only with the owner's yes) plus what they create.
  Why in the queen and not RBAC: agents hold no cluster credentials (tools are their only path) and the cluster
  sees one write identity; admission adds what it can see: the owner label is required on create and frozen on
  update. Ownership changes only through `sudo melissa-queen app-adopt` (root, admin kubeconfig), never an agent
  tool, not even behind a confirmation (persuasion/confirmation fatigue on the input every other check trusts).
  Trade-offs: the Queen's reinstalls/config changes of owner apps now always ask; jobs' apps outlive their job
  (Queen may still change them; the owner adopts them); without k3s the owner can't be checked → fail closed.
- **2026-11-18 — Hive model from the owner's diagram (`docs/hive-model.svg`).** Comb = device with one Queen; main-comb
  Queen has the final say; Bees are worker agents that own Cells; Cells host Services; only a Queen or the User
  reassigns Cells; cross-comb takeovers only on the User's say. The merged ownership work (owner/job labels, scope check
  in the tool registry) is the foundation and gets adapted to Bee/Cell ownership next.
- **2026-11-18 — Desktop variant = KDE plus the full server running by default (owner).** Same server setup as headless
  (k3s, system queen, web UI at boot); built after the headless demo. Supersedes "k3s disabled, per-user queen on desktop".
  Implemented 2026-11-18: desktop enables `melissa-queen.service` (User=melissa, state `/var/lib/melissa`, UI on
  127.0.0.1:8080), `k3s.service` and `melissa-kubeconfig.service` exactly like headless. The per-user unit
  (`/usr/lib/systemd/user/melissa-queen.service`) is deleted, not just disabled: a user starting it would fight the
  system queen for port 8080 and split state. The system queen's agent logins come from `sudo melissa-setup agent
  <name>` (melissa's home); a human's own CLI logins in their home stay theirs. Desktop also ships a firewalld zone
  (`melissa-k3s`) accepting k3s's pod/service networks, because Kinoite runs firewalld and fedora-bootc does not.
- **2026-11-18 — No first-boot account prompt on the desktop image.** `melissa-firstboot` (unit + script) ships only
  in the headless image. Why: a desktop either has a human user already (the laptop) or Plasma's own first-run setup;
  a console prompt on tty1 (and its kmscon ordering) has no place in front of the graphical login. Headless keeps the
  guard that it exits at once when a human user exists.
- **2026-11-18 — Code terms follow the hive model.** `crates/bee` (the per-device daemon stub) is
  `crates/comb` (`melissa-comb`); "worker" is "bee" everywhere (events, API, CLI, config keys with one-release
  aliases, prompts). Agent-facing tool names: `create_bee`, `give_bee_job`, `bee_status`, `list_bees`,
  `stop_bee`, `message_bee`, `reassign_cell`: verb-first plain words an LLM picks correctly (Grok found them by
  searching "create bee"); "job" for a unit of work because a bee is long-lived and gets many. The tool that
  describes the device is `comb_info` (renamed from `bee_info` the same day, no alias; open question 46).
- **2026-11-18 — Bees are persistent conversations; their registry lives in Melissa's state directory.**
  `$XDG_STATE_HOME/melissa/bees/registry.json` (0600, atomic rename) + `bees/<id>/` (agent session store,
  `session.json`, transcript). Why there and not in k3s: bees are this comb's agents, not workloads; they exist
  on combs without k3s; transcripts are private; the Queen's state already lives there. Ids `<comb>.b<n>` with
  a persisted, only-growing counter: never reused (an old audit line or label never points at a new bee), and the
  comb part makes ids hive-unique for cross-comb takeovers. Comb id: random `c-<8 hex>` made once
  (`comb.json`), so two combs that join later don't collide. Each bee runs the Queen's conversation machinery
  (extracted to `queen/conversation.rs`) instead of one-shot board tasks: it keeps context between jobs and
  resumes after restarts, and the user can chat with it directly. A job is a message into that conversation
  ("A job from the Queen: …"); time limit = cancel the turn. Alternatives: board tasks that resume a session
  per job (no direct chat, two code paths), a registry in a ConfigMap (needs k3s, leaks names into the cluster).
- **2026-11-18 — Cells owned by bees; the reassign path (owner's hive model).** Labels on the app namespace:
  `melissa.io/cell`, `melissa.io/comb`, `melissa.io/owner-bee` (absent = unassigned) + `assigned-by/at`.
  Scopes: `User`, `Queen{comb}`, `Bee{id}`, `Nothing`. A bee changes only its own cells and new ones (refused
  otherwise, audited "not yours", no confirmation); the Queen acts on any cell of her comb, destructive
  confirmations name the owning bee, unassigned cells need the user's yes; another comb's cell is refused.
  Reassign: the Queen's `reassign_cell` has `Effect::Ownership` (always confirmed: ownership is the user's
  decision) or the user's `cell reassign` (local API, no confirmation). Both patch with the melissa-apps
  identity; admission lets `owner-bee` change only with a new `assigned-at` and never back to unassigned. This
  replaces `sudo app-adopt` (root, admin kubeconfig): the root path could not read the user's bee registry, and
  the local API already carries the user's authority (`confirm approve`, `/api/tools`); a buggy or compromised
  queen daemon could already uninstall anything. The Queen must name a bee when she installs ("Ada looks after
  Nextcloud"), so no new cell is unowned by accident.
- **2026-11-18 — Migration: cells from before bees are unassigned.** `melissa.io/owner=owner|job-<key>` (or no
  label) reads as unassigned; the label is dropped at the next install or reassignment. Why not a default bee:
  it would get rights over the user's apps that the user never gave; "unassigned" keeps exactly the old
  protection (only the Queen with the user's approval, or the user). Old jobs' apps lose their job's rights
  (jobs no longer exist). One-off UI tasks get no scope (read-only on cells): they are neither Queen nor bee.

- **2026-11-18 — Headless queen runs as a fixed system user `melissa`, not DynamicUser.** Agent CLIs and their logins
  live in its home `/var/lib/melissa/home` (on the service's PATH), at the paths the sandbox presets already bind.
  Why: the demo VM must run agents in the system service, and DynamicUser has no stable owner or home for vendor
  installs and credential files. Each agent still runs in its own bwrap sandbox seeing only its own credential file.
  Existing state moves once (`melissa-state-migrate.service`; systemd's own migration is blocked by SELinux).
  Alternatives: DynamicUser + credentials as systemd credentials (no place to install CLIs, logins can't refresh),
  the admin's own user (couples the server to a login user).
- **2026-11-18 — Agent CLIs are never baked into images; `sudo melissa-setup agent <claude|grok|opencode>` installs
  the vendor's CLI with the vendor's official installer, as the `melissa` user, then runs its login.** Why: Claude
  Code is proprietary ("All rights reserved", use under Anthropic's terms; no redistribution grant); Grok's CLI
  source is Apache-2.0 but the official binary and service are under xAI's terms; OpenCode is MIT (could be
  shipped, but kept consistent and current via the vendor's installer; the box ships empty). The installer and
  login run outside the agent sandbox, started by root: they need download and login hosts that the agents'
  egress allowlists deliberately exclude, and only the admin can start them. Alternatives: mirror the binaries
  (licence), npm installs (adds Node to the image), API keys only (no subscription use).
- **2026-11-19 — The user switches the Queen's (or a bee's) backend and model at runtime; the choice lives in
  Melissa's state, applies from the next turn, and context carries over through the existing resume/recap path.**
  Queen: `$XDG_STATE_HOME/melissa/queen/backend.json`; bee: the bee registry. A running turn always finishes on the
  backend it started on. Same backend + other model resumes the agent's session; another backend gets a new
  session primed from Melissa's transcript (switching back never resumes the old, now stale session). Model lists
  come only from the agents (ACP `configOptions` category `model`, Claude's `initialize` answer) and are cached
  per backend once seen; Melissa never invents model ids, so a backend not yet used offers none. Unusable backends
  (not installed, not logged in, no sandbox) are refused. UI: one fixed component in the composer status line,
  native `details`/`select`, no new JS. Why: the owner set up several agents and asked for a visible switch;
  state (not config) because it is a user choice the daemon writes. Alternatives: editing `[queen] backends`
  (needs a restart, rewrites the owner's config), probing each CLI for models at startup (spawns agents, network
  calls, slow), switching mid-turn (would lose the turn's tool state).
- **2026-11-18 — Published VM image ships with no account; the tester creates one on first boot**
  (`melissa-firstboot.service` on tty1, before the console login; optional SSH key, or SSH password login on
  request). Why: never bake credentials into a downloadable image. Alternatives: a well-known password with forced
  change (still a published credential), cloud-init seed ISO (too fiddly for testers).
- **2026-11-18 — The web UI stays on loopback in the demo VM; testers reach it with `ssh -L 8080:127.0.0.1:8080`.**
  Why: there is no login yet and any API client can approve confirmations (PLAN Q37); SSH adds authentication and
  encryption at no cost. Alternative: bind the VM's host-only address with a warning (rejected: anything on that
  network gets full control).
- **2026-11-19 — Agent prose is rendered as Markdown (CommonMark + GFM tables, strikethrough, task lists) with
  `pulldown-cmark`, through our own maud renderer.** Owner tested the chat and asked for it (agents write Markdown;
  links should be clickable). Safety: raw HTML shows as text, links only to http/https/mailto with
  `noopener noreferrer nofollow` in a new tab, images are links (never loaded; the CSP blocks them anyway), nesting
  capped, bare URLs linked by a small tested linkifier. User messages stay plain text with linked URLs. Why
  pulldown-cmark: the standard Rust CommonMark parser, MIT, pure safe Rust, 3 small deps already allowed, no
  default features needed. Why not its HTML writer or an HTML sanitizer (ammonia): rendering our own tree keeps
  every text in maud escaping and the allowed elements in `components.rs`; a sanitizer would add html5ever and
  still start from raw HTML. Alternatives: `comrak` (heavier, its own HTML), `markdown-rs`, keeping `plain_text`.
- **2026-11-19 — Melissa is MIT-licensed.** Owner decision. `LICENSE` (copyright "The Melissa authors"), `license =
  "MIT"` for every crate via `[workspace.package]`, OCI label `org.opencontainers.image.licenses="MIT"`; the image
  already ships `LICENSE*` in `/usr/share/licenses/melissa/`. Why: the owner leaned towards MIT; simple, permissive,
  matches the dependencies' licenses. Alternatives: AGPL-3.0 + commercial dual license (needs a CLA), Apache-2.0.
- **2026-11-19 — Apps can be stopped and started, without removing anything** (owner's Nextcloud test: "turn off
  nextcloud" could only become `uninstall_app`). `stop_app` scales the app's Deployment to 0 and records the count
  in `melissa.io/replicas-before-stop`; `start_app` restores it. Namespace, volumes, Secret, Services, Ingress and
  the app's port stay. Labels: `stop_app` gets a new effect, **`Effect::Disruptive`**: always confirmed (it takes a
  service offline, like a destructive action), but its reason never says "destructive" and the UI card is not marked
  dangerous ("Stop Nextcloud? It can be started again; nothing is deleted."). `start_app` is **Reversible**, like
  `install_app`: it only brings back a state the user approved at install, so the attended, untainted Queen starts
  an app when asked without a second question; unattended or tainted tasks are asked (it uses resources and puts the
  app back on the network, so it is not read-only). Installing over a stopped app updates it and keeps it stopped: a
  confirmed stop is only undone by `start_app`. Prompts: "turn off/stop/pause/shut down" = `stop_app`;
  `uninstall_app` only on an explicit uninstall/remove/delete request. Alternatives: scale via the uninstall path
  (destructive, loses Services/port), a Reversible stop (an agent could take apps offline unasked while attended).
- **2026-11-19 — Apps are reached at `http://<device address>:<own port>` by default; `<app>.<domain>` stays as an
  option.** Nothing resolves `nextcloud.melissa.home.arpa` on the owner's devices, so the old link "just gives
  nothing". Each app with a web UI gets a NodePort from **30000-30079** in its own Service `<app>-port` (one port,
  the ingress port), recorded once in `melissa.io/port` on its namespace and fixed while the namespace exists (kept
  across stop, keep-data uninstall and reinstall). The range is the static band (first 86 ports) of k3s's default
  NodePort range, which Kubernetes' allocator avoids for automatic ports. Admission allows NodePort only for
  `<app>-port`, one port, in range, and keeps `melissa.io/port` in range and unchanged. Tools return `url` (first
  address), `urls` (every address: LAN address of the default route, Tailscale 100.64.0.0/10 addresses, or
  `[apps] addresses`), `port` and `dns_url`; the web UI builds links from the `Host` the visitor used (LAN IP,
  host name or Tailscale all work). Address detection makes no network traffic (UDP `connect` for the route,
  `/proc/net/fib_trie`). Why not: a Traefik entrypoint per app (needs kube-system Helm config changes per install,
  i.e. more privilege for the write identity), path prefixes on one port (most apps, Nextcloud included, don't
  run under a sub-path cleanly), the Queen proxying ports itself (more code in the trusted process, same firewall
  question), k3s ServiceLB/LoadBalancer (svclb pods per app, arbitrary host ports), wildcard-DNS services such as
  nip.io/sslip.io (third-party lookups; rejected outright), mDNS (`.local` names don't reach Tailscale peers or
  Android reliably). Firewall: headless (fedora-bootc) has no firewalld; Kinoite's default zone `FedoraWorkstation`
  accepts TCP 1025-65535 and forwards within the zone, and the desktop image ships a firewalld service
  `melissa-apps` (30000-30079/tcp) for stricter zones. Trade-off: every app's web port is open to the LAN and the
  tailnet (as with the Ingress on port 80 before); apps still need their own login.
- **2026-11-19 — Nextcloud trusts exactly the names it is reached at, no wildcards** (pre-alpha trade-off,
  stated). Nextcloud answers only hosts in `trusted_domains` (400 "untrusted domain" otherwise; reproduced with the
  pinned image). New catalog builtin `device_hosts` = ingress host + device addresses + host name at install time
  (or `[apps] addresses`), passed as `NEXTCLOUD_TRUSTED_DOMAINS`. The image reads that only at the first install, so
  the manifest ships a reviewed entrypoint hook (`before-starting`, new catalog field `files`: ConfigMap,
  read-only `subPath` mount, never inside a data volume, ≤ 16 KiB, ≤ 8 per container, hash on the pod template)
  that sets `trusted_domains` to exactly that list on every start; verified locally with the pinned image (an old
  install with only the ingress host answered 400 on IP:port, 200 after a restart with the hook; removed hosts are
  dropped; 2 s). Why not wildcards: Nextcloud's `*` becomes `[-.a-zA-Z0-9]*`, so `192.168.*` or `10.*` would also
  trust `192.168.evil.example` (DNS rebinding); a bare `*` turns the check off. Why not `overwritehost`: it rewrites
  every generated link to one name. Cost: if the device's address changes (DHCP, new tailnet), Nextcloud refuses
  the new one until the next `install_app` (or a fixed `[apps] addresses`). Security trade-off for pre-alpha: plain
  HTTP on the LAN/tailnet (Tailscale encrypts its leg; the LAN leg is cleartext, including the login), and the
  host name is trusted as given by the kernel.
- **2026-11-19 — Container mode is a thin layer over the unchanged headless image** (`image/container`), run by
  podman with systemd as PID 1, `--privileged`, `/var` as a volume and `--network=pasta:--host-lo-to-ns-lo`.
  Why: one image to maintain; the queen keeps its loopback-only UI (pasta delivers host loopback to the
  container's loopback, so pods can't reach the unauthenticated UI). Alternatives: `--network=host` (collides with
  a host k3s, and rootless k3s can't make interfaces there), slirp4netns (removed in podman 6), a socket proxy on
  the container's address (exposes the UI to pods), a separate non-bootc image (two images to keep in step).
  Status: spike; k3s in a container is unverified (rootless needs `cpuset` delegation).

- **2026-11-19 — Container mode runs k3s rootless; app addresses come from `container-run.sh`.** With `cpuset`
  delegated to `user@.service` (host drop-in `Delegate=cpu cpuset io memory pids`), k3s, flannel vxlan, the
  native overlayfs snapshotter on the `/var` volume, Traefik, our admission policies, RBAC and NodePorts through
  pasta work with no other change: none of the expected fallbacks (fuse-overlayfs, host-gw, userspace proxy) was
  needed. Container-layer changes only: `/etc/melissa` is a symlink onto the volume (like k3s's node password),
  so the Queen's config survives a recreate, and `up` writes `[apps] addresses` there (`127.0.0.1` for the
  default loopback `BIND`; nothing for `0.0.0.0`, because pasta copies the host's LAN address into the container
  and automatic detection finds it). Why not a code change for host ports that differ from 30000+n
  (`APP_PORT_BASE`): it only matters next to another k3s; the default maps ports 1:1. Base image unchanged.

- **2026-11-19 — The agent menu offers every model the agent lists (q55b).** No curation: Melissa shows the
  list exactly as the agent reports it (Claude also lists older ids). Why: a curated list goes stale with every
  agent release and would mean Melissa picking models for the owner; the agent knows what it can run.

- **2026-11-19 — New bees follow the Queen's switch (q55c); no Queen tool to move a bee (q55d).** After the user
  switches the Queen's backend, a bee the Queen creates without naming a backend runs on the Queen's backend (the
  model stays the backend's default). Before any switch `[queen] bee_backend` (else the first Queen backend)
  applies as before. Existing bees keep their backend, and so does a bee the user moved, so a switch never changes
  an agent that already has memory. Why: a user who moves the Queen to another agent (e.g. a subscription ran out)
  expects new helpers there too; moving running bees behind the user's back would not be expected. Only the user
  changes a bee's backend.

- **2026-11-19 — The desktop image delegates cpuset to users' systemd managers; headless does not (q59a).**
  Desktop ships `/usr/lib/systemd/system/user@.service.d/delegate.conf` with `Delegate=cpu cpuset io memory
  pids`, so rootless container mode works there with no host step. It only lets a user's own containers use
  two more cgroup controllers (cpuset, io) next to what systemd already delegates. Headless gets nothing: it is
  the image that runs *inside* container mode, and on a server box nothing runs rootless containers (k3s is a
  system service), so the drop-in would widen the box for no use. An admin can still add the `/etc` drop-in, and
  `container-run.sh` keeps its preflight hint.

- **2026-11-19 — Model lists on demand (q55a).** Melissa learns a backend's models with a probe: a session opened
  through the backend's own code path (same sandbox and egress rules), models read from what the agent reports
  (Claude's `initialize` answer, ACP `session/new`), closed without a prompt. Cached per backend with a
  timestamp; automatic probes (chat page, backend list) only when the list is missing or older than a day, at
  most once per backend per 10 minutes; a forced refresh (CLI `backend refresh`, `POST
  /api/queen/backends/refresh`) skips the limits; 15 s timeout. Why not parse `--help` or vendor model APIs: the
  ids would not be what the agent accepts, and Melissa never makes up model ids.
- **2026-11-19 — Defaults the owner accepted for apps and container mode (q56–q59).** Plain HTTP for apps is fine
  for pre-alpha; TLS comes later via Tailscale certificates with hardening. App ports stay per device until
  multi-comb exists. Nextcloud's trusted addresses: the Queen will refresh them when the device's addresses change;
  until then `[apps] addresses`. `stop_app` keeps its confirmation card for the demo. Container mode becomes a
  supported try-it path once rootful is verified; publishing a container image needs the owner's explicit yes.
  Host ports other than 30000+n are handled by `[apps] port_offset` (supersedes "no code change" above).
- **2026-11-19 — Rootful container mode uses the bridge network and a filtered UI forward inside.** Rootful podman
  has no pasta, and bridge `-p` forwards arrive on the container's `eth0`, not its loopback. Chosen: a socket unit
  on `:8081` running `systemd-socket-proxyd` to the queen's `127.0.0.1:8080`, behind an nftables table that admits
  TCP to 8081 only on the default-route interface from the default gateway (the host); everything else to 8081
  (pods on `cni0`, other bridge containers, IPv6) is dropped. Enabled only by a flag file that rootful `up` writes
  on the volume, so rootless is unchanged. Why: the queen keeps its loopback-only bind and its "no
  non-loopback without login" rule, and the exception is one port, one source, one interface, checked by
  `smoke` (pod isolation) and a negative control. Alternatives: binding the queen to `eth0` with an allowlist
  (weakens a queen invariant for a container-only need, and needs code); an nft DNAT to 127.0.0.1 (needs
  `route_localnet`, the CVE-2020-8558 class); `--network=host` (collides with a host k3s); `--userns` tricks to
  get pasta (podman refuses rootful). Verified with Nextcloud and stop/start next to a host k3s.
- **2026-11-19 — Provider settings live in `providers.toml`, merged over `queen.toml` at startup.** The web UI and
  `melissa-queen provider` write `$XDG_CONFIG_HOME/melissa/providers.toml` (0600, `/etc/melissa` on the image);
  precedence defaults < `queen.toml` < `providers.toml`; a provider is an agent program by file name and its
  overrides apply to all its backends. Changes apply at the next restart (the UI says so and offers a restart).
  Why: `queen.toml` stays the owner's hand-written file (no rewriting comments), the UI never edits it, and the
  backends are built once at startup. A broken file is moved aside, never fatal; an override a backend refuses
  is dropped for that backend. Secrets: names with KEY/TOKEN/SECRET/PASSWORD/... are write-only (never in HTML,
  JSON, logs; audit has a SHA-256). Program paths must keep the provider's file name (sandbox preset, egress
  allowlist, login check depend on it); extra arguments go before Melissa's own and a denylist refuses
  permission/tool/mode flags. Alternatives: editing `queen.toml` in place (loses comments, mixes owner and UI
  edits), settings in the state dir (it is config, and survives a state reset), live backend re-creation
  (larger change in the agent crate; follow-up).
- **2026-11-19 — Disabling the agent Melissa runs on is refused.** Also refused while a helper uses it, or when
  no other Queen agent would be left. Why: "switch at the next turn" would silently move the Queen (or a bee)
  to an agent the owner didn't pick; switching first is one click in the chat. Disabled providers' backends are
  left out at startup (and from `[queen] backends`/`bee_backend`), never all of the Queen's.
- **2026-11-19 — OS updates through a fixed-verb root helper started via polkit** (narrow answer to PLAN 42 for
  bootc). `/usr/libexec/melissa/melissa-update status|check|apply|rollback` runs as root only from four oneshot
  units; `50-melissa.rules` lets user `melissa` `start` exactly those units and `restart melissa-queen.service`.
  The queen runs `systemctl start --no-block <unit>` for a value of its `UpdateAction` enum; nothing from a
  request reaches root. Status goes the other way through `/run/melissa-update/{status,last}.json` (root:melissa
  0640). Apply and rollback reboot, so the UI, API and CLI need an explicit yes. Alternatives: sudoers rule
  (needs setuid sudo, `NoNewPrivileges=yes` blocks it), a root daemon with a socket (more code, a long-running
  root process), running the queen as root (no). `polkit` is added to the image packages. Auto-update stays
  off until the update policy (PLAN 20) exists. *(Superseded 2026-11-19: bootc archived; see the container update entry below.)*
- **2026-11-19 — The update helper runs each `bootc` call as a transient systemd unit** (found testing PLAN
  60e on a bootc VM). bootc needs SELinux `install_t` to write labels the running policy may not know; systemd
  starts `/usr/bin/bootc` (`install_exec_t`) there, a shell script does not (AVC `mac_admin` on bootc's chcon
  fallback). `systemd-run --wait` with output to files in `/run/melissa-update/.work` keeps the fixed-verb
  helper and its error messages. Alternatives: an SELinux module or file-context rule labelling the helper
  `install_exec_t` (needs semanage/policy build in the image), one unit per bootc command with
  `StandardOutput=` and `OnSuccess=` chains (more units, loses the error text in `last.json`). *(Superseded 2026-11-19: bootc archived; see the container update entry below.)*
- **2026-11-19 — `/etc/melissa` belongs to the queen's user** (`tmpfiles.d/melissa-config.conf`, `melissa:melissa
  0750`), because Settings writes `providers.toml` there. systemd's `ConfigurationDirectory=` never chowns.
- **2026-11-19 — Host-first: rootful container and/or Nix service before a reference distro** (owner). The
  laptop returns to stock Fedora Kinoite (the Melissa desktop image was hard to iterate on: every change is an
  image build plus a reboot, and the laptop's disk is LUKS-encrypted, so remote reboots stop at the passphrase).
  Next: make Melissa install and run as a service on an existing Linux host, via a rootful podman container
  (container mode is already verified rootful) and/or a Nix flake with systemd units. The bootc images (headless,
  desktop) are kept but paused; a "reference distro" is built only after host-first works.
- **2026-11-19 — The container never handles power: logind and the sleep units are masked, host input devices
  hidden.** Found on the owner's laptop: the rootful `--privileged` container ran its own systemd-logind, which
  saw the host's lid switch and suspended the HOST on lid close (audit subject `spc_t`), ignoring the host's KDE
  and logind settings. The image masks `systemd-logind` (+ varlink socket), `sleep`/`suspend`/`hibernate`/
  `hybrid-sleep`/`suspend-then-hibernate` targets and the matching services; the image lint fails if they are not
  masked. The Quadlet (and `scripts/container-run.sh`) mounts a tmpfs on `/dev/input`: podman copies the nodes in,
  but the mount is `nodev`, so they cannot be opened (checked: "Permission denied" on `event*`). Verified: lid
  closed on AC no longer suspends; queen, k3s and the UI unaffected.
- **2026-11-19 — Container-first: rootful podman via Quadlet; bootc images archived** (owner). Melissa targets an
  existing Linux host as one rootful `--privileged` podman container with systemd as PID 1, started by
  `/etc/containers/systemd/melissa.container` (`packaging/container/install.sh`). The image is built on
  `registry.fedoraproject.org/fedora:45` (not bootc) with only what Melissa needs; the host provides networking,
  time, SSH and Tailscale. Host-agnostic files live in `packaging/common/` (shared with the Nix packaging,
  `packaging/nix/`). The bootc images, VM tooling and release disk are removed from the repo and kept at tag
  `bootc-archive-2026-11-19` for a later reference distro. **Rootless is unsupported** (it worked with the old
  image, but needs cpuset delegation and pasta, and SELinux denies the distro pasta on the dev laptop); the UI
  forward socket is therefore always on (its nftables filter admits only the bridge gateway). Alternatives: keep
  bootc as the main path (slow iteration: image + reboot per change), Docker (no Quadlet/systemd integration),
  rootless as default (fragile host prerequisites).
- **2026-11-19 — The kubelet's host sysctls are set openly by install.sh.** In a rootful privileged container the
  kubelet writes `vm.overcommit_memory=1`, `vm.panic_on_oom=0`, `kernel.panic=10`, `kernel.panic_on_oops=1`,
  `kernel.keys.root_maxkeys`/`root_maxbytes` on the host (not namespaced; it fails without them). install.sh writes
  them to `/etc/sysctl.d/90-melissa-kubelet.conf`, applies them, prints a notice and saves the old values;
  uninstall.sh removes the file and restores them. `SET_SYSCTLS=no` leaves them to the kubelet. Alternative:
  document only (the change would happen silently at every start).
- **2026-11-19 — Container updates: Quadlet AutoUpdate + a host helper triggered by fixed-name request files**
  (replaces the bootc helper through polkit). The queen can't run the host's podman. It creates an empty file
  named `check`, `apply` or `rollback` in `/run/melissa-update-request` (bind mount of
  `/var/lib/melissa-container/request`, group-writable for `melissa` inside). A host path unit
  (`DirectoryNotEmpty=`) starts `melissa-host-update requests`, which acts on those three names only, never reads
  file contents, empties the directory, and writes `status.json`/`last.json` to a root-owned directory mounted
  read-only at `/run/melissa-update`. Apply pulls (registry images), restarts `melissa.service` and goes back to
  the old image if the queen doesn't come up within 3 min; the image Melissa ran before is tagged
  `localhost/melissa-rollback:previous` whenever the running image changes (also after podman-auto-update), and
  rollback retags it. Automatic updates: Quadlet `AutoUpdate=registry|local` + `podman-auto-update.timer`
  (install.sh enables it if it was off and uninstall.sh turns it off again; `AUTO_UPDATE=off` to skip).
  Restarting the queen stays a polkit-allowed `systemctl restart` inside the container. Alternatives: the podman
  socket bind-mounted in (full root over the host's containers), SSH back to the host, a root daemon with an API
  (more code), only showing host commands (kept as the fallback when the mounts are absent).
- **2026-11-19 — Version info in the container image**: `/usr/lib/melissa/release` has `MELISSA_VERSION`,
  `MELISSA_REVISION` (git short hash) and `MELISSA_RUNTIME=container`; the OCI labels carry version and revision.
  The digest isn't known inside an image at build time: the host helper reports it, and without the helper the
  queen reads the image name and ID from podman's `/run/.containerenv`.
- **2026-11-19 — `podman exec` sessions get `XDG_CONFIG_HOME=/etc`** (Quadlet `Environment=`), so
  `melissa-queen tool ...` run as root inside reads `/etc/melissa/queen.toml` like the service (app links used the
  container's bridge address otherwise).
- **2026-11-19 — The container image ships `iptables-nft`.** k3s's bundled iptables picked the legacy backend in a
  fresh fedora container and needs the `ip_tables` module, which modern hosts don't load: kube-proxy exited.
- **2026-11-19 — Nix service: flake package + NixOS module; the sandbox learns the NixOS layout.** `nix build
  .#melissa` builds the workspace from `Cargo.lock` with nixpkgs' `rustPlatform` (no crane: one package, no new
  input); `nixosModules.melissa` reuses the shared queen unit, tmpfiles, polkit rule, k3s manifests and helper
  scripts (one `root` variable in `packaging/nix/common.nix`) and uses nixpkgs' `services.k3s` (k3s 1.37 from
  nixpkgs, not the pinned binary of `k3s.lock`). Vendor agent CLIs keep coming from `melissa-setup` and run
  through nix-ld (on by default); Nix-built CLIs are an option. The sandbox binds `/nix/store` and
  `/run/current-system` on NixOS and the host's real `/bin`, `/lib64` instead of usr-links. Why: the same
  files and code paths as the images, so a fix lands everywhere; nix-ld instead of `buildFHSEnv` because the
  vendors' self-updating installers write into the home and an FHS wrapper would hide the binary path the
  sandbox preset and egress allowlist depend on. Alternatives: crane (better incremental CI builds; later if
  needed), module-native units (would drift from the images' hardening), binding all of `/` in the sandbox
  (exposes far more than the store).
- **2026-11-19 — Non-NixOS hosts with Nix: a `melissa-host` bundle, not system-manager (prototype).** A root
  Nix profile (`/nix/var/nix/profiles/melissa`) is the GC root and the update/rollback unit; `melissa-host
  install` links the units, sysusers and tmpfiles into `/etc` through it. k3s comes from the distribution
  (on SELinux hosts k3s-selinux labels `/usr/bin/k3s`; a store path would not be labelled). Why: no extra
  tool, idempotent, easy to remove. numtide's system-manager (v1.1.0) officially supports only
  Ubuntu/Debian/NixOS and mocks the firewall; revisit if that changes.
- **2026-11-19 — Nix installs carry a release file; Settings keys on `MELISSA_RUNTIME=nix`.** The Nix package
  ships `lib/melissa/release` (version, revision, `MELISSA_RUNTIME=nix`) like the container's
  `/usr/lib/melissa/release`; the queen reads `$MELISSA_RELEASE_FILE` if set (the module and `melissa-host` set
  it to the store copy), else the `/usr` path. `HostKind::Nix` shows the Nix update commands and no update
  buttons; agents come from `services.melissa.agents.packages`. The node-IP pin moved from `k3s-prepare` into
  the shared `k3s-node-ip`, which the NixOS module also runs. Alternatives: detect NixOS by `/etc/NIXOS` alone
  (misses `melissa-host` on other distributions), a symlink at `/usr/lib/melissa/release` (NixOS has no /usr
  to put it in declaratively).
- **2026-11-19 — Settings → Providers installs agent CLIs through a polkit-limited systemd template unit.**
  `melissa-agent-install@.service` (container image) runs `melissa-setup agent %i --yes --no-login --no-restart`
  as root; its output goes to `/run/melissa-agent-install/<id>.log` (root:melissa 0750 directory). The polkit rule
  (now `51-melissa-agent-install.rules`) lets user `melissa` **start** only `melissa-agent-install@{claude,grok,opencode}.service`
  (exact names; besides restarting the queen). The queen checks the id against the fixed list and the configured
  providers before calling `systemctl start --no-block <fixed unit name>`; nothing from the web becomes an
  argument, and `melissa-setup` checks the name a third time. Progress: `systemctl show` (read-only) and the log
  tail, polled by the Providers region (`live_region`, every 2 s) while an install runs. Every request is audited
  (`settings.provider.install`, also refusals). No login and no queen restart in the unit: the page then shows
  "Installed · after a restart" with the Restart button, and sign-in guidance (API key / token as write-only
  variables; Melissa never handles passwords). Grok's device-code login (`grok login --device-auth`: prints a URL
  and a code, needs no input) runs the same way: `melissa-agent-login@.service` (`melissa-setup login %i
  --no-restart`), polkit allows only `melissa-agent-login@grok.service`; the page shows the URL (only on
  `https://accounts.x.ai/`) and the code from its log. Claude's login wants a code pasted back, so Claude gets
  `ANTHROPIC_API_KEY` or `CLAUDE_CODE_OAUTH_TOKEN` (`claude setup-token` on the owner's computer). The way to install depends on the packaging (see the container-only entry below): the
  container image with the unit and rule present → Install button; `HostKind::Nix` → only the note "This requires you add
  <Provider> via your Nix configuration." (Nix never uses the unit; agent CLIs come from
  `services.melissa.agents.packages`); otherwise a host command (older image) or a note (from source).
  Alternatives: the queen as root (no), a root daemon with an API (more code), a request-file path unit like the
  updates (works, but systemd + polkit already give a fixed, auditable verb), keeping the copy-command only
  (owner asked for a button).
- **2026-11-19 — Echo is not a provider people see.** The built-in echo stays in code (tests, VM/dev checks) but is
  never listed in Settings → Providers or its API (`/api/settings/providers/echo` → 404) and not on the Agent
  tasks page. Echo session backends someone configures in `queen.toml` (other names) still show in the chat's
  agent menu.
- **2026-11-19 — Agent installs from Settings are container-only; one host detection.** Integrating the
  Providers install work with the Nix service: `InstallWay` follows `HostKind` (the Nix service's
  `MELISSA_RUNTIME=nix` through `$MELISSA_RELEASE_FILE`), no second runtime check. The Install button needs
  `HostKind::Container` and the unit and its rule on disk; Nix → the Nix note; anything else → a manual
  command or note. The polkit rule is split: `packaging/common/…/50-melissa.rules` again only lets `melissa`
  restart the queen (shared with Nix), and `packaging/container/rootfs/…/51-melissa-agent-install.rules` adds
  the start rights for the four agent units. The two units and their tmpfiles rule moved to
  `packaging/container/rootfs` too, so the NixOS module and `melissa-host` never install them. Why: packaging
  decides what is installed, not a runtime check inside a shared rule; the Nix side copies only
  `50-melissa.rules` already. Alternative: one rule that checks for a container marker (polkit JS can't read
  files; would rely on a unit's presence).
- **2026-11-19 — App details dialog and the user's force delete.** Each app tile on "Apps and devices" has a
  Details button that loads a native `<dialog>` (opened with `showModal()` from `melissa.js`; Escape, focus
  return and the backdrop are the browser's; a full-screen sheet at phone width). It shows status, owner bee,
  device, link, version/image, pods, restarts, age, the last 200 log lines (escaped text, Refresh button, no
  polling: each read is an audited `app_logs` call) and Stop/Start (the existing tools; stop's confirmation is
  answered inside the dialog, same inbox). **Force delete** is the user's own action, not a tool: no agent
  can reach it (`Queen::user_force_delete` → `CellStore::force_delete`). It needs the app's name typed in
  (`--yes` in the CLI, `{"confirm": "<app>"}` in the API), ignores bee ownership (HIVE-MODEL rule 3), deletes
  the workloads, then the namespace with all data, waits up to 2 min, and after 15 s forces stuck pods
  (grace 0) and clears volume finalizers; a namespace still stuck after that is a clear error (Melissa holds no
  `namespaces/finalize`). Audited `cell.force_delete` (actor `ui`). The owning bee then gets one brokered
  message (tagged as data, `bee.brokered`; queued up to 1 h while it works); the Queen gets a note with her
  next message, never a turn of her own. A reinstall can't happen behind the user's back: bees run
  unattended, so `install_app` asks the user. **RBAC:** logs keep coming through the apps identity, which is
  bound per app namespace and already had `pods/log`; the queen's read-only ClusterRole stays without
  `pods/log` (cluster-wide it would expose system pods' logs). The per-namespace workload role gains `pods:
  delete` (never create) for the stuck-pod step. Alternatives: logs through `melissa-queen` (wider than
  needed), a `force_delete_app` tool (agents must not have it), an htmx-polled log tail (fills the audit log).
- **2026-11-19 — Settings: graphical sign-in, Melissa applies changes herself, much less text.** Owner feedback
  after installing Claude Code from the page ("this part needs to be graphical too", "way less text").
  (1) **Claude signs in on the page**: `melissa-agent-login@claude` (polkit list now claude + grok, still
  start-only) runs `claude auth login`; the page shows its URL (Anthropic hosts only) as a button and one
  "Paste the code" field. The code reaches the CLI through stdin only: the queen writes one line into a FIFO
  (`/run/melissa-agent-install/claude-login.code`, tmpfiles `p` root:melissa 0620 in a root:melissa 0750
  directory, non-blocking open), `melissa-setup` reads it in its own shell and passes it on a private pipe.
  Never an argument, environment variable, log or audit line. Success is `claude auth status` (exit 0), not
  text; wrong code / cancel / timeout are exit statuses 3/4/5. Verified against the real CLI 2.1.x without an
  account: URL printed, a fake code → "That code wasn't accepted" and exit 3; stdin EOF alone does not stop
  it, so cancel kills it. Alternatives: a socket (more code for one line), `claude setup-token` on another
  computer (a terminal), a PTY bridge (fragile). (2) **No manual restart**: an install or sign-in is picked up
  live (backends look up a missing program again, `ProgramPath`; logins were already checked live). Saved
  settings (API key, program, enable) still load at start, so Melissa restarts herself once idle (no chat
  turn, helper job, agent task or install/sign-in unit running; 4 s quiet), audited `settings.apply`; the page
  says "Ready after the current task". Re-reading `providers.toml` live was rejected for now: every backend's
  sandbox spec would need rebuilding under running sessions. (3) **Less text**: one status word, at most one
  primary action, one short sentence per section; internals behind "Details"; API keys behind "Use an API key
  instead" with the variable name chosen by the server; no host commands in the container UI or the chat's
  hints (only for runs from source); Updates and Developer shortened the same way. (4) Installs and sign-ins
  are audited with their outcome, not only the request.
- **2026-11-19 — Generic role names instead of bee terms** (owner decision): cluster (was hive), device (comb),
  primary device (main comb), lead agent (queen; `lead` in code, e.g. `Scope::Lead`), agent (bee; `agent` in
  code, `AgentId`-style names where the backend CLI is meant they say `backend`), workspace (cell), fleet
  (apiary). Melissa stays the product name, with the `melissa-` prefix. Why: contributors were confused by the
  metaphor; pre-alpha, so renaming is cheapest now. Renamed everywhere: crates (`melissa-server`,
  `melissa-device`, `melissa-cluster`), unit `melissa-server.service`, ServiceAccount `melissa-server`, prompts
  `lead.md`/`agent.md`, tools (`create_agent`, `give_agent_job`, `agent_status`, `list_agents`, `start_agent`,
  `stop_agent`, `message_agent`, `reassign_workspace`, `device_info`), routes (`/api/lead`, `/api/agents`,
  `/api/workspaces`), CLI (`chat --agent`, `workspace reassign`), labels (`melissa.io/workspace`,
  `melissa.io/device`, `melissa.io/owner-agent`), ids (`d-<hex>`, `<device>.a<n>`), audit kinds (`agent.*`,
  `workspace.*`), config `server.toml` `[lead]`. The UI's honeycomb became an app grid (`app_grid`,
  `app_tile`). **Migration** for existing installs, all idempotent: on start the daemon moves its state
  (`comb.json` → `device.json`, `bees/` → `agents/`, `queen/` → `lead/`, work dirs, ids, transcript event,
  field and tool names; `melissa_agent::migrate`) and `queen.toml` → `server.toml`; `[queen]` and the old
  `bee_*` keys and tool names are still read for one release, with a warning. Old namespace label keys
  (`melissa.io/cell`, `melissa.io/comb`, `melissa.io/owner-bee`) are read as aliases **and** moved once through
  the apps identity; the admission policy allows exactly that move (old keys only kept or dropped, the new
  device/owner must be the old value in the new id shape, no new `assigned-at` needed for it). Both, because
  reading aliases alone would keep the old keys forever, and relabelling alone would leave ownership unknown
  while k3s is still starting. The audit log keeps old lines as they are (history). Compatibility for one
  release: `Alias=melissa-queen.service` (host update helpers installed earlier wait for that name), the old
  feed words for `comb_info`/`bee_info` in old chats. Alternatives: keep the metaphor and only explain it
  (rejected by the owner); aliases only, no migration (two names forever).
- **2026-11-19 — No app catalog: agents deploy any app from a spec they write** (owner decision: "Abandon
  the idea of an app catalog. Melissa must be extremely flexible."). Trigger: asked "Set up Minecraft server",
  the lead agent refused because only Nextcloud was in the catalog. Now agents choose images and settings for
  whatever the user asks for. **Format:** Melissa's own small JSON app spec (`crates/appspec`, was
  `crates/catalog`), not a subset of Kubernetes YAML: containers (image, command/args, env with plain values,
  `{secret}` or `{builtin}`, small read-only files), ports (`tcp`/`udp`, `public`, one `web` port), volumes,
  generated secrets, resources (limits with defaults), probes (HTTP or TCP), sidecars in the same pod. Why not
  YAML: an LLM writes this one shape reliably without Kubernetes boilerplate; there are no
  privileged/hostPath/hostNetwork knobs to strip or forget (unknown keys are refused, not ignored); the
  confirmation can say exactly what runs; it is stored on the namespace (`melissa.io/spec`) so `update_app`
  edits a known shape. Alternatives: a validated K8s subset (bigger surface, every field a policy question),
  Compose (another format to translate, port/volume semantics differ), Helm (templating, no). **Tools:**
  `deploy_app {app, spec, agent?}` and `update_app {app, spec}` replace `list_catalog`/`install_app`; both are
  `Effect::Deploy`, **always confirmed** (also for the attended lead agent; was: an attended untainted
  `install_app` ran without asking), with a summary of images, public ports and storage; the spec is checked
  before anyone is asked (`Tool::check_args`). Stop/start/uninstall/status/logs/list unchanged in use;
  `list_apps` now lists app namespaces (the apps identity gained `list` on namespaces: names and labels only).
  **Safety is server-side:** pinned images only (a version tag other than `latest`, or a digest; Docker Hub
  short names expanded); the renderer only makes a single-replica Deployment, Services, PVCs, ConfigMaps,
  per-field Secrets and an Ingress in the app's own namespace, every container `privileged: false`,
  `allowPrivilegeEscalation: false`, limits always set (default 1 CPU / 512Mi), seccomp RuntimeDefault, no SA
  token. Root inside a container is allowed by default (many official images start as root and drop
  privileges; refusing it would break most of them), privileges never; `run_as_user`/`run_as_group` when an
  image supports it. New admission policies hold the same independent of the server: `melissa-apps-workloads`
  (workspace namespace with its labels, no host network/PID/IPC, only PVC/ConfigMap/Secret/emptyDir volumes,
  no init/ephemeral containers, not privileged, added capabilities only from baseline's default set, no host
  ports, CPU and memory limits, seccomp RuntimeDefault, no API token) and `melissa-apps-volumes` (local-path, no
  `volumeName`/`dataSource`); PSA baseline stays enforced per namespace. It does not require
  `allowPrivilegeEscalation: false`, because stop/start patches of Deployments rendered by older versions must
  still pass. **Ports:** the web port and each `public` port (TCP or UDP) get their own NodePort from
  30000-30079, allocated by the server (`melissa.io/port` for the web port, unchanged and still immutable;
  `melissa.io/ports` = `name:port/proto` for the others, kept across updates); the `<app>-port` Service may
  carry up to 6 TCP/UDP ports in the range. The container publishes the range for UDP too. Tools return
  `ports` with `address` and protocol; the UI shows `host:port (TCP)` for non-web ports. **Images from any
  public registry** are allowed (pre-alpha). Risk, accepted for now: a tag can be re-pushed and nothing checks
  provenance (no digest resolution, no signatures), a malicious image runs with the limits above but with
  network access to the internet and other apps (no NetworkPolicy yet); the user sees the image names before
  approving. Open: PLAN.md question 64. **Migration:** apps installed from the catalog (Nextcloud) keep
  running untouched; at startup the server stores their spec (`appspec::legacy`, rebuilt from the
  Deployment's `melissa.io/config`) and records that their password lives in the shared `<app>-secrets`
  (`melissa.io/shared-secrets`); the rebuilt spec renders the same volume, Secret, files and port, so
  stop/start/update/uninstall keep working and keep the data and password. `[apps] catalog` and the Nix
  option `apps.catalog` are accepted and ignored with a warning; `manifests/` and `/usr/share/melissa/catalog`
  are gone.
- **2026-11-20 — Agents may run commands in their apps' containers (`app_exec`).** An agent that deployed a
  Minecraft server was asked to "give him op" and could only read logs or change settings (a restart). The
  owner wants agents to send commands to the containers they control. `app_exec {app, command, container?,
  stdin?, timeout_secs?}` runs one argv (no implicit shell) in the newest running pod over the Kubernetes exec
  API (kube-rs `ws` feature; adds tokio-tungstenite). Scope `Change` (owning agent or lead agent; others
  refused, never asked). Labels: Destructive (always confirmed, the card shows the exact argv; same rule as
  agents' own arbitrary commands), reads private and untrusted data (taints the task). Bounded: 64 KiB per
  output stream (read to the end, the rest dropped), stdin ≤ 64 KiB, timeout 30 s default, 300 s max.
  Audit: the `tool.call` line names who, app, argv, stdin length (never its content), pod, container and
  exit code through a new `Tool::audit_detail` hook. RBAC: `pods/exec` `get`+`create` in the per-namespace
  workload role only (WebSocket exec is a GET, and the API server also checks `create` for the upgrade);
  no attach or port-forward, nothing cluster-wide. Trade-off: a command can read the container's
  environment, so generated secrets are reachable by an approved command (PLAN.md open question 65).
- **2026-11-20 — Standing permissions: "Always allow" rules and No permission prompts** (owner-approved, the owner
  accepts the risk). A rule is exactly (asking agent id or the lead agent, tool, target workspace); it answers
  matching confirmations without a prompt, audited with its id. The developer bypass approves every
  confirmation, audited as `bypass`, behind a confirm step and with a warning on every page. Both are checked
  after the scope check, so refusals stay refusals, and only the owner's UI/API changes them. Why: repeated
  prompts for the same thing from the same agent train people to click yes without reading; a narrow standing
  yes is better than a habit. Alternatives: per-argument rules (too narrow to be useful for spec updates),
  per-effect or per-agent blanket rules (too broad), rules with expiry (later, open question 67).
- **2026-11-20 — Scheduled jobs (cron) run unattended in the agent's own conversation** (owner request). A
  schedule is (name, agent id or the lead agent, instruction, 5-field cron in the box's local time, on/off, time
  limit). The scheduler sends the instruction as a labelled turn ("Scheduled: <name>") and marks the session
  unattended for that turn, so the owner's "Always allow" rules decide what it may do alone. Overlap: skip and
  record, never queue. Missed while down: run once if less than an hour late, else record "missed". Time limit:
  cancel and record "timed out" (default 60 min). Agents propose schedules with `schedule_job` (only for
  themselves; the lead agent for any agent); each create/remove needs the owner's yes, never coverable by
  "Always allow" rules but answered by the developer bypass like every confirmation. Why: recurring maintenance
  is what people want from an always-on box, and the owner's standing permissions are the one place that says
  what may happen unwatched. A schedule is itself a standing order, so letting a rule approve new ones would turn
  one yes into unlimited unattended work. Alternatives: queue on overlap (starts at times nobody chose, piles up
  behind a stuck run), catch up every missed run (a week offline would replay a week of jobs), a separate
  "scheduled" conversation per job (loses the agent's memory and the owner's one place to look), letting rules
  cover `schedule_job` (see above), excluding it from the bypass (the bypass would no longer mean "approve
  everything"). Cron crate: croner (maintained, standard Vixie/OCPS semantics, DST-aware, jiff support; jiff is
  already in the build).
- **2026-11-20 — Voice input: speech to text on the box with whisper.cpp's CLI, model downloaded on demand.**
  Owner decision: a mic in the chat composer, private and offline on the LAN; not the browser's Web Speech
  API (it sends audio to Google/Microsoft); a one-time model download of about 150 MB when the owner turns it
  on is accepted. Settings → Voice has the toggle (`voice_input` in `ui.toml`, off by default).
  **Engine: whisper.cpp's `whisper-cli` as a separate process per clip, not linked in (whisper-rs).**
  whisper-rs builds whisper.cpp through cmake and bindgen (libclang) inside our cargo build, in the container
  build stage and in the Nix package, puts C++ and the model's memory into the long-running server, and a crash
  there takes the UI with it. A separate program keeps `melissa-server` lean and pure Rust (`forbid(unsafe_code)`
  untouched, no FFI), frees the model's ~200 MB after each run, isolates crashes, and is killed on timeout.
  Cost: ~0.2 s model load per clip, negligible next to the run. Packaging: the container image builds the CLI
  in its own stage from the pinned v1.9.5 tarball (`packaging/container/whisper.lock`, sha256), CPU only with
  `GGML_CPU_ALL_VARIANTS` (best of SSE4.2 … AVX-512 picked at runtime, so the image runs on any x86-64), no
  SDL/ffmpeg/curl, 22 MB in `/usr/lib/melissa/whisper/`; Fedora's `whisper-cpp` RPM was rejected (library only,
  no CLI, and it pulls ROCm). Nix: `services.melissa.voice.package`, default nixpkgs' whisper-cpp without SDL and
  ffmpeg (231 MiB closure instead of 1.1 GiB; prebuilt in the binary cache), passed as `MELISSA_WHISPER_CLI`.
  **Model: multilingual `base` (`ggml-base.bin`, 148 MB), pinned by URL revision, size and SHA-256.** Fits the
  accepted budget; on a 16-core x86 box it transcribes faster than real time with 8 threads (11 s of speech in
  1.9 s measured on the dev laptop), so a 2-minute clip takes seconds. `small` (488 MB) is more accurate but over
  budget and ~3x slower; `small-q5_1` (190 MB) is the upgrade candidate (open question 69). Language is
  auto-detected. Download: hyper + rustls (already in the build through kube-rs, no new crates), https only,
  redirects followed (Hugging Face → its CDN), streamed to `models/ggml-base.bin.part` under
  `$XDG_DATA_HOME/melissa` (`/var/lib/melissa` in the service) while hashing; anything but the exact size and
  hash is deleted. Audited: toggle, download start (URL, sha256) and outcome, deletion.
  **Audio path:** the browser records (MediaRecorder), decodes its own recording and renders it at 16 kHz mono
  (`OfflineAudioContext`), and posts a WAV to `POST /ui/voice/transcribe`. No Opus/AAC decoder on the server
  (no pure-Rust Opus decoder is mature; ffmpeg avoided); the server parses the WAV in safe Rust, downmixes and
  resamples anything 8-48 kHz, and writes a fresh canonical file for whisper-cli, so the C++ parser never sees
  bytes from the network. Limits: 12 MiB body, 2 minutes, one clip at a time with 3 waiting (429 beyond), 120 s
  per run (504). The text goes into the message box for the owner to edit; nothing is sent automatically.
  Clips are never stored: in memory, plus one 0600 temp file in `/run/melissa/voice` (tmpfs) removed when the
  run ends; the log records lengths only. Alternatives: Web Speech API (cloud, rejected by the owner);
  whisper-rs (above); whisper.cpp's HTTP server as a resident service (keeps the model in memory all the time
  for little gain); candle (pure Rust, but much slower on CPU and a heavy build).
- **2026-11-20 — App addresses must match where app ports are published; checked at install and at run time.**
  Incident: a container install with `BIND=127.0.0.1` and the Tailscale address in `[apps] addresses` (written once,
  never rewritten): every app link was "connection refused", and an agent restarted a healthy Minecraft server over
  its normal "Server empty for 60 seconds, pausing" log line. Now install.sh's preflight **refuses** an
  `APP_ADDRESSES` entry `BIND` does not cover (override: `ALLOW_ADDRESS_MISMATCH=yes`, for addresses something else
  forwards), because a warning scrolls past and the dead links only show up when someone's client fails; on a
  reinstall it rewrites just the `[apps] addresses` key when it disagrees with an explicit `APP_ADDRESSES` or names
  uncovered addresses. The app tools (`deploy_app`, `update_app`, `start_app`, `app_status`) TCP-connect to each
  advertised address plus the NodePort on the device's own address (800 ms, cached 15 s); an address that fails while
  the control answers is dropped from links and reported under `reachability` for the owner. UDP and loopback are not
  checked (said so). Agent prompts: verify reachability before blaming an app, never restart for a connection problem
  without evidence, normal "pausing when empty" logs are not failures. Alternatives: warn only in install.sh (missed
  the incident's silent case), rewrite server.toml at every container start (would override hand edits), probe from
  outside the device (not possible from Melissa itself).
- **2026-11-20 — Every agent has its own file box; files are kept 5 days by default** (owner decision; replaces
  the shared attachment store). A box is `<state>/files/boxes/<lead|agent id>/`, bound read-write at the same
  path into that agent's sandbox only (`SandboxPlan::with_rw_dirs`, after the workdir); the agent is told the path
  in its system prompt. Owner uploads land there (attachment limits, sniffed type, cleaned name, unique in the
  box), and the agent saves what it wants to keep or hand over there, with its own tools or `my_files` (an MCP
  tool for agents without file tools; only the caller's own box, by task scope; labelled read-only like the
  sandbox's own writes). **Why a separate bind and not `<workdir>/files`:** the box directory itself is then
  Melissa's (a mount point the agent can't rename or replace with a symlink), so Melissa never acts on a path
  the agent controls; it reads, deletes and prunes only regular files directly in the box, opening them with
  `openat(box dir, name, O_NOFOLLOW)` and serving them by a generated id from an index kept outside every
  sandbox (`<state>/files/index/`). The same path inside and outside the sandbox keeps it right for agents that
  run unsandboxed. **Retention:** Settings → Files sets the default (5 days; 1–3650 days or Forever), each box
  may override it from its chat's file panel; an hourly prune removes files older than that since Melissa first
  saw them (audited `files.pruned`, one line per box); messages that sent a removed file show it as expired.
  Deleting from the panel or API is audited `files.deleted`. **Caps:** 2 GiB of owner uploads per box; agent
  files are counted but not limited (open question 70). **Migration:** at startup the old
  `<state>/lead/attachments` is copied once into the boxes (by which chat's transcript sent each file, else
  Melissa's), keeping each file's id so earlier messages still find it, counted as added at migration time;
  then the old directory is renamed `attachments.migrated` (nothing deleted). **UI:** a new component, the file
  panel (`components::file_panel`): a collapsed `details` beside the chat (a section above it below 1280 px),
  open/closed remembered in localStorage, live over the chat's SSE stream (`files_changed`, memory only).
  Alternatives: keep the shared store with a per-conversation index (no isolation in the sandbox; an agent
  could not save files for the owner); the box inside the workdir (simplest bind, but the agent controls the
  directory itself, see above); per-file retention (more UI for little gain).
- **2026-11-20 — A scheduled run's confirmation waits the owner's setting (default 10 minutes)** (owner
  decision). Settings → Schedules "When a run needs your OK" (1–1440 minutes, `approval_wait_minutes` in
  `schedules.json`, audited `schedule.settings`). The scheduler hands the wait to the run's turn, which puts it
  in its MCP session's policy context (`TaskContext::confirm_wait`) for the length of the turn; the registry and
  the agent's own permission requests wait that long, then withdraw the request and fail the call as before
  (`tool.confirm_timeout`). Interactive turns keep `confirm_timeout_secs`. Why: unattended runs should not hold a
  tool call open for the general timeout when nobody is there, and the owner wants to choose. Caveat: Claude Code
  abandons an MCP call after about 300 s (open question 68c).
- **2026-11-20 — Run-time app address check: the container's publish bind decides, a probe never drops a link.**
  The run-time probe above was wrong in the container: from inside it, a TCP connect to the host's published address
  (podman bridge, netavark nftables DNAT) fails, no hairpin, while the same connect from the host and from other
  tailnet machines works; it would have stripped working links. Now melissa.container sets
  `MELISSA_PUBLISH_BIND=<BIND>` (install.sh; `0.0.0.0` for an empty BIND, empty with `ALLOW_ADDRESS_MISMATCH=yes`)
  and the server unit gets it via `PassEnvironment` (systemd services don't inherit the container's environment;
  checked). With it, addresses are not probed: one the bind provably doesn't cover (loopback bind, other address; one
  address bind, another) is dropped with the owner warning, a covered one is kept, a host name is kept as
  `unverified`. Without it (Nix/host install, where k3s serves NodePorts on every host address; or a container not
  reinstalled yet) addresses are still probed, but a failure is only an `unverified` note, links kept. The control
  connect (NodePort on the device's own address) still tells "app not answering". Env var rather than a server.toml
  key: it comes from the same line that publishes the ports (can't drift), and an older image after a rollback
  ignores it, whereas `[apps]` rejects unknown keys and the server wouldn't start.
- **2026-11-20 — A turn's confirmations end with the turn; scheduled waits stop before the agent's own limit**
  (owner-approved direction, resolves 68c). When any turn ends (finished, cancelled, the schedule's time limit,
  an error), its conversation's confirmations that still wait are withdrawn (audited `confirmation.withdrawn`,
  actor `melissa`, "the run ended"; the waiting call gets `tool.withdrawn` and fails; the card shows "No
  longer needed"). This applies to attended turns too: every confirmation of a conversation's task comes from
  its agent's session, which asks only during a turn, so none legitimately outlives it. A scheduled run's wait
  is the owner's setting but at most its backend's own tool-call limit less a minute
  (`SessionBackend::tool_call_limit`; Claude Code 300 s, so 4 minutes); Settings → Schedules says so under the
  wait when a usable Claude backend is configured and the setting is longer. Why: an approval after the agent
  stopped waiting would run an action for a run that moved on. Alternatives: cap only (leaves stale requests
  from cancelled or failed turns); withdraw only (a Claude-backed run would still lose the call after 300 s
  while the owner thinks it waits 10 minutes).
- **2026-11-20 — An agent never touches another agent's app on its own; the user may allow one call** (owner
  request). New read-only tool `my_apps` (named like `my_files` and the `*_app(s)` tools): an agent's own apps
  with the essentials (phase, image, url, ports, reachability as `app_status` reports them, `assigned_by`/`at`)
  and, separately, the other apps by name and owner only; the lead agent gets every app grouped by owner. It is
  in every agent's tool list. The scope check now covers **reads too**: `app_status` (Read) and `app_logs`
  (ReadScoped) on another agent's app are no longer open to agents, and `list_apps` shows an agent the others by
  name and owner only. Why reads: the owner said "touching", and a status or log read of the wrong app is exactly
  how an agent ends up acting on it (two Minecraft servers); `my_apps` gives the only cross-owner read an agent
  needs (which apps exist and whose they are). Instead of the old hard refusal, an agent's call on another
  agent's (or an unassigned) app becomes a **user-only confirmation** ("Ada wants to run commands in Bo's
  `minecraft` (not Ada's). Only you can allow this, and only this once"): no "Always allow" offered, never
  matched by a rule, and **not answered by No permission prompts** (neither at request time nor when the bypass
  is switched on). Why not the bypass: ownership is a safety boundary between agents, not a convenience prompt;
  the developer switch exists to stop prompt fatigue on an agent's own work, and letting it silently open every
  agent's apps to every other agent would erase the boundary for anyone who turns it on. A yes covers that one
  call (nothing is remembered). The lead agent keeps acting on any app of its device (destructive still asks)
  and reassigns with the user's yes; reassignment flips access at once because the owner is read at every call.
  Audit: `tool.cross_owner` names both sides at each step (`asked`, `approved by the user (this call only)`,
  `denied by the user`, `not approved (…)`, `refused`, and the lead agent's `allowed (lead agent)` override).
  Alternatives: keep the hard refusal and make the agent ask the user in chat (no way to authorize a single
  call); a `cross_owner: true` argument on every tool (the agent decides when to ask, more surface); allow reads
  of others' status (an agent would keep using them to guess which app is its own).
- **2026-11-20 — Agents read the open web, read-only, through Melissa's egress proxy (owner decision).** Any public
  page may be fetched, every fetch is audited, and reading taints the task (the existing untrusted-content rule:
  afterwards its changes need the owner's OK; further reads do not, see below). Server config `[egress]
  web_read = true` (default); `false` restores allowlist-only egress. Design: a "web read" class in the proxy
  next to each backend's API allowlist (which keeps its own limits). HTTPS: `CONNECT` to any host on port 443
  whose addresses, resolved by the proxy, are public; the proxy connects only to the addresses it checked (no
  second lookup, so DNS rebinding cannot reach the LAN); loopback, RFC 1918 (incl. k3s 10.42/16 and 10.43/16),
  link-local/metadata, CGNAT/tailnet, ULA, IPv4 hidden in IPv6, and local names (`localhost`, single labels,
  `.local`, `.lan`, `.internal`, `.home.arpa`, `.ts.net`) are refused. HTTP: plain forward requests to port
  80, `GET`/`HEAD` only, no body (`Content-Length` > 0, `Transfer-Encoding`, `Expect` refused), rewritten to
  origin form with `Connection: close`; nothing the client sends after the head is relayed. Inside a TLS tunnel
  methods are invisible, so read-only is approximated by caps per web tunnel: 64 KiB upload (a TLS handshake
  plus request headers is ~2 KiB; HTTP/2 window updates for a 32 MiB download stay well under 16 KiB), 32 MiB
  download, 120 s lifetime, 30 s idle; the tunnel closes at a cap and the audit says which. Each CLI's known
  background hosts (Mixpanel, grok.com, Datadog intake, the npm registry; `egress::default_deny`) stay denied:
  they would otherwise pass as web reads and taint every task at start. Audit: `egress.allow host:443 -> addr
  (web read)` at open, `egress.web CONNECT host:443: N B up, M B down, T s[; closed: why]` (plain HTTP: `GET
  http://host/path`) at close; refusals as `egress.deny` with the reason, and the 403 body says `blocked by
  Melissa's network policy: host:port: reason`. Taint: the proxy taints the sandbox's MCP sessions when a web
  tunnel opens (source `web`, the host); the permission layer also taints on every allowed fetch (covers
  server-side search tools whose content never passes the proxy). Fetch permissions: the backend tells the
  handler how its agent reaches the web (`WebAccess`); a fetch to a private address, a local name, another
  port, with web reading off or with no network is refused with that reason (audited) instead of failing later
  inside the CLI; behind the proxy a fetch is labelled untrusted but not egress, so reading on in a tainted
  task needs no OK (the proxy, not the policy, keeps it read-only); with the host network the old labels apply.
  Grok: its `web_fetch` ignores `HTTPS_PROXY` (verified, grok 1.0.89), so the sandbox sets
  `GROK_WEB_FETCH_PROXY`; it also resolves the name itself first (an SSRF check in a static musl binary:
  `/etc/resolv.conf` and UDP 53, no NSS) and failed with "DNS resolution failed". Fix: a DNS sinkhole per
  sandbox that answers every A query with one placeholder, 192.88.99.53, and every other query with no
  records; nothing is forwarded, so DNS is not a channel out. The placeholder must look public (Grok refused
  203.0.113.53 as "private/internal"); 192.88.99.0/24 is the deprecated 6to4 relay anycast prefix (RFC 7526):
  no legitimate destination, no route from the sandbox, and the proxy treats it as non-public. Port 53 cannot
  be bound inside: bwrap's `--dev` puts the sandbox in a nested user namespace where no capability counts for
  its network namespace (verified: `--cap-add CAP_NET_BIND_SERVICE` binds without `--dev`, not with it). The
  lead agent owns the outer namespace, so `melissa-netbridge` creates an unbound UDP socket inside, hands it to
  the lead agent over `dns.sock` (`SCM_RIGHTS`, safe rustix), and the lead agent binds it to 127.0.0.1:53 and
  answers on it; the lease's `resolv.conf` (`nameserver 127.0.0.1`) is bound over the host's (or over its link
  target under `/run`). One hand-over per sandbox. Verified live: grok-acp in the container fetches
  https://example.com, github.com (redirects) and http pages; opencode-acp on the laptop (Bun honours
  `HTTPS_PROXY`) fetches https and plain http; a name resolving to 10.0.0.1 and private literals are refused.
  Residual risk: a hijacked agent can still carry data out in URLs, query strings and request headers (up to the
  upload cap per tunnel, any number of tunnels) to any public host it chooses; the proxy cannot see inside TLS.
  Mitigations: taint (its changes need the owner), the audit of every host and byte count, no request bodies
  over plain HTTP, the upload cap, no private destinations, no DNS channel; per-task budgets or a
  destination list for tainted tasks are open (PLAN open question 73). Web reads are also open to any process
  in the sandbox (e.g. an approved shell command's `curl`), not only to fetch tools. Alternatives: an allowlist
  of sites (rejected by the owner: open web wanted), TLS interception to see methods and bodies (CA per CLI,
  sees secrets), a permission-time `/etc/hosts` entry for the fetched host instead of DNS (breaks cross-host
  redirects), a fake systemd-resolved varlink socket (works for glibc NSS only; Grok is musl), dropping
  `--dev` so the bridge can bind port 53 (Grok uses ptys: `portable-pty`).
- **2026-11-20 — Each chat gets a details panel; files move between boxes only by the owner or the lead agent
  with the owner's yes** (owner request; replaces the collapsed file panel). **Panel:** the right side of a chat
  page is about the agent whose chat it is (Melissa on home, a helper on its page): name (rename in place,
  helpers only), role, what it runs on (Change opens the chat's model picker), status (live), who made it and
  when, Delete for a helper (in-page confirmation; not offered, with the reason, while it looks after apps);
  its apps (Melissa: every app with who looks after it, grouped by that; each opens the app dialog; polls every
  15 s like the app strip); its schedules (on/off in place, link to Settings → Schedules); its file box (list,
  add files from the panel, download, delete with in-page confirmation, "Reference in chat", "Send to…",
  "Keep files for…"). Components `details_panel`, `agent_card`, `detail_apps`, `detail_schedules`,
  `details_toggle`; the file panel's body is its Files section. **Layout:** at 1200 px and wider a column beside
  the chat; the Details button hides or shows it. Narrower, with script, a sheet over the chat: Details opens
  it, Close, Esc or a click outside shut it, focus moves into it and back to the button, the chat and rail are
  `inert` while it is open. Open/closed is remembered per browser and per width (localStorage, wrapped in
  try/catch; default open when wide, closed when narrow). Without script it is always shown (beside the chat, or
  a section after it on narrow screens) and every action is a plain form post that redirects back. **Reference
  in chat:** inserts `[file box: <name>]` at the cursor (the agent's prompt says what it means); without script
  the link opens the chat with that text as the draft (`?draft=`, now also on helper pages). Chosen over an
  attachment chip that resends the stored file: the file is already in the agent's box, so a name is all the
  agent needs (it reads the file with its tools or `my_files`), it costs no message plumbing and no second copy,
  and it reads plainly in the transcript; the chip would only add inlining images/PDFs into the prompt, which
  the owner can still get by attaching the file. **Transfers** (`FileBoxes::transfer`): copy by default (both
  keep it; moving is a checkbox, "Move (remove it here)"), because a copy can't lose anything and the owner
  can delete the original; a move removes the original only after the copy is written. The new file has a new
  id, counts as added now (the receiving box's retention starts again, so a file isn't pruned the moment it
  arrives), is shown "From <agent>" (`AddedBy::Sent`, `from` = the box it came from), takes a free name
  (`report-2.md`), and must fit the receiving box (2 GiB, 1000 files: `FileBoxError::Full`, nothing changes).
  The source is opened like a served file (by id, `O_NOFOLLOW`), never through a link the agent planted.
  Audited `files.transferred` (actor `ui` or `lead`, from, to, file, size, copy or move). **Who:** the owner
  (panel, `POST /api/files/{key}/{id}/transfer`) or the lead agent with the new lead-only tool `transfer_file
  {from_agent, file, to_agent, move?}` (`Melissa` names its own box), which always asks the owner naming the
  file and both boxes (`Tool::owner_approval`: no "Always allow" rule covers it). No permission prompts may
  answer it, unlike an agent's call on another agent's app: the lead agent already acts on every workspace of
  its device, so a transfer it asks for is within its authority, and the developer switch is the owner's own
  choice. Helpers never see the tool, and their sandboxes still bind only their own box: no agent reads another
  agent's box. Alternatives: move by default (one click loses the sender's copy); keep the original added time
  (an old file would be pruned on arrival); a helper-callable "send to" tool (breaks box isolation); a
  user-only confirmation like cross-owner app calls (the lead agent is the one asking, see above).
- **2026-11-20 — Model options come from the agents; per-agent Full access is a scoped bypass** (owner request).
  The model picker shows an option group only when the chosen model reports it: reasoning levels (as before),
  Speed (Standard / Fast) from Claude's `supportsFastMode` (set with `--settings {"fastMode":true}`) or an ACP
  option of category `speed`; nothing is inferred from model names, so a fast model shows no toggle. Unknown ACP
  options (OpenCode's `mode`) are ignored rather than rendered generically: they change what the agent does, and a
  generic control would be UI an agent picked, not a reviewed component. Access (Ask / Full access) shows only
  with the new Developer option "Full access per agent" (a sibling of No permission prompts, not a change to it:
  the global switch keeps its meaning, the new one only reveals a per-agent control). Full access approves the
  agent's own confirmations (its CLI's tools, Melissa's tools on its own or unassigned workspaces) without
  asking, audited `tool.auto_approved` actor `bypass` ("bypass (full access)"); it never answers user-only
  confirmations (another agent's workspace), owner-only steps (`owner_approval` such as schedules,
  `Effect::Ownership` such as moving workspaces, `Tool::owner_only` such as deleting agents) or the lead
  agent's calls on its agents' workspaces, and refusals stay refusals. It is stored with the rules
  (permissions file), changed only by the owner (picker with an in-page confirm, or API with `confirm`),
  audited `permissions.access`; switching the option off puts everyone back on Ask. No multi-agent "ultra"
  mode: no backend exposes one. Alternatives: hardcoded per-model option tables (go stale, and the owner
  asked for discovery), Full access as an agent-registry field (would split standing permissions across two
  stores), turning No permission prompts into the per-agent switch (would change what an existing setting
  means).
- **2026-11-20 — Provider logos only where the brand allows it; letter marks for the rest** (owner request:
  real logos instead of letter marks). Melissa ships small one-colour SVG marks in `crates/web/assets/logos/`
  from icon sets made for this: **Simple Icons** 16.38.0 (CC0 1.0; npm `simple-icons`, sha512 integrity
  `B0Wws90e…FhiWSQ==`) for OpenCode, Cursor and Cline, and **LobeHub Icons** 1.104.0 (MIT, © 2023 LobeHub; npm
  `@lobehub/icons-static-svg`) for Grok and Goose. T3 Code, our reference, inlines its marks as path data in
  its own components (some credited to Simple Icons, CC0); Melissa does the same through its own allowlist:
  `crates/web/src/logos.rs` parses each file (one `<svg>` with `xmlns` and `viewBox`, a `<title>`, `<path d
  fill-rule>` only; no script, `foreignObject`, `href`, `style`, events, entities or comments) and the component
  draws the paths itself, escaped; a file that fails is dropped (letter mark) and a test fails. No remote icons
  (the ACP registry's `icon` URLs are never loaded). Drawn in pure black, white in dark mode (the brands'
  one-colour variants), never recoloured to brand-unlike colours. The icon licences cover the drawings, not the
  trademarks: the logos are trademarks of their owners, shown only to identify which agent a model belongs to
  (nominative use), with no endorsement implied. Brand guidelines read on 2026-11-20 (public pages, not legal
  advice): **Anthropic/Claude** (anthropic.com/legal/trademark-guidelines; code.claude.com legal page) — logo use
  only as permitted in approved materials, no recolouring; third parties may say in plain text that they run
  Claude Code → **letter mark**. **OpenAI/Codex** (openai.com/brand) — logo only exactly as provided, no added
  colours, "don't use the logo without permission" → **letter mark**. **Google Gemini/Gemini CLI** (Google brand
  resource centre; geminicli.com/brand-kit) — prefer plain text, don't alter icons (gradient) → **letter
  mark**. **GitHub Copilot** (github.com/logos) — prior written permission → **letter mark**. **Moonshot Kimi**
  (kimi.com logo usage terms) — non-commercial use only, no recolouring → **letter mark**. **Mistral**
  (mistral.ai/brand) — welcome to use, prefers the gradient, no modifications → **letter mark** (we draw one
  colour). **xAI/Grok** (x.ai/legal/brand-guidelines) — marks may be used to accurately refer to xAI's services,
  unaltered, no endorsement → **logo** (its one-colour mark, black/white). **OpenCode** (opencode.ai/brand) —
  one-colour variant offered; don't recolour, no endorsement → **logo**. **Cursor** (cursor.com/brand), **Cline**
  (cline.bot/brand), **Goose** (Linux Foundation project trademark policy: no implied endorsement) — asset kits
  without written restrictions → **logo**, one colour, identification only. Amp, Qwen, Augment, Factory: no logo
  shipped (Simple Icons' `amp` is a different project; Qwen's mark is purple, and one colour would alter it).
  Unknown providers keep the letter mark. Alternatives: logos for everyone (conflicts with the guidelines above),
  the brands' own asset files (heavier, multi-colour, and several forbid any change in size context), T3 Code's
  inline drawings (owner: reference for backends only; sources not stated per icon).
- **2026-11-20 — Add provider: built-ins, the public ACP registry (on request), or a local ACP command; Remove
  provider uninstalls what Melissa installed** (owner request, twice for Remove). Modelled on T3 Code's add-provider
  flow and ACP registry support (MIT; ideas re-implemented in Melissa's own components and words, no code
  copied). **Registry:** the Agent Client Protocol project's
  `https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json` (format 1.0.0: `agents[]` with `id`,
  `name`, `version`, `description`, `repository`/`website`, `license`, `icon`, `distribution.{binary.<os-arch>
  {archive, cmd, args, sha256}, npx, uvx}`), the list other ACP clients (T3 Code among them) use. Fetched only on
  the owner's explicit action (Load the list / Refresh / the API's refresh), never in the background or when a
  page opens; cached in Melissa's state directory; searched locally. Melissa installs only `binary` entries for
  this machine that carry a SHA-256, from https, ≤ 512 MiB, unpacked with the system's `tar`/`unzip` (which
  refuse `..` members) into `<agents' home>/.local/share/melissa-agents/<id>/<version>`; the program must resolve
  inside that folder. `npx`/`uvx` entries and binaries without a checksum show "Enter manually". Built-in names
  (`claude`, `grok`, `opencode`, `echo`) can't be taken. **Manual:** a name and a command line (no shell: words,
  quotes only); the program must exist, its file name is the provider id (lowercase), and it must answer ACP
  `initialize` with protocol 1 and MCP over HTTP (Melissa's tools need it) within 20 s
  (`melissa_agent::acp::probe`, the program run once as Melissa's user with a clean environment, like an
  installer). Added agents: `providers.toml` `[added.<id>]` (label, command, args, allowed hosts, registry id,
  version, install folder) → session backend `<id>-acp` (generic ACP, Melissa's sandbox with an empty home,
  network through the egress proxy with only the owner's hosts), appended to the lead agent's backends, applied
  at the next idle restart. Identity: an API key under a variable name the owner may change (must look secret:
  KEY/TOKEN/...), stored like other provider secrets. **Remove:** refused, naming them, while Melissa or helpers
  use it ("Ada and Melissa use Grok. Switch them to another provider first."), while its install or sign-in runs,
  or when no other provider would be left; else, where Melissa installed it (container image), a fixed list of
  install and sign-in paths per built-in (from `melissa-setup` and the sandbox presets) or the registry install
  folder is deleted inside the agents' home (no symlink following, never outside home); elsewhere (Nix, a run
  from source) files stay. Its settings and keys go (built-ins: `removed = true`, hidden; added: deleted), audited
  `settings.provider.remove`, applied at the next idle restart, so it leaves the model picker. Alternatives:
  fetching the registry when the panel opens (owner allowed it; a button is more clearly explicit and costs one
  click), installing `npx` packages (needs Node in the image), signing registry downloads (the registry publishes
  only checksums), uninstalling through a root unit (the files belong to `melissa`, which the server runs as).
- **2026-11-20 — The lead agent hands app creation to helpers; it never deploys a new app itself** (owner feedback:
  asked to set up a Minecraft server, Melissa deployed it herself). Enforced in code, not only in the prompt:
  `deploy_app` targets the new `Access::Create`, and the scope check refuses `Scope::Lead` creating a workspace that
  does not exist yet (`LEAD_HANDS_OFF`, before any confirmation; `Deployer::new_owner` refuses too). Every app so has
  a helper owner from the start (the deploying agent, `melissa.io/owner-agent`). The lead keeps every oversight and
  override power from the ownership rules on existing apps (status, logs, stop/start, exec, update, uninstall,
  reassign, bringing back an uninstalled app), with the same confirmations. Fast hand-off: `create_agent` takes an
  optional first `job` (plus `user_request`, the user's words verbatim, and `research`, the lead's read-only
  findings; `give_agent_job` takes the same two), so creating and starting a helper is one call. Why: the owner
  wants helpers to do hands-on work and own the result, the lead to direct and relay. Alternatives: prompt-only
  (the model ignored it), hiding `deploy_app` from the lead (loses bringing back an uninstalled app and the
  refusal's hint), letting the lead deploy on a helper's behalf with `agent` (the old way; the helper never saw the
  work it now owns).
- **2026-11-20 — Settings, Apps and Usage and resources are panels in a modal, opened from a bar at the bottom of
  the rail** (owner request with a reference screenshot of a coding app's settings). The gear, hexagon and chart
  icons replace the rail's "Settings" and "Apps and devices" items; an updates icon sits alone on the right and
  shows a dot when the last Melissa update check found something (or a download waits). Each panel stays a page
  (works without script, deep links and reloads work); the modal asks for the panel alone with
  `HX-Target: overlay-body`, so the server needs no second set of routes. Devices went to Usage and resources
  rather than the Apps panel: a device card is CPU, memory, storage and uptime, i.e. resources, and the Apps panel
  stays one list of one kind of thing. Providers have no "newer version" check (only the installed version), so
  the update dot covers Melissa only until one exists; adding one would mean a new third-party call (open
  question). Usage numbers are counted, never estimated: Melissa counts turns, time in turns and approvals
  asked; context and cost appear only as the provider reports them. Alternatives: a separate settings page per
  section (what we had; the owner wanted the modal), devices in the Apps panel (mixes two kinds of item), an
  estimated cost from token counts (would be invented numbers).
- **2026-11-20 — The page is the viewport above phone width.** The banner takes its height and the rail and main
  column fill the rest and scroll on their own. The rail and the details panel used to be sticky and
  `100vh - 2rem` tall, which left a 2rem gap at the bottom once the banner was dismissed or the page scrolled
  (owner: "a weird gap with the bottom left and right panels"). Phones keep the page scrolling (the browser's bars
  collapse as usual).
- **2026-11-20 — Gemini CLI, Codex and Muse Code are built-in providers; the image ships Node.js 24; Pi is not
  built** (owner request: the popular agents as first-class providers). Researched from the vendors' repos,
  npm, the ACP registry and T3 Code (MIT; it drives Codex through `codex app-server`, Pi through `pi --mode
  rpc` and Muse through its own MSP protocol, none over ACP; Melissa keeps one protocol, ACP, for all of them).
  **Gemini CLI** (Google, Apache-2.0): native `gemini --acp`; npm only (`@google/gemini-cli`, a bundle). **Codex**
  (OpenAI, Apache-2.0): through the ACP project's adapter `@agentclientprotocol/codex-acp` (Apache-2.0; Zed's
  Rust `codex-acp` is deprecated in its favour), which runs `codex app-server` from `@openai/codex`; npm only for
  the adapter. **Muse Code** (Meta, proprietary): native `muse acp` behind `MUSE_EXPERIMENTAL_ACP=on`; one static
  binary per platform with a published SHA-256 (also in the ACP registry). All three take Melissa's MCP server
  over HTTP. **Install:** Node.js 24 from Fedora in the image (`nodejs24`, `nodejs24-npm`, ~73 MiB; Node 22
  would pull a compat OpenSSL, ~98 MiB) and in the Nix package's `melissa-setup` PATH; `melissa-setup agent
  <id>` (the existing root unit, polkit start-only, three more instances) installs as `melissa`: npm packages
  pinned by version and SHA-512 integrity (the top-level packages and Codex's platform binary package, checked
  against the lockfile npm verified the downloads with; nothing installed globally, `--ignore-scripts`,
  Gemini's optional pty/keyring packages left out) into `~/.local/share/melissa-npm/<id>`, swapped in whole,
  run through a two-line wrapper with the system's `node` (linked from `~/.local/bin`); Muse's binary checked
  against its pinned SHA-256 into `~/.local/bin/muse`. Release binaries were preferred where they exist with
  checksums (Muse); Gemini has none for Linux, and Codex's release binaries lack the adapter. **Run:** session
  backends `gemini-acp`, `codex-acp`, `muse-acp`, each in its own sandbox preset (package folder read-only, the
  agent's state fresh per run, only its sign-in file read-write) and egress allowlist (learn mode, 2026-11-20:
  Gemini `generativelanguage.googleapis.com` + `cloudcode-pa.googleapis.com`, `oauth2.googleapis.com` for a
  Google sign-in; Codex `api.openai.com`, `chatgpt.com`, `auth.openai.com`; Muse `api.meta.ai`, `auth.meta.com`;
  denied: Gemini's Clearcut `play.googleapis.com`, Codex's `api.github.com` update check, `files.openai.com`
  and Statsig `ab.chatgpt.com`, npm, Meta's download host). Node honours the proxy only with
  `NODE_USE_ENV_PROXY=1`, which every proxied sandbox now gets. Melissa switches every session to the agent's
  strictest mode that still asks (new backend key `mode`; Gemini `default`, Codex `read-only` — its default
  "Auto review" runs what it deems safe unasked — Muse `promptUnmatched`), failing closed if the agent lacks
  it; Muse runs with `-c telemetry.enabled=false -c approval_judge="off"` (no LLM approving on Melissa's
  behalf); owner arguments may not add `-y`, `--approval-mode`, `-c`/`--config` or sandbox switches. Codex
  wants ACP `authenticate` even with its key set, so Melissa calls `authenticate api-key` when
  `OPENAI_API_KEY`/`CODEX_API_KEY` is set (keeping the ChatGPT sign-in file out of that sandbox, since the
  call rewrites it). Gemini reports models only in ACP's older `models` field: the client now reads it and
  switches with `session/set_model`. **Sign-in:** Codex (`codex login --device-auth`) and Muse (`muse login`) by
  device code through the login unit, shown like Grok's (URL only on the vendor's host); API keys
  `GEMINI_API_KEY`, `OPENAI_API_KEY`, `META_API_KEY`. Google's own sign-in for Gemini is not offered yet.
  **Logos:** letter marks (Google and OpenAI, see the logo decision above; Meta's brand resources restrict use
  of its marks, not re-read in detail, so no logo). An `[added.<id>]` entry whose id is built in now (e.g. Muse
  added from the registry earlier) is dropped at load with a warning instead of failing the whole file.
  **Pi** (Mario Zechner's agent, MIT) is not built: its only ACP path, the third-party `pi-acp`, takes no MCP
  servers and runs Pi's tools without asking (PLAN 76h). Alternatives: Zed's Rust `codex-acp` (one static
  binary with a checksum, but deprecated and pinned to an old Codex), `npx` at run time (downloads on every
  start, unpinned), bundling Node per agent (bigger, more to update), Node 22 (larger here), installing npm
  packages into the image (the image ships no vendor agent software; `melissa-setup` installs it on request).
- **2026-11-20 — One chat per agent; "Fresh start" instead of "New chat"** (owner design "Melissa — Agent
  workspace"). The talk with Melissa is the only talk with Melissa, a helper's the only talk with it. "New chat"
  (UI, `POST /api/lead/new-chat`, CLI `new-chat`/`chat --new`) is gone; no new archives are made. Chats archived
  before stay on disk and readable (`/chats`, Melissa's menu "Earlier chats"). **Fresh start**
  (`Lead::fresh_start`, `AgentPool::fresh_start`, the agent menu, `POST /api/{lead,agents/{a}}/fresh-start`, CLI
  `fresh-start [--agent]` with the old names as aliases) clears the model's memory: running turn stopped, session
  closed without keeping its id, taint cleared (still the owner-approved way to clear it), Melissa's own pending
  confirmations withdrawn; the chat goes on with a `fresh_start` event (a "Fresh start" divider), and the next
  session is primed with nothing from before it. After a restart the chat comes back with its last 100 turns
  (was 30); chat pages render the newest 60 entries and load older ones as the user scrolls up (`/ui/older`,
  `?all=1` without script). Why: one place per agent to look, no lost context by accident; memory is cleared on
  purpose, not by starting over.
- **2026-11-20 — Silent compaction from Melissa's transcript; Claude Code compacts itself.** When a turn ends with
  the context (as the provider last reported it) at `[lead] compact_at_percent` (70) or more of the window and the
  conversation then stays idle `compact_idle_secs` (120), Melissa condenses the older messages since the last
  fresh start into a summary (the previous summary, then each older message on one shortened line; the last 6
  messages stay whole; about 6,000 characters), writes it to the transcript as `context_compacted` (not shown in
  the chat; the visible history stays complete), forgets the session id and marks the next turn to open a new
  session, primed with the summary and the recent messages. After an overflow (a turn ending in `max_tokens` or an
  error with the context at 90%+) it condenses at once. Backends that compact on their own
  (`SessionBackend::compacts_itself`: Claude Code's auto-compact, which also runs in `-p` stream-json mode and
  keeps the session) are left alone. Why not a model-written summary: it needs a hidden turn whose events must not
  reach the chat, and ACP has no compaction call; the extractive summary is deterministic, testable and costs no
  tokens. Audited `<kind>.compact`; `POST /api/lead/compact` does it on request.
- **2026-11-20 — The sidebar groups agents by state.** It replaces the rail: a device menu ("Melissa ▾", this
  device only for now, Collapse sidebar), agent search, "+" (a new helper: name and icon), then **Needs
  attention** (an approval it asked for waits, its last turn failed, or an app it looks after has a problem),
  **Pinned** (Melissa always; helpers the user pins), **Working** (a turn runs; a still bar, as the share is not
  known) and **Settled** (closed until opened). An agent is in the first group that applies. Live over the chat's
  stream (registry changes, Melissa's log in a helper's chat, quiet ticks). Helpers get an icon from Melissa's own
  shapes and colours (`lead::icon`; existing ones get a default for their id, saved once). Each row and the chat
  header have the same menu (Rename agent, Change icon, Agent settings, Pin/Unpin, Fresh start, Earlier chats,
  Delete helper); renaming, icons and deleting live on the helper's page in Settings → Agents. The chat header
  shows "device / name" and a status (Needs permission, Needs a look, Working). The details panel became "Agent
  details": on-device services, uploaded files (previews), permissions (waiting, with Review, and the agent's
  Always-allow rules, removable), recent activity, schedules when there are any. The app strip above the chat is
  gone (its apps are the services; the disk warning is on the device row). Tool calls of a turn are one checklist
  card; permission cards carry a tag line ("Network access · TCP 25565") in small capitals as the design shows,
  and every card's no is "Decline". Deviations from the design: primary buttons stay `--action` (honey is not for
  buttons, ui-craft); apps show the facts Melissa knows (a game port, a web port), not "local only" or player
  slots; there is no "System agent" (Melissa keeps updates and device health); the caption "Device changes require
  your permission" is left out (not true for every change). Phone layouts are a separate change.
- **2026-11-21 — Sidebar order: idle agents above Working; small motion across the UI** (owner request, desktop
  and phone). Replaces the group order of the 2026-11-20 sidebar entry. Top to bottom: **Needs attention**,
  **Pinned** (pinned helpers stay there while they work), the **idle** helpers (no heading, count or collapse;
  most recently active first: the last event of their chat, or when its saved chat last changed), then
  **Working** at the bottom. The "Settled" group is gone. When an agent finishes it moves back up into the idle
  list; an agent that changes place glides there (melissa.js measures the rows before and after the live
  update and slides each from where it was, 200ms), a new one fades in, a removed one fades out. Motion
  elsewhere, all 160 to 200ms ease-out on transform and opacity, and none of it under
  `prefers-reduced-motion: reduce`: new chat entries (messages, cards, events), queued messages, a form's answer
  (a settings section, a dialog's content, a notice) fade in with a slight rise; dialogs, sheets and menus fade
  in as they open; pages cross-fade with cross-document View Transitions where the browser has them, the sidebar
  held still on wide screens. Nothing fades on page load, nor for polled regions, results as you type, toggles,
  the composer or the model picker. Why: the owner asked for small, quick animations, and for working agents to
  sit apart at the bottom while finished ones come back to the list.
- **2026-11-21 — People, logins and permissions** (PLAN section 6c). Accounts per person with username +
  password (Argon2id); the first person claims the box with a one-time console code and is the owner, others
  join by single-use invite; passkeys later (need HTTPS). No guest role. Members get granular permissions
  (per-app Use/Operate/Manage, install apps, app access, agent count off/N/unlimited, providers/models, approvals)
  filled from editable presets. Agents belong to a person, private unless the owner shares them; an agent never
  does more for a person than that person could. One lead agent per device with a separate conversation per
  person, limited to that person's permissions. Providers shared first (owner-only settings, "may use" per
  member), per-person provider accounts second. Why: Melissa is a household sysadmin tool; a shared key gives no
  attribution, no per-person revocation and all-or-nothing power; fixed user types don't fit different
  households. Alternatives: one shared passkey (too coarse), fixed roles only (too rigid), guest role (not needed).
- **2026-11-21 — Melissa decides who may open apps; the host publishes app ports on every address.** Owner
  decision. Container install: install.sh's `BIND` defaults to `0.0.0.0` (was `127.0.0.1`), and a setting,
  **Apps open to** (Settings → Apps), chooses *This device* (default, the old exposure), *Tailnet* or *Home
  network*, each including the ones before. Enforced inside the privileged container by the nftables table
  `melissa_app_access` (root oneshot `melissa-app-access.service` running `melissa-server app-access apply`):
  traffic to the NodePorts 30000-30079, TCP and UDP, is matched on its source address in `prerouting` at
  priority -150, before kube-proxy's DNAT, except from `lo` and the pod bridge `cni0`. This device = the bridge
  gateway (rootful netavark masquerades host-loopback connections to it); Tailnet = 100.64.0.0/10 and
  fd7a:115c:a1e0::/48; Home network = RFC 1918, 169.254.0.0/16, fc00::/7, fe80::/10; everything else dropped.
  Private ranges rather than the host's current subnet: the container can't see the host's interfaces, a
  laptop's network changes, and a fixed list is verifiable (limits in PLAN.md open questions). **Fail closed:**
  the unit reads the table back and checks it; on failure it loads "this device", then a table that lets
  nothing in; loads are atomic; k3s starts only once the table exists (drop-in `ExecStartPre=nft list table`);
  app links (`app_status` `url`/`urls`/`ports`) name only addresses the verified filter lets in, and the page
  and tools show the problem. The server (user `melissa`) writes `app-access.toml` and starts the unit
  (polkit rule `52-melissa-app-access.rules`: that unit, verb start, nothing else); it reads
  `/run/melissa-app-access/status.json`. Agents use `set_app_access`: widening asks the owner every time
  (`Tool::owner_approval`, no "Always allow"), narrowing runs under the usual policy. Changes are audited
  `settings.app_access` (actor `ui` or `agent`). Applies live: no restart, no sudo. An explicit `BIND` still
  restricts at the host (the server keeps leaving out addresses it doesn't cover, and tells the owner that only
  a reinstall changes it); `MELISSA_PUBLISH_BIND` stays. Nix/host installs: no filter of Melissa's, the page
  says the host's firewall decides. Why: the owner wants agents to fix "this app isn't reachable from my
  tailnet" themselves, without sudo or terminal steps, while keeping today's default exposure. Alternatives: a
  host-side helper that rewrites the Quadlet's `PublishPort` and restarts the container (needs root on the host
  for every change, restarts everything), nftables on the host (Melissa would need to manage the host's
  firewall and its firewalld), filtering in k3s with NetworkPolicies (NodePort traffic is SNATed by kube-proxy,
  so the source address is gone by then).
- **2026-11-21 — Queue and steer instead of "busy"; helpers' reports give the lead agent a turn** (owner report:
  Melissa sat in `agent_status` with `wait_secs` while Iris worked, so her chat stayed busy, and every message
  sent during a turn was refused). Supersedes the 2026-11-18 "a second message is refused, not queued" and, for
  job results, the "the Queen gets a note with her next message, never a turn of her own" of the force-delete
  entry (force-delete notes are unchanged). (1) **Queue:** every conversation (Melissa's and each helper's)
  keeps a FIFO of messages sent while a turn runs (`message_queued`; audited `<kind>.queued`); when the turn
  ends (finished, failed or cancelled) the next one starts (`message_unqueued` `started`). Cancel keeps the
  queue. The user can take a message back (`removed`, or `edited`: back into the composer). In memory only: a
  restart drops it. Scheduled runs and `give_agent_job` still need an idle conversation (skip / error, as
  before); `message_agent` and Melissa's notices to a working helper queue instead of the old 1 h retry loop.
  (2) **Steer** ("Send now", double Enter): the backend says whether its sessions take a message mid-turn
  (`SessionCapabilities::steer`, `AgentSession::steer`). Claude Code does: a second `user` line on stdin during
  a turn is read at its next step and answered in the same turn, which ends with one `result` (verified with
  Claude Code 2.1.330). ACP v1 has no such call: the running turn is cancelled and the message starts next. A
  steered message shows as a normal user message in the running turn (`lead.steer`). (3) **Helper jobs don't
  block:** `give_agent_job` returns at once and its description and the lead prompt say to end the turn;
  `agent_status` no longer takes `wait_secs` (an error explains why). When a job ends, its report (outcome plus
  the helper's last reply, framed as data, the helper's taint carried over) is queued for the lead agent as a
  `helper_report` turn: the chat shows a small "Iris finished" line, not a user message. No loops: a report
  comes only from a job's end (never from a lead turn), once per job, and after 3 report turns in a row without
  a user message further reports wait as notes for the user's next message. (4) **UI** (owner design in Figma,
  2026-11-21): queued messages show after the chat as the user's bubble with a dashed outline and muted text,
  with "Send now", Edit and Remove under it; Enter while an agent works queues, double Enter (within 400 ms)
  steers; Settings → Developer → Chat "Enter while working: Queues / Steers" swaps the two. API: message posts
  return `started`/`queued`/`steered`/`next` (no 409), plus `GET …/queue`, `POST …/queue/{id}/steer|remove`.
  Alternatives: keep refusing (the owner's report), a client-side queue (lost on reload, invisible to other
  clients), interrupt-and-replace for every backend (needlessly loses Claude's work in progress).
- **2026-11-21 — The user can rename the lead agent like a helper; the product stays "Melissa".** Its menu's
  Rename agent opens `/settings/agents/lead` (the helper page's name form, nothing else); the API is the helpers'
  (`POST /api/agents/lead/rename`, or by its name). Same name rules as a helper, unique among the lead agent and
  every helper; `lead` is reserved. The name is stored in the agent registry file (`lead_name`, default "Melissa")
  so one lock checks every name; audited `agent.rename` (`lead: old → new`, actor `ui`). System prompts (the lead
  agent's `{lead_name}` and each helper's, which now also follow a helper's own rename) are built when a session
  starts, and an open session gets a note with its next turn. `lead`, its name and "Melissa" (while no helper is
  called that) all reach the lead agent. The brand, page titles, device menu, docs and the project keep "Melissa".
  Alternatives: a separate file in the lead agent's state directory (two files to keep names unique across), a
  fixed name (the owner asked for it).
- **2026-11-21 — Setup passwords: a one-time chat card, server to browser only, through a get-only identity;
  Dismiss makes Melissa forget it for good.** (Owner design.) When an install generates an app's password, the
  install pipeline (`deploy_app`'s Secret step, not an agent's message) sends a notice with the app and login
  name only; the lead's forwarder puts a `setup_password` event in the chat where the install ran (the helper's
  own chat for its task, else Melissa's). The card ("<App> setup password", login, `••••••••`, Copy, Dismiss,
  "Change this password after you first sign in.") loads its state and, on a click, the password from
  `/ui/apps/{app}/setup-password[/reveal|/dismiss]`: `Cache-Control: no-store`, both POSTs need positive
  same-origin evidence (`Sec-Fetch-Site` or `Origin`; a request with neither is refused, unlike other UI
  posts), no value in a URL, the page's first HTML, an event, a transcript or a log. Every Show/Copy is audited
  `app.credentials_viewed` and Dismiss `app.credentials_dismissed` (actor `ui`, "<app>: setup password
  shown|copied|dismissed"; never the value). **Agents never see it:** no tool calls this path; the tool result
  only says values are never shown to agents; an agent's context (recap, `agent_status`, condensing) is built from
  an allowlist of other events, so the card event never primes a model; sandboxes reach only `POST
  /mcp/<token>`, not `/ui`. **Access:** `melissa-server app-credentials` needed `sudo` because it reads Secrets
  with k3s's root-only admin kubeconfig: none of the server's identities may read Secrets (the write identity
  creates them, never gets them). Rather than widen `melissa-apps` (which every app tool, including
  agent-driven `app_exec`, runs on), a third ServiceAccount `melissa/melissa-credentials`
  (`credentials-rbac.yaml`, kubeconfig `/run/melissa-kube/credentials.kubeconfig`, root:melissa-kube 0640 like the
  others, not mounted in any sandbox) holds a ClusterRole with only `get` on Secrets (no list/watch: the server
  computes the one Secret's name from the stored spec). It is never bound cluster-wide or by a manifest:
  `melissa-apps` applies the RoleBinding `melissa-app-credentials` in that one app namespace right before a
  reveal (it may `bind` exactly that role, and the admission policy allows exactly that binding name, role and
  subject). `resourceNames` would be narrower still, but per-app Roles need `escalate` or the reader's
  permissions on the write identity; per-namespace `get` with Melissa-generated Secrets only (app pods get no
  service account token, so nothing else writes Secrets there) is the narrowest that works without either.
  **Dismiss** (asked once in the card, "Forget it?" Yes/No): the namespace gets
  `melissa.io/credentials-dismissed=<UTC time>` and the binding is deleted (`melissa-apps` may delete only that
  binding). The admission policies make it final: the annotation can't change or go away, and the binding is
  refused in a namespace that has it, so a later bug can't bring the reader back. The app's Secret stays
  untouched: workloads read the password at every start (e.g. an admin password env value), so deleting or
  rotating it would break restarts or lock the owner out; what goes is Melissa's ability to read it.
  `app-credentials` leaves a dismissed password out too (root can still read etcd; that is the device owner).
  After a dismiss the reveal answers 410 and the card is one line "<App> setup password dismissed". Once logins
  exist, Show and Copy are sensitive actions (PLAN 6c: re-enter the password). Open items: PLAN Q86.
  Alternatives: rows in the app dialog (the owner's first design, replaced by the card the same day); giving
  `melissa-apps` `get` on Secrets (every agent-driven app tool would hold it); keeping `sudo` only (the owner
  asked for a UI); deleting the Secret on dismiss (breaks the app's restarts).
- **2026-11-21 — Agents are addressed by id; names resolve strictly and never silently pick** (owner: "must be
  fixed for resilience"). Agent-facing output (tool results, `list_agents`, `my_apps`, schedules, job reports,
  relayed messages, helper prompts) names agents `Name (id)`; the lead agent is `Name (lead)`. Tool parameters
  ask for the id (preferred) or the exact current name, and the prompts tell agents to give ids. The registry
  keeps every agent's former names, the lead agent's (its default "Melissa" counts once it is renamed) and each
  deleted agent's id and names. `AgentRegistry::lookup` resolves `lead`, then ids, then a name only when exactly
  one agent has it now and no other agent had it before; a result found by name says which id it used
  (`"resolved"`). A former name nobody has now is refused with the new name and id; a name that one agent has and
  another had (rename, then a new agent takes the name), or a deleted agent's name, is refused naming every
  candidate with its id. Names shaped like ids (`d-` and 8 hex digits) are refused. The user's routes use the same
  resolver (404 unknown/deleted, 409 ambiguous, 422 former name); the web chat still shows names. This replaces
  the earlier "`Melissa` reaches the lead agent while no helper is called that". Alternatives: ids only in tool
  arguments (models type names; the owner wanted names to keep working where they are clear), first-match by
  current name (the misrouting this fixes), forbidding the reuse of former names (blocks the user for a rare
  case; the refusal is enough).
- **2026-11-21 — Tailscale is built into the container image, off by default; the host's wins when it has
  one.** Owner decision (resolves PLAN Q85's investigation). **Detect first:** install.sh checks the host
  (`tailscale0` exists or `tailscaled.service` is active; `HOST_TAILSCALE=yes|no` overrides) and writes
  `MELISSA_HOST_TAILSCALE` and the node name (`MELISSA_TAILSCALE_NAME`, `melissa-<host>`) into the Quadlet.
  With the host's Tailscale, Settings → Tailscale only says "Tailscale is not managed by Melissa on this
  system" and Melissa's own never starts (the unit checks the flag too); an install without the flag (from
  before) is treated the same. **Otherwise one click:** the image ships the official static `tailscale` and
  `tailscaled` (1.104.7, sha256-pinned in `packaging/container/tailscale.lock` with their BSD-3-Clause
  `LICENSE` and the Go modules' license list, shipped in `/usr/share/licenses/tailscale/`), rather than
  downloading at run time (no network step or third-party fetch when the owner clicks, works the same on
  every install, verified at build) or the `tailscale/tailscale` container image (a second container with
  host networking would touch the host's network, which the host owns). `melissa-tailscaled.service`
  (root, enabled in the image) runs tailscaled in kernel mode in the privileged container (`/dev/net/tun`,
  `tailscale0` in the container's network namespace), state in `/var/lib/melissa-tailscale` on the volume,
  socket `/run/tailscale/tailscaled.sock` in a `root:melissa 0750` directory, `--no-logs-no-support`, and
  only while `/etc/melissa/tailscale.toml` says enabled (`ExecCondition=melissa-server tailscale enabled`)
  and the app access table exists (fail closed). Its `ExecStartPost` makes the server's user tailscaled's
  **operator**, so the server runs `tailscale up --hostname=<node> --accept-dns=false --operator=melissa`
  and `tailscale logout` itself; polkit (`53-melissa-tailscale.rules`) lets it start and stop that one unit.
  Enable/Disable are audited `settings.tailscale` (actor `ui` or `agent`); Disable asks once in the page, logs
  the node out and stops it. **Sign-in:** the login URL lets whoever opens it attach this device to their
  tailnet, so it is handled like the setup password: the server starts `tailscale up` (output discarded),
  reads the URL from `tailscale status --json`, accepts only `https://…tailscale.com/…`, and returns it only
  in the answer to `POST /ui/settings/tailscale/sign-in` (positive same-origin evidence, `no-store`), as an
  "Open Tailscale sign-in" button that opens a new tab; the region then polls until the sign-in is done. The
  URL type has no `Display` and a redacted `Debug`; it is never logged, put in the chat, a transcript or a
  tool result; audited `tailscale.login_started` without it. **States** (existing components only): Not
  managed by Melissa | Off [Enable] | Starting | Sign in needed [Sign in] | Waiting for approval |
  Connected · node · 100.x [Disable] | Error (one line). **Apps:** tailnet traffic now arrives on
  `tailscale0` inside the container. The `melissa_app_access` table accepts app-port traffic from that
  interface under Tailnet and Home network (a peer's subnet routes can carry non-Tailscale sources), never
  under This device (its gateway rule excludes `tailscale0`), and a new `input` chain drops everything else
  `tailscale0` sends to the container itself (the UI forward, k3s's API, the kubelet) at every level, closed
  included, replies excepted; verification checks both. App links add the node's MagicDNS name and IPv4
  address at the NodePort (they reach the container directly, so no `APP_PORT_BASE` offset and no
  publish-bind check), only where the access setting allows them. **Agents:** `tailscale_status` (read only)
  and `enable_tailscale` (owner approval every time, like widening app access); neither sees the URL;
  disabling is the owner's. **Network calls** (owner-approved, PLAN section 4): only while enabled,
  Tailscale's coordination server and DERP relays. **Not in this change:** the web UI over the tailnet
  (`tailscale serve`, after logins), an auth-key field (PLAN Q85). Alternatives: tailscaled on the host
  installed by Melissa (needs root on the host, changes the host's network), userspace networking (no
  `tailscale0`, so the app filter couldn't tell tailnet traffic apart and apps would need a proxy per port),
  downloading Tailscale when enabled (a run-time fetch, unpinned unless Melissa ships the hashes anyway).
- **2026-11-21 — Logins (PLAN 6c, step 1): accounts, the owner's claim with a setup code from the host,
  invites, sessions, the password again for sensitive actions; the CLI on a host-local socket.** Supersedes
  the 2026-11-18 "Until login exists" entry's "no auth yet" (loopback bind, same-origin check and CSP stay).
  **Store:** `crates/auth` (`melissa-auth`), one file `<state>/auth/auth.json` (directory 0700, file 0600,
  written atomically): accounts (Argon2id PHC hashes with argon2's default cost, 19 MiB / 2 passes / 1 lane),
  pending invites and sessions; session cookies and invite tokens are 256-bit random values stored only as
  SHA-256. Roles `owner` (exactly one, can't be removed) and `member`. Usernames 1–32 of `a-z 0-9 . _ -`
  (`host` reserved), passwords 8–256 characters, nothing else. **Failed tries** (in memory): per username 3
  free, then a wait that doubles up to 60 s, a 15-minute lockout at 10; per source address 10 free, locked at
  30; a check during a wait is refused without hashing; unknown names verify against a dummy hash. Audited
  `auth.login`, `auth.login_failed`, `auth.lockout` (and claim, invites, resets, removals, sign-outs, password
  changes), with a new `person` field. **Claim:** with no owner every page goes to `/claim` (the API answers
  401); a 10-character code (about 50 bits, no look-alike characters) made at start, printed on the console
  and in the log, shown again by `melissa-server setup-code`; valid 1 hour, a new one at each start or when
  asked after it expired, unusable after 10 wrong tries; per-address limits too. **Invites:** the owner's
  single-use links in Settings → People, 24 hours, withdrawable; reset links for a member (24 h) or, from the
  host, `melissa-server reset-owner` (1 h); a reset signs that person out everywhere. **Sessions:** idle 7
  days, at most 30; Settings → Account lists them ("Sign out" each, "Sign out everywhere") and changes the
  password (other sessions end). Cookie `__Host-melissa_session` with `Secure` when the request came over
  HTTPS (`X-Forwarded-Proto: https` or an https `Origin`), else `melissa_session; HttpOnly; SameSite=Strict;
  Path=/`: browsers refuse `Secure` cookies from `http://` pages and `__Host-` requires `Secure`, and the UI is
  plain HTTP on loopback today. **CSRF:** a token per session in `<meta name="csrf-token">`; melissa.js sends
  `X-CSRF-Token` with htmx and fetch; every POST form also has a `_csrf` field first (forms without script;
  the gate strips it from urlencoded bodies, and reads it as a multipart body's first part); the same-origin
  check stays in front. **Gate:** one middleware with one path table (public: sign-in pages, assets,
  `/healthz`, `/mcp/<token>`; owner-only: People, providers, app access, updates and restart, force delete;
  members don't see those Settings sections), the person and token in a task-local for components and the
  audit. **Password again (5 min):** the setup password's Show/Copy/Dismiss, app access changes, provider
  settings, People, force delete, and approving an agent's `set_app_access` (owner only); pages go to
  `/reauth` and come back, the API answers 401 with `reauth`. Approvals count only from a signed-in session
  or the host socket and name the person in the audit. **CLI:** `melissa-server chat`, `ask`, `confirm` and the
  rest talk over `<runtime>/melissa/api.sock` (the service's `/run/melissa/api.sock`; `$MELISSA_SOCKET`
  overrides), 0600 in a 0700 directory, accepting only uid 0 and the server's own uid (peer credentials),
  serving the same routes with the owner's authority (audited as `host`); `/api/host/setup-code` and
  `/api/host/reset-owner` exist only there. Sandboxes don't see the runtime directory. Why the socket over
  per-person API tokens: nothing to store, show or leak, no CLI setup, and access to the box is already the last
  authority (6c); tokens come with remote clients (6b). Why server-side sessions: revocation and a session
  list. Alternatives: per-person API tokens now (a token file per person and CLI config for a host-only CLI),
  signed stateless cookies (no revocation), `__Host-` only (breaks plain-HTTP use), token-in-script-only CSRF
  (fails without script), lockout in the file (a restart clearing it is acceptable: only the host restarts).
- **2026-11-21 — Factory reset (Melissa only).** Owner decisions: every agent-deployed app and its data goes (no
  "keep apps"), every provider sign-in and API key goes, the device gets a **new device id**, the audit log and
  the journal are wiped (afterwards one "factory reset" audit line), and k3s's data is wiped entirely. Kept: the
  install (`server.toml`, the host's `install.env`, image, Quadlet file, drop-ins), k3s's node password
  (`rancher-node`) and the voice model. **Who:** the owner only, password entered within 5 minutes (the gate's
  `sensitive()` plus `require_owner_reauth`), typing `melissa` (no device name setting yet); members never see
  Settings → Reset; refused while an update waits to install. Also `melissa-server factory-reset --confirm
  melissa` over the host socket, and `podman exec melissa /usr/libexec/melissa/factory-reset --confirm melissa`
  when the server is down. **How:** the server writes a marker (`/var/lib/melissa-container/reset/
  factory-reset.json`, 0600, atomic, in a root:melissa 0770 directory on the volume) and an audit line, answers
  with a "Resetting…" card that polls the public `/reset-status` (htmx; `HX-Redirect: /claim` once she is back
  unclaimed), and tidies up while it runs (scheduler paused, requests denied, its own Tailscale logged out, apps
  deleted, sessions closed; bounded). `melissa-factory-reset.path` starts the root oneshot
  `melissa-factory-reset.service`, which waits up to 90 s for that, stops the server, Tailscale and k3s, kills
  k3s's leftovers (cgroup.kill, shims, `cni-*` namespaces, its interfaces), unmounts anything below a target,
  removes a **fixed list** (constants in `melissa_settings::factory_reset`; never paths from a request; no
  symlink followed, no mount crossed, a symlinked parent refused), writes the audit line, deletes the marker
  **last**, and starts k3s, its credentials, the app filter and the server again. Each step is recorded in the
  marker. The same unit runs at boot (`ConditionPathExists=`, before the server and k3s): a stale marker always
  means "finish the wipe"; a failed run waits a minute and the path unit retries. Why a root unit behind a marker:
  the server runs as `melissa` with `ProtectSystem=strict` and can't (and shouldn't) remove k3s's or the journal's
  files; the marker on the volume makes the reset crash-safe and idempotent. Why a list over "everything in
  /var/lib/melissa but models": nothing outside what Melissa is known to write can go (new state files must be
  added to the list; a test checks the list never reaches what stays). Alternatives: delete the volume from the
  host (also drops the voice model and node password, needs the host), a polkit rule letting the server start
  the unit directly (no crash-safe record of the request), keeping apps (owner: no).
- **2026-11-21 — Onboarding: a first-run wizard plus a checklist** (owner decision). After the claim the owner
  goes through `/setup`: Device (a display name only: `device_name` in `ui.toml`, shown where the UI names this
  device; the Tailscale and k3s node names stay), Provider (tiles with the live status; the chosen provider's
  Install / Sign in / API key are the Settings → Providers forms, whose handlers answer with the setup region
  when the request comes from `/setup`), Model ("Use this model" through `Lead::use_backend`, i.e.
  `lead/backend.json`) and Extras (Voice and Tailscale switches over their Settings handlers). One primary
  action and one quiet "Later" per step. **Progress is the real state** (a device name, a usable backend, a
  saved model choice, voice or Tailscale on); setup itself keeps only `[setup] skipped` and `dismissed` in
  `ui.toml`, so a restart (adding a provider restarts Melissa) resumes where it was. While Device, Provider or
  Model is neither done nor put off, the owner's `/` and `/chat` redirect to it; steps put off show as a
  "Finish setup" card in Melissa's empty chat until done or hidden. Members never see setup (invites land in
  the chat). The empty chat shows three fixed suggestion chips (hardcoded strings filling the message box via
  `?draft=`) and, when no provider works, a notice in place of pretending to be ready. **Claim, shortened:** one
  password field with a "Show password" switch (melissa.js, CSP-safe; hidden without script), no "Password
  again"; the setup code is kept 0600 in `<state>/auth/setup-code.json` until used or expired, and
  `install.sh` prints it at the end (or how to get it if the server isn't answering yet). Why: a person who
  just installed Melissa should reach a working chat without a terminal or a hunt through Settings, and
  deriving progress from state means nothing goes stale. Alternatives: a checklist only (no guidance on the
  first visit), a wizard only (no way back to skipped steps), storing step completion flags (drift from the
  real state), renaming the node from the device name (owner: display only), keeping the code in memory
  (a restart after install invalidated the printed code).
- **2026-11-22 — Setup fixes from the demo rehearsal.** The wizard walks Device → Provider → Model → Extras in
  order, also when a step is already done (a signed-in provider made Provider "done" and setup jumped from Device
  to Model; the owner picks the provider there); only Model is passed over while no provider works. The `/` and
  `/chat` redirects still go only to open steps. "Choose a model" carries the chosen provider (`/setup/model?p=`):
  the list shows its models only, starting on its default; one default entry per provider (the agent's own
  `default` model is that entry), the provider's own name is dropped from model labels (ids unchanged). Extras
  with neither voice input nor Tailscale possible is one line and "Finish setup"; "Later" shows only while
  something is left to turn on. A model chosen before the chat's first message posts no "Switched to" line (the
  chat keeps its greeting, suggestions and checklist), and the "Connected to" line right after a switch to the
  same backend is left out. The claim page's hint names `melissa-server setup-code` (container and source).
- **2026-11-22 — No page fades; Settings swaps only the section; remembered looks come from the server**
  (owner request: "Every time I tap a category I get a fade-in", "a lot of the UI just jumps", "the pre-alpha
  banner pops in"). Replaces the cross-fade and the "a form's answer fades in" parts of the 2026-11-21 motion
  entry. (1) The cross-document View Transition (160ms page cross-fade) is gone: moving somewhere is instant.
  (2) With Settings open, a link to another Settings page loads only `#settings-main` (breadcrumb and content;
  `HX-Target: settings-main` answers with the panel alone like `overlay-body`); the section list, its scroll
  and the search stay, `aria-current` moves, focus stays on the clicked entry (or goes to the section when it
  was inside the old one); URL pushed, so reload and Back work. (3) Of a form's answer only a notice fades in,
  not the section it replaces; panels and sections that load don't fade. (4) Looks melissa.js remembers in
  localStorage (banner dismissed, sidebar collapsed, details hidden on wide screens) are mirrored into plain
  cookies (`melissa_banner`, `melissa_sidebar`, `melissa_details`; no data, not secret, one year, SameSite=Lax),
  and the server renders the page that way, so nothing flashes or shifts on any full page load (an agent switch
  in the sidebar, reload, Back). (5) Controls that need script (collapse sidebar, details toggle and close,
  dismiss banner, voice) are shown from the first paint and hidden only under `@media (scripting: none)`,
  instead of being un-hidden by script after load (the chat header's buttons moved 52px). (6) Scrolling panel
  bodies and the wide chat column keep `scrollbar-gutter: stable`. Alternatives: an inline pre-paint script
  (the CSP forbids it), naming the Settings parts for View Transitions (still motion where none is wanted),
  re-rendering the whole panel without the fade (the list's scroll and search reset).
- **2026-11-22 — A design system: standard components, design tokens, thinner buttons, current browsers** (owner:
  "a lot of the UI elements are non-standardised", "your buttons are too thick", themes later). Every UI element
  becomes one component family with a minimal set of variants, designed first in the local Penpot, then built
  once as a maud component that all pages call. Values are **design tokens** in three layers (primitives →
  semantic roles such as `color.bg.surface` → component tokens such as `button.height.md`); a Penpot name
  `a.b.c` is the CSS variable `--a-b-c`; themes swap only the semantic layer (`data-theme`, rendered by the
  server from a cookie so nothing flashes). Owner choices: buttons **32 px** (small 28 px, large 40 px) instead
  of 44 px; on touch-first devices (`pointer: coarse`) controls keep a **44 px** touch target (touch laptops,
  whose main pointer is a mouse, get thin controls); button corner radius **8 px** (6 px small); a switch that is
  on is **accent blue** (green stays for status); the Penpot token export is **committed to the repo** and a test
  fails when the CSS drifts from it; supported browsers are the **latest Chrome and Firefox** (newer platform
  features may be used; anything Firefox lacks must degrade gracefully). Inputs from a code audit and a
  component spec (2026-11-22): 10 control heights, 14 radii, 22 font sizes, duplicated rules (`.pill` twice),
  three status-colour schemes; colours were already tokenised. Migration in small steps, each checked with a
  screenshot diff harness. Alternatives: polish components one by one without tokens (no themes, drift
  continues), keep 44 px buttons everywhere (the owner finds them heavy), `any-pointer: coarse` (thick buttons on
  every touch laptop).
- **2026-11-22 — Apps list: grouped by kind, by person; who an app was installed for** (owner design "Apps" A1-A5 in
  the local Penpot; replaces the hexagon tiles). The Apps panel lists apps as rows (icon, name and address, whose it
  is, status, Details) in one card per kind: Games, Media, Home, Files, Network, Developer, Other, from a new optional
  spec field `kind` (`deploy_app` asks agents for it; specs without it are Other). **Icons**: the helpers' shapes
  and colours, **derived** from the app name (FNV-1a plus MurmurHash3's finalizer) so every app has one at once,
  the same on every page and after a reinstall, with nothing stored and no migration; an icon picked in the app's
  details is kept in `ui.toml` (`[app_icons.<app>]`), because people asked to change them (board A5) and a
  derived icon can't be changed. **Who an app is for**: the signed-in person's account id travels with the
  request (`melissa_agent::PERSON_ID`, set by the sign-in gate next to the audit's person), is captured with each
  chat message, becomes the turn's `TaskContext::person` on the agent's MCP session, and is set again around every
  tool call, so `deploy_app` writes it to the new namespace as `melissa.io/installed-for` (first install only,
  kept on updates) and a helper the lead agent creates for that message remembers it (`creator`). For the shared
  lead Melissa this is the person whose message started the turn. The list resolves "installed by" as: the
  annotation; else the person the looking-after helper was made for (installs in scheduled jobs); else the owner
  for workspaces from before agents (`melissa.io/owner=owner`, when the owner was the only person); else
  **unknown**. **Visibility**: the owner sees every app, with a person chip on each row and a filter (Everyone, Me,
  each other person with apps, Unknown) that swaps the list in place (`hx-get` into `#cluster-apps`; plain links
  `/cluster?by=` without script); a member sees only apps that are theirs, and **unknown apps stay with the owner**
  (nothing says they are the member's; the owner can see and sort them out). Enforced on the server: the list,
  `GET /api/apps` (new), the chat's details panel and `/api/details`, and every app route (`/cluster/apps/{app}`,
  `/ui/apps/{app}/…`, `/api/apps/{app}/…`) answer 404 for an app the person may not see; force delete stays
  owner-only. **Details** (board A5) are a page of the Apps panel (`/cluster/apps/{app}`, breadcrumb back) instead of
  a second dialog: head (icon, status, person, Open, Stop/Start), the stop question in the page, Facts, Icon, Logs,
  Force delete. Buttons there are the first `.btn` (32px, 8px radius; 44px hit area only under `pointer: coarse`);
  new component tokens (`--chip-*`, `--segmented-*`, `--app-*`) and `--size-target-touch`. Alternatives: storing an
  icon per app at install (needs a migration for every existing app and a k3s write to change it), deriving the
  person from the looking-after agent's creator only (wrong for the shared lead Melissa and for anyone chatting
  with someone else's helper), showing unknown apps to everyone (leaks apps between members), kind guessed from the
  app name (a list that is never complete). Open items: PLAN Q90.
- **2026-11-22 — Rename this device in Settings** (owner request). The device's name could only be set in
  first-run setup. It is now a "Device name" field with "Save name" on Settings → Updates, below the "This
  device" card: that page is already about Melissa on this device and owner only, so no new section and no new
  gate. Owner only on the server too (`/ui/settings/device-name` is an owner path, and the handler checks
  again). Setup's Device step and the Settings field share one save (`settings/device.rs`: the same
  `clean_device_name`, the same "Give the device a name." message, audited `settings.device_name`) and the
  same prefill (the saved name, else the host name). The form swaps only its own region with a "Name saved."
  notice; other pages read the name when they render, and now every place that names this device uses it:
  chat header, sidebar, the details panel's device card, Usage and an app's "Device" fact (in place of the k3s
  node name or the device id). Live updates of other open panels are left to the live-settings work.
  Alternatives: a new "Device" section (one field does not need a section of its own yet), Settings → Apps
  (about who opens apps, not the device).
- **2026-11-22 — Provider plan usage like T3 Code, Melissa calling the providers' usage endpoints itself**
  (owner decision: "everything like T3", accepting that the endpoints are undocumented and may break, and the
  providers' terms). A data layer (`melissa_agent::provider_usage`) and an owner-only JSON endpoint
  (`GET /api/usage/providers`); the panel is designed later. One provider-agnostic model: plan, windows (used %,
  reset, length, model scope, pace = elapsed share of the window), credits, cost, tokens per model, and a status
  per source. **Where numbers come from** (researched from T3 Code, MIT, and the installed CLIs; no code
  copied): Claude's `rate_limit_event` and `result.modelUsage` from running chats (until now dropped), and the
  `get_usage` control request (`skip_behaviors: true`; experimental in Claude Code's SDK schema, read
  defensively) in a short prompt-less session like the model probe; Codex's own session files (`token_count`
  with `rate_limits`), since its ACP adapter passes no rate limits; Grok's ACP extension `_x.ai/billing` (checked
  against Grok CLI 1.0.92: it exists and wants a sign-in) instead of T3's direct call with the sign-in token,
  so Melissa never reads Grok's token; Gemini CLI's Code Assist `loadCodeAssist` + `retrieveUserQuota`, and
  OpenCode Go's `zen/go/v1/usage`, called by Melissa with the agents' sign-in (Google access token, Go key). In
  the first three the agent makes the call inside its sandbox. **Rules:** counted, never estimated (no LiteLLM
  price download, no cost from tokens); each source fails alone ("unavailable" with a reason) and hangs or
  panics are contained (own task, 45 s limit); providers not installed or not signed in are "off" and nothing
  is asked; calls only when the data is asked for, reused for 60 s (a forced refresh for 15 s), never in the
  background; Melissa's HTTPS client refuses every host but `cloudcode-pa.googleapis.com` and `opencode.ai`,
  follows no redirects, times out after 10 s; tokens are redacted types, sent only as a bearer header to their
  provider, never logged, stored, renewed or shown. Hosts and timing are in PLAN (6. Web UI, "Provider plan
  usage"). **Not built:** Muse (its numbers come only over its own protocol, not `muse acp`); OpenCode 2's local
  server (unreachable inside the sandbox) and credential database (SQLite); ACP's draft end-of-turn token usage
  (the schema contradicts itself on per-turn vs. per-session); renewing Google tokens (would mean shipping Gemini
  CLI's OAuth client); Claude's reset credits and Codex's credit grants (T3 extras). Alternatives: Melissa
  reading Grok's and Claude's tokens and calling their hosts directly (T3's way; more token handling for the
  same numbers), background polling (owner: on open/refresh only), estimating cost from token counts (invented
  numbers).
- **2026-11-22 — Moving between pages swaps only the main column** (owner: "we should not need full page loads
  in general if we can avoid that"). A link to a page with the sidebar (`/`, `/chat`, `/agents/<id>`, `/chats`,
  `/chats/<id>`, `/approvals`, with or without a query) loads with htmx into `#main` and pushes its address:
  wherever the link is (the sidebar, the phone's agent list and back button, a chat card, the details panel, a
  panel in the modal). The request carries `HX-Target: main`; `crate::panel` notes it and `pages::shell`
  answers with the page's `<title>`, the main column's content and the sidebar's agent groups as an
  out-of-band update (the selected agent and the latest states, as the live stream sends them), with `Vary:
  HX-Target`. Every other request (a load, a reload, no script) gets the whole page, so deep links, reloads and
  pages without script are unchanged. The sidebar is not re-rendered: its scroll, collapse, search and open
  groups stay. melissa.js (`openPage`, one controller beside the panels' own) keeps one history model with the
  panels: page entries `{page}`, panel entries `{panel, depth}`; Back and Forward between pages swap again
  (the phone's agent list comes back where it was scrolled); a page link inside a panel closes the panel and
  its history entries first; closing a panel opened by a deep link opens `/` the same way (was a full load).
  The chat's stream goes with the old content (htmx's cleanup closes its EventSource) and the new chat opens
  its own; page swaps settle at once (`settle:0ms`) because htmx processes new content as it settles, and
  with the default 20ms a quicker next click could replace a chat before its stream opened, which then opened
  for a chat already gone. A newer click aborts the request in flight. After a swap: the composer, its draft
  and files, voice button and details panel are set up again (the width listener is bound once), the chat
  scrolls to its end, the older-messages watcher restarts, and focus goes to the page's heading (no ring),
  to the details sheet when it is open over the chat, or on the phone's agent list to the agent just left.
  Anything that does not come back as part of a page (an error status, a whole document such as the sign-in
  page or the task pages, a network failure) loads as a whole page instead. Still whole loads: form posts
  that redirect (creating a helper, Fresh start, onboarding steps, the provider pages), the sign-in pages,
  task pages, and any link while a voice message is being recorded (the load ends the recording, as before).
  Fixed on the way (also on master before): the phone's agent list no longer opens scrolled to its end (the
  chat it hides was scrolled), and a details sheet remembered open no longer makes that list inert. Why not
  `hx-boost`/`hx-push-url`: htmx's history keeps snapshots of the swapped part in localStorage (chat text in
  storage) and restores them stale with a dead stream, its Back/Forward would fight the panels' own history
  entries, and boosting a container also boosts its forms. A small controller over `htmx.ajax`, like the
  panels', keeps links plain `<a href>` (no per-link attributes, agent-written same-origin links included).
- **2026-11-22 — A chat opens at its end from the first frame; earlier messages page by 60 without script**
  (owner: "for a moment it flashes the top of the conversation and then scrolls down"; "I hope we're not
  rendering the entire conversation"). **Opening at the end:** the composer has `scroll-initial-target:
  nearest`, so the browser starts the chat's scroller (the chat column above 1200px, the main column above
  760px, the page on phones) at the composer during the first layout, before any script (Chromium 133+).
  Where the property is missing (Firefox and Safari today) the chat's messages stay invisible
  (`@supports not` + `@media (scripting: enabled)`) until melissa.js has scrolled once the page is parsed and
  set `data-chat-ready`; a 1.5s CSS fallback shows them if the script never runs. In-page navigation scrolls
  in the same task as the swap, so the first frame after it is already at the end. Checked in the browser at
  1440, 1024 and 390px: the scroller was at its end before the script ran (full loads) and at the next frame
  (swaps); with the property disabled, the chat was at its top but invisible until the scroll. Rejected:
  `flex-direction: column-reverse` (phones scroll the page, which can't be reversed; scroll positions turn
  negative under the lazy loading and auto-scroll code), a parser-blocking script after the chat (an extra
  request, and the first paint can still come before it), a `#end` fragment (in every URL).
  **Paging without script:** `?all=1` (every entry at once) is gone; the older marker's plain link is
  `?before=<n>`, a page of at most 60 entries with a "Load newer messages" link at its bottom (to the next page,
  or the chat itself). With script the marker still loads 60 at a time into the chat. **Audit** (synthetic
  100-turn chat, 2100 events, debug build): the server keeps at most 2000 events per conversation in memory
  (`EventLog::CAPACITY`; after a restart the last 100 turns, read once from a transcript capped at 4 MiB) and
  folds them per page or older-page request (page 52ms / 178 KiB, older chunk 32ms / 132 KiB, both for 60
  entries); the stream folds once when it opens and resumes from `Last-Event-ID` without replaying; Markdown is
  rendered only for the entries sent (60 per page, changed ones over the stream). In the browser, all 219
  entries loaded: 11,450 elements, 10 MB heap, a full relayout 32ms (9.5ms for the first 60). Open items in
  PLAN/Open questions: entries beyond 2000 events drop out of a long live session and older loads count by
  index (a key would survive eviction); `content-visibility: auto` on entries made a relayout 1.3ms but clips
  focus rings at an entry's edge and needs scroll anchoring (not in Safari); archived chats (`/chats/<id>`)
  still render whole.
- **2026-11-22 — Live updates show each person only what they may see; a device rename is live** (merging the
  live settings and details panel with the Apps list by person). A chat's stream keeps running after its request
  ended, outside the sign-in scope, so it captures who is looking when it opens and builds every live update of
  the details panel for that person: a member's panel never gets an app that isn't theirs, the same rule as the
  page, `/api/details` and the Apps list. The panel's device card now takes the owner's device name too, so a
  rename in Settings → Updates reaches open chats at once: header, sidebar and details panel. Alternatives:
  reading the person again on each update (there is no request then), sending live updates only to the owner
  (members' panels would go stale).
- **2026-11-22 — Design-system answers: today's dark palette, two dark-mode fixes, limits without pace.** The
  owner chose, after the Penpot design system (boards A1–A8, B1–B4, C1–C3 and specimens DS-00–DS-17) was done:
  (1) the dark theme keeps today's melissa.css colours (blue primary button), not the bluer, lighter palette of
  the owner's earlier Figma file; (2) dark mode gets one lighter step for hovered and selected rows, ghost-button
  hover and similar fills, because the spec mapped them to the page background and they could not be seen, and
  disabled text gets a dimmer grey than muted text so disabled controls read as disabled; (3) provider usage
  limits show the share left, a bar and the reset time, with no pace marker ("On pace"/"Ahead of pace").
  Where the component spec and the audit disagreed, the spec's names stand (`danger-quiet`; tones neutral, info,
  ok, attention, caution; switch 32×18), and component sizes use the `button.height.md` form. Alternatives: the
  Figma dark palette (a bigger visual change on demo day), keeping the spec's invisible hover in dark, the pace
  marker (more to read for a number most people only glance at).
- **2026-11-22 — Provider limits in Usage and resources** (owner design "Usage" B1–B4, approved today; no pace
  marker, today's colours). The panel gets a **Limits / Cost / Tokens** switcher. It is navigation, so per
  COMPONENT-SPEC 2.6 it is links with `aria-current="page"` in the segmented look (the Apps filter's), never a
  tablist: plain links `/usage?view=…` without script, with htmx they swap the view region (`#usage-view`).
  **Limits**: one group per provider that is set up (logo or letter mark as elsewhere; providers not installed
  or not signed in are hidden), a card per plan window: label ("Session" for a model-wide session window, else
  the provider's label such as "Weekly · Opus"), the share left in big type with "left", a bar, and the reset
  ("Resets in 2 h 10 min" within a day, "Resets Mon 09:00" within a week, else a date). The bar is a native
  `<meter>` (with `aria-valuetext="62% left"`) that fills with the share **used**, like the boards and the
  devices' meters. **Tones**: ok while more than a third is left; attention at a third or less (about two days
  of a weekly window, enough notice to plan); caution at a tenth or less (about one long session) or when the
  provider says the limit is reached (`rejected`; `allowed_warning` makes it at least attention). The meter's
  native `low`/`high` ranges match these (a test walks every value). A provider with no windows shows one
  "Unavailable" card with a one-line reason, or a quiet "<provider> doesn't report limits." row; the data
  layer's reasons are mapped to short UI words ("Sign in again to read the limits.", "Numbers come with the
  next chat.", …) and anything unrecognised becomes "Couldn't read the limits." (no raw error text). The agents
  table and the devices stay below the limits. **Cost**: by provider (as reported, else "Not reported") and by
  agent; **Tokens**: per provider a table of tokens per model, then each agent's context. Credit pools are not
  shown yet (their units are unknown, PLAN Q91c). **Freshness**: a page never waits for the providers. With a
  report younger than the collector's 60 s cache (new `Collector::cached`, which neither asks nor waits) the
  numbers are in the page; else the providers' part loads itself after the page (`hx-trigger="load"`), and
  without script it says "Not checked yet." next to the Check again form. "Checked N min ago" comes from the
  report's new `checked_at` (when the round started). **Check again** is a POST form (`/ui/usage/check`;
  without script it answers with a redirect to the page) whose htmx swap replaces only the providers' part
  and updates "Checked …" out of band, so the button keeps focus; `hx-sync="this:drop"` drops presses while
  one runs. The collector's own limits make repeated presses cheap: one round at a time, and a refresh reuses
  a round younger than 15 s, so at most one round of provider calls per 15 s however often it is pressed; no
  background polling. **Owner and Member**: the providers' numbers are the owner's only, like
  `/api/usage/providers` (they are read with the agents' sign-ins): `/ui/usage/*` is an owner path, and a
  member's `/usage` is the page as before (agents table and devices; the agents table's rules are unchanged),
  with no switcher and no provider data. Buttons are the 32 px `.btn` (44 px hit area only under
  `pointer: coarse`). Phones (B3/B4): the cards stack, Check again sits above a full-width switcher with equal
  segments; the button keeps its label (the board shows an icon only). The screenshot harness has a
  `fake-usage` feature (`MELISSA_FAKE_USAGE`, like `fake-apps`) and shoots `usage`, `usage-cost`,
  `usage-tokens` and `usage-member`. Alternatives: a tablist (the spec reserves it; these are views with
  URLs), a radio group (a value, not navigation), the bar filling with the share left (the boards fill with
  what is used), rendering the page only after asking the providers (up to 45 s), showing members the
  providers' limits (the data layer keeps them owner-only).

- **2026-11-22 — Settings on a phone: a list, then one section at a time** (owner design C1/C2 in Penpot,
  Android style). Under 760px (the phone layout's breakpoint) Settings no longer shows its sections as a row of
  tabs above the section. `/settings` is level 1: the title, the search field and the groups (Agents, Chat,
  System, People) as cards of rows; each row is a link in a real list with its icon, name, a short value where
  one is cheap to know ("3 ready", "2 rules", "Off", "5 days", "Tailnet", "Connected", "Up to date") and a
  chevron, 44px high; Reset is in the danger colour; the list has its own close control. Any other Settings page
  is level 2: the section alone under a back link (labelled "Back to Settings", or "Back to Agents" on a page
  below a section) and its title, which takes focus on entering; the section's own heading is hidden there so
  the title isn't said twice. Above 760px nothing changes: the same markup carries the phone parts, hidden by CSS,
  and `/settings` shows the first section this person may open beside the list (it was a redirect to it), so the
  section swap, the search and live settings work as before on both levels. Back: the section swap is unchanged
  (only `#settings-main` is replaced, no fades, URL pushed); going back to the list is instant (the list was only
  hidden), restores its scroll and puts focus on the row just left, while the list's answer replaces the hidden
  section. Each panel history entry notes the panel page below it (`back`), so the back link calls
  `history.back()` when that is where it leads (the link and the Back gesture then do the same, no entry is
  added) and otherwise replaces the current entry. A deep link to a section on a phone gets `/settings` as the
  entry below it, so Back goes to the list, then out (a reload keeps what was below). Without script the rows
  and the back link are plain links. The values are worked out only for `/settings` (a task-local, like the
  panel flags); slow sources are not asked (Tailscale's cached state only). Not taken from the board: the bottom
  tab bar under the list (the panel covers the phone's screen; the list's close control stands in) and the
  Providers rows with chevrons instead of Details (a separate change to that section). Alternatives: deciding the
  level in the server from the user agent (guesswork, breaks on resize), a separate `/settings/list` URL (a
  second address for the same page and a redirect loop without script), keeping the redirect and building the
  list in script (no list without script).
- **2026-11-22 — The design system in code: tokens from Penpot with a drift check, one Button, thin controls**
  (owner: the Penpot design system DS-00–DS-17 is approved; implement it). `melissa.css`'s `:root` now holds the
  design library's tokens under their Penpot names (`a.b.c` = `--a-b-c`, primitives renamed to the library's
  `neutral`/`night`/`blue`/… steps); `crates/web/design/tokens.json` is the export, written by
  `scripts/tokens/tokens_gen.py` (`just tokens`), and `crate::tokens` fails when the two disagree: a token on one
  side only, or a different value in either theme after resolving aliases (docs/DEV-ENV.md, Design tokens). The
  stylesheet stays hand-written (no build step); names only it has are listed in `tokens::CSS_ONLY`. The two
  dark-mode fixes are semantic tokens: `color.bg.hover`, `color.bg.selected` and the new `color.bg.fill` (neutral
  badges, disabled filled buttons) are `night.850` in dark, one step lighter than a card; `color.text.disabled`
  is dimmer than muted text in both themes (`neutral.500` / `night.600`). Components: `components::Btn` is the
  one button, rendered as classes (`.btn` + variant + size + shape) so the same look fits `<button>`, `<a>`,
  `<summary>` and `<label>` with their own attributes; every button, link-as-button, icon button and confirm
  summary uses it (32 px, 28 small, 40 large, radius 8/6, the hit area 44 px only under `pointer: coarse`).
  Fields (`.input`, `.search-field`, `span.select-wrap`) are as high as the buttons; native checkboxes and radios
  take the accent colour; the switch is 32×18 and blue when on; one segmented control (`.segmented`/`.seg`) serves
  the picker, the Apps filter and the Usage views; `Tone` is Neutral/Info/Ok/Attention/Caution and one badge
  (`badge`, `status_badge` with a dot or spinner) replaces the pills; rows, cards, insets, callouts and notices
  read the row/radius/space tokens; the remaining size literals and old colour aliases were swept to tokens.
  Kept as aliases (in `CSS_ONLY`): `--ground`, `--surface`, `--ink`, `--muted`, `--border`, `--honey*`,
  `--action*`, `--caution*`, `--ok`, `--idle`, `--bubble*`, `--focus`, `--backdrop`, `--shadow-float`,
  `--icon-*`, `--radius`, `--font`, `--font-head`, `--mono`: no rule here reads them any more, but branches in
  flight may; remove them once those have landed. Not changed (no behaviour changes): the picker's
  Speed/Access stay submit buttons with `aria-pressed` (the spec's radios would change how they post),
  `notice()` keeps `role="status"` at page load, a locked switch keeps its reason in `title`, triggers of
  destructive confirms keep the secondary look (the spec's `danger-quiet` exists for when the owner wants it).
  Alternatives: generating `:root` from `tokens.json` (a build step), struct components with every attribute
  (`Button { does, hx, … }`, a large API for ~80 call sites with different htmx attributes), keeping the old
  names (two vocabularies).
- **2026-11-22 — Without k3s, the cluster and app tools give the lead the words to pass on (`app_platform`).** The
  server tells two cases apart: no kubeconfig and no `/run/melissa-kube` (from source, a desktop: k3s *not
  installed*) and k3s set up but its API not answering (credentials pending, or the API down). Read tools add
  `app_platform` to their result: "not installed on this device: this copy of Melissa runs without the app
  platform (k3s) … Apps need the full install, the container or NixOS (see docs/QUICKSTART.md). Chat, helpers,
  schedules, files and describing this device still work." or "installed but not answering right now … Melissa
  keeps trying"; write tools fail with the same words. One prompt rule: pass `app_platform` on in two or three
  short sentences, never give helpers or tool checks as the reason. The UI keeps its words ("isn't set up on this
  device", "isn't answering right now. Melissa keeps trying."). Why: the lead (Grok from source) said only "not set
  up, no helpers", a dead end. Alternatives: long prompt prose (every provider must read it; a tool result is
  where the fact lives), a new `ClusterError` Display (also shown in the UI's workspace errors).
- **2026-11-22 — The container installer lets podman's network through Docker's firewall (`DOCKER-USER`).** On a host
  that also runs Docker, its iptables `FORWARD` policy is DROP and only its bridges are accepted; netavark's nftables
  accept ends only its own hook, so Docker's chain still dropped every packet of Melissa's container (no DNS, agent
  installs failed with "Could not resolve host"). install.sh detects Docker (its `DOCKER-USER` chain with a DROP
  policy, or `docker.service`/`docker.socket` running or enabled) and installs `melissa-docker-forward.service`:
  `WantedBy=`/`After=`/`PartOf=docker.service`, an `ExecCondition` on the chain (no Docker: skipped, boot is
  fine), and two idempotent rules (`-s` and `-d` podman's subnet, read from `podman network inspect` at install
  time, `-d` also for the published app ports). `install.env` records the subnets; uninstall.sh removes the unit
  and those rules only then. `DOCKER_FORWARD=no` prints the commands instead. After the start install.sh looks up
  `example.com` inside the container (short timeouts, `NET_CHECK=no` skips it: one DNS lookup, nothing sent) and
  warns with likely causes; melissa-setup checks the download host's name (5 s) before downloading and stops at
  once ("This container can't reach the internet") instead of curl retrying a lookup four times. Why: Docker's
  documented place for user rules is `DOCKER-USER` and it never flushes it; firewalld zones (`trusted`) don't help
  because the drop is in Docker's own chain. Alternatives: Docker's `"ip-forward-no-drop"` / `"iptables": false`
  (changes Docker for everyone), nftables rules of our own (an accept can't override another base chain's drop),
  a hard-coded 10.88.0.0/16 (wrong for a changed podman network).
- **2026-11-22 — Settings → About, the last section, for everyone signed in (owner).** Its own group "Melissa" at
  the end of the list (the phone's row shows the version). It shows the mark, the name, the version and revision
  (the same `Release` values as Updates; no tagline, owner), the pre-alpha line, and three links that open a new
  tab (`new_tab_button`: `rel="noopener noreferrer"`, "(opens in a new tab)" for screen readers): Sponsor with a
  new `Glyph::Heart` stroked in the rose token, Source code and Report a problem (the public GitHub pages). Below,
  in disclosures: the MIT `LICENSE`, the whole `ACKNOWLEDGMENTS.md` as credits (`markdown_document`: the safe
  Markdown tree with wrapped lines joined), and the bundled fonts', htmx's and logos' licenses. All are
  `include_str!` in the binary, so the Containerfile's build stage and the Nix package's source set now include
  `LICENSE` and `ACKNOWLEDGMENTS.md`. Where the image ships the cargo-about report
  (`/usr/share/licenses/melissa/THIRD-PARTY-LICENSES.md`), a row opens it as plain text
  (`GET /settings/about/rust-crates`, new tab); Nix and source installs have none, so the row is absent. Why: the
  owner asked for license, credits and a sponsor link in the app; nothing remote (no GitHub button or badge), so
  it works offline. Alternatives: the device menu (no room for the texts), one subpage per text (more clicks,
  more routes), a hand-written credits list (would drift from ACKNOWLEDGMENTS.md).
- **2026-11-23 — Every agent sandbox runs under a seccomp deny-list (bwrap `--seccomp`), on by default.** Denied
  syscalls return `EPERM` (not kill) so CLIs degrade instead of crashing; `clone3` returns `ENOSYS` (libc falls
  back to `clone`, whose namespace flags the filter checks). Melissa writes the classic BPF itself in safe Rust
  (one program for x86_64 and aarch64, tested with an in-test BPF interpreter and against libc's numbers);
  `command-fds` hands it to bwrap as fd 3 from a sealed memfd. Other host architectures: no filter, a warning
  (refusing would leave only "sandbox off"). `[backend.sandbox] seccomp = false` turns it off per backend. Why:
  no agent CLI needs these calls, and they are where kernel escapes start. Alternatives: `seccompiler` (one arch
  per program, a crate for ~150 lines), libseccomp (C library), clearing `FD_CLOEXEC` with rustix (leaks the fd
  into every process spawned at the same time), an allow-list (breaks CLIs when they update).
- **2026-11-23 — Agent sandboxes get resource limits through systemd cgroup delegation, placed before they run.**
  `melissa-server.service` has `Delegate=memory pids cpu io`; at startup the server moves itself into a `server`
  leaf (cgroup v2: no processes in a cgroup whose controllers are enabled for children), enables the controllers
  and creates `sandboxes/`. Each sandbox gets its own child with `memory.max` 2 GiB, `memory.swap.max` 0,
  `pids.max` 512, `cpu.max` 2 CPUs (per backend: `[backend.sandbox.limits]`), written as plain files in safe
  Rust. bwrap runs with `--block-fd`: it makes its namespaces and waits before it forks or executes anything
  inside; the server writes bwrap and its one child to `cgroup.procs`, then lets it go on. On exit:
  `cgroup.kill`, `rmdir`, and a log line if the OOM killer or `pids.max` struck. No delegation (from source,
  cgroup v1): agents run unlimited, logged once and shown in the startup summary. Why: one runaway or hostile
  agent must not starve the box; Node CLIs need ~0.5-1.5 GB, so 2 GiB leaves room for their tools. Alternatives:
  `systemd-run --scope -p MemoryMax=` per sandbox (needs the user bus or root and a systemd per deployment, a
  process per spawn, and still races the first fork), `clone3(CLONE_INTO_CGROUP)` (needs unsafe), moving bwrap
  after it starts without a gate (its child may already be outside), `setrlimit` (per-user, not per-sandbox,
  no memory accounting for children).
- **2026-11-23 — `[backend.sandbox]` changes the CLI's preset instead of replacing it.** The preset is the base;
  `enabled` and `seccomp` replace its value when given, `limits` key by key (`LimitsOverride`), and `tmpfs`, `ro`,
  `rw`, `env` add to its lists (no duplicates). `preset = false` starts from an empty home instead (the only way
  to drop a preset path, a narrowing). An empty list is refused: under the old rule `rw = []` meant "no
  credentials", and silently ignoring it would now widen that config; the error points at `preset = false`.
  `allow` (a backend key) still replaces the built-in host list: replacing is how it narrows, and widening it
  needs hosts written out. A sandbox set in code (`with_sandbox`, tests, provider variables) still wins whole.
  Why: a table with only `limits` or `seccomp = false` dropped the credential binds and state tmpfs, so the CLI
  lost its login (a footgun noted in the docs). Behaviour change: a config that narrowed by leaving preset paths
  out of a non-empty list now gets them back; such configs need `preset = false`. Alternatives: lists replace
  (adding one path means repeating the preset, which drifts when presets change), a separate `extra_ro` etc.
  (more keys, same effect), a per-entry removal list (more surface than one switch).
- **2026-11-23 — The server checks `vm.panic_on_oom` at startup and keeps the memory limit on.** Kernel
  semantics (admin-guide/sysctl/vm.rst): 0 never panics; 1 panics only on a system-wide OOM, an OOM confined to a
  memory cgroup just kills; 2 panics on every OOM, the cgroup's included. So 2 is dangerous always (an agent at
  its `memory.max` takes the host down), 1 only when agents run without limits (a hog then causes a system-wide
  OOM). Then the `Limits:` line says PROBLEM with the fix (`sysctl -w vm.panic_on_oom=0`, plus a sysctl.d file)
  and the log warns once. The limit stays on: without it a hog still ends in an OOM (a panic under 1 and 2, the
  OOM killer picking k3s or the server under 0), the pids and CPU limits are unaffected, and quietly loosening
  the sandbox because of a host setting would be a surprise. Alternatives: drop `memory.max` under 2 (an agent
  could then use all memory before the same panic), refuse to start (too harsh for a host the owner may not
  control), set the sysctl ourselves (needs root; not the server's business).
- **2026-11-23 — Every agent sandbox runs as its own host user, from the server's subordinate ids (agent users).**
  Owner-approved design, the rootless-podman way (`crates/agent/src/sandbox/users.rs`, PLAN section 5
  "Agent users", open question 97). Choices:
  - *Namespace:* Melissa prepares the user namespace; bwrap never makes one. A holder (`unshare --user cat`)
    creates it, `newuidmap`/`newgidmap` map root to a per-agent *setup id*, the agent's inside id (1000, or the
    next id the server's own ids leave free) to the agent's host id, and the server's own uid/gid to themselves.
    `nsenter --user=/proc/<holder>/ns/user --setuid 0 --setgid 0` then execs bwrap (explicit `--unshare-*`, no
    `--unshare-all`) as root of that namespace: setup has capabilities there and, with the server's ids mapped,
    can reach the server's 0700 directories it binds; `nsenter`'s `setgroups(0)` drops the server's
    supplementary groups (`melissa`, `melissa-kube`: the kubeconfig group). bwrap keeps only `CAP_SETUID`,
    `CAP_SETGID`, `CAP_SETPCAP` (`--cap-add`) for `setpriv`, the first program inside, which becomes the agent
    (`--reuid/--regid 1000 --clear-groups --bounding-set -all --inh-caps -all`). The holder is dropped once
    bwrap has joined. Rejected: bwrap `--userns FD` with `--uid` (works, but the server's supplementary groups
    stay with the agent: bwrap never calls `setgroups`, and a process in the sandbox with those groups could
    read the kubeconfig after an escape); bwrap `--userns-block-fd` (bwrap then never changes uid: the agent
    would stay the server's user); the agent as root of its namespace (simplest, but uid 0 inside, a full
    bounding set, and CLIs that refuse root); `unshare --map-users` (util-linux >= 2.39 syntax only).
  - *Allocation:* two ids per agent (setup + agent) from the range, by file box key (`lead`, agent id;
    `shared` for runs without an owner), in `<state>/agent-users.json`, never reused (agent ids are never
    reused either; 32768 agents per 65536 ids). The table records the range starts; a changed range turns the
    feature off rather than handing old files to new owners. One id per sandbox run was rejected: files would
    outlive their owner id.
  - *Files:* POSIX ACLs written directly as `system.posix_acl_*` xattrs (`sandbox/acl.rs`, rustix; no `setfacl`,
    no `unsafe`). The workdir, file box and session store get `u:<agent>:rwx` plus a default ACL naming the
    agent and the server (owner/group/other `---`), applied to the existing tree once (deepest first, never
    following symlinks, only the server's own entries) and to new direct entries each run; credential and
    config files bound from `$HOME`, the CLI binary, extra files and the bridge's sockets get a grant on the
    file itself when "other" does not already cover it; the setup id gets search on the workdir (bwrap's
    `--chdir`). Other agents are named nowhere, and every box is 0700. Rejected: chown to the agent through a
    helper namespace (the server could then no longer read results without that helper), a shared group with
    setgid directories (every agent would share it). Deleting an agent removes what it locked through a
    namespace where the server is root and the agent's id is mapped (`rm -rf` as that root).
  - *Inside:* bwrap makes missing parents of a bind 0700 and owns its tmpfs mounts, both as the setup id, so
    each bind gets its parent as a `--dir` (0755) first, `/tmp` and `/var/tmp` are `01777`, and a CLI's state
    tmpfs is `0777` (not sticky: `fs.protected_regular` would stop the CLI rewriting a credential file bound
    into it). `/etc/passwd` and `/etc/group` are the host's with `agent` (uid/gid 1000) added, so git, `id`
    and Node's `os.userInfo()` find the user; `safe.directory=*` comes through `GIT_CONFIG_*` (the server's
    workdir is another user's inside).
  - *Off:* without subordinate ids, the helpers (`newuidmap`, `newgidmap`, `unshare`, `nsenter`, `setpriv`),
    ACLs on the state directory, when the server is root or has `no_new_privs`, when a trial mapping fails, or
    with `MELISSA_AGENT_USERS=off`. Then sandboxes run as before, with one warning and `Agent users: off:
    <why>` in the startup summary. `melissa-server.service` no longer sets `NoNewPrivileges=`: no_new_privs
    disables newuidmap's file capabilities. The server runs no other setuid program, `melissa` has no sudo or
    su rights, and every agent still has no_new_privs from bwrap. Both images give `melissa`
    `1879048192:65536` (0x70000000, a block systemd's id layout leaves unused). In the container image
    `newuidmap`/`newgidmap` are setuid root (shadow's classic install mode): Fedora's file capabilities do not
    survive a rootless image build (found by `just escape-test-image`: "write to uid_map failed"); NixOS uses
    its own capability wrappers.

- **2026-11-23 — Settings slimmed: five owner decisions (owner, all as recommended).** Brief:
  [design/SETTINGS-AUDIT-2026-11-23.md](design/SETTINGS-AUDIT-2026-11-23.md); boards: Penpot "Settings v3" (exports
  `settings-v3/`, D1–D7, P1, P2). (1) **The section's title is only in the head bar** on every width: no
  breadcrumb, no big h1, no repeated lead (`settings_frame_at`'s `h1.settings-here`; a page below a section reads
  "Agents / Backup"). The head bar's right side holds the section's tools ("Checked just now · Check again", Add
  provider, Add schedule, New helper, Check for updates; `SettingsHead`, refreshed after a swap by
  `settings_tools_oob`). (2) **A section's description is at most one short line** above the first card
  (`SettingsHead.lead`, hidden on phones), and only where it adds something. (3) **Big forms open behind a
  button, in place**: Add schedule, Change password and Developer → New task are links to the same page with the
  form open (`?add=1`, `?password=1`, `?task=new`; Cancel links back), so they work without script and never use
  a browser dialog; a refused save keeps the form open. The form is a full-width row (`setting_row_full`), fields
  at most 24rem, its submit the card's one primary button. (4) **A helper's settings are one card**: Name (inline
  field + Save), Icon (a row that opens the picker), Pinned (a switch), Fresh start, Delete helper; when it was made
  and what it runs on go in a quiet line below. (5) **Rows are 40px with one line and 56px with a description**
  on desktop, controls 32px, disclosures 32px (44px under a finger); the phone's Settings list rows are 56px and
  every row has a value. With them, the audit's fixes: one line per boundary (no disclosure line inside a card,
  an extra part or a detail pane; rows inside a region get their divider), one card style (`setting_card`
  everywhere in Settings, tables and forms inside it), the split card stacks by its own width (a container query,
  below 640px: Providers at 768px), People's row actions behind one "⋯" (`row_menu`, the agent menu's native
  popover), Delete/Remove/Sign out all in the quiet caution style (`action_button` takes the look), About in two
  cards with "Show" rows (`setting_row_details`), inline code in Markdown wraps (About's credits scrolled sideways
  at 390/320), and Settings opens with focus on the title, so the nav shows no ring. Why: the owner found Settings
  "way too fat" with "double lines"; sections are 40–90% shorter (e.g. Account 826→260px, Schedules 1090→134px,
  a helper 861→285px at 1440). Alternatives: a modal `dialog` for the forms (needs script; the owner asked for
  the form in place), keeping the h1 and dropping only the breadcrumb (the title twice, 80px more above the
  first card), buttons beside each person on desktop and "⋯" only on phones (two markups for one row).

- **2026-11-23 — Modern Linux hosts only (owner).** Melissa supports Linux hosts with systemd as init, cgroup v2,
  a current kernel (user namespaces, seccomp), nftables and KVM (owner: "don't worry about kvm, it exists
everywhere modern hardware is"); nothing else. Why: "modern Linux devices with modern
  Linux protocols and init systems like systemd tend to be the most secure systems", and the sandbox layers
  (seccomp, cgroup delegation, agent users, gVisor) depend on them. Consequence: fallbacks kept only for older
  hosts may become hard requirements in the container and NixOS installs; the from-source run keeps its warnings.
  Alternatives: best-effort support for other init systems or cgroup v1 (more code paths, weaker defaults).

- **2026-11-23 — gVisor is the first extra isolation for apps (owner: "let's try gvisor").** Prototype gVisor
  (`runsc`) as a k3s RuntimeClass chosen per app, per docs/design/ISOLATION-RESEARCH-2026-11-23.md. Kata (needs
  KVM) second; Nix-built services and bare microVMs parked.
