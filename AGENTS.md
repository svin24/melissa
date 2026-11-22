# AGENTS.md — rules for coding agents working on Melissa

Read docs/PLAN.md and docs/DEV-ENV.md before changing anything. These rules are mandatory.

## Language and layout
- **Rust** is the primary language, as a cargo workspace under `crates/` (`queen`, `bee`, `web`, more as needed). Prefer safe Rust: keep `#![forbid(unsafe_code)]` in crates unless a human approves an exception.
- Go (or other languages) only where truly needed, e.g. glue for the k3s ecosystem. Ask first; the default answer is "write it in Rust".
- Error handling: `thiserror` for library errors, `anyhow` at binary edges. No `unwrap()`/`expect()` in non-test code without a comment explaining why it cannot fail.
- Prefer the standard library and well-maintained crates; add a dependency only when clearly worth it and say why in the commit message.
- Before finishing a task run (inside the dev shell): `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo nextest run` (or `just check`). Add tests for non-trivial logic.
- Dev environment: Nix + devenv + direnv (`direnv allow` or `devenv shell`). Do not install toolchains globally.

## Secrets and personal data
- **No personal info, secrets, tokens, passwords, API keys, private keys, real hostnames, IP addresses or email addresses in this repo.** Use placeholders such as `example.com` and `203.0.113.x`.
- Runtime secrets and config live in XDG directories, e.g. `~/.config/melissa` (config), `~/.local/share/melissa` (data), `~/.local/state/melissa` (state), never in the repo.
- Keep `.gitignore` covering env files, key material and local state.

## Publishing
- **Do not push, publish, create remotes, open public issues or PRs, or release anything without an explicit yes from the user.** Local commits are fine when asked.
- No telemetry or network calls to third parties from the code unless documented in PLAN.md and approved.

## UI rule (important)
- The web UI is a fixed component library plus app manifests. **AI agents must never generate custom UI elements** (no ad hoc HTML/CSS/JS per request). Agents only choose from existing components and manifests. Adding a new component is a human-reviewed code change.
- The UI is server-rendered Rust (axum + maud or askama templates) with htmx **embedded in the binary** (rust-embed). No CDN assets; it must work offline on a LAN.

## Scope and naming
- The project name is Melissa. Do not rename the project or introduce other working names.
- Keep this project fully separate from any employer: no code, assets, data, docs or snippets from outside work may be copied in.
- Immutable dev host: do not assume `dnf install` on the host. Use containers, flatpak or Nix/devenv.

## Reference material (outside this repo)
- Agents may read `../reference/first-draft` (that is, `~/dev/reference/first-draft`) for ideas. It is an earlier, partial draft of similar concepts and lacks the Kubernetes and Tailscale parts.
- It is reference only: **never copy code verbatim, never commit anything from it, and never carry over its project name or any other names from it into Melissa.** Re-implement ideas in your own words and structure.
- Do not move, symlink or vendor it into this repo, and do not add it to git (it lives outside the repo on purpose).

## Working style
- Small, reviewable changes. Update docs/PLAN.md when decisions change; record unresolved items in its Open questions section.
- Ask the user rather than guessing when a choice affects licensing, naming, security or publishing.
