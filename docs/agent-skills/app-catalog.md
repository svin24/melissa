# App catalog and app installs (M2)

**When:** adding or changing an app manifest, bumping an app's image, touching the app tools
(`list_catalog`, `install_app`, `app_status`, `app_logs`, `uninstall_app`), the write identity
`melissa-apps` or its admission policies, or the owner's `app-credentials` command.
Code: `crates/catalog` (schema, validation), `crates/hive/src/{render,apps,app_tools}.rs`,
`crates/queen/src/credentials.rs`, `image/rootfs/usr/lib/melissa/k3s/manifests/apps-rbac.yaml`,
`image/rootfs/usr/libexec/melissa/melissa-kubeconfig`. Manifests: `manifests/*.toml` (shipped at
`/usr/share/melissa/catalog/`).

## Split of duties
- Agents pick an app and may override its declared, non-secret config fields. They never write
  manifests, never see generated secrets, and hold no cluster credentials.
- The queen validates against the manifest, applies policy (labels below) and renders/applies
  the objects with the `melissa-apps` identity. containerd pulls the image when the pod starts
  (runtime pull from the manifest's registries; the box ships no app images).
- The owner reads generated credentials with `sudo melissa-queen app-credentials <app>`.

## Manifest schema (TOML, `schema = 1`, every table `deny_unknown_fields`)
```toml
schema = 1
name = "nextcloud"            # DNS label <= 40; namespace app-<name>, host <name>.<domain>
version = "34.0.6"            # shown to people; the digest is what runs
description = "..."
registries = ["docker.io"]    # allowlist; every image must name one of these explicitly

[[config]]                    # typed fields; overrides from agents must match
name = "admin_user"           # lower_snake_case, unique
type = "string"               # string | integer | boolean | choice | secret
description = "..."
default = "admin"             # required except for secret
max_len = 64                  # string only (default 128); no control characters
# min/max (integer), options = [...] (choice), length = 16..128 (secret, default 32)

[[volumes]]
name = "data"
size = "10Gi"                 # Mi/Gi/Ti; PVC on local-path (size is not enforced there)

[[containers]]
name = "nextcloud"
image = "docker.io/library/nextcloud:34.0.6-apache@sha256:<64 hex>"   # tag optional, digest required
ports = [{ name = "http", port = 80 }]           # names <= 15 chars, unique across containers
mounts = [{ volume = "data", path = "/var/www/html" }]
[containers.env]              # exactly one source per variable
A = { value = "literal" }
B = { config = "admin_user" }         # non-secret field
C = { secret = "admin_password" }     # secretKeyRef to <name>-secrets, never a literal
D = { builtin = "ingress_host" }      # or ingress_url
[containers.resources]        # all four required (small boxes)
cpu_request = "100m"; cpu_limit = "1"; memory_request = "256Mi"; memory_limit = "1Gi"
[containers.probe]            # HTTP; used for startup, readiness, liveness
path = "/status.php"; port = "http"
ingress_host_header = true    # send Host: <name>.<domain> (apps that check trusted domains)
startup_timeout_secs = 900    # first start may install/migrate; period_secs default 10

[ingress]
port = "http"                 # Traefik Ingress for <name>.<domain>, path /

[credentials]                 # what app-credentials prints
user = "admin_user"           # string field (value from the Deployment's config annotation)
password = "admin_password"   # secret field
```
Validation errors name the field (`containers[0].image: must be pinned by digest ...`). The file
name must be `<name>.toml`. `Catalog::load_dir` never lets one broken file hide the others
(`list_catalog` reports them under `invalid_manifests`).

Why TOML: same as `queen.toml`, no implicit typing (YAML's `no`/`1.10` surprises), comments,
`toml` is already a dependency (`serde_yaml` is unmaintained). Why our own format and not
Helm/Compose: a tiny reviewed surface the queen fully understands and can validate (no
templating, no hostPath/privileged knobs to forget), rendered to plain objects.

## Adding an app
1. Copy `manifests/nextcloud.toml`, pick resources that fit a 3 GiB bee, keep secrets as `secret`
   fields, use a probe the app answers without login.
2. Pin the image by digest (multi-arch index, so arm64 bees work later):
   `skopeo inspect docker://docker.io/library/<image>:<tag> | jq -r .Digest` (the `Digest` of the
   tag, not of `--override-arch`). Check the version with `skopeo inspect --override-os linux
   --override-arch amd64 ... | jq .Env,.Created`, note the compressed size
   (`... | jq '[.LayersData[].Size]|add'`).
3. `devenv shell -- cargo nextest run -p melissa-catalog` (the repo catalog must load without
   errors), add a golden test in `crates/hive/src/render.rs` if the app needs new rendering, and
   update goldens with `MELISSA_UPDATE_GOLDEN=1` after reviewing the diff.
4. Test in the VM (below). Check disk: the image is pulled into `/var` at install time.

## Rendering (`crates/hive/src/render.rs`, pure)
Namespace `app-<name>` (labels `melissa.io/app=<name>`, `melissa.io/managed=true`, Pod Security
`enforce=baseline`) → RoleBinding `melissa-apps` (ClusterRole `melissa-app-workload` →
ServiceAccount `melissa/melissa-apps`) → Secret `<name>-secrets` (created once, POST, 409 = keep)
→ PVC `<name>-<volume>` → Deployment `<name>` (1 replica, `Recreate`, no SA token, no service
links, RuntimeDefault seccomp, config annotation `melissa.io/config`) → ClusterIP Service →
Ingress (class traefik). All but the Secret are server-side applied, field manager `melissa`,
forced. Deployment, not StatefulSet: one replica on one RWO volume; the PVC is a separate object
so "keep data" and "delete data" are explicit, and `Recreate` never runs two pods on the data.

## Tools and labels (policy keys off them)
| tool | labels | notes |
|---|---|---|
| `list_catalog` | read-only | works without k3s |
| `install_app {app, config?}` | Reversible, target `app` (Change) | attended Queen: allowed for a new app; over an owner's app: confirmation naming the owner; unattended or tainted: confirmation. Records `melissa.io/owner` (+ installed-by/at, origin) on a new namespace, keeps an existing record. Secrets generated from `/dev/urandom` (62-letter alphabet, rejection sampling), never returned. Reinstall keeps the Secret and PVCs. |
| `app_status {app}` | read-only, target `app` (Read) | reports `owner`, `installed_by`, `installed_at`; phases `not_installed`, `starting`, `ready`, `failing`, `uninstalled_data_kept`, `deleting`; pods with kubelet reason codes only (no messages) |
| `app_logs {app, lines?, container?, previous?}` | reads_untrusted, target `app` (ReadScoped: workers only for their apps) | default 100, max 500 lines, 64 KiB, ANSI/control chars stripped, lines > 2000 chars cut; taints the task |
| `uninstall_app {app, delete_data?}` | Destructive, target `app` (Change) | always a confirmation (naming the owner for their apps); workers: refused outside their scope. Default deletes Deployment/Service/Ingress, keeps namespace, PVCs, Secret. `delete_data: true` deletes the namespace (data gone: local-path reclaim is Delete). |

Before the labels, the registry checks the caller's scope against the app's owner (see
[ownership-and-scopes.md](ownership-and-scopes.md)): a worker touching an app that isn't its job's or
granted is refused outright (`tool.refused`), never asked. `melissa-queen tool` and `/api/tools` act as
the owner.

`melissa-queen tool --wait <name> [json]` runs a tool in the daemon (`POST /api/tools/{name}`) as
an unattended operator call (task 0): reversible/destructive calls wait for `melissa-queen
confirm approve <id>` from another shell. `melissa-queen tool <name>` (no `--wait`) runs in-process
and refuses anything that needs a confirmation.

## RBAC: two identities (`apps-rbac.yaml`, `queen-rbac.yaml`)
- `melissa-queen` (read-only, `queen.kubeconfig`): `cluster_health` etc. Unchanged.
- `melissa-apps` (`apps.kubeconfig`, same dir/group, written by `melissa-kubeconfig`): cluster-wide
  only namespaces get/create/patch/delete, rolebindings create/patch, `bind` on
  `melissa-app-workload`. ValidatingAdmissionPolicies (fail closed, matched on the SA's username):
  namespaces must be `app-<name>` with `melissa.io/app=<name>` and PSA baseline/restricted;
  RoleBindings only in such namespaces, only `melissa-app-workload`, only to `melissa-apps`;
  Services ClusterIP without externalIPs. Inside an app namespace, `melissa-app-workload`:
  deployments/statefulsets, services, PVCs, configmaps, ingresses (full), **secrets create/delete
  only (no get/list)**, pods + pods/log read. No PVs, nodes, exec, other namespaces.
- PSA baseline in app namespaces is what keeps the write identity off the host (no privileged
  pods, hostPath, host namespaces).
- `rotate [queen|apps]` in `melissa-kubeconfig` (default both).

## Ownership (2026-11-18)
Namespace label `melissa.io/owner` = `owner` | `job-<key>`; no label = the owner's. The namespaces
admission policy requires it on create and freezes it on update (`melissa-apps` can never change an
owner). The owner takes over a job's app with `sudo melissa-queen app-adopt <app>` (admin kubeconfig,
merge patch, records `melissa.io/adopted-from`/`-at`). Details: [ownership-and-scopes.md](ownership-and-scopes.md).

## Owner credentials
`sudo melissa-queen app-credentials nextcloud` reads `app-nextcloud/nextcloud-secrets` with k3s's
admin kubeconfig (`/etc/rancher/k3s/k3s.yaml`, 0600 root). Not a tool, not an API route; neither
queen identity can read Secrets; the queen and its agents run as an unprivileged DynamicUser.
Without root: "needs root". The Secret is the single source of truth (no copy in the queen's state).

## VM test (headless, from the main checkout)
```
devenv shell -- just image-build headless          # in the worktree
scripts/vm-dev.sh deploy headless                   # main checkout (owns vm/)
vm='ssh -p 2222 -i vm/test_key bee@127.0.0.1'
$vm 'melissa-queen tool --wait install_app "{\"app\":\"nextcloud\"}"' &   # prints the confirmation id
$vm 'melissa-queen confirm list; melissa-queen confirm approve <id>'
$vm 'melissa-queen tool --wait app_status "{\"app\":\"nextcloud\"}"'
curl --resolve nextcloud.melissa.home.arpa:18080:127.0.0.1 http://nextcloud.melissa.home.arpa:18080/status.php
$vm 'sudo melissa-queen app-credentials nextcloud'
curl -u admin:<pw> -H 'OCS-APIRequest: true' --resolve ... 'http://nextcloud.melissa.home.arpa:18080/ocs/v2.php/cloud/user?format=json'
```
Negative RBAC checks: `sudo kubectl --kubeconfig /run/melissa-kube/apps.kubeconfig ...` (create
namespace `kube-evil`, label kube-system, get secrets anywhere, rolebinding in default, ...).

## Verified (2026-11-18, headless VM, 3 GiB RAM, 8.6 GiB /var, k3s v1.37.2)
- **Confirmation flow:** `melissa-queen tool --wait install_app '{"app":"nextcloud"}'` printed
  "waiting for confirmation #2 (changes state while nobody is watching): approve with
  `melissa-queen confirm approve 2`"; `confirm list` showed `#2 task 0 install_app`; after
  `confirm approve 2` from a second shell the call returned (applied Namespace, RoleBinding, PVC,
  Deployment, Service, Ingress; secrets `generated`). `uninstall_app` asked for confirmation
  ("destructive action") even with data kept; a `deny` returned 403 "denied by a human" and
  nothing was deleted.
- **Ready:** `app_status` went `starting` → `ready` (1/1, URL `http://nextcloud.melissa.home.arpa`).
  Fresh install with the image removed from containerd: approve → ready in 429 s, of which the
  pull was **4 m 29 s for 513,286,258 bytes** (kubelet event; compressed size; ~1.3 GB unpacked),
  after a first attempt that failed on a transient DNS lookup of `registry-1.docker.io` (kubelet
  backed off ~14 s and retried; `app_status` reports such a window as `failing`/`ErrImagePull`).
  First start (copy + SQLite install) then took ~40 s. Pod: 168 MiB, ~0.5 CPU right after start.
- **From the host:** `curl --resolve nextcloud.melissa.home.arpa:18080:127.0.0.1
  http://nextcloud.melissa.home.arpa:18080/status.php` →
  `{"installed":true,"maintenance":false,"needsDbUpgrade":false,"version":"34.0.6.2",...}`.
- **Credentials:** `sudo melissa-queen app-credentials nextcloud` printed URL, `user: admin`,
  `password: <32 chars>`; as `bee` and as the queen's DynamicUser (+ `melissa-kube`): "needs root".
  OCS `GET /ocs/v2.php/cloud/user` with them → 200, id `admin`, groups `[admin]`; wrong password
  → 401. The password appears 0 times in the audit log and the queen's journal.
- **Logs cap:** `lines: 501` → 422 "`lines` must be 1-500"; `lines: 500` after 700 requests →
  exactly 500 lines, 51,951 bytes, no control characters; default 100 lines.
- **Uninstall/reinstall:** keep-data uninstall deleted Ingress, Service, Deployment, kept
  `nextcloud-data` and the Secret (`app_status`: `uninstalled_data_kept`; ingress → 404). Reinstall:
  Secret `kept existing`, ready in 7 s with no new install, a WebDAV file uploaded before the
  uninstall was still there and the old password still worked. `delete_data: true` (approved):
  `deleting`, then namespace, PV and the on-disk `local-path` directory gone; `not_installed`. The
  next install generated a new password (old one → 401).
- **RBAC (apps.kubeconfig):** denied: namespaces `kube-evil`, `app-evil` without label or with PSA
  `privileged`, labelling kube-system, deleting `melissa`, secrets anywhere (incl. list at cluster
  scope), deployments in kube-system, RoleBindings in `default`, RoleBindings in the app namespace
  to `app-nextcloud:default`, to two subjects, to a user, or of ClusterRole `admin` (RBAC
  escalation check), ClusterRoleBindings, PVs, nodes, kube-system logs.
- `vm-k3s-smoke.sh` passes; SELinux enforcing, 0 `avc:  denied` this boot; volume directory
  labelled `container_file_t` with MCS categories; k3s and queen `NRestarts=0`.
- Deploying the change moved 11 layers / 580 MB the first time (base layers had moved on), 7 layers
  / 9 MB for the fix.

## Gotchas
- **CEL field names:** Kubernetes 1.37 exposes a RoleBinding subject's `namespace` as
  `s.namespace`. The older escaped form `s.__namespace__` is "no such key", and
  `has(s.__namespace__)` is silently false (the policy denied every binding).
  `crates/hive/tests/rbac_manifests.rs` guards against it.
- **Live-patching a k3s manifest doesn't stick:** `k3s-prepare` reinstalls
  `/usr/lib/melissa/k3s/manifests/*` on every k3s start, so a hot fix in
  `/var/lib/rancher/k3s/server/manifests` is reverted by the next k3s restart. Rebuild the image.
- **Disk:** Nextcloud's image is 513 MB compressed (~1.3 GB unpacked) plus ~0.7 GB copied into the
  data volume at first start. The test VM's 8.6 GiB `/var` filled up once (k3s crash-looped,
  `NRestarts=10`) because root's podman storage still held a 2.1 GB image from an old
  `vm-update-test`; `sudo podman rmi -a` freed it. Check `df -h /var` before installing.
- The startup probe allows 900 s: the first start copies Nextcloud into the volume and runs the
  installer; on the VM that takes about 1-2 minutes after the pull.
- local-path does not enforce PVC sizes; `size` is a request, not a quota.
- A keep-data uninstall leaves the namespace (with Secret and RoleBinding); `install_app` refuses
  while the namespace is `Terminating` after a delete-data uninstall.
