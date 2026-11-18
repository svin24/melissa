---
name: melissa-ui
description: Melissa's binding web UI rules. Load before any work on crates/web (components, pages, CSS, htmx) or when another design skill (frontend-design, modern-web-guidance) is in play; these rules override them.
---

# Melissa web UI rules (binding; AGENTS.md wins on conflict)

1. **Ask the owner first.** Before changing UI/UX (components, pages, layout, styling), stop and summarize the
   proposed change for the owner; they have UI ideas. Router plumbing and security headers are not UI.
2. **Fixed component library.** All markup lives in `crates/web/src/components.rs` (maud). Pages in `pages.rs`
   only call components. A new component is a human-reviewed code change. AI agents at runtime never generate
   markup; they pick components and fill data/manifests.
3. **Server-rendered Rust + htmx.** axum + maud, htmx vendored in `crates/web/assets/` and embedded with rust-embed.
   No CDN, no web fonts from the internet, no JS frameworks or build step; must work offline on a LAN.
4. **Security.** Strict CSP (`default-src 'self'`, no inline script/style; htmx `allowEval:false`,
   `includeIndicatorStyles:false`). All text through maud escaping; never `PreEscaped` with data. Agent/model output
   is untrusted: render as escaped text (`log_view`), never HTML/markdown, no remote images. Mutating requests must
   pass the same-origin check.
5. **Style.** Calm, boring, reliable over flashy: Melissa is infrastructure people trust with their data. Use
   design skills (frontend-design, modern-web-guidance) for typography, spacing, accessibility and modern CSS,
   but not for "aesthetic risk". Accessible by default: semantic HTML, keyboard use, contrast, light/dark via
   `prefers-color-scheme`, works at phone width.
6. **Verify.** `devenv shell -- just check`; router tests with `tower::ServiceExt::oneshot`. Details and patterns:
   `docs/agent-skills/htmx-component.md`.
