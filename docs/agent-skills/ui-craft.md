# UI craft: tokens, layout, copy, motion, states and the visual check

**When:** you change or review anything a person sees in `crates/web` (components, pages, CSS, UI strings in
`feed.rs`, `pages.rs`, `settings/*`). Load the `melissa-ui` skill first; its rules and AGENTS.md win over this file.
Component and htmx mechanics are in [htmx-component.md](htmx-component.md), Settings in
[settings-ui.md](settings-ui.md).

Written 2026-11-20. Each rule is tagged with where it came from:

- **[M]** Melissa's own: AGENTS.md, the `melissa-ui` skill, owner decisions, or what `melissa.css` already does.
- **[T]** adapted from *tasteskill* (`Leonxlnx/taste-skill`, MIT), a skill for marketing websites. Only the parts
  that fit a calm app were kept, rewritten in our words.
- **[I]** adapted from *interface-design* (`Dammyjay93/interface-design`, MIT), a skill for dashboards, admin
  panels and settings pages. Short quotes are marked as quotes; its copyright and license notice is at the end.

Both are credited in ACKNOWLEDGMENTS.md. Section 13 lists what was left out on purpose. Do not bring those parts back.

## 1. One system: tokens and the values in use

Everything on screen comes from one small set of values. If a value you need isn't in the lists below, use the
nearest one that is. Don't add a one-off. A new token is a reviewed change to `melissa.css`. [M][T][I]

**Tokens on `:root`** (`crates/web/assets/melissa.css`; light first, dark redefined under
`prefers-color-scheme: dark`) [M]:

| Token | Use |
|---|---|
| `--ground` | page background, hover fill of rows and nav links, the current nav item |
| `--surface` | cards, rail, panels, secondary buttons |
| `--ink` | text, strong glyphs |
| `--muted` | labels, dates, meta, help lines, inactive states (never body text people must read) |
| `--border` | 1px hairlines, card and field borders |
| `--honey`, `--honey-text`, `--honey-soft` | Melissa's mark and attention: the banner, approval cards, the "read text from outside" guard note, add-app tile. Not for buttons or links |
| `--action`, `--action-ink` | the one accent: primary buttons, links, Send, selected states |
| `--focus` | the focus ring (same hue as `--action`) |
| `--caution`, `--caution-soft`, `--caution-ink` | destructive buttons, failures, "can't be undone", warnings |
| `--ok` | success marks (a finished tool call, a healthy status) |
| `--idle` | stopped/idle status dots |
| `--bubble`, `--bubble-ink` | the user's chat bubble |
| `--icon-green` … `--icon-slate` | helpers' icon colours only (green, teal, blue, violet, rose, orange, lime, slate; 2026-11-20) |
| `--radius` | 12px: cards and panels |
| `--font` | Atkinson Hyperlegible Next: all text |
| `--font-head` | Bricolage Grotesque: h1-h3, the brand word |
| `--mono` | code, logs, commands |

Local custom properties (`--w`, `--h`, `--gap`, `--n`) are per-component geometry (hexagons, meters). They
are not tokens.

**Every colour is a token.** [I] No hex or `rgb()` outside `:root` and its dark block. Known drift to fix when
touched: `.count` text is a raw `#1b2533`, and the overlay shadows and backdrops (`rgb(0 0 0 / 0.18)`,
`rgb(10 14 20 / 0.45|0.55)`) have no tokens yet.

**There are no spacing or type-size tokens yet.** These are the values already in use. Reuse them [M]:

- **Spacing** (padding, margin, gap), the common steps: `0.25rem`, `0.5rem` (by far the most common),
  `0.75rem`, `1rem`, `1.5rem`, plus `0.4rem`/`0.6rem` for tight control padding and `2rem`/`3rem` for page-level
  space. Many near-duplicates exist (`0.35`, `0.45`, `0.55`, `0.7`, `0.85`, `0.9`, `1.1`, `1.25rem`). Don't add
  more. When you touch a rule, move it to the nearest common step if nothing visibly shifts. [I: one base
  unit, multiples only]
- **Type sizes:** body `1rem`, line height 1.5; h1 `1.75rem`, h2 `1.15rem`, h3 `1rem` (all `--font-head`,
  weight 650); small text `0.85rem` (most common), `0.8rem`/`0.875rem`/`0.9rem` for meta and code. There are
  near-duplicates here too (`0.78`, `0.82`, `0.92`, `0.925rem`). Use `0.85rem` for new small text.
- **Weights:** 400 body, 600 buttons and strong labels, 650 headings and the current nav item, 700 the brand.
- **Radius:** `--radius` (12px) for cards, `10px` buttons/nav items/inputs, `8px` small controls,
  `999px` pills and badges, `50%` round buttons and dots. Small radius on small things. [I]
- **Controls:** every button, link row and toggle is at least 44px tall or wide (`min-height: 44px` on
  `button`, nav links, the brand). Focus is `outline: 3px solid var(--focus); outline-offset: 2px`.
- **Breakpoints:** `1200px` (details panel: a column at ≥ 1200, a sheet below), `760px` (rail becomes a top
  bar, one column; settings two-pane from 761), `560px` (model picker stacks), `520px` (one-field forms stack),
  `40rem` (sheets go full screen).

Consolidating spacing and small type into a few tokens (`--space-1..6`, `--text-sm`) is a **proposal** for the
owner (PLAN.md open question 75), not a rule. Don't start it unasked.

**Rules that follow from "one system":**
- One size per kind of element across all pages: every card title, every badge, every help line, every
  table header looks the same everywhere. Before styling a new element, find its twin and copy its rule. [T]
- **Hierarchy comes from weight and colour first, size second.** [I] Melissa has two text colours (`--ink`,
  `--muted`) and three weights. Use them before adding a new size. If you blur your eyes and can't tell heading
  from body from label, the hierarchy is too weak ("squint test", section 11).
- Text people read is 1rem. `--muted` text is only for labels, meta and short help. Nothing below about
  `0.8rem` (13px); a few picker hints use `0.7-0.75rem` today; don't add more. [T]
- Sentence case for every label, button, heading and menu item. No uppercase-with-tracking labels in new work
  (`.divider` and `.picker-group-head` have them today; leave them unless the owner asks; the permission card's
  tag line "NETWORK ACCESS · TCP 25565" is the owner's design, 2026-11-20). [T]
- Colour means state (`--ok`, `--caution`, `--honey` attention, `--action` actionable). Highlight by weight or
  contrast, never by coloured text, marker backgrounds or decorative gradients. [T][I]
- The interface stays almost all neutral (ground, surface, ink, border). `--action` is the only accent; honey
  is the brand and "look here", used sparingly. One hue family per surface; light and dark shift lightness,
  not hue. [T][I]
- **Depth: one strategy.** [I] Melissa's is hairline borders (`--border`) plus the ground/surface shift. A
  shadow only on things that float over the page (popovers, the details sheet). No shadows on cards or
  buttons, no mixing.
- **Nested corners:** a rounded box inside a rounded box uses a smaller radius: outer = inner + the padding
  between them. The same radius on parent and child looks off. [I]
- **Padding is symmetrical** unless the content needs otherwise (an icon side may be ~2px tighter for optical
  balance). [I]
- **Numbers that change** (counts, sizes, timers, meter values, table columns) use
  `font-variant-numeric: tabular-nums` so they don't jitter. Today only the voice timer does. [I]
- A number never wraps away from its unit ("1.7 GB", "5 days"): use a non-breaking space in the string. [T]
- Lines of reading text stay under ~70 characters (settings help and cards already cap their width). [T]
- New headings may use `text-wrap: balance` and help paragraphs `text-wrap: pretty`; both degrade
  safely. [I]

## 2. Edges and layout

- **Equal edges.** Within a page, the header, cards, tables and help lines start on the same left edge
  and end on the same right edge. Nothing pokes out on one side. Inside a card, content shares the
  card's padding. [T]
- **Nothing scrolls sideways** at any width, down to 320px. Wide content (tables, code, logs) scrolls inside
  its own box (`div.md-table`, code blocks), never the page. [T]
- **Nothing is cut off:** no clipped headings, no buttons pushed off-screen, no text hidden under the sticky
  rail/top bar or the composer. Long names wrap or truncate with the `truncated` component, which keeps the full
  text available. [T]
- **One height per row:** cards in a grid row match heights. Lists of the same kind use the same row
  height. [T]
- **No box in a box in a box.** One card level per region. Inside a card, group with spacing and
  hairlines. [T][I]
- **Space groups things; lines are the last resort.** [I] Put related things close and real space between
  groups before reaching for another border or divider. Borders should be findable, not the first thing you see.
- **No dead space** next to short content. A narrow form doesn't sit alone in a wide empty card. [T]
- **No structural hacks:** negative margins that undo a parent's padding, escape-hatch `calc()`, absolute
  positioning to dodge the layout. Fix the layout instead. [I] Known: `.file-more` has `margin-right: -48px`.

## 3. Actions and focus of a view

- **One focal point per view.** [I] Each screen has one thing the person came to do (the chat: the composer
  and the newest reply; an approval: Allow/Deny; Updates: the one state-appropriate action). It leads through
  position, weight or contrast; everything else steps back.
- **One primary action per view and per card:** one filled `button.primary` (or `button.primary.danger` when the
  main action destroys something). Everything else is `secondary`, `ghost` or `link`. Two filled buttons
  side by side means one of them is wrong. [T][M]
- **One state, one action** (owner, 2026-11-19): show the action that fits the current state ("Sign in",
  "Install", "Update and restart"), not every possible action at once. [M]
- **Button words are a verb plus the thing** where the thing isn't obvious from the card: "Delete helper",
  "Check again", "Open the sign-in page". A bare "Save" is fine only when the card's title names what is
  saved and there is one Save in view. Never "Learn more", "Click here", "Submit", "OK" or a bare
  "Continue". [T]
- **One wording per action everywhere.** If the agent menu says "Fresh start", every other place says "Fresh
  start", not "Clear chat". Every permission card says "Decline". [T]
- **Confirm in the page, never with a browser dialog** (`action_button`'s in-page confirm). Skip the
  confirmation when the action is easy to undo. A confirmation says what goes with it and whether it can be
  undone, in one or two short sentences. [M]
- **Destructive styling only for destructive actions** (`--caution`); never red for emphasis. [M]

## 4. Copy

Melissa's voice is **facts only, technical and dry**: plain sentences in the user's words, Melissa or the
helper as the subject, no adjectives about ourselves. [T] Less text is better (owner, 2026-11-19): a status is
one short phrase, help is one short sentence at most, internals go behind a disclosure, no host commands in
the container UI. [M]

**Words** [M]. The UI says **"helper"** for an agent the user made, **"Melissa"** for the lead agent, **"app"** for a
workspace, **"provider"** for an agent program (Claude Code, Grok, ...). Internal terms (lead agent, workspace,
backend, sandbox, ACP, SSE, toml, systemd, polkit, k3s, namespace, stdout/stderr) don't appear in normal UI.
They may appear in Settings → Developer, logs and disclosures labelled as details. All mappings from internal
names to UI words live in `feed.rs` (htmx-component.md).

**Microcopy recipes** [T, adapted to an app]:
- **Label:** says what goes in ("Display name", "Keep files for"), never a question; a placeholder is never
  the only label.
- **Status:** what is true now, short: "Signed in", "Installing…", "Checked just now", "Stopped".
- **Error:** what didn't happen and what to do: "Couldn't save the schedule: give a number of minutes, 1 to
  60." Never a raw error string (`{e}` from Rust, an HTTP status) as the whole message; put the raw text
  behind a disclosure or in the log. Never "Something went wrong", "Oops", "An error occurred".
- **Success:** say what happened, using the button's words ("Helper deleted", "Update downloaded"). Show it
  only after the server confirmed it.
- **Empty state:** what will appear here and how it gets here ("No files yet. Files you send or the helper
  saves show here."). Never sample or made-up data.
- **Waiting:** name the thing: "Waiting for Claude Code…", not "Loading…". Use the `…` character, not
  three dots.
- **Can't-do states:** say what is missing and where to fix it ("Signing in from here isn't set up on this
  device. Sign in on the host: Settings → Developer."), not only that it isn't available.

**Tells to avoid** [T] (things that make UI text read as machine-written or careless). Severity: *ban* =
never; *cap* = fine once on a page, a tell when repeated.

- *ban* **Hype and filler words:** seamless(ly), effortless, powerful, robust, smart, intelligent, magic,
  revolutionary, cutting-edge, next-generation, world-class, unlock, unleash, empower, elevate, supercharge,
  leverage, harness, delve, realm, journey, ecosystem, landscape, "the world of", "at your fingertips",
  "in just a few clicks", "X made simple". Each hides a fact; write the fact.
- *ban* **Virtue words about Melissa:** safe(ly), secure(ly), honest, calm, quiet, carefully, genuinely, truly,
  simply, just, easily. Melissa never calls itself safe. Say what it does instead ("Changes need your OK until
  a fresh start"). Literal uses are fine ("a quiet note" as a component name, "just now").
- *ban* **Chat residue in UI strings:** "Certainly", "Sure!", "I hope this helps", "Great question", leftover
  Markdown (`**`, `##`) in non-Markdown text, `TODO`, `[placeholder]`, `{{token}}`, "lorem ipsum".
- *ban* **Stock app phrases:** "Oops", "Uh-oh", "Something went wrong", "Successfully …" (drop the word),
  "Please …" and "Sorry" in routine text, "Are you sure?", "Hang tight", "You're all set".
- *ban* **Invented specifics:** numbers, names, times or examples that aren't real data. No "e.g. 98% faster",
  no sample names in empty states. Example placeholders in fields use generic stand-ins (`example.com`).
- *ban* **Exclamation marks** and **emoji** in UI text.
- *ban* **Em dashes** in UI strings. Use a colon, a full stop or brackets.
- *cap* **Contrast frames:** "X, not Y", "rather than", "instead of", "without the", "isn't just". State the
  positive fact. Literal uses are fine ("Use an API key instead" names a real alternative).
- *cap* **Negation runs:** "No setup. No fuss." Use one only when each item is a real, checkable limit.
- *cap* **Two-fragment headings:** "Your files. Your rules." A heading names the thing: "Files", "Keep files
  for".
- **Process talk:** "This page shows…", "Below you'll find…", "Here you can…". The UI never describes
  itself. Name the thing.
- **Perfect evenness:** every card with the same three-line description. Give text where there is something
  to say; a card with nothing to explain gets no description. [T][I: "monotone layout"]
- **Title Case Labels** ("Keep Files For"): sentence case. Proper nouns keep their case.
- **Questions as labels** ("What should the agent do?"): a label names what goes in ("Prompt"). Confirmation
  titles may be questions ("Update and restart Melissa?"). That's the one place.

**Over-correction to watch for** [T]. When you remove a tell, check what replaced it: em dashes turned into a
run of full-stop fragments, "not X but Y" turned into "rather than", hype words turned into virtue words.
The fix is a fact, never a synonym.

**Check every string:** read it aloud as a short status from a careful sysadmin. Would the sentence be true
in any other app? Then it says nothing. Rewrite it with what Melissa actually did or needs. [T]

## 5. Interaction and motion

Melissa moves little: motion shows a state change, nothing else. What exists today is the rule [M]:

- **Hover only for a fine pointer:** wrap hover styles in `@media (hover: hover)`. Every hover state also has a
  focus state, and every hover-only control works by tap and keyboard. [M][T]
- **Hover changes colour, not geometry:** row and nav hover fill with `--ground`, secondary buttons darken
  their border, primary buttons `filter: brightness(1.08)`. No lift, scale, tilt, glow or shadow on hover. [M]
- **Should it animate at all?** [I] Quote: "Actions repeated 100+×/day (keyboard shortcuts, command palette) get
  **no** animation — it makes them feel slow." In Melissa: sending a message, Ctrl+1..9 in the model picker,
  toggles, approving. Occasional surfaces (the details sheet, popovers) may have a short transition.
- **Timings:** `0.15s` for small state changes (toggles, disclosure chevrons, a thought's chevron),
  `0.2s ease` for a panel or sheet sliding, the spinner `0.9s linear` loop. Everything under 300ms. [M][I]
  Ease-out curves; never `ease-in` (it delays the first frame), no bounce or overshoot. Nothing reacts after a
  delay. [I][T]
- **Animate only `transform`, `opacity` and colours,** and name them: never `transition: all`. Never width,
  height, max-height, margin, padding or top/left (no animated accordions; `details` opens at once). [T][I]
- **Nothing appears from nothing:** no `scale(0)`; a popover that animates starts near full size and grows
  from its trigger, not the centre. [I]
- **The focus ring never animates** and is never removed without an equally strong replacement. [T]
- **`prefers-reduced-motion: reduce`:** turn movement off (`transition: none` or opacity only), stop loops (the
  spinner becomes a still dotted honey ring), keep colour changes. Every new transition or animation gets its
  reduced-motion rule in the same commit. [M][T][I]
- **Transitions only after the page is ready** (the details sheet waits for `data-ready`), so nothing slides
  on load. [M]
- **Never:** load animations or entrances, staggered lists, scroll-triggered reveals, parallax, carousels or
  auto-advancing content, rotating notices, marquees, blinking or shimmer, typing effects on agent text beyond
  the real stream, confetti. A change on screen always means real data or a real state. [T]
- **Live regions:** streamed updates swap in place (SSE + oob) without moving the user's focus or scroll
  position; status text that changes uses `role="status"` / `aria-live="polite"`. [M]

## 6. States

Every interactive element has default, hover, active (pressed), focus and disabled looks. [I] Every region that
loads, polls or acts has all of these, written before the happy path is called done [T][I]:

| State | How it looks |
|---|---|
| Empty | `empty_state`: one line saying what will appear and how. An action only if there is one obvious next step ("Add an app" → chat) |
| Loading / working | the thing's name + `…`, the `spin` icon for a running row; never a blank box or a fake skeleton |
| Partial / stale | the last known data with a quiet "Checked 2 min ago" or "Couldn't read device details yet" |
| Error | `notice` with `Tone::Error`: what failed and what to do; typed input is kept; raw detail in a disclosure or the log |
| Success | a short `notice` or the new state itself (a row that appears is its own confirmation); only after the server confirmed |
| Disabled | `--muted` text, `cursor: not-allowed`, and the reason next to it ("Switch Melissa to another provider first") |
| Without script | the plain form post + 303 redirect path works and the region still renders (htmx-component.md) |

htmx doesn't swap 4xx/5xx by default: an htmx request returns 200 with the error fragment. [M]

## 7. Controls, forms and access

- **Use what exists** [I][M]: an existing component first (the fixed library is the rule anyway), then a
  native element (`button`, `a`, `details`, `dialog`, `[popover]`, `input type=checkbox` for toggles,
  `select`, `meter`). Never a `div` or `span` acting as a button or link.
- **A hand-built control owes the whole contract** [I]: keyboard (Tab, Enter/Space, arrow keys in lists,
  Esc to close), focus moved in and returned on close, ARIA roles and state, click-outside for overlays. The
  model picker and details sheet are the bar. One without these is broken however it looks.
- Visible `<label>` above each field. Text inputs use 1rem text, which stops phones from zooming on focus.
  Fields and buttons are at least 44px tall; a smaller visible control gets a bigger hit area, and hit areas
  never overlap. Set `autocomplete`, `inputmode` and `type` where they apply. [T][I]
- Validate on submit, not while typing. A field that fails gets `aria-invalid="true"` and its reason under it
  (`aria-describedby`); focus moves to the first failed field. [T]
- One status line per form (`role="status"`), keeping what was typed on failure. [T]
- Every control has an accessible name; icon-only buttons get `aria-label` (the details toggle hides its
  label below 760px but keeps it for screen readers). [T][M]
- Headings go down one level at a time; the chat message's hidden `h3` and Markdown's `h4-h6` already follow
  this. [T][M]
- No `tabindex` above 0; tab order is reading order. A focused control is never hidden under the sticky top
  bar, the composer or an open sheet. [T]
- Toggles and disclosures expose their state (`aria-expanded`, `aria-pressed`, native `details`/`checkbox`). [T]
- Contrast: body text and controls at least 4.5:1 on their ground, large text and UI glyphs 3:1, in light and
  dark. Check new colour pairs in both schemes. [T][M]

## 8. Icons

- Melissa's icons are hand-drawn inline SVG in `components.rs`: line icons, `fill: none`,
  `stroke: currentColor`, round caps and joins, `aria-hidden="true" focusable="false"`, the label lives on
  the button. [M]
- Keep to one look [T]: 16-unit viewBox, stroke 1.5 for UI glyphs (`.file-icon`, paperclip, upload, more,
  close, panel), stroke 2 for status marks (`svg.icon` ok/fail/stopped) and the Send arrow. Rendered sizes: 16px
  inline/status, 20px in buttons and the composer, 26px the mark in the brand, 44px the large mark.
  (`mic_icon` uses a 20-unit viewBox; match the rest when it is next touched.)
- A new icon copies the shape language of the existing ones: same stroke, same corner radius, centred in its
  box (nudged optically when geometric centring looks off). One icon per meaning across the app (the same
  "more" dots everywhere). [T][I]
- No icon fonts, no remote icon sets or CDNs, no brand logos fetched from the web. Providers are shown by
  `provider_mark` (a letter in Melissa's hexagon) and their name. [M]

## 9. The visual check (before you call UI work done)

Look at every page you changed in a real browser. Passing tests doesn't count as looking. [T][I]

1. Run the dev Melissa (restarts are fine without asking; see dev-shell-and-checks.md).
2. Open each changed page with T3 Code's preview tools (`preview_open`, `preview_resize`,
   `preview_set_appearance`, `preview_snapshot`), or without them headless Firefox as in htmx-component.md
   ("Screenshots without the T3 preview"; fresh profile, `ui.systemUsesDarkTheme` for dark).
3. **Widths** [T]: 1440×900, 1024×768, 768×1024, 390×844, then Melissa's own edges [M]: 1199 and 1200 (details
   column vs sheet), 760 and 761 (rail vs top bar), and a quick pass at **320** for sideways scroll.
4. **Both schemes:** light and dark at 1440 and 390 at least.
5. **States:** empty, loading/working, error, a long name, many rows; the confirmation open; the details sheet
   open on a narrow screen; an approval card pending.
6. **Input:** hover and pressed states with a mouse; a full Tab walk (focus visible, nothing hidden, order
   makes sense, Esc closes sheets and popovers); with JavaScript off for the pages that promise to work without
   it; with reduced motion on.
7. **Squint** [I]: blur your eyes at each screenshot. You should still see what is above what, where groups
   divide and the one thing that leads, and nothing should jump out harshly.
8. Read each screenshot against this file: edges line up, one primary per view, sizes from section 1, no
   sideways scroll, nothing cut off, copy without tells. Fix, re-shoot, and say in your report what you looked
   at and what you couldn't check.

A quick in-page check to run with `preview_evaluate` (prints problems; empty output is a pass):

```js
(() => {
  const out = [], vw = document.documentElement.clientWidth;
  if (document.documentElement.scrollWidth > vw) out.push(`page scrolls sideways (${document.documentElement.scrollWidth} > ${vw})`);
  for (const el of document.querySelectorAll('button, a, input, select, textarea, summary, [role=button]')) {
    const r = el.getBoundingClientRect(), cs = getComputedStyle(el);
    if (!r.width || cs.visibility === 'hidden') continue;
    if (r.right > vw + 1 || r.left < -1) out.push(`off-screen: ${el.outerHTML.slice(0, 80)}`);
    if (Math.max(r.width, r.height) < 44 && !el.closest('p, li > p, .md')) out.push(`small target ${Math.round(r.width)}x${Math.round(r.height)}: ${el.outerHTML.slice(0, 80)}`);
    if (!(el.labels?.length || el.getAttribute('aria-label') || el.textContent.trim() || el.getAttribute('title'))) out.push(`no name: ${el.outerHTML.slice(0, 80)}`);
  }
  for (const el of document.querySelectorAll('body *')) {
    if (el.childElementCount || !el.textContent.trim()) continue;
    const fs = parseFloat(getComputedStyle(el).fontSize);
    if (fs < 12.5) out.push(`text ${fs}px: ${el.textContent.trim().slice(0, 40)}`);
  }
  return out.slice(0, 40).join('\n');
})()
```

It is a helper for looking, not a gate: an inline link inside a sentence may be smaller than 44px, and a hit
may be a known exception (picker `kbd` hints). Say in your report which hits you kept and why.

## 9b. The agent workspace's measurements (2026-11-20)

- Columns: sidebar 264px, details 18.5rem at ≥ 1200; each column's head is 56px with a hairline under it.
- Sidebar rows: icon 18px, name 0.9rem/600, status and sub 0.8rem; Needs attention rows on `--honey-soft` with a
  3px honey left edge; group heads 0.8rem/600 muted (attention: `--honey-text`), count right.
- Agent icons: 16-unit line drawings, stroke 1.5, 18px in lists, 22px in headers; Melissa's is the honey hexagon.
- Messages: agent items indented 28px under the head (icon, name 0.9rem/650, time 0.8rem muted); "You" 0.8rem
  muted over a `--surface` bubble with a hairline; date marks 0.8rem muted, no rule.
- Steps card: `--surface`, hairline, radius 10px, rows 32px, 0.875rem, the state 0.8rem on the right (Done in
  `--ok`, Failed in `--caution`).
- Permission card: `--honey-soft` with a 1px honey border (caution colours when destructive), radius 10px; tag line
  0.8rem/600 uppercase with 0.04em tracking.
- Menus: `[popover]`, min 13rem, radius 12px, items 40px, 0.9rem, a 16px glyph each.

## 10. Recording decisions

When a component is used in two or more places, or has measurements worth keeping, write its values here or
in htmx-component.md (height, padding, radius, type size and weight), so the next change reuses them instead
of guessing. A value recorded here is a decision, not drift. [I, adapted: it keeps a `system.md` per project]

## 11. Reviewing UI work

For a review of your own or a delegate's UI change [I, adapted]:

1. **Bound it:** the diff, or the named page. Things outside it are out of scope unless asked; note them, don't
   fix them.
2. **Look first, then read the code:** screenshots per section 9, the squint test, then the CSS and maud.
3. **Two passes.** The whole view: one focal point? clear tiers? groups by space rather than lines? colour only
   where it means something? Then the lines: values off the scale, raw colours, `transition: all`, missing
   states, hand-built controls, structural hacks, copy tells.
4. **Grade each finding:** *blocker* (reads as broken or generic: no focal point, flat hierarchy, missing
   states, a hand-built control without keyboard support, a rule in this file or `melissa-ui` broken),
   *should-fix* (a real craft gap, still works), *note* (minor, once).
5. **Drop false positives:** taste is not a defect; a choice recorded in this file or by the owner is a
   decision; formatting is the formatter's job. If you can't say what a finding costs the user, cut it.
   A few strong findings beat a long list.

## 12. Proposed automated checks (not built; owner decision)

Ideas adapted from tasteskill's scripts (`copy-lint.mjs`, `consistency.mjs`, `assets/layout/check.mjs`) and
interface-design's review passes. None of their code is vendored. [T][I]

- **Copy lint as a Rust test.** Render each page through the router test app (`oneshot`, as the existing
  tests do), strip tags, and check the visible text against the *ban* list in section 4: hype and virtue words,
  stock app phrases, residue, `!`, em dashes, three dots instead of `…`, internal terms outside Settings →
  Developer, and "Couldn't …: " followed by raw error text. A *cap* item over its count warns. Literal uses get
  an allowlist entry with a reason, never a reworded string written to slip past the check. Runs in
  `just check`, no browser, no new dependency.
- **CSS drift check.** A small test over `melissa.css`: no hex/`rgb()` outside `:root`, no `transition: all`,
  no `ease-in`, every `transition`/`animation` has a reduced-motion override, spacing and font sizes from the
  allowed lists in section 1 (with an allowlist for today's known exceptions).
- **Layout check in a headless browser.** The snippet above, run by a script at the widths in section 9 in
  light and dark, failing on sideways scroll, off-screen controls, unnamed controls and text under 12.5px.
  Needs a browser in the dev shell (Firefox headless is already used for screenshots); no Node requirement.
- **A fresh reader.** For a bigger UI change, give a subagent only the page's visible text and ask: what is this
  page for, what do I do next, what would make me hesitate, which three lines sound like any other app? Fix
  what it finds.

## 13. What does not apply to Melissa (do not import)

From **tasteskill** (it builds company and landing websites):

- **Page shape for marketing:** a hero on the first screen, "one section per screen" heights, h1 naming a
  product category for a buyer, a hard fact on the first screen, words-per-screen budgets, the closing band,
  nav mega menus, phone action bars, footers with legal rows and credits lines, share/OG tags and canonical links.
- **Imagery:** real or stand-in photographs, photo searches and credits, product renders, "every visual
  renders" rules for decorative visuals. Melissa shows real data, not pictures.
- **Spectacle:** shader and WebGL grounds, 3D objects and prints, big scenes, award-site "moments", hero
  entrance animations on load, card loops, counting numbers, scroll-linked effects, and the rule that
  "every page moves". Melissa's motion is for state changes only.
- **Hover geometry:** card lift, photo zoom, button step-up/press-down, drawn underlines. Melissa hovers by
  colour only (section 5); changing that is an owner decision.
- **"Deal, never choose":** dealt palettes, font pairings and layouts from a history file so pages differ.
  Melissa has one fixed look; its palette and fonts are set and vendored.
- **Remote assets:** Google Fonts links, CDN icon and logo sets, full-colour brand logos, the "never violet"
  palette rule and the list of banned font families. Melissa embeds everything and works offline under a strict
  CSP.
- **Website copy rules:** testimonials, proof figures, pricing and FAQ moves, voice registers chosen per
  client, addresses and contact formats, coined brand names.
- **Forms posting to `mailto:` or a third-party endpoint**, and any modal "thank you".
- **Its gate and hooks:** the Stop hook, the `.taste-skill/` build folder, waivers files, rounds, Node 22
  scripts and headless Chrome runs. Melissa's checks run in Rust and its dev shell (`just check`), plus the
  visual check above.

From **interface-design** (it aims for a new, distinctive look per product):

- **Finding a new direction:** domain exploration, "colour world", a signature element, "no two interfaces
  should look the same", the swap and signature tests, and the Suggest + Ask direction block. Melissa's
  direction is set (calm, honey and hexagons, vendored fonts); a new direction is the owner's call (`melissa-ui`
  rule 1).
- **Its per-component checkpoint** (intent, palette, depth, surfaces, typography, spacing stated before each
  component): useful for a new direction, noise for changes inside a fixed system. Section 1 is our answer.
- **Press feedback by `scale(0.97)`, staggered entrances and shadows to lift cards and buttons:** they
  conflict with colour-only hover and the borders-plus-surface depth strategy. Owner decision if ever wanted.
- **Sidebar on the same background as the canvas, inputs darker than their surroundings:** Melissa's rail is
  `--surface` on `--ground` by design; don't restyle unasked.
- **React-ecosystem primitives** (Radix, React Aria, shadcn, Tailwind/CVA advice): Melissa has no JS
  framework or build step. Native elements and our components play that role.
- **Image outlines, font smoothing tweaks, raster mood boards and image-generation passes:** no images in the
  UI; no generated assets.
- **The `.interface-design/system.md` memory file and its slash commands:** this file is Melissa's record.

## Third-party notices

Quoted passages marked **[I]** above come from interface-design:

> interface-design, <https://github.com/Dammyjay93/interface-design> (commit `2f9be32`)
>
> MIT License
>
> Copyright (c) 2026 Damola Akinleye
>
> Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated
> documentation files (the "Software"), to deal in the Software without restriction, including without
> limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the
> Software, and to permit persons to whom the Software is furnished to do so, subject to the following
> conditions:
>
> The above copyright notice and this permission notice shall be included in all copies or substantial
> portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT
> LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
> SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN
> ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR
> OTHER DEALINGS IN THE SOFTWARE.

tasteskill (<https://github.com/Leonxlnx/taste-skill>, commit `f6b7657`, MIT License, Copyright (c) 2026
Leonxlnx): ideas only, restated; no text quoted.
