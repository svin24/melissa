# Isolation beyond k3s pods: Nix services and microVMs (research, 2026-11-23)

PLAN open question 94 (owner request, 2026-11-22). Research plus a throwaway spike on the dev laptop; nothing
here is built into the product. Versions and licences checked on 2026-11-23.

## Summary

- **Best first step: gVisor (`runsc`) as a k3s RuntimeClass.** It gives agent-built apps their own
  user-space kernel without KVM, root or a new orchestrator. The app spec and approval flow stay the same:
  a new `isolation` field, rendered to `runtimeClassName`. NetworkPolicy, PVCs, image pulls and the
  admission policies keep working, because traffic and volumes still go through the host side of the pod.
  The cost is about 30 MiB per pod (measured here: 31 MiB PSS for an idle sandbox) and slower syscall-heavy
  apps. It works in all three install modes, offline, under Apache-2.0.
- **Second step, where `/dev/kvm` exists: Kata Containers (Rust runtime, Cloud Hypervisor or Dragonball) as
  a second RuntimeClass.** Each pod gets its own guest kernel, and the spec field is the same. It is heavier
  (around 120-180 MiB per pod) and more moving parts to ship (guest kernel and image, shim, containerd
  config).
- **Not now: Nix-built systemd services as an app runtime.** They are fast and tiny (spike: about 0.2 s to
  serve, 0.6 MiB RSS). But they duplicate k3s next to it, and the per-service IP filter needs the system
  manager (root), so it is silently ignored in user units. They don't exist in the container install. And
  agent-written Nix runs arbitrary build code. Worth revisiting only for NixOS-only "host services" (things
  that need host devices).
- **Not now: bare microVMs (Firecracker, Cloud Hypervisor, microvm.nix) as an app runtime.** They boot fast
  (spike: about 0.25 s to the guest's init) and work inside a podman container given `/dev/kvm`. But
  Melissa would have to rebuild what containerd and k3s already do for us: OCI image to rootfs, volumes,
  networking, logs, exec, restarts. The interesting later use is **running agent sandboxes themselves in a
  microVM** (open question 19), not apps.
- **No: Unikraft for this.** Apps would have to be ported or rebuilt per app. Agents deploy arbitrary OCI
  images, and Unikraft's managed offering is a hosted cloud.

## Comparison

| Option | Kernel boundary | Needs from host | Start (spike / docs) | Memory per instance | Default-deny network with explicit links | Fits manifest + approval | Install modes (container / NixOS / source) | Licence |
|---|---|---|---|---|---|---|---|---|
| k3s pod, runc (today) | shared | k3s | ~1-2 s (pod) | ~5 MiB runtime | NetworkPolicy (planned, not built yet) | yes (today) | yes / yes / yes | Apache-2.0 |
| **gVisor RuntimeClass** | user-space kernel (Sentry) over a narrow seccomp'd host surface | none extra (systrap platform); KVM optional | `runsc do true` 0.17-0.20 s | **31 MiB PSS** measured idle | NetworkPolicy unchanged (host veth) | yes: `runtimeClassName` | yes* / yes / yes | Apache-2.0 |
| **Kata RuntimeClass** | separate guest kernel (KVM) | `/dev/kvm`, vhost devices, guest kernel+image | ~0.5-1 s typical (not measured) | ~120-180 MiB (docs example / 3rd-party bench) | NetworkPolicy unchanged (tap behind host veth) | yes: `runtimeClassName` | yes* (KVM) / yes / yes (KVM) | Apache-2.0 |
| Firecracker / Cloud Hypervisor directly | separate guest kernel | `/dev/kvm`; tap needs CAP_NET_ADMIN (or vsock only) | **0.27 s / 0.24 s** to guest init (measured) | **84 / 92 MiB PSS** measured (generic 6.18 kernel, 128 MiB guest) | own nftables per tap, or no NIC + vsock proxy | new runtime, new renderer | yes (KVM) / yes / yes (KVM) | Apache-2.0; CH Apache-2.0 + BSD-3 |
| microvm.nix | as above (8 hypervisors) | NixOS + KVM; guests are NixOS configs | as above | as above | NixOS options + host firewall | agents would write NixOS modules | no / yes / no | MIT (+ hypervisor's) |
| QEMU `microvm` machine | separate guest kernel | `/dev/kvm` | 1.65 s boot+poweroff (SeaBIOS, bzImage) | 78 MiB PSS measured | as above | as above | yes (KVM) / yes / yes | GPL-2.0 (separate process) |
| Nix closure as hardened systemd unit | shared | systemd; IP filters, DynamicUser, RootImage need the **system** manager (root) | **0.2-0.45 s** to first HTTP 200 (measured) | **0.6 MiB RSS** measured (busybox httpd) | `IPAddressDeny/Allow` (system manager only; ignored in user units, measured) or `PrivateNetwork` + socket proxy | new runtime; agents write Nix | no / yes / partly | systemd LGPL-2.1+ (separate) |
| systemd-nspawn / NixOS container / portable service | shared | root; NixOS containers are "not perfectly isolated" per the NixOS manual | ~0.5-1 s (not measured) | small | veth + host firewall | new runtime | no / yes / no | LGPL-2.1+ / MIT |
| Unikraft | own unikernel on KVM | KVM; app ported or built with kraftkit | ms (vendor claim) | a few MiB | as microVMs | no: needs per-app builds | — | BSD-3 (kraftkit BSD-3) |

\* Container install: `runsc`, or the Kata shim, must be in the image and in k3s's containerd config. That
is not verified inside our privileged container yet; it is the prototype's first check (risks below).

## Recommendation

1. **Prototype gVisor first** (small, reversible, no new crate):
   - `crates/appspec`: optional `isolation: "standard" | "sandboxed" | "vm"` (absent = `standard`).
     Validation refuses a level the device can't run, using what `app_platform` already reports plus
     runtime detection. The confirmation summary shows it ("Runs in its own sandboxed kernel").
   - `crates/cluster` renderer: `runtimeClassName: melissa-gvisor` (or `melissa-kata`) and a label
     `melissa.io/isolation`. A ValidatingAdmissionPolicy pins the two together: a pod labelled
     `sandboxed` without the class is refused. A missing runtime fails closed: the pod doesn't start, and
     there is never a silent fallback to runc.
   - Packaging: `runsc` + `containerd-shim-runsc-v1`, pinned and sha256-checked like `k3s.lock`. A
     containerd config template entry (k3s auto-detects only crun, nvidia and the wasm runtimes, not
     runsc or Kata). RuntimeClass objects in `packaging/common` manifests. NixOS: `pkgs.gvisor` and
     `services.k3s.containerdConfigTemplate`.
   - Policy question for the owner: should **agent-built images** (and images outside a trusted list)
     default to `sandboxed`? That is the case PLAN section 5 names ("gVisor/microVM runtime for
     agent-built apps").
2. **Then Kata**, behind the same field (`vm`), only where `/dev/kvm` exists. Measure a real pod's
   overhead on the laptop before offering it by default. Set RuntimeClass `overhead` so the scheduler and
   our limits count it.
3. **Park** Nix-built app services, bare microVMs and Unikraft. Revisit: (a) microVMs for **agent
   sandboxes** (Cloud Hypervisor with virtiofs for the workdir, no NIC, the egress proxy over vsock),
   when the bwrap escape suite shows a gap only a kernel boundary closes; (b) Nix services on NixOS for
   host-device workloads.

Honest framing for the UI: gVisor and Kata narrow the **kernel** attack surface. They don't change what an
app may reach on the network (that is NetworkPolicy, which is still unbuilt and matters more for
app-to-app isolation today) or what it does with its own data.

### Escape-suite probes the prototype needs (`crates/agent/tests/escape` style, run in an app pod)

- **Runtime is real:** `uname -a` / `dmesg` / `/proc/version` show gVisor's or the guest's kernel, not the
  host's. A `sandboxed` spec whose pod would run under runc is refused at admission.
- **Fail closed:** remove the handler, deploy a `sandboxed` app → the pod doesn't start; the status says why.
- **Host reach:** the k3s API (6443), the server UI (8081), the node's addresses and the host's
  `127.0.0.1` from inside the pod are unreachable (same as today, now under the new runtime).
- **App to app:** another app's Service is unreachable without a declared link (once NetworkPolicy lands).
- **Devices and kernel:** no `/dev/kvm`, `/dev/mem`, `/dev/net/tun`; `mount`, `bpf`, `perf_event_open`,
  `userfaultfd`, `keyctl`, `io_uring_setup` fail; `/proc/sys` and `/sys` writes fail.
- **Limits:** fork bomb, memory hog and CPU hog stay inside the pod's limits *plus* the runtime overhead
  (checks that Sentry or VM memory is accounted to the pod cgroup).
- **Kata extras:** guest-to-host vsock ports other than the agent's are closed; virtiofs shares only the
  pod's volumes (no `..` escape out of the shared dir).

### Risks

- **runsc inside our privileged podman container, nested under k3s's containerd:** gVisor's systrap needs
  seccomp and ptrace-like features; k3s already runs pods there, so it should work. Not tested yet.
  First thing to check.
- **App compatibility under gVisor:** some apps rely on io_uring, unusual `/proc` files, raw sockets or
  inotify edge cases. Fallback: the owner flips that app to `standard` (an explicit, audited choice).
- **Performance:** syscall- and I/O-heavy apps (databases, Nextcloud's PHP) run slower under gVisor.
  Kata's I/O goes through virtiofs or virtio-blk.
- **Memory on 8-16 GB laptops:** gVisor's ~30 MiB per pod is fine for a handful of apps. Kata's ~150 MiB
  per pod adds up fast, so keep it opt-in.
- **Supply chain:** two more pinned binaries (and, for Kata, a guest kernel and image) to update and
  verify.
- **Kata shipping:** kata-deploy is a DaemonSet that writes host files. We would package the pieces
  ourselves instead (image or Nix), which is more work.

## Details

### 1. Isolation strength and host needs

- **Today's layers.** Apps are runc pods sharing the host kernel, restricted by Pod Security baseline,
  admission policies and limits. NetworkPolicy is planned but not built. Agents run under bwrap with seccomp
  (deny-list, EPERM), cgroup v2 limits and a per-agent host user (DECISIONS 2026-11-23). Agents and apps
  share one kernel: one kernel bug can cross every layer. That is the honest "shared kernel" caveat in
  PLAN section 5.
- **gVisor.** The app's syscalls are served by the Sentry, a Go user-space kernel. The Sentry itself uses
  a small set of host syscalls under its own seccomp filter, and a separate gofer mediates file access.
  The default platform is `systrap` (seccomp `SECCOMP_RET_TRAP`), which needs no KVM; the KVM platform is
  faster on bare metal ([platforms](https://gvisor.dev/docs/architecture_guide/platforms/)). That is
  weaker than a hardware VM boundary but much smaller than the full Linux syscall surface. It runs
  rootless (spike: `runsc --rootless do`).
- **Kata, Firecracker, Cloud Hypervisor, crosvm, QEMU.** A separate guest kernel behind KVM. The attack
  surface is the VMM's device models: Firecracker and Cloud Hypervisor are Rust, with minimal devices and
  per-thread seccomp. Firecracker's jailer adds chroot, namespaces, cgroups and a uid drop
  ([jailer](https://github.com/firecracker-microvm/firecracker/blob/main/docs/jailer.md)). Real CVEs
  still happen ([CVE-2026-5747](https://aws.amazon.com/security/security-bulletins/2026-015-aws), a
  Firecracker virtio-pci OOB write; [CVE-2026-27211](https://nvd.nist.gov/vuln/detail/CVE-2026-27211),
  a Cloud Hypervisor disk image issue), so the VMM should itself run as an unprivileged user in a sandbox.
  Needs `/dev/kvm` (on this host: `crw-rw-rw- root kvm`, open to every user). Nested virtualization is
  **not** needed for our container install: a container is not a VM. Verified: Cloud Hypervisor booted a
  guest inside a rootless `podman run --device /dev/kvm`. Our Quadlet already runs `--privileged`, which
  shares the host's `/dev`, so `/dev/kvm` is already visible inside the Melissa container when the host
  has it. Nested virt matters only when Melissa's host is itself a VM.
- **systemd units / nspawn / portable services / NixOS containers.** Shared kernel; the hardening is
  namespaces, seccomp and capability drops, roughly bwrap-equivalent. The NixOS manual warns that NixOS
  containers "are not perfectly isolated from the host system"
  ([manual](https://nixos.org/manual/nixos/stable/#ch-containers)). nspawn, portable services,
  `DynamicUser=`, `RootImage=` and IP filtering all need the system manager (root).
- **Unikraft.** A unikernel per app on KVM: a tiny guest. But each app must be built for it (kraftkit,
  BSD-3; [unikraft](https://github.com/unikraft/unikraft) BSD-3 core). The turnkey product is Unikraft
  Cloud, hosted, which conflicts with offline and no-third-party.

### 2. Start time, memory, density, storage, networking, devices

Spike host: dev laptop, 16 threads, 14 GiB RAM, Fedora 45 kernel 7.3.4, systemd 262, cgroup v2, Nix 2.35.3.
nixpkgs versions: Firecracker 1.16.1, Cloud Hypervisor 54.0, gVisor 20260406.0, guest kernel 6.18.62
(nixpkgs' generic config), busybox 1.37 static initramfs (835 KiB). Upstream latest on 2026-11-23:
Firecracker v1.18.0, Cloud Hypervisor v55.0, Kata 4.4.0, gVisor release-20261116.0, kraftkit v0.13.1.

| Measurement (3-5 runs) | Result |
|---|---|
| Firecracker: launch → guest `/init` prints READY (1 vCPU, 128 MiB) | 267-280 ms; guest uptime at init 0.18 s |
| Cloud Hypervisor: same | 225-258 ms (boot+poweroff 244-256 ms); guest uptime 0.16 s |
| QEMU `-M microvm` (SeaBIOS, bzImage): boot+poweroff | 1.65 s |
| Idle PSS of the VMM process (includes guest memory the generic kernel touched) | FC 84.7 MiB, CH 91.8 MiB, QEMU 77.6 MiB |
| Cloud Hypervisor in rootless `podman run --device /dev/kvm` | boots (GUEST-UP 0.20 s); without the device: fails |
| gVisor `runsc --rootless --network=none do /usr/bin/true` | 169-201 ms end to end |
| gVisor idle sandbox (sentry + gofer) | 28.3 MiB PSS (19.1 + 9.2) |
| Nix-built busybox httpd as transient hardened `systemd-run --user` unit → first HTTP 200 | 0.20-0.45 s (one 16 s outlier on the first run: the user manager was busy) |
| Same, memory | 0.6 MiB RSS, 0.5-1.7 MiB cgroup `MemoryCurrent` |
| `IPAddressDeny=any IPAddressAllow=127.0.0.1` in a **user** unit, connect to 127.0.0.53:53 | **connected**: not enforced without the system manager |
| `PrivateNetwork=yes` + `PrivateUsers=yes` in a user unit | works (only `lo`) |

- **Memory notes.** Firecracker's spec promises ≤ 5 MiB VMM overhead and ≤ 125 ms to guest init with a
  tuned kernel ([SPECIFICATION](https://github.com/firecracker-microvm/firecracker/blob/main/SPECIFICATION.md)).
  Our 85 MiB is mostly guest memory a generic distro kernel touches at boot; a tuned kernel plus balloon
  or free page reporting brings it down a lot. Kubernetes' own example sets 120Mi overhead for a Kata
  Firecracker RuntimeClass ([pod overhead](https://kubernetes.io/docs/concepts/scheduling-eviction/pod-overhead/)).
  A third-party benchmark (Kata 4.3.1, k3s 1.37) found idle pods at runc 5.3 MiB, gVisor 30.9 MiB, Kata
  (QEMU) 179.9 MiB ([computingforgeeks](https://computingforgeeks.com/?p=179575); indicative only). The
  gVisor figure matches ours.
- **Density on 8-16 GB.** gVisor: dozens of apps are fine. Kata: 10 apps cost about 1.5-2 GiB on top of
  the apps themselves, which is noticeable next to agent CLIs (0.5-1.5 GB each) and k3s itself.
- **Storage.** gVisor and Kata reuse PVCs (gVisor through the gofer, or directfs; Kata through virtiofs
  or block). Bare microVMs need per-volume disk images (virtio-blk, ext4 made by the server) or virtiofsd
  shares; Firecracker has no virtiofs. microvm.nix shares the host's `/nix/store` read-only over virtiofs
  or 9p ([hypervisor table](https://github.com/microvm-nix/microvm.nix#hypervisors)).
- **Networking.** gVisor (own netstack) and Kata (tap inside the pod netns) both still leave the pod
  through the host-side veth, where k3s's NetworkPolicy controller enforces policy, so the planned
  "default-deny + declared links" model carries over unchanged. Kata doesn't support host networking
  ([limitations](https://github.com/kata-containers/kata-containers/blob/main/docs/Limitations.md)),
  which apps never use anyway. Bare microVMs would need either per-tap nftables (CAP_NET_ADMIN, which the
  container has) or, more in Melissa's style, **no NIC at all**: published ports via vsock (a Unix socket
  on the host side in Firecracker and Cloud Hypervisor), proxied by the server like `melissa-netbridge`.
  That is default-deny by construction, with links as explicit proxies. systemd units: `IPAddressDeny=`
  (cgroup BPF, system manager only) or `PrivateNetwork=` plus `socket-proxyd`.
- **GPU and devices.** gVisor: NVIDIA only, through `nvproxy`. Kata and Cloud Hypervisor: VFIO
  passthrough of a whole device (IOMMU group, root setup, the device is lost to the host). Firecracker:
  no device passthrough (PCI hotplug of virtio devices only, developer preview). systemd units and runc
  pods: trivial (`DeviceAllow=` / device plugins). So media transcoding or Home Assistant USB sticks stay
  on `standard` pods.

### 3. Fit with Melissa

- **Manifest and approval.** RuntimeClass options keep the whole flow: the agent writes the same JSON plus
  one enum. The server validates it, the user confirms a summary that now names the isolation, and the
  server applies it with `melissa-apps`. Agents still never get root or cluster credentials. Bare
  microVMs or Nix units would need a second renderer and a new privileged component that creates VMs or
  units: in the container install the server would talk to systemd's D-Bus as root, or own `/dev/kvm`
  and taps itself. That is a larger, new attack surface in the server.
- **Agent-written Nix** means the agent's Nix expression *executes* at build time (in the Nix builder's
  sandbox, but with fixed-output fetches reaching the network). Approval would have to show a store path
  and closure, which the owner can't meaningfully review. Pinned OCI digests are easier to reason about.
- **Offline.** gVisor is one static binary pair. Kata is a shim, a guest kernel and a guest image, all
  shippable in the image or the Nix closure. Neither needs a registry beyond the app images themselves
  (unchanged). microvm.nix and Nix units are offline once the closure is built; building needs a binary
  cache or a long local build.
- **Rust integration.** RuntimeClass: nothing new, `runtimeClassName` in the rendered Pod (kube-rs
  already). Firecracker and Cloud Hypervisor: REST over a Unix socket (hyper or ureq over a UnixStream),
  or Cloud Hypervisor's CLI. systemd units: zbus (MIT) to `org.freedesktop.systemd1` `StartTransientUnit`.
- **Licences.** All compatible with an MIT project shipping them as separate binaries with notices (the
  existing `cargo-about` and image licence report): gVisor Apache-2.0, Kata Apache-2.0, Firecracker
  Apache-2.0, Cloud Hypervisor Apache-2.0 + BSD-3-Clause, crosvm BSD-3, libkrun Apache-2.0, microvm.nix
  MIT, Unikraft and kraftkit BSD-3, QEMU GPL-2.0 (fine as a separate process, never linked), systemd
  LGPL-2.1+.

### 4. Install modes

| | Rootful container (Quadlet, `--privileged`) | NixOS module | From source |
|---|---|---|---|
| gVisor RuntimeClass | add runsc + shim to the image, containerd template; no KVM | `pkgs.gvisor`, `services.k3s.containerdConfigTemplate` | same as host k3s; dev may lack k3s |
| Kata RuntimeClass | needs host `/dev/kvm` (already visible through `--privileged`), `vhost_vsock`/`vhost_net` modules on the host; ship shim + guest assets | nixpkgs `kata-runtime` + containerd template; `boot.kernelModules` | rarely |
| Firecracker / CH direct | works (verified rootless with `--device /dev/kvm`) | native, or microvm.nix | works as the user (this host's `/dev/kvm` is 0666) |
| microvm.nix | no | native (`microvm.vms.*`) | no |
| Nix + systemd units | no Nix inside the image | native | user units only: no IP filter, no DynamicUser |

### 5. Design sketch (gVisor prototype)

```
appspec::AppSpec { ..., isolation: Option<Isolation> }   // standard | sandboxed | vm
cluster::render  -> Pod.spec.runtimeClassName = "melissa-gvisor" | "melissa-kata"
                    labels["melissa.io/isolation"] = "sandboxed" | "vm"
packaging/common  RuntimeClass melissa-gvisor (handler runsc, overhead 32Mi),
                  melissa-kata (handler kata, overhead 160Mi), admission policy: label <-> class
                  containerd config template entry; runsc.lock (version + sha256)
app_platform      reports available isolation levels (runsc found, /dev/kvm present)
escape suite      the probes above, run in a sandboxed app pod (image run + NixOS VM test)
```

No new crate is needed. A `crates/vmm` crate would only be justified by the parked microVM-for-agents
idea.

## Open owner decisions

1. Default isolation for agent-built or untrusted images: `sandboxed` by default, or opt-in per app?
2. Ship gVisor in the container image (+~45 MB: runsc 32 MB, shim 12 MB) for everyone, or only on request?
3. Kata at all, given about 150 MiB per pod on small boxes, or keep `vm` for later hardware?
4. Should NetworkPolicy (default-deny, declared links) come before any of this? It is the bigger
   isolation gap for apps today.
5. Microvms for **agent sandboxes** (question 19): wanted as a direction, or bwrap plus seccomp plus
   users is enough?

## Sources

- Firecracker [releases](https://github.com/firecracker-microvm/firecracker/releases), [SPECIFICATION.md](https://github.com/firecracker-microvm/firecracker/blob/main/SPECIFICATION.md), [jailer](https://github.com/firecracker-microvm/firecracker/blob/main/docs/jailer.md), [AWS bulletin 2026-015](https://aws.amazon.com/security/security-bulletins/2026-015-aws)
- Cloud Hypervisor [releases](https://github.com/cloud-hypervisor/cloud-hypervisor/releases), [CVE-2026-27211](https://nvd.nist.gov/vuln/detail/CVE-2026-27211), [Phoronix: v54](https://phoronix.com/news/Cloud-Hypervisor-54)
- Kata Containers [releases (4.4.0)](https://github.com/kata-containers/kata-containers/releases), [4.0 preview: runtime-rs default](https://katacontainers.io/blog/release-4-0-0-preview/), [limitations](https://github.com/kata-containers/kata-containers/blob/main/docs/Limitations.md)
- gVisor [platforms](https://gvisor.dev/docs/architecture_guide/platforms/), [containerd quick start](https://gvisor.dev/docs/user_guide/containerd/quick_start/), [repo](https://github.com/google/gvisor)
- k3s [alternative container runtimes](https://docs.k3s.io/advanced#alternative-container-runtime-support)
- Kubernetes [pod overhead](https://kubernetes.io/docs/concepts/scheduling-eviction/pod-overhead/); third-party benchmark [computingforgeeks](https://computingforgeeks.com/?p=179575)
- microvm.nix [README](https://github.com/microvm-nix/microvm.nix) (MIT, 8 hypervisors)
- NixOS manual [containers](https://nixos.org/manual/nixos/stable/#ch-containers); systemd [systemd.resource-control](https://www.freedesktop.org/software/systemd/man/latest/systemd.resource-control.html), [systemd.exec](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html)
- Unikraft [unikraft](https://github.com/unikraft/unikraft), [kraftkit](https://github.com/unikraft/kraftkit), [unikraft.com](https://unikraft.com/)
- crosvm [repo](https://github.com/google/crosvm), libkrun [repo](https://github.com/containers/libkrun)

Spike scripts (throwaway, not in the repo): `fc.sh`, `vm.sh`, `svc2.sh` in the session scratchpad.
