# packaging/common: files every Melissa packaging installs

Host-agnostic system files for running Melissa: the server and k3s as systemd services, the server's
user and directories, the k3s config and Melissa's k3s manifests. Every packaging uses this tree:

- `packaging/container/` copies it into the container image (`COPY packaging/common/ /`);
- `packaging/nix/` (the Nix service) installs the same files from here (`packaging/nix/common.nix`;
  it uses its own k3s units, so not `k3s.service`, `k3s-prepare`, `k3s.lock` or
  `melissa-kubeconfig.service`).

**Paths mirror the install layout.** `packaging/common/<path>` installs to `/<path>`. Keep it that
way and don't rename files: other packagings refer to these paths. Packaging-specific changes go in
that packaging's own tree as drop-ins or overrides (same file name in a later layer, a
`*.service.d/*.conf` drop-in, or a `config.yaml.d/2x-*.yaml`), not here.

| Path | What |
|---|---|
| `usr/lib/systemd/system/melissa-server.service` | The server (web UI on 127.0.0.1:8080), fixed user `melissa`, XDG dirs mapped to `/etc`, `/var/lib`, `/run` |
| `usr/lib/systemd/system/k3s.service` | k3s server; `ExecStartPre=k3s-prepare` |
| `usr/lib/systemd/system/melissa-kubeconfig.service` | Writes the server's two read-only/app-install kubeconfigs after k3s |
| `usr/lib/sysusers.d/melissa.conf` | User `melissa` (home `/var/lib/melissa/home`), group `melissa-kube` |
| `usr/lib/tmpfiles.d/melissa-{config,home,kube}.conf` | `/etc/melissa`, `/var/lib/melissa{,/home}`, `/run/melissa-kube` |
| `usr/share/polkit-1/rules.d/50-melissa.rules` | Lets `melissa` restart `melissa-server.service` (Restart in Settings → Updates), nothing else |
| `usr/bin/melissa-setup` | Root CLI: install/log in agent CLIs for the server's user (`--yes`, `--no-login`, `--no-restart`) |
| `usr/libexec/melissa/k3s-prepare` | Before k3s: link the airgap images, install the manifests (and gVisor's, when runsc is on PATH), pin the node IP (`k3s-node-ip`) |
| `usr/libexec/melissa/k3s-node-ip` | Writes `MELISSA_K3S_ARGS=--node-ip=<default-route IPv4>` to the env file given (k3s-prepare; the NixOS module's k3s) |
| `usr/libexec/melissa/melissa-kubeconfig` | ServiceAccount tokens → `/run/melissa-kube/{server,apps}.kubeconfig` |
| `usr/lib/melissa/k3s.lock` | Pinned k3s version and sha256 sums (binary, airgap images, LICENSE; k3s-selinux for SELinux hosts) |
| `usr/lib/melissa/k3s/manifests/*.yaml` | Deploy-on-start manifests: server RBAC, app RBAC and ValidatingAdmissionPolicies, Traefik HelmChartConfig |
| `usr/lib/melissa/k3s/gvisor/` | gVisor for apps: the `gvisor` RuntimeClass manifest, runsc's options (`runsc.toml`, platform systrap) and k3s's containerd template with the `runsc` runtime; installed by `k3s-prepare` only where runsc is (the Nix module writes its own template with store paths) |
| `etc/rancher/k3s/config.yaml.d/10-melissa.yaml` | k3s defaults (kubeconfig mode, node label); no SELinux setting |

Not here (built or fetched by each packaging): the Rust binaries `melissa-server`, `melissa-device`
and `melissa-netbridge` (installed as `/usr/bin/…`), k3s itself and its airgap images (from
`k3s.lock`; `k3s-prepare` expects them in `/usr/lib/melissa/k3s/images/`). No app list ships:
agents deploy apps from specs they write.

Packages the host or image needs at runtime: systemd, bubblewrap (agent sandbox), nftables and
iproute (k3s, egress), polkit (Restart in Settings → Updates), ca-certificates, curl (agent installers).

Checks: `crates/cluster/tests/rbac_manifests.rs` parses the manifests here.

**Release marker.** Each packaging writes `MELISSA_RUNTIME` in its release file (`/usr/lib/melissa/release`,
or `$MELISSA_RELEASE_FILE`): `container` for the image, `nix` for the Nix package (`HostKind::Nix`).

**Agent installs from Settings are container-only.** The Install / Sign in units
(`melissa-agent-install@.service`, `melissa-agent-login@.service`), their tmpfiles rule and their polkit rule
(`51-melissa-agent-install.rules`) live in `packaging/container/rootfs/`, not here, so the Nix packaging never
installs them or grants the extra polkit rights. On Nix the Providers page says to add the agent via the Nix
configuration.
