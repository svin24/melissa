# Decisions

Format: date — decision. Why. Alternatives considered.

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
