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
  replaces the whole preset (paths), so changing only the network would mean copying the preset. Repeated
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
