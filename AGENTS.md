# AGENTS.md — rules for coding agents working on Melissa

Read docs/PLAN.md before changing anything. These rules are mandatory.

## Language and layout
- Write code in **Go**. Prefer the standard library; add dependencies only when clearly worth it and note why in the PR/commit.
- Suggested layout (adjust in PLAN.md if changed): `cmd/` binaries, `internal/` packages, `web/` embedded templates and static assets, `image/` Containerfile and bootc image sources, `manifests/` app manifests, `docs/`.
- Add tests for non-trivial logic. Run `go vet ./...` and `go test ./...` before finishing.

## Secrets and personal data
- **No personal info, secrets, tokens, passwords, API keys, private keys, real hostnames, IP addresses or email addresses in this repo.** Use placeholders such as `example.com` and `203.0.113.x`.
- Runtime secrets and config live in XDG directories, e.g. `~/.config/melissa` (config), `~/.local/share/melissa` (data), `~/.local/state/melissa` (state), never in the repo.
- Keep `.gitignore` covering env files, key material and local state.

## Publishing
- **Do not push, publish, create remotes, open public issues or PRs, or release anything without an explicit yes from the user.** Local commits are fine when asked.
- No telemetry or network calls to third parties from the code unless documented in PLAN.md and approved.

## UI rule (important)
- The web UI is a fixed component library plus app manifests. **AI agents must never generate custom UI elements** (no ad hoc HTML/CSS/JS per request). Agents only choose from existing components and manifests. Adding a new component is a human-reviewed code change.
- The UI is server-rendered Go `html/template` + htmx, embedded with `go:embed`. No CDN assets; it must work offline on a LAN.

## Scope and naming
- The project name is Melissa. Do not rename the project or introduce other working names.
- Keep this project fully separate from any employer: no code, assets, data, docs or snippets from outside work may be copied in.
- Immutable OS: do not assume `dnf install` on the host. Use containers, flatpak, or the bootc image build.

## Working style
- Small, reviewable changes. Update docs/PLAN.md when decisions change; record unresolved items in its Open questions section.
- Ask the user rather than guessing when a choice affects licensing, naming, security or publishing.
