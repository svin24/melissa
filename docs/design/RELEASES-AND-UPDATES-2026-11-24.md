# Releases and updates built by GitHub (research, 2026-11-24)

> **Update 2026-11-27 (owner decisions; DECISIONS.md, PLAN §2):** publishing is approved
> (`ghcr.io/svin24/melissa`, public package). Two things differ from this research's recommendation: **signing
> is key-based** (a cosign key pair, enforced by the podman `policy.json` entry install.sh writes, legacy
> signature format; keyless provenance via `actions/attest` is published too), and **builds go out from master
> first** as `:edge` (the install default for now, pre-alpha), with `:stable` from `vX.Y.Z` tags; also
> `:X.Y.Z`, `:X.Y`, `:sha-<rev>`, no `:latest`. Section 7 marks what is answered. The on-device verifier
> (phase 2 below) is deferred; helper-driven verified updates, a channel switch and richer Updates page states
> are the next phase. Follow-ups: PLAN question 101.

PLAN open question 50 (owner goal, 2026-11-24): people can just grab updates, built by GitHub. For now only
**stable releases** are built and served; nightly builds are researched here for later. Nothing in this
document is built: no workflow exists in `.github/workflows`, nothing was pushed, tagged or published. The
YAML below is a draft for review. Sources were read on 2026-11-24; numbered references are at the end.
Anything marked **(unverified)** needs a check, usually a first dry run on a fork or a throwaway repo.

## Summary

- **Cost: zero for us, as long as the repo stays public and we use standard runners.** Standard
  GitHub-hosted runners are free and unlimited in public repos [1][3], public GHCR packages are free
  (storage and transfer) [5], release assets have no total-size or bandwidth limit [7]. Larger runners are
  billed even for public repos and need a Team/Enterprise plan anyway [1], so we don't use them. The real
  limits are technical: 6 h per job, 20 concurrent jobs, 14 GB of documented SSD per runner and a 10 GB
  Actions cache per repo [2][3].
- **The build fits.** Public standard Linux runners are 4 vCPU / 16 GB, x64 and arm64 [3]. `ubuntu-26.04`
  ships podman 5.7.0 on a 7.0 kernel [13] (`ubuntu-24.04` has podman 4.9.3, too old for install.sh). Our
  image is about 1.2 GB; a clean release build should take well under the 6 h job limit (estimate: 30-60 min,
  **unverified**).
- **Most release gates can run on GitHub.** Linux runners are full VMs with passwordless sudo and `/dev/kvm`
  [3][4], so rootful podman, `--privileged --systemd=always`, k3s inside the container, gVisor (systrap, no
  KVM needed), the escape suite and even the NixOS VM test are possible there. Unknowns: disk space for
  everything in one job, and Ubuntu's AppArmor limit on unprivileged user namespaces (bwrap).
- **The one real gap is on-device signature checking.** podman's `policy.json` can check sigstore keyless
  signatures only against an *email* identity [10]. GitHub Actions' keyless certificates name the workflow
  (a URI), so podman can't pin "built by svin24/melissa's release workflow". Cosign 3's new bundle format
  can't be verified by podman/skopeo at all yet (containers/container-libs#388, open since 2025-10) [11].
  **Recommendation:** sign keyless and attest on GitHub, and verify on the device with Melissa's own
  verifier, run from the *currently installed* (already trusted) image, before the host helper switches. No
  long-lived signing key anywhere.
- **Updates on the device:** keep `melissa-host-update` and Settings → Updates, but for registry installs
  let the helper (not `podman-auto-update`) do the daily update: resolve `:stable` → digest → verify → pull
  by digest → restart → roll back on failure. `podman auto-update` would pull and switch without any
  signature check. The device only talks to ghcr.io (plus GitHub's blob host behind it, see Privacy).
- **NixOS:** no binary cache in phase 1. Users pin the flake to a release tag and run `nixos-rebuild`; the
  package builds in about 3 min. A cache (Cachix, FlakeHub) needs a third-party service and usually a
  long-lived token; later, if people ask.
- **Nightly later:** about 30 runner-hours a month on free standard runners, a few GB of public GHCR
  storage (free), a skip when master didn't change, 14-day retention. Money cost stays at zero while public;
  the risks are reputational (a broken nightly) and the 60-day schedule auto-disable.

## 1. Costs and limits today (public repo, personal account on GitHub Free)

| Item | Limit / price | Source |
|---|---|---|
| Standard GitHub-hosted runners, public repo | Free and unlimited ("Use of the standard GitHub-hosted runners is free and unlimited on public repositories"); does not draw from the $12 included compute | [1][3] |
| Larger runners | "Larger runners are billed for public repositories"; only on Team / Enterprise Cloud | [1] |
| Runner specs, public | Linux x64 `ubuntu-24.04`, `ubuntu-26.04`: 4 CPU, 16 GB RAM, 14 GB SSD. Linux arm64 `ubuntu-24.04-arm`, `ubuntu-26.04-arm`: same. `ubuntu-slim`: 1 CPU container | [3] |
| arm64 runners | Free in public repos since 2025-08-07, public repos only | [6] |
| KVM on Linux runners | `/dev/kvm` present; GitHub's own changelog shows a udev rule to open it to the runner user | [4] |
| Job time | 6 h per job on GitHub-hosted runners; 35 days per workflow run (incl. waiting for approval) | [2] |
| Concurrency | Free plan: 20 concurrent jobs on standard runners | [2] |
| Actions cache | 10 GB per repo (repo admin can raise it; beyond 10 GB is billed), entries unused for 7 days are evicted; 200 uploads/min | [2][9] |
| Cache isolation | A run restores caches from its own branch/tag or the default branch | [9] |
| Artifact storage | Free plan 500 MB is the private-repo allowance; public-repo usage is free (the billing page only meters private repos) | [1][2] |
| GHCR (public package) | "GitHub Packages usage is free for public packages"; anonymous pulls of public images allowed; 10 GB per layer, 10 min upload timeout | [5][8] |
| GHCR deletion | A public version can be deleted only while it has ≤ 5000 downloads; restore within 30 days | [12] |
| Release assets | ≤ 1000 assets per release, each < 2 GiB; no limit on total size or bandwidth | [7] |
| Schedules | Default branch only; min every 5 min; can be delayed or dropped under load (start of the hour); disabled after 60 days without repo activity in public repos | [14] |
| Private-repo fallback (if the repo ever goes private) | Linux 2-core x64 $0.006/min, arm64 2-core $0.005/min | [15] |

Notes:
- **Disk is the tightest limit.** 14 GB is the documented SSD; in practice hosted Ubuntu images have more
  free space on `/` plus a scratch disk, and many projects delete preinstalled toolchains first
  **(unverified for 26.04; measure with `df -h` in the first dry run)**. Our build stages (Rust `target/`,
  k3s airgap images, gVisor, whisper.cpp) plus a second throwaway image for the escape suite and k3s's
  containerd store in the gVisor test may need 15-25 GB.
- **No cache for release builds** (recommended): a stable release happens rarely, a clean build avoids cache
  poisoning, and the cost is only wall time. Nightly can use a registry layer cache
  (`podman build --cache-to/--cache-from ghcr.io/svin24/melissa-cache`, free while public) or
  `actions/cache` for `~/.cargo/registry`; sccache gains little in a single containerised `cargo build`.
- **Cost of builds is not zero for the owner's time:** GitHub can change these terms; the billing model
  already changed in 2026 (dollar-based "included compute usage" [1]). Re-check before nightly.

## 2. Stable release flow

### 2.1 Shape

```
owner: bump all crates to X.Y.Z, commit "Release X.Y.Z", tag vX.Y.Z (signed tag), push tag
  └─ workflow "release" (on: push: tags: v*), environment "release" (owner approval)
      1. check     tag == every crates/*/Cargo.toml version; tag commit is on master; tree clean
      2. build     podman build (Containerfile, MELISSA_VERSION=X.Y.Z, MELISSA_REVISION=<sha>,
                   SOURCE_DATE_EPOCH=commit time) on ubuntu-26.04 → record the image ID
      3. gates     just licenses (cargo-about, also in the image), container-test (no root),
                   escape-test-image, gvisor-test-image, install.sh + container-smoke  (section 2.3)
      4. push      podman push --digestfile ghcr.io/svin24/melissa:X.Y.Z  (only the tested image ID)
      5. sign      cosign sign (keyless, GitHub OIDC) on the digest
                   actions/attest (SLSA build provenance) + SBOM attestation, pushed to GHCR
      6. promote   tag the same digest :stable  (and :latest, see decisions)
      7. release   GitHub Release vX.Y.Z: notes (CHANGELOG section), image digest, verify commands,
                   THIRD-PARTY-LICENSES.md, SBOM; immutable releases on
  └─ job "verify-published" (after promote): pull by digest anonymously, verify signature + attestation
     as a device would, install.sh IMAGE=ghcr.io/svin24/melissa:stable, container-smoke
```

Key points:
- **Test the bytes we ship.** Gates run on the locally built image; push sends the same image (same image
  ID / config digest). `podman push` compresses layers at push time, so the manifest digest only exists
  after the push; the verify job pulls that digest and checks its image ID matches the tested one.
  The escape suite's existing rule fits: only an image that earned `localhost/melissa:escape-tested`
  from its own revision may be pushed (packaging/container/README.md, "Release gate"; PLAN 95).
- **Version check** (step 1): every `crates/*/Cargo.toml` has the same `version` (DECISIONS 2026-11-24)
  and equals the tag without `v`. A small `scripts/release-check.sh` (also usable locally) keeps the
  workflow short.
- **Tags on GHCR:** `X.Y.Z` (never moved), `X.Y` (optional, moves within a minor), `stable` (moves),
  `sha-<shortrev>` (optional). No `latest` unless the owner wants it (question 50 used `:latest` as the
  stable channel; `stable` is clearer and leaves `latest` free).
- **Release notes:** a `CHANGELOG.md` in the repo (Keep a Changelog style), the release job extracts the
  `X.Y.Z` section. Generated notes from commit titles are noisy here (many small commits per day).
- **Licenses:** the image already contains the cargo-about report and LICENSE/ACKNOWLEDGMENTS (Containerfile,
  build stage); the release also attaches `THIRD-PARTY-LICENSES.md` from `just licenses` (same command). The
  non-Rust parts (k3s, gVisor, Tailscale, whisper.cpp, Fedora packages) are covered by the SBOM; whether
  their notices also belong in a release asset is an owner/legal question (decision 9).
- **Signing and provenance:** two complementary things:
  - `actions/attest` (GitHub artifact attestations, Sigstore public-good instance for public repos,
    transparency log) gives SLSA v1 Build L2 provenance, L3 with a reusable workflow [16][17]. Verifiable with
    `gh attestation verify oci://ghcr.io/svin24/melissa@sha256:… -R svin24/melissa` [17].
  - `cosign sign` keyless (same OIDC identity) gives a plain image signature other tools understand. Use
    the legacy signature format until podman/skopeo read cosign 3 bundles [11] **(unverified which cosign
    flag keeps the old format in the cosign version we'd pin)**.
- **SBOM:** generated in the workflow (e.g. syft on the image, SPDX JSON) and attested (`actions/attest`
  with `sbom-path`) [17]; also attached to the release.

### 2.2 Draft workflow (not installed; for review only)

Pinning: replace every `@vN` with a full commit SHA, or adopt GitHub's workflow lockfiles
(`.github/workflows/actions.lock`, GA 2026-10-28 for Free plans) which GitHub now recommends over SHA pins
[18][19]. Versions below are examples **(unverified that they're current)**.

```yaml
# .github/workflows/release.yml  (DRAFT — not in .github/workflows)
name: release
on:
  push:
    tags: ["v[0-9]+.[0-9]+.[0-9]+"]
permissions: {}                      # nothing by default; each job asks
concurrency: { group: release, cancel-in-progress: false }
env:
  IMAGE: ghcr.io/svin24/melissa
jobs:
  build-and-gate:
    runs-on: ubuntu-26.04            # podman 5.7
    environment: release             # owner approves before the job starts
    timeout-minutes: 240
    permissions: { contents: read }
    outputs:
      image_id: ${{ steps.build.outputs.image_id }}
    steps:
      - uses: actions/checkout@v5    # pin by SHA / lockfile
        with: { persist-credentials: false }
      - name: Version matches tag
        run: scripts/release-check.sh "${GITHUB_REF_NAME#v}"
      - name: Free disk, enable KVM and user namespaces
        run: |
          sudo rm -rf /usr/local/lib/android /usr/share/dotnet /opt/ghc || true
          echo 'KERNEL=="kvm", GROUP="kvm", MODE="0666", OPTIONS+="static_node=kvm"' \
            | sudo tee /etc/udev/rules.d/99-kvm.rules && sudo udevadm trigger --name-match=kvm
          sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0 || true
          df -h /
      - name: Build
        id: build
        run: |
          SOURCE_DATE_EPOCH=$(git log -1 --format=%ct) podman build --layers \
            -f packaging/container/Containerfile \
            --build-arg MELISSA_VERSION="${GITHUB_REF_NAME#v}" \
            --build-arg MELISSA_REVISION="$(git rev-parse --short HEAD)" \
            -t localhost/melissa:dev .
          echo "image_id=$(podman image inspect localhost/melissa:dev --format '{{.Id}}')" >> "$GITHUB_OUTPUT"
      - name: Gates
        run: |
          bash packaging/container/test-install.sh
          packaging/container/escape-test.sh localhost/melissa:dev     # tags :escape-tested on pass
          packaging/container/gvisor-test.sh localhost/melissa:dev
      - name: Hand the tested image to the publish job
        run: podman save -o melissa.oci.tar --format oci-archive localhost/melissa:escape-tested
      - uses: actions/upload-artifact@v4
        with: { name: image, path: melissa.oci.tar, retention-days: 1 }

  publish:
    needs: build-and-gate
    runs-on: ubuntu-26.04
    environment: release
    permissions:
      contents: write        # create the release
      packages: write        # push to GHCR with GITHUB_TOKEN
      id-token: write        # keyless signing / attestations (OIDC)
      attestations: write
    steps:
      - uses: actions/download-artifact@v4
        with: { name: image }
      - name: Push the tested image by version
        run: |
          V="${GITHUB_REF_NAME#v}"
          podman load -i melissa.oci.tar
          test "$(podman image inspect localhost/melissa:escape-tested --format '{{.Id}}')" \
               = "${{ needs.build-and-gate.outputs.image_id }}"
          echo "${{ secrets.GITHUB_TOKEN }}" | podman login ghcr.io -u "${{ github.actor }}" --password-stdin
          podman push --digestfile digest localhost/melissa:escape-tested "$IMAGE:$V"
          echo "DIGEST=$(cat digest)" >> "$GITHUB_ENV"
      - uses: sigstore/cosign-installer@v3
      - run: cosign sign --yes "$IMAGE@$DIGEST"          # keyless; legacy format flag tbd
      - uses: actions/attest@v4                          # SLSA provenance
        with:
          subject-name: ${{ env.IMAGE }}
          subject-digest: ${{ env.DIGEST }}
          push-to-registry: true
      # SBOM: syft → sbom.spdx.json, then actions/attest with sbom-path (omitted here)
      - name: Promote to stable
        run: podman push "$IMAGE@$DIGEST" "$IMAGE:stable"   # or skopeo copy / crane tag (tag only)
      - name: GitHub Release
        run: gh release create "$GITHUB_REF_NAME" --verify-tag --notes-file notes.md \
               THIRD-PARTY-LICENSES.md sbom.spdx.json
        env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}

  verify-published:
    needs: publish
    runs-on: ubuntu-26.04
    permissions: { contents: read }
    steps:
      - run: echo "pull by digest anonymously, verify signature + attestation, install.sh, container-smoke"
```

### 2.3 Which gates can run on GitHub's runners

| Gate | Needs | On `ubuntu-26.04` | Notes |
|---|---|---|---|
| `cargo fmt/clippy/nextest` (`just check`) | Nix dev shell or plain Rust | Yes | Nix install adds ~1-2 min; or run inside the image's build stage |
| `just licenses` | cargo-about, network for `cargo fetch` | Yes | Already runs inside the image build |
| `container-test` (install.sh/melissa-setup unit tests) | bash only | Yes | |
| Image lint (in the Containerfile) | — | Yes | Part of the build |
| `escape-test-image` | rootful podman, `--privileged`, systemd in the container, user namespaces, newuidmap, seccomp | Probably | Passwordless sudo and a full VM [3]. Ubuntu's `kernel.apparmor_restrict_unprivileged_userns=1` may block bwrap inside (PLAN 49a mentions the same AppArmor issue) → set it to 0 on the runner **(unverified)** |
| `gvisor-test-image` | the above + k3s in the container, br_netfilter/overlay, cgroup v2 controllers, busybox pull | Probably | Kind/k3d-style nested k3s commonly works on hosted runners; gVisor systrap needs no KVM. Disk and 16 GB RAM are the risk **(unverified)** |
| install.sh + `container-smoke` | systemd on the host (Quadlet), free ports 8080 and 30000-30079, sysctls | Probably | The runner is a throwaway VM, so writing `/etc/containers/systemd` and sysctls is fine. Docker is preinstalled and sets FORWARD to DROP; install.sh already handles Docker's rules |
| NixOS VM test (`checks.x86_64-linux.melissa-nixos`) | Nix, KVM | Yes, separate job | `/dev/kvm` via the udev rule [4]; long, so separate job, release blocks on it only for the Nix track |
| Real agent backends (Claude/Grok/OpenCode logins), Tailscale, reboot/persistence, voice with a real mic | accounts, secrets, hardware | **No** | Stay manual pre-release checks on the owner's machine; no third-party credentials in CI |
| arm64 image gates | native arm64 runner, KVM on arm64 | Later | `/dev/kvm` on `ubuntu-24.04-arm` **unverified**; the Containerfile is x86-only today (k3s, Tailscale, gVisor downloads are amd64/x86_64) |

### 2.4 arm64 later

Free native runners exist [6], so no QEMU emulation. Work needed: `TARGETARCH` in the Containerfile's k3s,
Tailscale and gVisor stages, per-arch hashes in `k3s.lock`, `tailscale.lock`, `gvisor.lock`, whisper.cpp's
CPU variants, then one build job per arch and `podman manifest` to push a multi-arch index. Signing and
attestation go on each per-arch digest and on the index. Not needed until someone runs Melissa on arm64.

## 3. How installed devices get it

### 3.1 Container installs (the main path)

Today (packaging/container/README.md, "Updates"): Quadlet `AutoUpdate=registry|local`, daily
`podman-auto-update.timer`, Settings → Updates Check/Apply/Roll back through `melissa-host-update`, the
previous image kept as `localhost/melissa-rollback:previous`. That already works with registry images.
What changes:

1. **Default image.** install.sh defaults to `IMAGE=ghcr.io/svin24/melissa:stable` (a local build stays
   possible with `IMAGE_ARCHIVE`). Public package, anonymous pull, no login on the device [8].
2. **Verify before switching.** podman's `policy.json` would be the natural place, but:
   - `sigstoreSigned` with `fulcio` requires `oidcIssuer` **and** `subjectEmail` [10]; GitHub Actions
     certificates carry the workflow as a URI SAN, not an email. A GitLab user reports the same block for
     CI keyless signatures (2026-05-24) [20]; checked today: container-libs `main` still documents only
     `subjectEmail`/`subjectHostname` [10].
   - podman/skopeo can't read cosign 3's default bundle format (container-libs#388, open) [11].
   - A key-based `sigstoreSigned` + `keyPath` policy would work in podman today, but needs a long-lived
     private key in GitHub secrets (or a KMS), which section 4 wants to avoid.

   **Recommended:** the host helper verifies through Melissa's own verifier, taken from the image that is
   **running now** (trusted when installed), never from the candidate:
   `podman run --rm --network=host --entrypoint /usr/libexec/melissa/verify-image <current image ID>
   ghcr.io/svin24/melissa@sha256:<candidate>`. The verifier checks the keyless signature against a fixed
   identity (issuer `https://token.actions.githubusercontent.com`, workflow
   `svin24/melissa/.github/workflows/release.yml` on `refs/tags/v*`) and the Rekor inclusion proof, with
   Sigstore's trusted root baked into each release (no TUF fetch at run time). Implementation options:
   the `sigstore` Rust crate (fits AGENTS.md; maturity **unverified**) or a pinned `cosign` binary in the
   image (Apache-2.0, adds tens of MB). Each release can roll the trusted root forward. Trust chain: the
   first install is trust-on-first-use (the owner can check it with `cosign verify` or `gh attestation
   verify` by hand; install.sh can print the commands).
3. **The helper drives registry updates, not `podman-auto-update`.** `podman auto-update` pulls and
   restarts when the tag's digest changes [21] and would skip step 2. For `AUTO_UPDATE=registry` the
   installer would write no `AutoUpdate=` line and instead enable a `melissa-update.timer` (daily, random
   delay) that runs `melissa-host-update auto`: resolve `stable` → digest, stop if unchanged, verify,
   `podman pull …@sha256`, tag locally as the Quadlet's image, restart, health-wait, roll back on failure.
   This reuses `apply` almost as is. `AUTO_UPDATE=local` stays on podman's timer (nothing to verify).
4. **Health check and rollback.** Apply already waits for the server and reverts. `podman auto-update`'s
   own rollback only notices a failed unit start unless the container reports READY via sd_notify [21];
   the Quadlet has no `Notify=` today, which is another reason to let the helper own updates. Open: health
   beyond "server active" (k3s Ready), PLAN 20.
5. **Data migrations vs rollback.** A rollback after a release that migrated on-disk state (server.toml,
   SQLite, k3s versions) can break. Rule proposal: a release that migrates says so in its notes and keeps a
   pre-migration copy; the helper refuses a rollback across a "no-rollback" marker (an image label).
6. **What Settings → Updates shows:** channel (Stable), running version + revision + short digest,
   "verified: built by svin24/melissa release workflow" (or the failure), available version (read from the
   image config's `org.opencontainers.image.version` label after resolving the tag; helper does it, no new
   UI component needed, fits the existing rows), last check time, the release notes link
   (`https://github.com/svin24/melissa/releases/tag/vX.Y.Z`, opens in a new tab; no fetch from the device).
7. **Channel setting (later).** One more fixed request name per channel (`channel-stable`,
   `channel-nightly`) fits the fixed-name pattern; the helper rewrites the Quadlet's image tag. Only the
   owner may switch; switching from nightly back to stable means waiting for the next stable (no downgrade)
   or an explicit rollback.

### 3.2 NixOS (experimental)

- **What users run:** in their system flake
  `inputs.melissa.url = "github:svin24/melissa/v0.0.3";` (a release tag), then
  `nix flake update melissa && sudo nixos-rebuild switch`; rollback with `nixos-rebuild switch --rollback`
  or the boot menu (unchanged from PLAN §2b). Following a moving "stable" ref needs either a `stable`
  branch the release job fast-forwards to each tag, or FlakeHub's semver URLs
  (`https://flakehub.com/f/svin24/melissa/0.0.*`), published from a tag workflow with OIDC, no secret [22].
- **Binary cache:** not in phase 1. The package builds from `Cargo.lock` in about 3 min cold
  (docs/agent-skills/nix-service.md); dependencies come from cache.nixos.org / devenv.cachix.org. Options
  later: Cachix (5 GB free for open source, needs an auth token secret) [23]; FlakeHub Cache (paid,
  **pricing unverified**); a self-hosted cache. All are third parties the device would contact, so they need
  the owner's approval.
- **Release gate for Nix:** the NixOS VM test on a KVM runner (section 2.3).
- Settings → Updates on Nix keeps showing commands only (PLAN 62); it could show "vX.Y.Z is out" only by
  contacting GitHub, which is a new third-party call (decision 8).

## 4. Trust, reproducibility, privacy

- **Pin actions:** workflow lockfiles (GA 2026-10-28, Free plans included; Dependabot updates them) or full
  commit SHAs [18][19]. Few actions: checkout, upload/download-artifact, attest, cosign-installer.
- **Minimal permissions:** `permissions: {}` at the top, per-job grants as in the draft; `packages: write`,
  `id-token: write` and `contents: write` only in the publish job; `persist-credentials: false`.
- **No long-lived secrets:** `GITHUB_TOKEN` for GHCR and the release [8], GitHub OIDC for keyless signing and
  attestations [16]. No PAT, no signing key, no registry password.
- **Who can release:** only the owner.
  - Tag ruleset on `v*`: restrict creation, update and deletion, owner as the only bypass [24] (rulesets are
    available for public repos on GitHub Free [24]).
  - Environment `release` with the owner as required reviewer, limited to `v*` tags **(availability of
    required reviewers for public repos on Free: believed yes, unverified)**.
  - Immutable releases on: tag and assets can't change after publishing, and GitHub adds a release
    attestation [25].
  - CODEOWNERS on `.github/workflows/` [19].
  - No `pull_request_target` (GitHub blocks it by default in public repos from 2026-12-02 anyway) [26]; fork
    PRs never get publish permissions.
- **Reproducibility:** `SOURCE_DATE_EPOCH` from the commit (already in `just container-build`), `--locked`
  cargo builds, pinned and hash-checked k3s/gVisor/Tailscale/whisper downloads (already). Not reproducible
  yet: the Fedora base and dnf packages float (`fedora:45` tag). Pin the base by digest per release and
  record `packages.txt` versions in the SBOM. Bit-for-bit reproducible images are "later later"
  (BuildStream, PLAN §2).
- **Supply chain notes:** Rekor entries are public (they show the repo, workflow and digest; fine for a
  public project). Third-party downloads during the build (pkgs.tailscale.com, storage.googleapis.com/gvisor,
  github.com) are hash-pinned. The release workflow runs third-party code (actions); keep it minimal.
- **Privacy (PLAN requires third-party calls to be documented and approved):** a device on the stable
  channel contacts, once a day plus when the owner presses Check/Apply:
  - `ghcr.io` (token endpoint for anonymous pull, manifest for `:stable`, signature/attestation objects);
  - GitHub's package blob host for layer downloads (`pkg-containers.githubusercontent.com`,
    **unverified hostname; confirm with a pull through a logging proxy**);
  - nothing else: no Rekor/TUF calls if the verifier is offline-capable with a baked-in trusted root, no
    api.github.com, no telemetry. GitHub sees the device's IP, the user agent (podman/containers-image
    version) and counts downloads per version. `AUTO_UPDATE=off` stops the daily call; a local image never
    calls out. This list goes into PLAN §2 next to the other documented third-party calls once approved.

## 5. Nightly builds (research only)

- **Trigger:** `schedule: cron "17 3 * * *"` (off the hour, to avoid the start-of-hour delays [14]) plus
  `workflow_dispatch`. Runs on master's head only [14].
- **Skip if nothing changed:** first step reads the `org.opencontainers.image.revision` label of
  `:nightly` (anonymous `skopeo inspect` / registry API) and exits when it equals `git rev-parse --short HEAD`.
  Optional path filter: skip when only `docs/` changed.
- **Tags:** `nightly` (moves), `nightly-YYYYMMDD-<shortrev>` (fixed), version label `X.Y.Z-nightly.YYYYMMDD`.
- **Gates:** same as stable minus the release; a failed gate means no `nightly` move (yesterday's stays).
- **Retention:** keep 14 dated nightlies; a cleanup job deletes older versions with
  `actions/delete-package-versions` (needs `packages: write`). Public versions with more than 5000 downloads
  can't be deleted [12]; unlikely for dated nightlies. Stable versions are never deleted.
- **Cache:** a registry layer cache or `actions/cache` (10 GB, 7-day eviction [9]) brings typical nightly
  runs down; not used for stable.
- **Cost estimate per month (public repo):** about 30 runs × 45-75 min ≈ 25-40 runner-hours, $0 on
  standard runners [1][3]; GHCR storage for ~15 images, mostly shared base layers, a few GB, $0 [5]. If
  the repo were private: 2-core runners at $0.006/min → about $10-25/month, beyond the $12 included
  compute [1][15] (slower on 2 cores). Concurrency (20 jobs) is not an issue.
- **Opt-in channel:** never the default; Settings → Updates channel switch (owner only), with a plain warning.
- **Risks:** broken nightly on testers' boxes (gates reduce, don't remove); migrations that stable later
  changes; the 60-day inactivity auto-disable of schedules in public repos [14] (harmless: no changes → no
  builds needed, but it must be re-enabled); public Rekor entries every night; cleanup deleting the wrong
  versions (match by tag prefix only).

## 6. Recommended phased plan

**Phase 0 — prepare (no publishing; can start now, all local):**
1. `scripts/release-check.sh` (versions equal, matches a tag, clean tree) + `just release-check`.
2. `CHANGELOG.md` with a 0.0.2 section; the release notes come from it.
3. Containerfile: pin `fedora:45` by digest; add `org.opencontainers.image.source=https://github.com/svin24/melissa`
   (links the GHCR package to the repo).
4. Dry run the draft workflow's steps on a **throwaway fork or a private scratch repo the owner creates**
   (owner's yes needed; the private run would bill a little), to measure time, disk and the AppArmor/userns
   question. No pushes to `ghcr.io/svin24`.

**Phase 1 — first stable release, verified by hand on devices:**
5. Add `.github/workflows/release.yml` (tag-triggered, owner-approved environment, pinned actions), tag
   ruleset, immutable releases, CODEOWNERS.
6. Tag `v0.0.3` (or re-release 0.0.2): build, gates, push `:0.0.3` + `:stable`, keyless sign, provenance +
   SBOM attestations, GitHub Release. Make the GHCR package public.
7. install.sh: default `IMAGE=ghcr.io/svin24/melissa:stable`; README/QUICKSTART "install from the
   published image"; print how to verify (`cosign verify …` / `gh attestation verify …`).
8. Device updates keep today's mechanism (`AutoUpdate=registry`) **only if the owner accepts "no on-device
   signature check" for one or two releases**; otherwise set automatic updates off by default in phase 1 and
   ship phase 2 first.

**Phase 2 — verified updates on the device:**
9. `verify-image` in the image (sigstore crate or pinned cosign), fixed identity, baked trusted root.
10. `melissa-host-update auto` + `melissa-update.timer`; registry installs stop using `podman-auto-update`;
    Settings → Updates shows channel, verified state, available version (existing components only).
11. A "no rollback across this release" label for migrating releases.
12. Document the device's network calls in PLAN §2 (third-party calls list).

**Phase 3 — later:** nightly channel (section 5), arm64 images, Nix binary cache or FlakeHub, a custom
registry domain in front of GHCR (PLAN §2, "Update distribution"), maybe a key-based podman `policy.json`
if containers/image gains URI identities or bundle support (then podman enforces it on every pull).

## 7. Owner decisions needed

Answered items are marked with the date; the original question follows in brackets.

1. **Answered 2026-11-27: yes.** GitHub Actions publishes to `ghcr.io/svin24/melissa`; the package is public.
   [Go-ahead for publishing at all: a public GHCR package, GitHub Releases, and a workflow that pushes on tags.]
2. **Answered 2026-11-27:** `:stable` (`vX.Y.Z` tags; the default from the first release) and `:edge` (every
   master commit that passes the gate; the install default for now, pre-alpha), plus `:X.Y.Z`, `:X.Y`,
   `:sha-<rev>`; no `:latest`. [Channel tag name: `:stable` or `:latest`, or both?]
3. **First release number:** re-release 0.0.2 as the first published image, or start at 0.0.3? (Open.)
4. **Partly answered 2026-11-27:** the escape suite inside the image is a hard gate (the runner needs
   `kernel.apparmor_restrict_unprivileged_userns=0`); the gVisor test is advisory at first; a separate check
   workflow runs fmt, clippy and nextest with pinned Rust (not the Nix shell). Container smoke, NixOS VM test and
   manual checks before tagging are still open. [Gates that block a release.]
5. **Answered 2026-11-27:** GitHub environments `edge` (master) and `stable` (tags, owner approval).
   [Approval step: tag + environment approval, or tag only?]
6. **Answered 2026-11-27: key-based, not the recommendation.** A cosign key pair (private key in GitHub
   environment secrets, public key at `packaging/container/melissa-signing.pub`), legacy signature format,
   enforced by a podman `policy.json` entry (`sigstoreSigned`, `keyPath`, `matchRepository`) for this repository
   only; keyless provenance (`actions/attest`) is published too. Revisit keyless when containers/image supports
   URI identities. [Keyless + on-device verifier, or a key so podman enforces it today?]
7. **Answered 2026-11-27 (follows from 6):** podman checks the key signature on install, Settings → Updates and
   `podman-auto-update`, so automatic updates stay on with a signature check. Helper-driven verified updates are
   the next phase. [Phase 1 without on-device verification?]
8. **Partly answered 2026-11-27:** devices pulling from ghcr.io and GitHub's blob host is approved (PLAN §2).
   Whether Nix installs ever check GitHub is still open. [Device network calls.]
9. **Licenses in releases:** attach only the Rust report + SBOM, or also collect notices for k3s, gVisor,
   Tailscale, whisper.cpp and Fedora packages as a release asset (legal check, ties to PLAN §8)?
10. **Dry run:** OK to create a throwaway fork/private repo for measuring the workflow (small cost if
    private), or measure only after the yes on the real repo?
11. **NixOS:** tags only, a `stable` branch, or FlakeHub (third party, OIDC publish)? Any binary cache?
12. **arm64:** wanted soon, or only on request?
13. **Nightly (later):** when to switch on, retention (14 days?), who may opt in (owner only), and should a
    nightly box ever move back to stable automatically?

## Sources (read 2026-11-24)

1. GitHub Docs, GitHub Actions billing — https://docs.github.com/en/billing/concepts/product-billing/github-actions
2. GitHub Docs, Actions limits — https://docs.github.com/en/actions/reference/limits
3. GitHub Docs, GitHub-hosted runners reference — https://docs.github.com/en/actions/reference/runners/github-hosted-runners
4. GitHub changelog 2024-04-02, hardware-accelerated Android virtualization (KVM udev rule) — https://github.blog/changelog/2024-04-02-github-actions-hardware-accelerated-android-virtualization-now-available/
5. GitHub Docs, GitHub Packages billing — https://docs.github.com/en/billing/concepts/product-billing/github-packages
6. GitHub changelog 2025-08-07, arm64 hosted runners for public repositories GA — https://github.blog/changelog/2025-08-07-arm64-hosted-runners-for-public-repositories-are-now-generally-available/
7. GitHub Docs, About releases (storage and bandwidth quotas) — https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases
8. GitHub Docs, Working with the Container registry — https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry
9. GitHub Docs, Dependency caching reference — https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching
10. containers-policy.json(5) — https://raw.githubusercontent.com/containers/image/main/docs/containers-policy.json.5.md and https://raw.githubusercontent.com/containers/container-libs/main/image/docs/containers-policy.json.5.md ; containers-registries.d(5) (`use-sigstore-attachments`) — https://raw.githubusercontent.com/containers/image/main/docs/containers-registries.d.5.md
11. containers/container-libs#388 "Support Sigstore bundle format" (open; created 2025-10-12, updated 2026-10-10) — https://github.com/containers/container-libs/issues/388
12. GitHub Docs, Deleting and restoring a package — https://docs.github.com/en/packages/learn-github-packages/deleting-and-restoring-a-package
13. actions/runner-images, Ubuntu 26.04 and 24.04 readmes (image 20261116; Podman 5.7.0 / 4.9.3) — https://github.com/actions/runner-images/blob/main/images/ubuntu/Ubuntu2604-Readme.md , https://github.com/actions/runner-images/blob/main/images/ubuntu/Ubuntu2404-Readme.md
14. GitHub Docs, Events that trigger workflows (`schedule`) — https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows
15. GitHub Docs, Actions runner pricing — https://docs.github.com/en/billing/reference/actions-runner-pricing
16. GitHub Docs, Artifact attestations (concepts) — https://docs.github.com/en/actions/concepts/security/artifact-attestations
17. GitHub Docs, Using artifact attestations — https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations
18. GitHub changelog 2026-10-28, workflow lockfiles GA — https://github.blog/changelog/2026-10-28-github-actions-workflow-lockfiles-are-generally-available/
19. GitHub Docs, Secure use reference (lockfiles, SHA pinning, CODEOWNERS) — https://docs.github.com/en/actions/reference/security/secure-use
20. GitLab issue (third party, 2026-05-24): keyless signatures blocked by containers/image's `subjectEmail` matcher — https://gitlab.com/dunn.dev/immutable/basef/-/issues/15
21. podman-auto-update(1) — https://docs.podman.io/en/latest/markdown/podman-auto-update.1.html ; podman-systemd.unit(5) — https://docs.podman.io/en/latest/markdown/podman-systemd.unit.5.html
22. DeterminateSystems/flakehub-push README (OIDC publish) — https://github.com/DeterminateSystems/flakehub-push
23. Cachix pricing (5 GB free for open source) — https://www.cachix.org/pricing
24. GitHub Docs, Available rules for rulesets — https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets
25. GitHub Docs, Immutable releases — https://docs.github.com/en/code-security/supply-chain-security/understanding-your-software-supply-chain/immutable-releases
26. GitHub changelog 2026-11-02, `pull_request_target` blocked by default from 2026-12-02 — https://github.blog/changelog/2026-11-02-pull_request_target-blocked-by-default-december-2/
27. Sigstore docs, Signing containers with cosign (OCI 1.1 referrers, bundles) — https://docs.sigstore.dev/cosign/signing/signing_with_containers/
