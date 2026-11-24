# App deploys: any app from an agent's spec

**When:** touching the app tools (`my_apps`, `deploy_app`, `update_app`, `list_apps`, `app_status`,
`wait_for_apps`, `app_logs`, `app_exec`, `stop_app`, `start_app`, `uninstall_app`, `app_network`,
`set_app_network`), the app spec format, rendering, app ports and links, app network permissions
(NetworkPolicies), the write identity `melissa-apps` or its admission policies, the owner's
`app-credentials` command.
Code: `crates/appspec` (spec, validation), `crates/cluster/src/{render,apps,app_tools}.rs`,
`crates/cluster/src/render/network.rs`, `crates/cluster/src/app_tools/network.rs`,
`crates/server/src/credentials.rs`, `packaging/common/usr/lib/melissa/k3s/manifests/apps-rbac.yaml`.

There is **no catalog** (dropped 2026-11-19, docs/DECISIONS.md): agents choose images and settings
for whatever the user asks for. Safety is server-side: the validator, the renderer (which only
produces locked-down objects) and the admission policies, never the prompt.

## Split of duties
- Agents write the spec (`deploy_app {app, spec, agent?}`, `update_app {app, spec}`), never see
  generated secrets, hold no cluster credentials.
- The server validates (`AppSpec::validate`, also expands Docker Hub short names), asks the user
  (every deploy and update: `Effect::Deploy`, always confirmed, with a summary of images, ports
  and storage), renders and applies with the `melissa-apps` identity, stores the spec on the
  namespace (`melissa.io/spec`). containerd pulls the images when the pod starts.
- The owner gets a new app's setup password as a card in the chat where it was installed (reveal, copy,
  dismiss; server to browser only, DECISIONS 2026-11-21). `sudo melissa-server app-credentials <app>` shows every
  generated value on the device (a dismissed setup password is left out).

## Spec (JSON, every object `deny_unknown_fields`)
```json
{
  "description": "Minecraft Java server",          // 1-300 chars; "version" optional
  "containers": [{                                  // 1-6, one pod; sidecars share localhost
    "name": "server",
    "image": "itzg/minecraft-server:java21",        // tag or @sha256 digest; no tag/`latest` refused;
                                                    // short names -> docker.io/[library/]...
    "command": [], "args": [],                      // optional, replace entrypoint/args
    "env": {"EULA": "TRUE", "MAX_PLAYERS": 10,      // string, number, bool, or
            "RCON_PASSWORD": {"secret": "rcon"},    //   a generated secret (secretKeyRef)
            "HOSTS": {"builtin": "device_hosts"}},  //   ingress_host | ingress_url | device_hosts
    "ports": [{"name": "game", "port": 25565, "protocol": "tcp", "public": true}],
    "mounts": [{"volume": "data", "path": "/data"}],
    "files": [{"path": "/etc/x.conf", "content": "...", "executable": false}], // <= 8 x 16 KiB, read-only
    "resources": {"cpu": "2", "memory": "1536Mi"},  // limits; default 1 / 512Mi; requests default
                                                    // min(100m, cpu) and memory/4
    "probe": {"port": "game", "path": null,         // HTTP if path, else TCP; startup_timeout_secs
              "host_header": false},                // (300), period_secs (10)
    "run_as_user": 1000, "run_as_group": 1000       // optional
  }],
  "volumes": [{"name": "data", "size": "5Gi"}],     // PVC <app>-<name>, local-path (size not enforced)
  "secrets": [{"name": "rcon", "length": 32}],      // generated once, kept on updates
  "web": "http",                                    // port name of the web UI: link + Ingress
  "credentials": {"user": "admin", "password": "admin_password"}, // what app-credentials prints
  "kind": "games",                                  // the UI's group: games | media | home | files |
                                                    // network | developer | other (default)
  "isolation": "gvisor",                            // gvisor (default) | standard; see "Isolation"
  "network": {"internet": true, "lan": false,       // what it may reach (all off if absent), see
              "links": [{"app": "postgres", "port": "db"}]} // "Network" below
}
```
Limits: 6 public ports (web included), 16 ports, 8 volumes, 16 secrets, 128 KiB stored spec,
16 CPU / 64 GiB per container. Errors name the field (`containers[0].image: ... not pinned`).
The spec is checked before the confirmation is asked (`Tool::check_args`), so the user never
approves a call that fails on its arguments.

Why our own small JSON spec and not Kubernetes YAML (decided 2026-11-19): an LLM writes it
reliably (one shape, no Kubernetes boilerplate, no `securityContext` to forget), there is nothing
to strip (no privileged/hostPath/hostNetwork knobs exist in it), the confirmation summary is
exact, and updates diff cleanly. Unknown keys are refused, not ignored.

Example: the Nextcloud spec in `crates/cluster/tests/fixtures/nextcloud.json` (test-only; generated admin password,
`device_hosts` for its trusted domains, an entrypoint hook as a `files` entry, HTTP probe with
the ingress host).

## Isolation (gVisor, 2026-11-23)
`isolation` says how the pod is kept apart from the device's kernel (DECISIONS 2026-11-23,
docs/design/ISOLATION-RESEARCH-2026-11-23.md):
- `gvisor` (the **default**: every agent spec without the field): RuntimeClass `gvisor`, handler
  `runsc`. The app's system calls go to gVisor's user-space kernel (the Sentry, platform systrap);
  the device's kernel sees a small, filtered set. `/proc/version` in the pod is gVisor's
  (`4.19.0-gvisor`; `4.4.0` in older releases such as nixpkgs'), `dmesg` shows gVisor's banner. Costs memory (an idle busybox pod: 16.1 MiB in
  its cgroup vs 0.7 MiB under runc; the research measured ~30 MiB PSS for a sandbox), about 0.4 s
  more per pod start (1.7 s vs 1.3 s to Ready, image cached), and syscall-heavy work runs slower. The
  RuntimeClass adds `overhead` 48Mi / 50m to the pod's limits (64Mi limit → 112Mi pod cgroup).
- `standard`: an ordinary container (runc), sharing the device's kernel. For apps that need host
  devices (USB sticks, GPUs) or kernel features gVisor lacks (io_uring, raw sockets, some `/proc`
  files), and apps that fail under gVisor. Only a spec can ask for it: there is no per-app or
  per-image exception.
- `kata` (a VM per pod) is reserved for later; unknown values are refused.

There is no list of trusted images or registries, so the default doesn't depend on the image. The
user sees the level in the confirmation (`isolation: gVisor sandbox (its own kernel)` /
`isolation: standard (shares the device's kernel)`); agents ask for `standard` only with a reason
the user can judge. The deployer stores the level it used with the spec (`"isolation":"gvisor"` in
`melissa.io/spec`), so a later change of the default never moves a running app; specs deployed
before 2026-11-23 have no field and move to gVisor at their next update or bring-back (the
confirmation says so).

**Fail closed.** A `gvisor` deploy first reads the `gvisor` RuntimeClass (the write identity may
`get` exactly that one); missing or with another handler, the call fails with the reason and
nothing is applied. Admission policies hold it in the cluster too (`apps-rbac.yaml`):
`melissa-apps-workloads` refuses a Deployment whose template or namespace is labelled
`melissa.io/isolation=gvisor` without `runtimeClassName: gvisor`, `melissa-apps-isolation` refuses
such **pods** from any creator in app namespaces, and no other RuntimeClass is allowed there;
`melissa-gvisor-runtimeclass` keeps the class's handler `runsc`. Without the RuntimeClass the API
refuses the pod; with it but no runsc on the node, the pod never starts (`app_status` shows the
sandbox error). Never a fallback to runc.

**Where it comes from.** Container: the image ships runsc, its containerd shim and runsc's
`gvisor-bin/` sidecars (`/usr/lib/melissa/gvisor`, linked into `/usr/bin`; `gvisor.lock`), and
`k3s-prepare` installs containerd's `runsc` entry
(`/var/lib/rancher/k3s/agent/etc/containerd/config-v3.toml.tmpl`; k3s doesn't detect runsc) and the
RuntimeClass manifest at every start, only when runsc is on PATH. NixOS:
`services.melissa.k3s.gvisor.enable` (default on) puts nixpkgs' `gvisor` on k3s's PATH and writes
the same template and manifest. The startup summary says `gVisor: on (runsc <version>)` or
`off: <why>`. Options: `packaging/common/usr/lib/melissa/k3s/gvisor/runsc.toml` (platform
`systrap`; KVM measured ~30% faster on syscall loops but widens the host surface to KVM and is
poor when the host is a VM).

**Check it:** `just gvisor-test-image` (a throwaway container: both levels deployed through
`deploy_app`, kernels compared, cheap escape probes, the fail-closed paths, cost printed); the
NixOS VM test does the same on NixOS.

## Rendering (`render.rs`, pure)
Namespace `app-<name>` (labels `melissa.io/app`, `melissa.io/managed`, Pod Security
`enforce=baseline`; annotations spec, ports, workspace record) → RoleBinding → one Secret per
generated secret `<app>-secret-<name>` (POST, 409 = keep) → PVCs → ConfigMap `<app>-files` → Deployment `<app>` (1 replica, or
0 when stopped; `Recreate`; `runtimeClassName: gvisor` and the pod and namespace label
`melissa.io/isolation` per the spec's level; no SA token, no service links, seccomp RuntimeDefault, every container
`privileged: false`, `allowPrivilegeEscalation: false`, limits always) → ClusterIP Service (all
ports, TCP/UDP) → Ingress (web port only) → NodePort Service `<app>-port` (web + public ports) →
the namespace again with the new port records. Server-side apply, field manager `melissa`, forced.
An update deletes what the old spec had and the new one doesn't (Ingress, port Service, files);
volumes and secrets are never deleted by an update (`unused_volumes_kept`).

## Ports and links
- Each public port (and the web port) gets its own NodePort from `APP_PORTS` = 30000-30079, the
  lowest no app records (`melissa.io/port` for the web port, `melissa.io/ports` =
  `game:30001/tcp voice:30002/udp` for the others). A port keeps its number across updates,
  stop, keep-data uninstall; the web record never changes while the namespace exists (admission),
  so it stays reserved even if a later spec has no web port. New ports are recorded only after
  their Service exists; on "already allocated" other free ones are tried.
- Tools return `url`/`urls`/`port`/`dns_url` for the web port and `ports`: `[{name, protocol,
  port, node_port, web, address: "<addr>:<port>", url}]`. `port` = NodePort + `[apps] port_offset`.
- Web UI: the link uses the visitor's `Host` + port; non-web ports show
  `<host>:<port> (TCP|UDP)` on the tile (when there is no link) and in the details dialog.
- Reachability (`reach.rs`), in `deploy_app`, `update_app`, `start_app` and `app_status`. A
  control connect to the NodePort on the device's own address (in the container: its bridge
  address, k3s's node address) tells whether the app answers; if not: `app_not_answering`
  (starting, or listening on another port), links kept. The advertised addresses:
  - Container install (`MELISSA_PUBLISH_BIND`, the host's `BIND`): judged by the bind, not
    probed (from inside the container the host's published address never answers: no
    hairpin). An address the bind provably doesn't cover (e.g. `BIND=127.0.0.1` and a
    Tailscale address in `[apps] addresses`) is `unreachable`: dropped from
    `url`/`urls`/`ports[].address` with a `warning` for the owner. Covered: kept. Host names:
    kept, `unverified`.
  - Container install, app access (Settings → Apps, `melissa_settings::app_access`): before the
    bind check, addresses the filter in force doesn't let in are left out, under
    `reachability.access` (`open_to`, `setting`, `left_out`, `warning`, `problem`). The warning
    names the `set_app_access` level that would open them; the owner approves each widening.
  - Melissa's own Tailscale connected (Settings → Tailscale, `melissa_settings::tailscale`):
    the node's MagicDNS name and IPv4 address come last, at the NodePort itself (they reach
    the container directly: no `port_offset`, no bind check), and only under Tailnet or wider.
  - No bind known (Nix/host install): a TCP connect to every `address:port` (800 ms; open and
    timed-out results cached 15 s). A failure while the app answers is `unverified` with a
    `note`; the link is kept (a connect from the device isn't proof either way). Loopback and
    UDP are not probed (said so).

### Connection problems (agent guidance)
- Verify reachability (`app_status`: `reachability`, pods ready) before blaming the app. Never
  restart an app to fix a connection problem without evidence it is down. An address left out
  under `reachability.access` needs a wider app access: propose `set_app_access`. One left out as
  `unreachable` is an install setting (an explicit `BIND`, `[apps] addresses`): tell the owner
  the `warning`.
- An app's own normal log lines are not failures: vanilla Minecraft logs "Server empty for 60
  seconds, pausing" and idles until a player joins (pause-when-empty, `pause-when-empty-seconds`
  in server.properties); it still accepts connections. Don't restart for it.

## Network (2026-11-24, DECISIONS "Per-app network permissions")
Every app is isolated by Kubernetes NetworkPolicies (k3s's built-in kube-router controller; our k3s config
does not disable it). In `app-<name>`, always three policies (`render/network.rs`):
- `melissa-isolation`: default deny ingress and egress for all pods; allowed: the namespace's own pods
  (sidecars share the pod anyway) and DNS to kube-dns (kube-system, UDP/TCP 53).
- `melissa-ports`: ingress to the web port and `public` ports from anywhere (NodePort traffic reaches the pod
  from the node, SNATed; Traefik for the web host). Other ports: nobody outside the namespace.
- `melissa-egress`: the spec's grants. `internet`: 0.0.0.0/0 and ::/0 except private, CGNAT/tailnet,
  loopback, link-local, multicast, documentation, and NAT64 ranges and this device's own addresses.
  `lan`: 10/8, 172.16/12, 192.168/16, fc00::/7 except the pod and service networks (10.42/16, 10.43/16) and
  this device's addresses. Never reachable: the Kubernetes API, Melissa, other apps' device ports (all on
  this device's addresses), the tailnet (100.64/10), other namespaces without a link, kube-system except DNS.
  "This device's addresses" (`app_tools/network.rs::device_addresses`): local IPv4 (fib_trie) and IPv6
  (if_inet6), `[apps] addresses` that are IPs, and in the container the publish bind and the default gateway
  (the host, which publishes Melissa's UI port).
- Links: `{app, port?}` (port = the other app's port name; absent = all its ports). Egress in the source to the
  target namespace (matched by `kubernetes.io/metadata.name`) on the resolved port number, and in the target
  namespace `melissa-link-from-<source>` (label `melissa.io/link-from=<source>`) letting the source's namespace
  in on those ports. The deployer applies the app's own policies right after the RoleBinding (before any pod),
  the target sides after; deletes the target side of a dropped link (old spec vs new); re-applies links into
  an app when it is deployed again (`refresh_incoming`: scans the other apps' specs); removes an app's target
  sides on uninstall and force delete. A new link to a missing app or port is refused (`no app ...`, `has no
  port ... (its ports: ...)`); an existing one that no longer resolves stays in the spec, inactive, with a
  `network_warnings` entry.
- gVisor pods: traffic still leaves through the pod's veth, so the policies apply the same (verified
  with the smoke under runsc).
- RBAC: `melissa-app-workload` may manage `networkpolicies` (only in app namespaces, like the rest).

Editing:
- `set_app_network {app, internet?, lan?, add_links?, remove_links?}` changes only the stored spec's
  `network` (namespace annotation, merge patch) and re-applies the policies: no pod restart. A remove
  without `port` removes every link to that app. An agent's call always waits for the owner
  (`Tool::owner_approval`, no "Always allow", an agent's Full access can't answer it; approving needs the
  owner's recent password, `OWNER_REAUTH_TOOLS`); the card says it plainly ("Let vuls-web reach the
  internet; Let vuls-web connect to postgres on its port db"). Scope as `update_app` (an agent's own apps;
  the lead agent any; another agent's: the user's own yes). Audited `app.network` (actor, change, result).
- `deploy_app`/`update_app` with a spec that grants anything: also never coverable by "Always allow" for an
  agent; the card's summary ends with `may reach: ...`; the `tool.call` audit line carries
  `app.network <app>: may reach ...`. The user's own deploy (scope `User`) is not affected.
- `app_network {app}` (read-only): `internet`, `lan`, `summary`, `links` [{app, port, ports: [{name, port,
  protocol}], active, problem}], `incoming` (links other apps have into it, same shape), `link_options`
  [{app, ports}] (the other apps' named ports). `app_status` has `network` {internet, lan, links, summary}.
- Owner: `GET /api/apps/{app}/network` (the `app_network` view + `pending` [{id, tool, task, fields:
  internet|lan|links, reason}] for agents' requests waiting on this app; members: their own apps,
  read-only); `POST /api/apps/{app}/network` with the `set_app_network` arguments minus `app` (owner only,
  password entered recently; immediate, audited). CLI: `melissa-server app network <app> [--internet on|off]
  [--lan on|off] [--link <app>[:<port>]]... [--unlink <app>[:<port>]]...` (no flags: show).

Agent guidance: declare what the app needs in the spec at deploy time: `internet` for apps that download
things (game servers fetching their jar, package mirrors, updates, external APIs), `links` for a database or
broker in another app (a sidecar in the same pod needs nothing), `lan` only for apps that talk to devices at
home (Home Assistant, a media server's DLNA). The owner sees it on the card. Ask with `set_app_network` when an
app turns out to need more (`app_logs` shows DNS working but connections timing out).

## Tools and labels
| tool | labels | notes |
|---|---|---|
| `deploy_app {app, spec?, agent?}` | **Deploy** (always confirmed), target `app` (Create) | new app, or one uninstalled with data kept (spec may be left out: the stored one); refuses an app that is deployed ("use update_app"). An agent's new workspace is its own; the user may name `agent`. The lead agent may not create a new app (refused before any confirmation: it hands the job to a helper, 2026-11-20); it may bring back an uninstalled one. |
| `set_app_network {app, internet?, lan?, add_links?, remove_links?}` | Reversible, target `app` (Change), owner approval every time for agents | see "Network"; no restart |
| `app_network {app}` | read-only, target `app` (Read) | see "Network" |
| `update_app {app, spec}` | **Deploy**, target `app` (Change) | whole new spec (start from `app_status.spec`); keeps volumes, secrets, ports; a stopped app stays stopped |
| `my_apps {}` | read-only, no target (filters by `ctx.scope` in `call_in`) | **call first when a request mentions an app.** Agent: `yours` (essentials from `app_status`: `app`, `description`, `phase`, `image`, `url`, `ports` name/protocol/address, `reachability`, `assigned_by`, `assigned_at`) and `not_yours` (`app` + `owner` name only, null = unassigned; nothing else of them is read). Lead agent/user: `by_owner` groups (`owner`, `agent` id, `apps`), unassigned last. Cost: one namespace read per app, a status only for the caller's own apps (the lead: every app). Always in an agent's tool list. |
| `app_status {app}` | read-only, target `app` (Read: the owner, the lead agent, the user; another agent → user-only confirmation) | phase, pods (reason codes), `spec`, `description`, links, `ports`, `assigned_by`/`assigned_at` |
| `wait_for_apps {apps, until, timeout_secs?}` | read-only, no target (an agent: refused unless every existing app is its own, checked in `call_in`) | waits in the same turn until every app is `gone` (namespace removed; an unknown app counts), `ready` or `stopped` (`app_status` phases; an unknown app is an error). Polls every 3 s; timeout default and max 240 s (below Claude Code's 300 s tool-call limit, less the 60 s margin), then answers `done: false` with each app's `state` and the agent calls again. Cancel-safe: Stop drops the call (MCP `notifications/cancelled`). Feed: "Waited for apps". |
| `list_apps` | read-only | every app namespace (labelled list), same phases; for an agent, other agents' apps only as `{app, owner, not_yours}` |
| `app_logs`, `stop_app`, `start_app`, `uninstall_app` | unchanged | uninstall keeps data, secrets and the spec; `deploy_app` brings it back |
| `app_exec {app, command, container?, stdin?, timeout_secs?}` | **Destructive** (always confirmed, the card shows the exact argv), reads private + untrusted, target `app` (Change) | one command in the newest running pod's container (default: the first), argv only, no implicit shell; stdin optional (≤ 64 KiB, then closed); timeout 30 s default, 300 s max (on timeout: output so far, `timed_out`, the command may still run); stdout/stderr ≤ 64 KiB each, `*_truncated`, control chars stripped; returns `exit_code`. Audited: the `tool.call` line names the agent, app, argv (each arg cut to 200 chars), stdin length (never its content), pod, container and exit code (`Tool::audit_detail`). |

### Live console commands (`app_exec`)
Only on the caller's own app (`my_apps` → `yours`); an agent's `app_exec` on another agent's
app waits for the user's own yes (see workspaces-and-ownership.md). Use `app_exec` for anything
that must happen inside a running app without a restart: a game
server's console (`itzg/minecraft-server`: `["rcon-cli", "op", "Steve"]`, `["rcon-cli", "list"]`),
an app's own admin CLI (`["occ", "user:list"]` style tools), a quick read of a file. Prefer the
app's own console mechanism over `update_app` (which re-renders and restarts) or stop/start.
There is no shell unless the agent passes `["sh", "-c", "..."]` and the image has one.
The agent that owns the workspace and the lead agent may exec (scope `Change`); others are
refused before anyone is asked. RBAC: the per-namespace workload role has `pods/exec`
`get`+`create` (WebSocket exec needs both), nothing cluster-wide, no attach or port-forward.
A command sees what the container sees, including environment variables that hold generated
secrets: the user's confirmation is what stands between an agent and those.

## Safety rules (server and admission)
Validator: unknown keys refused, pinned images, sizes and counts bounded, mounts and files
absolute without `..`, files never inside a volume. Renderer: the only knobs are the ones above.
Admission (`apps-rbac.yaml`, fail closed, matched on `melissa-apps`):
- `melissa-apps-namespaces`: `app-<name>` + `melissa.io/app`, PSA baseline/restricted, workspace +
  device labels on create, ownership rules, `melissa.io/port` and `melissa.io/ports` formats.
- `melissa-apps-workloads` (Deployments/StatefulSets): only in app namespaces with workspace
  labels; no hostNetwork/PID/IPC; volumes only PVC/ConfigMap/Secret/emptyDir; no init/ephemeral
  containers; never privileged; added caps only from baseline's default set; no hostPort; CPU and
  memory limits; seccomp RuntimeDefault; no SA token.
- `melissa-apps-services`: ClusterIP, or `<app>-port` NodePort with ≤ 6 ports all in
  30000-30079; TCP/UDP only; no externalIPs, never LoadBalancer.
- `melissa-apps-volumes`: new PVCs on `local-path`, no `volumeName`/`dataSource`.
- PSA baseline on every app namespace checks the pods themselves.
Root inside a container is allowed (many images need it), privileges never. Images from any
public registry are allowed (pre-alpha; supply-chain risk noted in DECISIONS).

## Test in a container
```
q='sudo podman exec melissa-test melissa-server'
$q tool --wait deploy_app '{"app":"minecraft","spec":{...}}' &   # prints the confirmation id
$q confirm approve <id>
$q tool app_status '{"app":"minecraft"}'      # "ports": [{"address": "<BIND>:30000", "protocol": "tcp"}]
nc -z <BIND> <APP_PORT_BASE+n>
```
Admission checks as the apps identity: `kubectl --kubeconfig /run/melissa-kube/apps.kubeconfig
apply -f <privileged deployment>` → denied with the policy message.

## Verified (2026-11-19, rootful test container, APP_PORT_BASE 35000, k3s v1.37)
- **Minecraft** (`itzg/minecraft-server`, EULA, 1536Mi, 5Gi volume, game 25565/tcp + query 25565/udp public):
  NodePorts 30001/30002 (35001/35002 on the host). Java 21 tag crash-looped (Minecraft 26.3 needs Java 25;
  `app_logs` said so); `update_app` to the java25 tag kept both ports. From the host: `nc -z` open, a status
  ping answered (26.3, MOTD), a UDP query answered. Web UI: tile `127.0.0.1:35001 (TCP)`, dialog "Game" and
  "Query" with protocol.
- **HTTP app:** `traefik/whoami:v1.12.0`, `web: http`: link `http://127.0.0.1:35003` answered.
- **Chat (OpenCode, free model):** "Set up a Minecraft server" → `cluster_health`, `list_apps`,
  `create_agent` (Ada), `deploy_app` (4 specs refused by validation first, then a valid one) → confirmation
  with the summary, approved → deployed. It hit the same Java 25 problem; asked to fix it, the lead agent read
  `app_logs`, called `update_app` with the java25 image (confirmation named Ada first), reported "Connect at
  127.0.0.1:35001 (TCP)"; a status ping answered (Paper 26.3).
- **Negatives:** the tool refused `privileged`, `hostNetwork`, `hostPath`, `nodePort` (unknown fields) and an
  unpinned image, before any confirmation. As the apps identity (`kubectl --dry-run=server`): privileged,
  hostPath, hostNetwork, SYS_ADMIN, no limits, hostPort → `melissa-apps-workloads`; NodePort 31000 and SCTP →
  `melissa-apps-services`; a namespace without `melissa.io/workspace` and a bad `melissa.io/ports` →
  `melissa-apps-namespaces`; a PVC with `volumeName` → `melissa-apps-volumes`; a Deployment in kube-system →
  RBAC. A valid Deployment passed. `container-run.sh smoke` passed.
- **NixOS VM test** (`nix build .#checks.x86_64-linux.melissa-nixos -L`): the test app deployed with
  `deploy_app` (image pinned by the digest k3s reports), NodePort answered from the client VM, stop/start.

## Verified: network permissions (2026-11-24, rootful test container, k3s v1.37.2, APP_PORT_BASE 35000)
`SMOKE_APPS=1 container-run.sh smoke`: policies in every app namespace; a default app reaches neither another
app, 1.1.1.1:443 nor the node's 6443; with `internet` it reaches 1.1.1.1:443 but not the node's 6443, the
Kubernetes service 10.43.0.1, Melissa's UI forward (node:8081), the host's UI port via the gateway, or another
app; the public port answers from the host through the NodePort; the owner's `POST /api/apps/{app}/network`
link opens a→b (c still blocked) and removing it closes it again. DNS works under default deny. First run found
that the API refuses `::ffff:0:0/96` in an ipBlock (removed). Re-run on master with gVisor (2026-11-24): the three test apps ran under RuntimeClass `gvisor` (runsc
release-20261116.0) and every check passed the same.

## Gotchas
- CEL: Kubernetes 1.37 exposes a RoleBinding subject's `namespace` as `s.namespace`, never
  `__namespace__` (`rbac_manifests.rs` guards it).
- `k3s-prepare` reinstalls `/usr/lib/melissa/k3s/manifests/*` on every k3s start: rebuild the
  image instead of live-patching policies.
- The workloads policy checks stop/start patches too: it must accept Deployments rendered by
  older versions (no container `securityContext`), so it does not require
  `allowPrivilegeEscalation: false` (the renderer sets it).
- local-path does not enforce PVC sizes. Check `df -h /var` before big images.
