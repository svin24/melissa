---
name: melissa-ui
description: Melissa's binding web UI rules. Load before any work on crates/web (components, pages, CSS, htmx, UI strings) or when another design skill (frontend-design, modern-web-guidance, design-taste-frontend/tasteskill, interface-design) is in play; these rules override them.
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
   is untrusted: agent prose goes through `markdown` (safe CommonMark + GFM tree, raw HTML as text, only
   http/https/mailto links, images never loaded), user text through `plain_text`, logs through `log_view`.
   Never raw HTML, no remote images. Mutating requests must pass the same-origin check.
5. **Style.** Calm, boring, reliable over flashy: Melissa is infrastructure people trust with their data. Use
   design skills for typography, spacing, accessibility and modern CSS, but not for "aesthetic risk", a new
   direction, or anything website-only (heroes, photos, effects, load animations, remote assets).
   Accessible by default: semantic HTML, keyboard use, contrast, light/dark via
   `prefers-color-scheme`, works at phone width.
   No browser dialogs (`alert`, `confirm`, `hx-confirm`): confirm inside the page (`action_button`'s
   in-page confirm), or not at all when the action is easy to undo (owner, 2026-11-20).
6. **Craft** (details: `docs/agent-skills/ui-craft.md`):
   - Values come from the existing tokens and the sizes listed there: no one-off spacing or font sizes, no hex or
     `rgb()` outside `:root`. One look per kind of element on every page.
   - One focal point and one filled primary button per view and per card; button text is a verb plus the thing.
   - Every region has empty, working, error and disabled states, and works without script.
   - Copy: facts only, short; UI words "Melissa", "helper", "app", "provider", no internal terms. No hype or
     virtue words, `!`, em dashes, "Oops"/"Something went wrong", or raw error text as the message.
   - Motion only for a state change: colour-only hover under `(hover: hover)`, under 300ms, `transform`/`opacity`/
     colours only (never `transition: all`), and a `prefers-reduced-motion` rule for each.
   - An existing component, then a native element (`button`, `a`, `details`, `dialog`, `[popover]`); never a
     clickable `div`.
7. **Verify.** `devenv shell -- just check`; router tests with `tower::ServiceExt::oneshot`. Then look at every changed
   page in a browser at 1440, 1024, 768 and 390 px (and 320 for sideways scroll), light and dark, with a Tab walk
   (`ui-craft.md` section 9). Patterns: `docs/agent-skills/htmx-component.md`.
