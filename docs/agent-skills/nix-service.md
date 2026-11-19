# Nix service: Melissa from the flake (NixOS module, other distributions)

**When:** packaging or changing Melissa for Nix, the NixOS module (`services.melissa`), the NixOS VM
test, or the non-NixOS "host bundle" (`melissa-host`). Host-first track 2 (DECISIONS 2026-11-19); the
other track is the rootful container ([container-mode.md](container-mode.md)).

## Layout
- `flake.nix` outputs (besides `devShells`):
  - `packages.<system>.melissa` (= `default`): `rustPlatform.buildRustPackage` from `Cargo.lock`
    (`-p melissa-queen -p melissa-comb -p melissa-agent`); rust-embed reads `crates/web/assets` at build
    time, so the UI is in the binary. Also ships the catalog, the k3s manifests, the shared queen unit
    (ExecStart pointed at the store), tmpfiles, the polkit rule, `melissa-setup` and
    `libexec/melissa/melissa-kubeconfig` (paths substituted, tools wrapped).
  - `packages.<system>.melissa-integration-tests`: the sandbox and egress integration tests of
    `crates/agent` as `melissa-it-sandbox`, `melissa-it-egress` (for the VM test).
  - `packages.<system>.melissa-host`: non-NixOS bundle (prototype, below).
  - `overlays.default` (`pkgs.melissa`), `nixosModules.melissa` (= `default`).
  - `checks.x86_64-linux.melissa-nixos`: the VM test.
- `packaging/nix/common.nix`: **one** variable, `root`, where the shared host files live (today
  `image/rootfs`, soon `packaging/common`). Never copy those files into `packaging/nix`.
- `packaging/nix/{package,module}.nix`, `tests/{vm,integration-tests}.nix`, `host/default.nix`.
- nixpkgs is the flake's (`devenv-nixpkgs/rolling`, full nixpkgs incl. `nixos/`). A NixOS user's own
  nixpkgs is used when they import the module into their system (`package` defaults to the flake's).

## NixOS module options (`services.melissa`)
| option | default | maps to |
|---|---|---|
| `enable` | false | everything below |
| `package` | flake's `melissa` | |
| `listen` | `127.0.0.1:8080` | `listen` (the queen refuses non-loopback: no login yet) |
| `backends` | `[]` | extra `[[backend]]` entries |
| `settings` | `{}` | free-form queen.toml, merged last |
| `apps.catalog` | package's catalog | `[apps] catalog` |
| `apps.addresses` | null (auto) | `[apps] addresses` |
| `apps.portOffset` | 0 | `[apps] port_offset` |
| `apps.openFirewall` | false | TCP 30000-30079 |
| `k3s.enable` | true | nixpkgs `services.k3s` (server) |
| `k3s.package` | `pkgs.k3s_1_37` | the API version Melissa's client is built for |
| `k3s.airgapImages` | false | `services.k3s.images` += k3s's airgap tarball (needed offline) |
| `k3s.images` | `[]` | more image archives (the VM test's app) |
| `k3s.nodeIp` | null | null: default-route IPv4 pinned at each start (like `k3s-prepare`) |
| `agents.nixLd` | true | `programs.nix-ld.enable` |
| `agents.packages` | `[]` | agent CLIs from Nix on the queen's PATH (file name must stay `claude`/`grok`/`opencode`) |

What it sets: users `melissa` (home `/var/lib/melissa/home`, nologin) and groups `melissa`,
`melissa-kube`; `systemd.packages`/`systemd.tmpfiles.packages` = the package, so the **shared**
melissa-queen.service (hardening, State/Configuration/RuntimeDirectory, XDG mapping) is used as is, plus a
NixOS drop-in for PATH (agent CLIs in the home, bubblewrap, `/run/current-system/sw`) and
`MELISSA_VERSION=<version>-nix`; `/etc/melissa/queen.toml` (store symlink, restart trigger; the dir
stays melissa:melissa 0750 for `providers.toml`); k3s with `--write-kubeconfig-mode=0600`,
`node-label melissa.io/managed=true`, Melissa's manifests as `services.k3s.manifests`
(`melissa-<file>.yaml`, the same names `k3s-prepare` uses), `cni0` trusted in the firewall (pods → API
and kubelet; opens nothing to the LAN, like the images' firewalld zone); `melissa-kubeconfig.service`
(native unit, k3s on its PATH); polkit on (the shared rule).

Not on NixOS: SELinux (`selinux: true` from the image config is not used), Tailscale, sshd, bootc
updates, `melissa-update*` units, `melissa-state-migrate` (nothing to migrate), the first-boot prompt.

## Sandbox on NixOS (code change, 2026-11-19)
`crates/agent/src/sandbox.rs` `SystemRoot`: the sandbox used to show `/usr` + `/etc` and make `/bin`,
`/lib64`, ... symlinks into `/usr` (Fedora). On NixOS `/usr` holds only `env`; so `SystemRoot::detect`
binds top-level dirs that are not usr-links (`/bin` with `sh`, nix-ld's `/lib64`), and when
`/run/current-system` exists binds `/nix/store` and `/run/current-system` read-only and appends
`/run/current-system/sw/bin` to the sandbox PATH. A CLI whose ELF loader or `#!` is in the store (e.g. a
nixpkgs wrapper on Fedora) gets `/nix/store` too. The store is world-readable on the host and holds no
secrets by Nix convention. Fedora's argv is unchanged (unit tests pin it).

Unprivileged user namespaces: on by default on NixOS (`security.allowUserNamespaces`, asserted).
bubblewrap is used **non-setuid** (the unit has `NoNewPrivileges=yes`, as on the images). Hardened
kernels need `security.unprivilegedUsernsClone = true`.

## Agent CLIs on NixOS
`sudo melissa-setup agent <claude|grok|opencode>` works as on the image: it downloads the vendor's
installer and runs it as `melissa` into `/var/lib/melissa/home` (writable; the queen's StateDirectory). The
package substitutes the installer's PATH (coreutils, curl, tar, unzip, ... and `/run/current-system/sw/bin`).
The installed CLIs are prebuilt glibc binaries with `/lib64/ld-linux-x86-64.so.2`: they run through
**nix-ld** (`agents.nixLd`, default on), inside the sandbox too (it binds `/lib64` and
`/run/current-system`, where nix-ld finds its loader and libraries without env vars). Alternative:
`agents.packages = [ pkgs.opencode ]` etc. (Nix-built, updated with the system; claude-code is unfree).
Self-updating CLIs write into the home, which is fine.

## VM test (passes 2026-11-19, 111 s of VM time after the builds)
| feature | checked |
|---|---|
| web UI | queen active as `melissa`; `/healthz`, `/tasks` on 127.0.0.1:8080; only a loopback listener; the LAN client can't connect |
| chat turn | `melissa-queen ask` on an echo session backend |
| sandbox / egress | `melissa-it-sandbox`, `melissa-it-egress` as `melissa`: bwrap, own netns, netbridge, proxy allowlist, MCP, no leaks, no skips |
| vendor CLI via nix-ld | a /lib64-loader binary as a CLI backend task, inside the sandbox |
| k3s | node Ready (v1.37.1), `melissa.io/managed` label, Traefik rolled out offline (airgap images) |
| policies | ServiceAccounts, 3 admission policies + bindings, kubeconfigs root:melissa-kube 0640, RBAC negatives |
| app install + NodePort | test app from the catalog (digest-pinned), `install_app` (confirmed), NodePort 3000x answers from the client VM, `app_status` link |
| stop/start | `stop_app` (port stops answering), `start_app` (answers again) |
| settings | providers.toml written melissa 0600; restart through polkit (new InvocationID) |
| melissa-setup | status runs on NixOS (agent download needs internet: not in the test) |
`nix build .#checks.x86_64-linux.melissa-nixos -L` (KVM; untrusted users can run it: the daemon has
`kvm nixos-test`). Two VMs, no internet: `server` (4 GiB, 4 cores, k3s airgap images, test app image
built with dockerTools) and `client` (the LAN). Debug: `nix run .#checks.x86_64-linux.melissa-nixos.driverInteractive`.
The test app's catalog entry is written at test time with the digest k3s reports for the imported image
(`crictl inspecti`), because the catalog requires `@sha256:` pins.

## Other distributions with Nix (prototype: `melissa-host`)
`packages.<system>.melissa-host` = the package + sysusers file + a PATH drop-in (vendor CLIs, `/usr/bin`
with the distribution's bwrap) + `bin/melissa-host install|uninstall`, which links units, sysusers,
tmpfiles into `/etc`, copies the polkit rule, links `melissa-queen`/`melissa-setup` into
`/usr/local/bin`, runs sysusers/tmpfiles and starts the queen. The GC root is a root profile:
```
sudo nix profile install --profile /nix/var/nix/profiles/melissa <flake>#melissa-host
sudo /nix/var/nix/profiles/melissa/bin/melissa-host install
# update:  sudo nix profile upgrade --profile /nix/var/nix/profiles/melissa --all && sudo .../melissa-host install
# back:    sudo nix profile rollback --profile /nix/var/nix/profiles/melissa && sudo .../melissa-host install
```
k3s is **not** in it: install k3s the distribution's way (on SELinux hosts k3s-selinux labels
`/usr/bin/k3s`, not a store path); `install` then adds the manifests and `melissa-kubeconfig.service`.

Why not numtide's system-manager (checked 2026-11-19, v1.1.0, MIT): officially Ubuntu/Debian/NixOS only,
other distros behind `allowAnyDistro` ("unsupported and untested"); firewall only mocked; no confirmed
`services.k3s`; one more tool to install as root. Revisit if it supports Fedora.

## Power and input devices: never Melissa's
Only the host's own systemd-logind handles the lid switch, power keys and input devices. The module and
the `melissa-host` bundle add no logind, no `services.logind` / `HandleLidSwitch` settings, no udev rules,
no access to `/dev/input`, and no inhibitors. (Found 2026-11-19: a privileged rootful container running its
own logind saw the host's lid switch and suspended the laptop.) The queen unit runs as `melissa` with
`ProtectSystem=strict`, and k3s pods get the default device cgroup. Keep it that way. A review of any new
unit or option should check it.

## Gotchas
- `nix build` of the package is ~3 min cold (full release build); the integration tests build it again.
- The local tool API (`POST /api/tools/<name>`) is unattended: `install_app`, `stop_app`, `start_app` wait in the
  confirmation inbox; the VM test approves them through `/api/confirmations/<id>/approve`.
- The built-in `echo` is a task backend only; a Queen chat on echo needs a `[[backend]] kind = "echo"` entry.
- Settings' restart button needs only Melissa's polkit rule (`UpdateEnv::polkit_rule_installed`: `/usr/share`,
  `/etc` or NixOS's `/run/current-system/sw/share`), not the bootc update helper.
- `patchShebangs` only finds interpreters in `buildInputs`: `bash` must be there for `melissa-setup`.
- `git add -N` new files before `nix build .#...`: flakes only see tracked files.
- The k3s module's `ExecStart` does not escape `extraFlags`, so `"$MELISSA_K3S_ARGS"` is expanded by systemd
  from `/run/melissa-k3s/env` (written by the node-IP `ExecStartPre`).
- `services.k3s.environmentFile` is kept: the module force-sets `EnvironmentFile` to it plus the node-IP file.
- Unit tests that assume `/usr/bin/true` skip that part on NixOS/in the Nix sandbox; the package does not run
  `cargo test` (the dev shell does).
