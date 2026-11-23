# Acknowledgments and third-party notices

Melissa stands on the work of others. This file credits projects that shaped its design and lists third-party
software that Melissa redistributes (in its binaries or container image), with their licenses.

## Design inspiration (ideas only, no code copied)

These projects influenced how Melissa is designed. Melissa does not contain their code; their ideas were studied
and re-implemented in Melissa's own words and structure. If code is ever copied from them, its original copyright
and license notice must be kept with it and listed below under "Included third-party software".

- **T3 Code** — <https://github.com/pingdotgg/t3code>, MIT License, Copyright (c) 2026 T3 Tools Inc.
  Inspired how Melissa talks to many agent backends: one session interface over several protocols, the Agent Client
  Protocol (ACP) flow, mapping agents' permission requests onto an approval policy, and testing with recorded,
  replayed agent sessions. Since 2026-11-20 also the Settings → Providers "Add provider" flow and its use of the
  public ACP agent registry (studied as a reference; Melissa's flow, registry parsing and components are its own
  code, and no T3 Code icons or source files are included).
- **Universal Blue** (ublue-os) — <https://github.com/ublue-os>, Apache License 2.0, by the Universal Blue
  contributors. Inspired how Melissa built and maintained its bootc images: Containerfile structure and layer order,
  image linting, signing and update practices, and the choice to stay on Fedora's signed kernel for Secure Boot.
  That bootc work is archived (git tag `bootc-archive-2026-11-19`); the container image keeps the Containerfile
  layering and image-lint ideas.

- **Hermes Agent** (Nous Research) — <https://github.com/NousResearch/hermes-agent>, MIT License, Copyright (c)
  2025 Nous Research. Inspired how Melissa plans to delegate work to short-lived sub-agents: a child gets only its
  goal and a subset of the parent's tools, cannot delegate further, and returns a size-capped summary, with "what was
  done" derived by the harness rather than trusted from the model.
- **tasteskill** (design-taste-frontend) — <https://github.com/Leonxlnx/taste-skill>, MIT License, Copyright (c)
  2026 Leonxlnx (studied at commit `f6b7657`). A design skill for marketing websites. Inspired parts of Melissa's
  UI craft guide (`docs/agent-skills/ui-craft.md`): one system of sizes, equal edges, one main action, plain copy
  and a list of machine-written copy tells, restrained hover timing with reduced motion, and checking every page
  in a browser at fixed widths in light and dark. Restated in Melissa's words; no text or scripts copied.

## Included third-party software

Redistributed **in the Melissa binaries**:

| Component | License | Notes |
|---|---|---|
| htmx 2.0.4 | BSD Zero Clause (0BSD) | Vendored in `crates/web/assets/`, embedded in the binary |
| htmx SSE extension (htmx-ext-sse) 2.2.4 | BSD Zero Clause (0BSD), Copyright (c) 2023 Alexander Petros | `crates/web/assets/htmx-ext-sse.min.js` from the npm package (integrity checked); license in `htmx-ext-sse.LICENSE.txt` |
| Atkinson Hyperlegible Next (font) | SIL Open Font License 1.1, Copyright 2020-2024 The Atkinson Hyperlegible Next Project Authors (Braille Institute) | `crates/web/assets/fonts/`, from google/fonts, Latin subset as WOFF2; license in `AtkinsonHyperlegibleNext-OFL.txt` |
| Bricolage Grotesque (font) | SIL Open Font License 1.1, Copyright 2022 The Bricolage Grotesque Project Authors | `crates/web/assets/fonts/`, from google/fonts, instanced (wdth 100, opsz 32), Latin subset as WOFF2; license in `BricolageGrotesque-OFL.txt` |
| Simple Icons 16.38.0 (OpenCode, Cursor and Cline marks) | CC0 1.0 Universal | `crates/web/assets/logos/{opencode,cursor,cline}.svg`, from the npm package `simple-icons` (integrity `sha512-B0Wws90e…FhiWSQ==`), reduced to `viewBox` + `path` |
| LobeHub Icons 1.104.0 (Grok and Goose marks) | MIT, Copyright (c) 2023 LobeHub | `crates/web/assets/logos/{grok,goose}.svg`, from the npm package `@lobehub/icons-static-svg`, reduced to `viewBox` + `path`; licence in `crates/web/assets/logos/LobeHub-icons-LICENSE.txt` |
| Rust crates (tokio, axum, maud, serde, kube, k8s-openapi, agent-client-protocol, ...) | Various, see below | Full report with license texts: `THIRD-PARTY-LICENSES.md` (generated, see below) |

Included **in the documentation** (short quoted passages, not shipped in binaries or images):

| Component | License | Notes |
|---|---|---|
| interface-design (Claude Code skill), <https://github.com/Dammyjay93/interface-design> at commit `2f9be32` | MIT, Copyright (c) 2026 Damola Akinleye | A design skill for dashboards, admin panels and settings pages. Its ideas are adapted and a short passage quoted in `docs/agent-skills/ui-craft.md` (marked [I]); the full copyright and license notice is kept at the end of that file |

Redistributed **in the Melissa OS images** (besides the Fedora base image and its packages, which carry their own
licenses under `/usr/share/licenses`):

| Component | License | Source |
|---|---|---|
| k3s (binary and airgap images) | Apache License 2.0 | <https://github.com/k3s-io/k3s> |
| k3s-selinux | Apache License 2.0 | <https://github.com/k3s-io/k3s-selinux> |
| Tailscale 1.104.7 (`tailscale`, `tailscaled`: Melissa's own Tailscale, off by default) | BSD-3-Clause, Copyright (c) 2020 Tailscale Inc & contributors; the Go modules built in are listed with their licenses in `THIRD-PARTY.md` | <https://github.com/tailscale/tailscale>, the official static binaries from pkgs.tailscale.com (sha256 in `packaging/container/tailscale.lock`); `LICENSE` and `THIRD-PARTY.md` (the tag's `licenses/tailscale.md`) ship in `/usr/share/licenses/tailscale/` |
| bubblewrap | LGPL-2.1-or-later | Fedora package |
| gVisor release-20261116.0 (`runsc`, `containerd-shim-runsc-v1` and runsc's `gvisor-bin/` sidecars: apps with isolation `gvisor`) | Apache License 2.0, Copyright The gVisor Authors | <https://github.com/google/gvisor>, Google's official static release binaries from storage.googleapis.com/gvisor (sha512 and the LICENSE's sha256 in `packaging/container/gvisor.lock`); `LICENSE` ships in `/usr/share/licenses/gvisor/`; on Nix nixpkgs' `gvisor` |
| whisper.cpp 1.9.5 (`whisper-cli` and its ggml libraries, voice input) | MIT, Copyright (c) 2023-2026 The ggml authors | <https://github.com/ggml-org/whisper.cpp>, built from the tag's tarball (sha256 in `packaging/container/whisper.lock`); on Nix from nixpkgs |

Downloaded **at runtime, only when the owner turns voice input on** (Settings → Voice), not redistributed:

| Component | License | Source |
|---|---|---|
| Whisper `base` speech model, ggml format (`ggml-base.bin`, 147,951,465 bytes, SHA-256 pinned) | MIT (OpenAI's Whisper weights; the ggml conversion by the whisper.cpp project) | <https://huggingface.co/ggerganov/whisper.cpp>, revision `5359861c` |

**Trademarks.** The provider logos above are trademarks of their owners (xAI, Anomaly/OpenCode, Anysphere/Cursor,
Cline Bot Inc., the Goose project). Melissa shows them only to identify which agent a model belongs to (nominative
use); no affiliation or endorsement is implied. The icon licences cover the drawings, not the marks. Brands whose
guidelines ask for permission or the unaltered official asset (Anthropic/Claude, OpenAI/Codex, Google Gemini,
GitHub Copilot, Moonshot Kimi, Mistral) get Melissa's letter mark instead; see docs/DECISIONS.md (2026-11-20).
Agent names from the ACP registry are shown as the registry lists them; its remote icons are never loaded.

## Where the notices live in the images

- `/usr/share/licenses/melissa/THIRD-PARTY-LICENSES.md`: every Rust crate compiled into `melissa-server` and
  `melissa-device` with its license expression and full license text. Generated at image build time by
  [cargo-about](https://github.com/EmbarkStudios/cargo-about) from `Cargo.lock` (config `about.toml`, template
  `about.hbs`); run `just licenses` to produce the same file locally (`target/THIRD-PARTY-LICENSES.md`).
- `/usr/share/licenses/melissa/ACKNOWLEDGMENTS.md`: this file (covers htmx, which is vendored, not a crate).
- `/usr/share/licenses/melissa/LICENSE`: Melissa's own license (MIT, see docs/PLAN.md section 8).
- `/usr/share/licenses/k3s/LICENSE`: the k3s Apache-2.0 LICENSE from the pinned release tag, verified by sha256
  (`K3S_LICENSE_SHA256` in `packaging/common/usr/lib/melissa/k3s.lock`). k3s has no NOTICE file (checked at v1.37.2+k3s1).
- `/usr/share/licenses/whisper.cpp/LICENSE`: whisper.cpp's MIT license from the pinned tarball.
- `/usr/share/licenses/gvisor/LICENSE`: gVisor's Apache-2.0 LICENSE from the pinned release tag, verified by sha256
  (`GVISOR_LICENSE_SHA256` in `packaging/container/gvisor.lock`). gVisor has no NOTICE file (checked at
  release-20261116.0). Like k3s, its binaries statically link Go modules under their own licenses (same to-do below).
- Fedora packages, k3s-selinux, Tailscale and bubblewrap: their RPMs install their own files under
  `/usr/share/licenses/<package>/`.

## Rust dependency licenses

`about.toml` holds an allowlist: Apache-2.0, MIT, BSD-2-Clause, BSD-3-Clause, ISC, Zlib, 0BSD, Unicode-3.0 and
Apache-2.0 WITH LLVM-exception. `just licenses` (also part of `just check`) fails when a crate's license is not on
the list or cannot be identified; adding a license to the list is a reviewed change.

Report as of this writing (200 crates; cargo-about picks one license from each `OR` choice, preferring the order
of the allowlist, and counts each license of an `AND`):

| License | Crates |
|---|---|
| Apache-2.0 | 142 |
| MIT | 54 |
| BSD-3-Clause | 3 (subtle; matchit and encoding_rs in addition to MIT/Apache) |
| ISC | 3 (ring, rustls-webpki, untrusted) |
| Unicode-3.0 | 1 (unicode-ident, Unicode data tables, with MIT/Apache) |
| Zlib | 1 (foldhash) |

Notable:
- **ring** 0.17.14: `Apache-2.0 AND ISC` (code from BoringSSL under Apache-2.0, new code under ISC). This version
  no longer carries the old OpenSSL/SSLeay license; recheck that if ring is ever downgraded.
- **encoding_rs**: `(Apache-2.0 OR MIT) AND BSD-3-Clause` (WHATWG encoding data).
- **ar_archive_writer** (`Apache-2.0 WITH LLVM-exception`) is a build dependency only (psm, via pest and
  kube-client's jsonpath-rust); it is listed because build scripts' output can end up in the binary.
- Several crates are `Unlicense OR MIT` (memchr, aho-corasick, jiff, walkdir, ...) and ryu is
  `Apache-2.0 OR BSL-1.0`; the report uses MIT and Apache-2.0 for them.
- No copyleft (GPL/LGPL/MPL) crates in the tree today.

## To do

- [x] Ship a generated license report for the Rust dependencies under `/usr/share/licenses/melissa/`.
- [x] Ship the k3s `LICENSE` file in the images (`/usr/share/licenses/k3s/`).
- [ ] The k3s binary statically links many Go modules, and the airgap tarball holds third-party container images
  (CoreDNS, Traefik, metrics-server, local-path-provisioner, klipper, pause, ...). k3s itself ships no notice
  bundle for these; check whether the images carry their own license files and decide whether Melissa should add
  a notice list for them.
- [ ] The same for gVisor's static binaries (runsc and its sidecars link Go modules such as gRPC and
  containerd's; upstream ships no notice bundle with the release tarball).
