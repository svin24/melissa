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
- **2026-11-18 — Desktop queen runs as a per-user systemd unit; headless as a system unit with DynamicUser.** Why: the
  agent CLIs and their logins are per user on desktops; servers need no login session.
- **2026-11-18 — VM testing with QEMU from the Nix dev shell + bootc-image-builder.** Why: no host packages needed on an
  immutable OS. Alternative: libvirt/virt-install on the host (not installed).
- **2026-11-18 — Code hosted in a private GitHub repo.** Owner approved; public release needs an explicit yes.
- **2026-11-18 — Base directly on Fedora's atomic images (Kinoite, fedora-bootc), not on Universal Blue or a custom
  kernel.** Why: Fedora's shim, kernel and modules are signed, so Secure Boot works out of the box with no MOK
  enrollment; owner requirement at least through the first demo. The build enforces it (one kernel, kernel-core
  signed by the Fedora release key, no `kmod-*`/`akmod-*`), and VM tests boot with Secure Boot on.
  Alternatives: ublue bases (own kernel/kmods signed with their key, needs MOK), sealed images with our own key (later).
- **2026-11-18 — Take ideas from Universal Blue (Apache-2.0), never code.** Notes: `~/.local/share/melissa/research/`.
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
- **2026-11-18 — Worker guards.** Workers are board tasks with the Queen as `parent`, started only through the
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
