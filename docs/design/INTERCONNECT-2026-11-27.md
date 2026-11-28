# Device interconnect (research, direction and runbook, 2026-11-27)

Owner goal (2026-11-27): a cluster network of "smart" and "dumb" devices, where some devices can go offline,
that scales a long way with AI. A dumb device can be, for example, a desktop or Android client app that connects
to a Melissa server. This document records the agreed direction, what the current code would do with a second
device (code reading only, **untested** when written), and a runbook for a first manual experiment. Section 6
has the experiment's results and the follow-ups built since.
Decision record: DECISIONS.md, 2026-11-27. Open items: PLAN.md questions 102, 103 and 107.

## 1. Terms

- **Smart device:** runs the full Melissa container (lead agent, k3s, apps).
- **Dumb device or client:** runs only `melissa-device` (`crates/device`, a stub today) or a client app (desktop,
  Android). It reports capacity, takes simple jobs, and has no AI and no k3s.
- **Offline is normal.** Laptops sleep, phones lose signal. Nothing in layers 0 and 1 may assume a device is up.

## 2. Direction: three layers

### Layer 0 — mesh and pairing

Every device gets a Melissa identity through an owner-approved, one-time join code.

Proposal (not final; PLAN question 102):
- The owner creates a join code on a smart device. About 50 bits, valid 1 hour, single use.
- The code carries the pairing address and the fingerprint of that device's device CA.
- The joining device connects over TLS pinned to that CA fingerprint. It sends its Ed25519 public key, a device
  id, a name, its specs (CPU, RAM, disk, GPU) and a certificate signing request.
- The owner approves the request in the UI and re-enters the password (like other owner approvals).
- The smart device returns a signed device certificate.
- Join codes and tokens never reach agents, chats or logs (same rule as setup passwords and the Tailscale
  sign-in URL).

Transport: the tailnet, using Melissa's built-in tailscaled or the host's Tailscale. The mesh stays behind a
swappable Rust trait (PLAN §4, `crates/mesh` planned), so Headscale or NetBird can follow later.

### Layer 1 — federation (build first)

- Every smart device keeps its own k3s and its own lead agent.
- Lead agents talk over an authenticated device-to-device API (layer-0 certificates, mutual TLS).
- The API offers a capacity view of every device and "run this there". The receiving device's lead agent
  brokers the request, under that device's own approval flow. A device never applies work on another device
  directly.
- Dumb devices and client apps speak a smaller part of the same API: report capacity, take simple jobs.
- A device that is offline simply drops out of the capacity view until it comes back.

Why first: it needs no shared control plane, survives devices going offline, and keeps each device's approvals
and audit local. It also matches AGENT-MODEL.md (one lead agent per device).

### Layer 2 — cluster (later, opt-in)

- A group of always-on smart devices can form one k3s cluster: the control plane on one device, the others as
  k3s agents.
- Joining uses the layer-0 identity to hand out a short-lived bootstrap token (`k3s token create --ttl`) [3].
  The k3s server token is never handed out: it is the cluster root.
- Devices that go offline often (laptops, phones) stay in layer 1.

## 3. What the current code would do (code reading, untested)

1. **Node names collide.** `packaging/container/melissa.container` sets `HostName=melissa` and k3s gets no
   `node-name`, so both devices register as node `melissa`. The second join fails with "Node password
   rejected".
2. **The tailnet input is closed.** `crates/settings/src/app_access.rs` drops everything from `tailscale0` to the
   container except app ports. That includes 6443/tcp (API), 10250/tcp (kubelet) and 8472/udp (flannel VXLAN).
3. **k3s settings don't survive a restart.** `/etc/rancher/k3s/config.yaml.d` is not on the `melissa-var` volume,
   and the node IP is the podman bridge address, which changes per start. Hand-written k3s config is lost when
   the container is recreated.
4. **The agent token is the cluster root by default.** Without `--agent-token`, k3s agents join with the server
   token [3]. Also, kubelets can set their own node labels, so role labels that steer placement
   (`melissa.io/role=...`) need a protected prefix (`node-restriction.kubernetes.io/`) or an admission check.
5. **The joining device's lead agent loses cluster access.** `melissa-kubeconfig` targets the local k3s server.
   As a k3s agent, the laptop has no local API server, so its lead agent can't see the cluster.
6. **No placement.** The app spec (`crates/appspec`) has no field for node selection or tolerations.
7. **MTU.** Flannel VXLAN over Tailscale gives pods an MTU of about 1230 (Tailscale's 1280 minus VXLAN
   overhead). Pods created before the change keep 1450 and can drop large packets.
8. **Skip k3s `--vpn-auth`.** k3s's built-in Tailscale integration is experimental [1] and would fight
   Melissa's own control of tailscaled.

k3s needs 6443/tcp from agents to the server, 10250/tcp between all nodes and 8472/udp between all nodes for
flannel VXLAN [2].

## 4. Effect on existing open questions

- **Q6 (lead agent placement):** narrowed. In layers 0 and 1 every device keeps its own lead agent; placement of
  a shared control plane only matters for layer-2 groups.
- **Q56 (unique ports cluster-wide):** narrowed. In layer 1 ports stay per device; uniqueness only matters
  inside a layer-2 cluster.
- **Q85a (Tailscale auth keys):** interactive sign-in is enough for now; layer-0 pairing doesn't need auth keys.

## 5. Next experiment (next session)

A manual k3s agent join of the owner's laptop to the desktop over the tailnet. It **measures the shared network
problems** (ports, MTU, latency, restarts, sleep). It is not the architecture: layer 1 is built first.

The laptop is not on the tailnet yet. Addresses below are placeholders: desktop `melissa-desktop`, 100.64.0.1;
laptop `melissa-laptop`, 100.64.0.2; tailnet `example.ts.net`.

Steps marked **[C]** change a machine or the tailnet: confirm with the owner first.

0. Record the current state: image digests on both devices, `kubectl get nodes -o wide`, interface MTUs, whether
   each device uses the host's or the built-in Tailscale, the tailnet's ACL type (default or custom) and whether
   device approval is on.
1. **[C]** Desktop: if it uses the host's Tailscale, reinstall with `HOST_TAILSCALE=no`. Then Settings →
   Tailscale → Enable and Sign in (`melissa-desktop`, 100.64.0.1).
2. **[C]** Laptop: install, claim, then Settings → Tailscale → Enable and Sign in (`melissa-laptop`, 100.64.0.2).
   **[C]** Only with a custom ACL: tag both nodes and grant `tag:melissa` → `tag:melissa` on tcp:6443, tcp:10250
   and udp:8472.
3. `tailscale ping` both ways; note direct or DERP.
4. **[C]** Temporary nftables rules inside each container (lost on restart):
   - Desktop:
     `nft insert rule inet melissa_app_access input iifname "tailscale0" ip saddr 100.64.0.2 tcp dport {6443,10250} accept`
     and `nft insert rule inet melissa_app_access input iifname "tailscale0" ip saddr 100.64.0.2 udp dport 8472 accept`.
   - Laptop: the same from 100.64.0.1 for tcp 10250 and udp 8472.
5. **[C]** Desktop: write `/etc/rancher/k3s/config.yaml.d/90-local.yaml` with `node-ip: 100.64.0.1`,
   `flannel-iface: tailscale0` and `tls-san` (100.64.0.1 and the MagicDNS name); restart k3s. Create a join
   token with `k3s token create --ttl 1h`. Keep the token out of agent chats.
6. **[C]** Laptop, fresh, no apps: stop k3s, remove `cni0` and `flannel.1`, write `90-local.yaml` with
   `server: https://100.64.0.1:6443`, the token, `node-ip: 100.64.0.2`, `flannel-iface: tailscale0`, a unique
   `node-name` and `node-taint: melissa.io/experiment=true:NoSchedule`. Add a runtime override for the k3s unit
   with `ExecStart=/usr/bin/k3s agent`, then start it.
7. Checks: `kubectl get nodes -o wide`, `kubectl get runtimeclass`, `kubectl top nodes`, and ask the desktop's
   lead agent what it sees.
8. **[C]** Deploy a small app, patch its Deployment with a `nodeSelector` and toleration for the laptop, check it
   runs under `runsc`, open its link from the desktop.
9. Close the laptop lid for 6 minutes: record when the node turns NotReady and when pods are evicted.
10. Record join time, errors, MTU, latency, and whether a desktop container restart breaks the join (expected:
    yes, see finding 3).

Rollback: `systemctl restart melissa` on each host (the container is recreated from the image, so the drop-ins,
nftables rules and unit override are gone), then `kubectl delete node <laptop node name>` on the desktop. The laptop's k3s data on the volume still holds
agent state; whether its server comes back cleanly is **unverified** (check `kubectl get nodes` on the laptop).

## 6. Experiment results 2026-11-27

The manual join from section 5 was run on the owner's two devices. Names and addresses here are the
placeholders from section 5.

- **Join:** the laptop joined the desktop as a k3s agent over Melissa's own Tailscale (built-in tailscaled on
  both, `HOST_TAILSCALE=no`), with the temporary nftables rules from step 4 and a `k3s token create --ttl`
  token.
- **Workload:** a small app pinned to the laptop (`nodeSelector` plus toleration) ran under gVisor (`runsc`);
  its pod was Ready 4 s after it was scheduled. The desktop reached it 3 times out of 3, in 0.18 to 0.26 s.
- **Logs:** `kubectl logs` from the desktop worked through the laptop's kubelet (10250/tcp over the tailnet).
- **Offline:** with the laptop's Wi-Fi off, its node turned `Unknown` (Ready condition) after about 1 minute.
  About 3 minutes after Wi-Fi came back the node was `Ready` again by itself. The pod survived with 0
  restarts (it came back before the default 5-minute eviction).
- **Path:** `tailscale ping` showed a DERP relay, never a direct connection: behind podman's NAT nothing
  forwarded tailscaled's UDP port.
- **k3s config:** an agent refuses server-only keys such as `write-kubeconfig-mode` from the shared
  `config.yaml.d`, so an agent needs its own config.
- **Persistence:** an image update recreates the container and drops `/etc` and `/run` (outside the
  `melissa-var` volume), so join state (token, role, node name) must live under `/var`.

Follow-ups built on the `multi-device-groundwork` branch (parked, multi-device is post-MVP):

- Unique node names, pinned under `/var/lib/melissa-container/cluster`; the server or agent role, server URL,
  node IP and peers in root's own setting there (root only; an older install's `/etc/melissa/cluster.toml` is
  taken over once); an agent gets its own k3s config without server-only keys; the join token lives in a
  root-only file under `/var` (finding 3, finding 1). Joining and invitations run only as root for now
  (`podman exec ... melissa-server cluster join|invite`); the server may only remove a device or leave (PLAN
  Q108).
- The app access filter lets the peers in root's setting reach k3s's ports on `tailscale0`, rebuilt at every
  k3s start (finding 2).
- install.sh publishes tailscaled's port 41641/udp when the host doesn't run Tailscale (the DERP-only path).
- The server labels nodes `node-restriction.kubernetes.io/melissa-role=server|agent`, a prefix a kubelet may
  not set on itself (finding 4).
- The web UI finds this device's own node by its pinned name and shows a node whose Ready condition is
  `Unknown` as Offline, apart from Not ready.

Two-device test of the groundwork branch, 2026-11-28 (same two devices, tailnet addresses 100.64.0.x):

- **Timings:** join 16 s; rejoin after a restart 12 to 21 s; leave 9 s.
- **Workload:** the gVisor pod was Ready 1.2 s after it was scheduled; the desktop reached it 6 times out of 6,
  in 0.15 to 0.25 s.
- **Path:** now direct (no DERP), but through the router's hairpin NAT (about 100 to 125 ms): the tailscaled in
  the container sees only its container address, so it never offers the LAN address as an endpoint. A
  LAN-direct path is a follow-up (PLAN Q107).
- **Node IP change:** when the node IP moves to the tailnet address (the first invitation), k3s exits once
  with status 1 (kube-proxy: "NodeIPs changed") and systemd restarts it; nothing to do.
- **Removing a device:** pods bound to the deleted node were garbage-collected by Kubernetes by itself within
  about 2.5 minutes; nothing needed force-deleting.

Still open: PLAN questions 103, 107, 108 and 109.

## Sources (read 2026-11-27)

1. k3s docs, Distributed hybrid or multicloud cluster — https://docs.k3s.io/networking/distributed-multicloud
2. k3s docs, Requirements (inbound rules) — https://docs.k3s.io/installation/requirements
3. k3s docs, `k3s token` — https://docs.k3s.io/cli/token
