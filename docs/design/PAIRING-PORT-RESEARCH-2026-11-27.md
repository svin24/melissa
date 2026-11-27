# Pairing port and OEM setup (research notes, 2026-11-27)

Notes for later. **Nothing here is built or decided.** Everything under "Proposal" is a proposal for the owner
to accept, change or reject. Open items: PLAN.md question 105. Related: `docs/design/INTERCONNECT-2026-11-27.md`
(layer 0 pairing), PLAN §4 (pairing, Q102), §6b (clients, Q14), §6c (people, claim and setup code, Q87, Q88).

## 0. The owner's idea and the hard constraints

The owner's idea (2026-11-27, as relayed to the agent that wrote this note):

> Melissa needs a "pairing" port on the LAN, on a port few things use, for (a) pairing devices/clients (phone
> app, desktop app, other Melissa devices) and (b) an OEM experience: someone buys a Melissa box, installs the
> app on phone/PC and sets the box up over the local network before setup is finished; a Wi-Fi-only box with
> no Ethernet might start its own temporary access point (SoftAP) where the user connects and enters the real
> network's SSID and password.

**Hard constraints from the owner. They shape every recommendation below.**

1. **Pairing during setup (before the owner has claimed the device) exists only behind an explicit OEM
   build/install flag.** Everyone else is warned against it.
2. **Otherwise the pairing port only ever opens from the Settings page, as an owner action.** Never by itself,
   never at boot, never by an agent (no agent tool may open it, propose opening it, or see its codes).

Context today: the UI listens on `127.0.0.1` only (install.sh `UI_BIND=127.0.0.1`, because sign-in runs over
plain HTTP); the first person claims with a one-time setup code (`crates/auth`: 10 characters from a
31-letter alphabet, about 50 bits, 1 hour, 10 wrong tries burn it, kept 0600 in `<state>/auth/setup-code.json`,
printed at the end of install.sh). Agents must never see codes or tokens.

## 1. How others do it

Each entry: what it does, then the lesson for Melissa.

### Matter commissioning
- Rendezvous over BLE, Wi-Fi SoftAP or on-network DNS-SD (`_matterc._udp`, registered with IANA; TXT carries a
  12-bit discriminator, optional vendor/product id, commissioning mode `CM`). Operational traffic uses port
  5540 (IANA `matter`, tcp/udp). [1][2][3]
- Onboarding payload: 27-bit setup passcode, 12-bit discriminator, vendor/product id; as a QR code or an
  11-digit manual pairing code. PASE (SPAKE2+ keyed by the passcode) sets up the first session; only one PASE
  session at a time. Then device attestation (a certificate chain from the vendor) and the commissioner
  installs an operational certificate (CASE afterwards). [2][4]
- The window: factory-new devices advertise after power-up (SDK default caps advertising at 15 minutes;
  locks and similar devices need a button press). For an already-commissioned device, an admin sends
  `OpenCommissioningWindow`, which makes a **new random passcode** for that window only (Enhanced
  Commissioning Method); the window closes on success or timeout. PASE session establishment times out after
  60 s. [3][5][6]
- Lesson: short-lived windows, a fresh secret per window, a PAKE so a 27-bit code is safe against offline
  guessing, and "open the window" is an admin action on an already-owned device. The factory-new passcode
  is static per device (printed), which is fine only because the window needs physical presence.

### Apple HomeKit (HAP)
- 8-digit setup code (`XXX-XX-XXX`), trivial codes like `12345678` forbidden; QR `X-HM://` URI with a setup id.
  Pair Setup is SRP-6a (3072-bit group, SHA-512), then Ed25519 long-term keys are exchanged; the code never
  crosses the wire. Unpaired accessories advertise `_hap._tcp` with `sf=1`. [7][8]
- Lesson: same as Matter: a PAKE over an untrusted network, then long-term keys. The `sf` flag shows that
  "I am unpaired" leaks in the advert, which is acceptable only because a PAKE protects the code.

### Home Assistant onboarding
- Port 8123, discovered via mDNS; the **first visitor** to the onboarding page creates the owner account. No
  code is required. [9]
- 2026: CVE-2026-64825 (critical, fixed in 2026.6.0): an **unauthenticated** attacker could write arbitrary
  files by uploading a crafted backup **during the onboarding window**; advice was not to expose instances
  during onboarding and to audit recently reset ones. [10][11]
- Lesson: an unclaimed device that accepts LAN requests is an attack surface for whoever gets there first,
  and pre-claim endpoints grow features (restore from backup) that turn the race into code execution.
  Melissa's setup code on loopback already avoids this; keep it.

### Ubiquiti UniFi adoption
- Devices look for the controller at `http://unifi:8080/inform` (TCP 8080, UDP 10001 discovery). Layer-3
  adoption is SSH with the factory default `ubnt`/`ubnt` and `set-inform`. Newer consoles and APs can be found
  and set up from the mobile app over Bluetooth (within about 3 m). [12][13][14]
- Lesson: a universal default SSH password is exactly what PSTI and ETSI EN 303 645 now forbid; the BLE path
  is the modern answer because radio range is a crude proof of presence.

### Synology (and QNAP)
- A new NAS is found by `find.synology.com` (web) or Synology Assistant (UDP 9997-9999 broadcast discovery),
  then installed through the Web Assistant in the browser; the first admin is created there. [15][16]
- Lesson: discovery and first-admin creation over the LAN with no secret; same race as Home Assistant. A
  cloud "find" service is a third-party call Melissa should not make (AGENTS.md).

### Umbrel, CasaOS, StartOS
- Umbrel: open `umbrel.local`, pick a name and password; the project's own notes say the LAN is trusted and
  served over plain HTTP; older versions shipped default OS logins (`umbrel`/`umbrel`). [17]
- CasaOS: first visit to the web UI (port 81 on some boards) creates the account; some images shipped a
  default SSH login. Whether anything gates the first visitor is **unverified**. [18]
- StartOS (0.4.x): open `http://start.local` from the same LAN (or Kiosk Mode with a monitor and keyboard),
  start fresh or recover; afterwards you download a file with the server's permanent `.local` URL and its Root
  CA, which you then trust on each client. **No pre-shared secret was found in the docs**; the protection is
  "be first on your LAN". (The task asked whether a key on printed paper or USB is used: not found.) [19][20]
- Lesson: the home-server field mostly trusts the LAN at first boot. Melissa already does better (setup code
  on the host). The StartOS idea worth keeping: hand the client its CA at the end of setup.

### Google Nest / Chromecast, Sonos
- Google Home app sets up Nest/Chromecast devices over BLE, falls back to Wi-Fi (the device's own SSID);
  QR code or a setup code when scanning fails; the phone must be close and on the same network. [21][22]
- Sonos: the app finds a new speaker on the same Wi-Fi, "Add", and ownership is tied to a Sonos account
  (transfer of ownership for second-hand units). [23]
- Lesson: phone app + BLE/SoftAP + a code on the device or packaging is the consumer norm. Both lean on a
  vendor cloud account, which Melissa does not have.

### Raspberry Pi Imager presets
- Imager 2.0 (November 2025) writes cloud-init: username and password are mandatory (no default user), SSH
  password or public key, Wi-Fi SSID and password, all chosen **before** the first boot. [24]
- Lesson: the cleanest "no default secret" path is to configure the image before it boots. For Melissa's
  appliance image this is the alternative to a SoftAP: put Wi-Fi and an owner SSH key or setup secret on the
  boot media.

### balena wifi-connect (captive-portal SoftAP)
- If no known network works at startup it creates an AP (default SSID "WiFi Connect", **open by default**,
  WPA2 only with `--portal-passphrase`), serves a captive portal on port 80, the user picks an SSID and
  password, it joins and drops the AP. Needs NetworkManager. Captive portals can take over a minute to pop
  up on iOS/Android. [25]
- Lesson: works and is open source, but the default (open AP) is the wrong default; set a per-device WPA2/WPA3
  passphrase and do not rely on captive-portal pop-ups.

### Improv Wi-Fi and Wi-Fi Easy Connect (DPP); why not WPS
- Improv (ESPHome): send Wi-Fi credentials over BLE or serial; optional **authorizer** = a physical button that
  must be pressed before the device accepts credentials. [26]
- Wi-Fi Easy Connect (DPP, Wi-Fi Alliance): the device's public key is bootstrapped by a QR code; the phone
  (configurator) scans it and provisions the network credentials; mostly Android 10+. [27]
- WPS: the PIN method's design flaw lets an attacker confirm each half of the 8-digit PIN separately (about
  11,000 tries); Reaver recovers it in hours; many routers had no lockout, and some don't really disable WPS.
  [28]
- Lessons: a button press is a cheap physical-presence factor; a QR code can carry a public key (not only a
  password); and a short code checked online in halves, without lockout, is how not to do it.

### Tailscale device approval, Syncthing device IDs, KDE Connect
- Tailscale: optional device approval; a new device cannot send or receive tailnet traffic until an admin
  approves it. [29]
- Syncthing: the device ID is the SHA-256 of the device's TLS certificate (base32); you add each other's IDs
  and the TLS handshake is pinned to them. [30]
- KDE Connect: both sides show a verification key derived from both certificates; the user compares and
  accepts (trust on first use plus a visual check). Bug reports show that users do compare these keys. [31]
- Lesson: identity = a key fingerprint; the human step is approval or comparison; this is what INTERCONNECT
  layer 0 already proposes.

### Bluetooth LE Secure Connections, numeric comparison
- ECDH (P-256) plus a 6-digit value shown on both sides; the user presses yes if they match. Protects
  against MITM; "Just Works" (no comparison) does not. Downgrade to legacy pairing must be refused. [32]
- Lesson: when both sides have a screen (phone app and Melissa's UI), a short comparison code after an
  unauthenticated key exchange is a valid alternative to typing a secret.

### Regulation: CRA, ETSI EN 303 645, UK PSTI
- **EU Cyber Resilience Act** (Regulation (EU) 2024/2847; main obligations apply from 11 December 2027;
  reporting from 11 September 2026). Annex I Part I (2): (b) "made available on the market with a secure by
  default configuration" (with a reset to the original state), (d) "protection from unauthorised access by
  appropriate control mechanisms", (e) confidentiality of transmitted data, (j) "limit attack surfaces,
  including external interfaces", (l) record and monitor relevant internal activity. [33][34]
- **ETSI EN 303 645** provision 5.1-1: where passwords are used in any state other than the factory default,
  they shall be unique per device or defined by the user; unique defaults must be generated so they resist
  automated attacks (not counters, not derived from public data such as serial numbers). [35][36]
- **UK PSTI Act 2022** (in force 29 April 2024): bans universal default passwords on consumer connectable
  products; a unique per-product password or a user-defined one is allowed; not based on counters, public
  information or identifiers like serial numbers (unless derived with an accepted cryptographic method). [37][38]
- Implications for an OEM factory secret: it must be **random per device** (not derived from the serial or MAC
  with a public method), the pairing interface must be **off by default** or gated (CRA (b), (j)),
  pre-claim access must still be authenticated (CRA (d)), traffic encrypted (CRA (e)), events logged (CRA
  (l)), and a factory reset must return to a secure state (the same label secret, never "open"). Whether
  Melissa as open-source software, or an OEM selling boxes, is the CRA "manufacturer" is an open question
  for the owner (licensing/legal, not for an agent to decide).

## 2. Proposal for Melissa (not decided)

### 2.1 Shape: closed by default, owner opens a window

- **No listener by default.** The pairing port is not bound at all until the owner opens a pairing window.
  Not at boot, not after an update, not after a restart (a restart closes any open window).
- **Settings → Devices → "Pair a device"** (exact place to be decided; built only from existing components,
  per the UI rule). The signed-in owner (or a member with a future "pair devices" permission, owner's call)
  presses it and **re-enters the password** (like other sensitive actions in §6c).
- **The window:** one device per opening; closes on the first success, on Cancel, or after a timeout
  (proposal: 10 minutes; Matter-like values are 3-15 minutes). A second device needs a new opening.
- **What the owner sees:** a QR code plus a typed fallback code, shown only to the owner's browser (the same
  channel as the setup password reveal and the Tailscale sign-in URL). Never in chats, agent context, logs or
  the audit text (the audit records that a window opened and who paired, never the code).
- **After the exchange the owner approves** the joining device (name, kind, key fingerprint shown), as
  INTERCONNECT layer 0 already proposes. Pairing = authenticated channel + owner approval; neither alone.
- **Agents:** no tool opens, extends or reads the window; the lead agent may at most see "a device was paired"
  after the fact. A `pairing_*` tool should not exist.
- **Rate limits:** the PAKE makes offline guessing useless; online guessing is bounded per window (proposal: 5
  failed attempts close the window, like Melissa's setup code burning after 10, and unlike WPS). Only one
  handshake in progress at a time (Matter does the same).
- **Audit lines:** `pairing.window_opened` (who, until when), `pairing.attempt_failed` (peer address, count),
  `pairing.window_closed` (reason: success, timeout, cancel, too many failures), `pairing.device_approved` /
  `pairing.device_declined` (device id, fingerprint). Codes never appear.

### 2.2 Port and discovery

- **Port proposal: 7380/tcp.** IANA lists 7366-7390 as unassigned (registry as of 2026-11-25); no common
  software default found for 7380 in a web search; below Linux's ephemeral range (32768-60999) and outside
  k3s's NodePort range (30000-32767), so it never collides with Melissa's app ports 30000-30079 or the
  tailnet port 41641/udp. Also clear of Melissa's UI 8080 (host) and 8081 (container), k3s 6443/10250,
  flannel 8472/udp, Matter 5540 and Home Assistant 8123. Alternatives in unassigned blocks: 25252, 24642. An
  IANA port assignment is possible later but not needed for a LAN service.
- **Configurable**, like `UI_PORT`: an install variable (for example `PAIR_PORT=7380`) for people who already
  use it. The container needs it published on the LAN address (not loopback) only while a window is open;
  with podman's port publishing that means either publishing it always and having Melissa's own nftables
  table drop it while closed (like the app access filter), or a host-side helper. To be designed.
- **DNS-SD: `_melissa-pair._tcp`** (service name 12 characters, under the 15-character limit; not registered with
  IANA today, registration is free and optional). Advertised **only while a window is open**.
- **TXT records may carry:** `v=1` (protocol version) and a random per-window discriminator (`d=`, say 12 bits,
  also inside the QR code) so the app picks the right box when several are pairing. **TXT must not carry:**
  the device or owner name, host name, serial number, model, Melissa version, whether it is claimed, or
  anything derived from the code. The instance name should be neutral ("Melissa") or the random
  discriminator.

### 2.3 Protocol

Two candidate designs; they can coexist (QR first, typed code as fallback).

1. **QR carries the certificate fingerprint and a long secret.** The QR encodes
   `melissa-pair:v1?h=<addr>:<port>&fp=<sha256 of the window's TLS cert>&s=<128-bit secret>&d=<discriminator>`.
   The client connects with TLS pinned to `fp` and proves `s` inside the TLS session (bound to the TLS
   exporter, RFC 5705/8446). Simple, no PAKE crate needed, but the QR must be scanned: no short code to type.
2. **A PAKE with a short typed code.** The window shows a code (Melissa's existing alphabet: `ABCDE-FGHJK`, about
   50 bits; or a shorter 8-digit code like Matter/HomeKit, safe only because of the PAKE plus the attempt
   limit). TLS with a self-signed certificate per window, then the PAKE runs inside it, and the PAKE
   transcript includes the TLS exporter value so a man in the middle who terminates TLS fails. Afterwards each
   side records the other's long-term key (the layer-0 identity).
   - Rust crates checked (2026-11-27): `spake2` (RustCrypto PAKEs, 0.4.0, says it has **never had an
     independent audit**) [39]; `spake2plus` (RFC 9383, small) [40]; `opaque-ke` (Meta, 4.0.x, NCC Group audit
     in 2021, maintained) [41]; `pake-cpace` (CPace, small, one maintainer) [42]. OPAQUE is an augmented PAKE
     for a client/server password and is heavier than needed for a one-off code; SPAKE2 or CPace (balanced)
     fits a one-time code. Choice pending a closer review (AGENTS.md: a dependency needs a reason).
- **Short vs long code:** with a PAKE plus 5 attempts per window, 8 digits (~27 bits) gives an attacker at most
  5 in 10^8 per window; 50 bits is more than enough and matches the existing setup code UX. Without a PAKE
  (plain code over TLS that the client cannot verify), even 50 bits leaks to a MITM, so a PAKE or the QR
  fingerprint is required either way.
- **Comparison alternative:** for device-to-device pairing where both screens are in front of the owner,
  BLE-style numeric comparison (both sides show 6 digits derived from the handshake, the owner confirms) is
  acceptable, but only after the owner opened the window.
- **What gets exchanged:** exactly the INTERCONNECT layer-0 payload: the joining device's Ed25519 key, id,
  name, kind, specs and a CSR; the answer is a device certificate from the device CA plus the CA certificate.
  For a phone or desktop client app it is a per-person, revocable client credential (ties to Q87h and Q14),
  issued to the person who opened the window.

### 2.4 Pre-claim OEM mode (only behind a flag)

**Off unless the build or installer sets it. Everyone else is warned against it.**

- **Flag:** proposal `MELISSA_OEM_PAIRING=label` (install/build time, not a Settings switch, not an env an
  agent can set; recorded in the install summary). Without it the pre-claim pairing code path is not
  compiled in, or not reachable (to be decided: a cargo feature `oem-pairing` is stronger than a runtime
  check).
- **Why plain LAN pre-claim setup is dangerous:** whoever reaches an unclaimed device first owns it. Home
  Assistant's first-visitor onboarding and its 2026 onboarding-window CVE, Synology/CasaOS/Umbrel first-admin
  pages, UniFi's `ubnt`/`ubnt` and WPS's PIN all show the same thing: an unclaimed, LAN-reachable device is
  attacked in the window between power-on and claim. A shared flat, guest, dorm or office network makes it
  worse.
- **Per-device secret on a label:** a random secret generated per unit at the factory (or first-boot
  provisioning station), **not derived from the serial or MAC**, printed as a QR code and a typed code on the
  device and in the box. The device keeps only a verifier (SPAKE2+ registration record), not the plaintext,
  where possible. This satisfies ETSI 5.1-1 and PSTI (unique per device, not guessable from public data).
- **Same protocol as 2.3**, with the label code instead of a window code; the "claim" over this channel
  creates the owner account exactly like today's claim page (username + password) and **closes pre-claim
  pairing forever** (until a factory reset, which returns to the same label secret, never to "open").
- **Physical presence on top (recommended for OEM):** the pre-claim window opens only after a button press
  on the box (Improv's authorizer, Matter locks), or only over BLE/SoftAP (radio range), and closes after 15
  minutes; another button press opens it again.
- **Loud warnings:** install.sh prints a boxed warning and requires typing a confirmation when the flag is
  set; README and the OEM guide explain the race; Settings shows "OEM pairing was enabled on this device until
  it was claimed" in the audit.
- Agents: before the claim there is no owner and no agent runs with any authority over pairing; the lead
  agent starts after the claim as today.

### 2.5 SoftAP for Wi-Fi-only boxes

- **Who creates it:** not the container install. Melissa in a podman container on someone's PC must never
  touch the host's Wi-Fi. Only an OEM/appliance image (the paused reference distro, §2) that owns
  NetworkManager (hotspot mode) or hostapd can do this, behind the same OEM flag.
- **When:** only if no network is configured and no Ethernet link is up at boot, or after a factory reset;
  never on a box that has a working network.
- **Security:** WPA2-PSK at least, WPA3-SAE where the radio supports it (SAE resists offline dictionary
  attacks; WPA2 does not, so the passphrase must be long and random), with a **per-device passphrase on the
  label** (the same label as 2.4 or a second line). Never an open AP (balena's default). SSID neutral, for
  example `Melissa-<4 random chars from the label>`.
- **App vs captive portal:** prefer the phone/desktop app (it scans the label QR, joins the AP, runs the 2.3
  protocol, sends SSID and password inside it). A captive portal is the fallback for people without the app,
  but pop-ups are slow and flaky on iOS/Android [25]; a plain-HTTP form on the AP is protected only by the
  WPA2/WPA3 link, so it is as strong as the AP passphrase.
- **After the join:** shut the AP down as soon as the box gets an address on the real network; tell the app
  the new address (or `melissa.local`); then the claim continues on the real network over the 2.3 channel.
  **Fallback:** if the join fails (wrong password, 5 GHz-only network, captive network), bring the AP back
  with the error for a limited number of tries, then stay on the AP until the user retries or presses the
  button. Image-time presets (Raspberry Pi Imager style, Wi-Fi on the boot media) are an alternative for
  makers flashing their own SD card.
- Alternatives: BLE provisioning (Improv-like or a Melissa GATT service) needs BlueZ access on the appliance
  image; Wi-Fi Easy Connect (DPP) is elegant but mostly Android 10+ and depends on the radio driver.

### 2.6 Fit with INTERCONNECT layer 0 and existing questions

- Same identity and certificate exchange as layer 0; the pairing port is the **LAN rendezvous** for it, while
  INTERCONNECT assumes the tailnet. Proposal: one protocol, two listeners (tailnet and LAN), both closed
  until the owner opens a window; the join code from layer 0 and the window code here become the same thing.
- **Q102 (a):** answers "which port and listener" with a proposal (7380/tcp, LAN and tailnet, only while a
  window is open) and "is ~50 bits enough" (yes with a PAKE and an attempt limit).
- **Q14 / §6b (client apps and how they authenticate)** and **Q87 (h)** (per-person API tokens): pairing
  issues the client credential.
- **Q22 (LAN TLS)**: the device CA from pairing can also serve the LAN UI; a paired client app trusts the CA it
  received, which sidesteps Android's distrust of user-installed CAs for Melissa's own app (not for browsers).
- **Q37** (loopback API auth) and **Q88** (onboarding): the OEM claim path reuses the claim and setup flow.
- **§2 reference distro (paused):** SoftAP and button press only make sense there.

### 2.7 Owner's follow-up (2026-11-27, not decided)

- The owner may drop the per-device initial key for OEM mode altogether: the box is unclaimed only for the
  short time between unboxing and setup, so a label secret may not be worth it.
- OEMs could instead be told to add a physical reset button that wipes everything and returns the box to its
  factory (OEM install) state. Melissa already has a factory reset (PLAN §6d) to build on.
- To weigh when this comes back: without a key, anyone on the same network during that window can claim the
  box first (the Home Assistant lesson, section 1); the CRA, ETSI EN 303 645 and PSTI rules (section 1) are
  about default passwords, so a keyless window needs checking against them; a short window that only opens
  after a button press or only on the setup Wi-Fi (2.5) narrows the risk.

## 3. Open questions for the owner (PLAN question 105)

(a) Port 7380/tcp and `_melissa-pair._tcp`, or another number; configurable by `PAIR_PORT`?
(b) QR with fingerprint + secret only, PAKE with a typed code only, or both? Which PAKE (SPAKE2 vs CPace;
    none of the Rust crates has a recent audit)?
(c) Window length (10 minutes proposed), attempts per window (5), and may a member with a permission open it,
    or only the owner?
(d) OEM flag form: cargo feature (`oem-pairing`, not compiled in by default) plus install flag
    `MELISSA_OEM_PAIRING=label`, or install flag only? Button press required for pre-claim OEM pairing?
(e) Who is the CRA "manufacturer" for a box sold with Melissa (the OEM, or also the project)? Legal question.
(f) SoftAP only on the appliance image: revive the reference distro track for it, or leave SoftAP to OEMs?
(g) LAN listener and the tailnet listener for layer 0: one protocol on both, or tailnet only for devices and
    LAN only for client apps?
(h) Does a paired client app get the CA for the LAN UI (helps Q22), and does that change `UI_BIND`'s default?

## Sources (read 2026-11-27)

1. IANA Service Name and Transport Protocol Port Number Registry (CSV, last modified 2026-11-25) —
   https://www.iana.org/assignments/service-names-port-numbers/service-names-port-numbers.csv
2. Nordic, Matter network commissioning — https://nrfconnectdocs.nordicsemi.com/addons/ncs-matter/latest/matter/overview/commissioning.html
3. Google Home Developers, Commissionable discovery — https://developers.home.google.com/matter/primer/commissionable-and-operational-discovery
4. Nordic, Matter network security — https://nrfconnectdocs.nordicsemi.com/addons/ncs-matter/latest/matter/overview/security.html
5. Amazon, Best practices to commission Matter devices with Alexa — https://developer.amazon.com/en-US/docs/alexa/smarthome/best-practices-commission-matter.html
6. Nordic, Advanced Matter Kconfig options (advertising duration) — https://nrfconnectdocs.nordicsemi.com/ncs/latest/nrf/protocols/matter/getting_started/advanced_kconfigs.html
   and connectedhomeip `CommissioningWindowManager.cpp` (PASE timeout 60 s) — https://gpl.boxmatrix.info/zzzz-none-000/packages/connectedhomeip/src/app/server/CommissioningWindowManager.cpp
7. hap-crypto crate docs (HAP Pair Setup, SRP-6a) — https://docs.rs/hap-crypto
8. Protocol::HAP::SetupCode (setup code rules) — https://metacpan.org/pod/Protocol::HAP::SetupCode
9. Home Assistant, Onboarding — https://www.home-assistant.io/getting-started/onboarding/ and Authentication — https://www.home-assistant.io/docs/authentication/
10. NVD, CVE-2026-64825 — https://nvd.nist.gov/vuln/detail/CVE-2026-64825
11. VulnCheck, Home Assistant Core path traversal via backup upload — https://www.vulncheck.com/advisories/home-assistant-core-path-traversal-file-write-via-backup-upload
12. Ubiquiti Help Center, adoption troubleshooting (TCP 8080, UDP 10001) — https://help.ui.com/hc/en-us/articles/360012622613
13. HostiFi, Adopt a UniFi AP via SSH — https://www.hostifi.com/support/unifi-adopt-a-unifi-ap-via-ssh
14. Ubiquiti Help Center, connecting to a console over Bluetooth — https://help.ui.com/hc/en-us/articles/4416276882327
15. Synology KB, How do I find my Synology NAS via Web Assistant or Synology Assistant — https://kb.synology.com/DSM/tutorial/Unable_to_Locate_NAS
16. Synology KB, network ports used by DSM services — https://kb.synology.com/en-uk/DSM/tutorial/What_network_ports_are_used_by_Synology_services
17. Umbrel, Setting up Umbrel Home — https://umbrel.com/support/getting-started/setting-up-umbrel-home and community thread — https://community.umbrel.com/t/how-insecure-is-umbrel-really/1649
18. Radxa, CasaOS introduction — https://docs.radxa.com/en/fogwise/airbox/casaos/casaos_intro
19. Start9, StartOS 0.4 initial setup — https://docs.start9.com/start-os/0.4.0.x/initial-setup.html
20. Start9, Trust your Root CA — https://docs.start9.com/latest/user-manual/trust-ca
21. Google Nest Help, Set up your Google Nest or Home speaker or display — https://support.google.com/googlenest/answer/7126472
22. Google Nest Help, Requirements for Google Nest and Chromecast devices — https://support.google.com/googlehome/answer/7035987
23. Sonos, Setup guide — https://www.sonos.com/en-us/guides/setup
24. Raspberry Pi, A new Raspberry Pi Imager (2.0) — https://www.raspberrypi.com/news/a-new-raspberry-pi-imager/
25. balena wifi-connect README — https://github.com/balena-os/wifi-connect/ and forum (captive portal delay) — https://forums.balena.io/t/captive-portal-takes-more-than-1-minute-to-appear-on-ios-and-android-devices/203597
26. ESPHome, Improv via BLE (authorizer) — https://esphome.io/components/improv_ble/
27. Espressif ESP-IDF, Wi-Fi Easy Connect (DPP) — https://docs.espressif.com/projects/esp-idf/en/v6.0.3/esp32/api-reference/network/esp_dpp.html
28. CERT/CC VU#723755, WPS PIN brute force — https://www.kb.cert.org/vuls/id/723755
29. Tailscale, Device approval — https://tailscale.com/kb/1099/
30. Syncthing, Understanding Device IDs — https://docs.syncthing.net/dev/device-ids.html
31. KDE Bugzilla 501352 (verification key) — https://bugs.kde.org/show_bug.cgi?id=501352
32. Bluetooth SIG, Bluetooth pairing part 4 (numeric comparison) — https://www.bluetooth.com/blog/bluetooth-pairing-part-4/
33. EUR-Lex, Regulation (EU) 2024/2847 — https://eur-lex.europa.eu/eli/reg/2024/2847/oj
34. Annex I text (secondary copy) — https://www.springlex.eu/en/packages/cra/cra-regulation/annex-1/
35. Zephyr, ETSI EN 303 645 mapping (5.1-1) — https://docs.zephyrproject.org/latest/security/standards/etsi-303645.html
36. UK NCSC, Device security principles: appropriate authentication — https://www.ncsc.gov.uk/collection/device-security-guidance/security-principles/support-appropriate-authentication
37. UK Government press release, PSTI regime in force 29 April 2024 — https://www.gov.uk/government/news/new-laws-to-protect-consumers-from-cyber-criminals-come-into-force-in-the-uk
38. techUK, The PSTI Act for consumer IoT explained — https://www.techuk.org/resource/the-psti-act-for-consumer-iot-explained.html
39. `spake2` crate — https://docs.rs/crate/spake2/latest
40. `spake2plus` crate — https://docs.rs/spake2plus
41. `opaque-ke` crate — https://docs.rs/crate/opaque-ke/latest
42. `pake-cpace` crate — https://docs.rs/pake-cpace

Caveats: the Matter spec's own window limits were not read (only SDK and vendor docs); the CRA wording comes
from a secondary copy of Annex I; CasaOS's first-visitor behaviour is unverified; no Rust PAKE crate has a
recent public audit.
