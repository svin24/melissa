# Settings audit: double lines, bulk and bugs

Research only, 2026-11-23. No UI code was changed. This document briefs the designer, who will redesign Settings in
Penpot, and then the builder. The owner's complaint: Settings has "slight bugs, double lines" and is "way too fat".

The audit builds on [UI-AUDIT-2026-11-22.md](UI-AUDIT-2026-11-22.md) and does not repeat it. Most of that audit's
proposal is now in `melissa.css`: the `--space-*`, `--size-control-*` (28/32/40), `--size-row-*` (32/40/56),
`--radius-*` and `--font-size-*` tokens, and the `pointer: coarse` touch-target switch. All targets below use those
tokens and the sizes in COMPONENT-SPEC §2.8 (rows) and §2.9 (cards). Nothing new is invented.

**Method.** The pages were rendered with the repo's screenshot harness (`scripts/ui-shots`) from this branch's source
(`3ba94b6`), with the fixed demo content and a throwaway server on localhost. A wrapper script added the widths 768
and 320 and measured the DOM in the browser:

- the height of each section,
- every visible border, with pairs closer than 16px flagged as possible double lines,
- the height of every control and row,
- anything wider than the viewport.

Line numbers are `melissa.css:<line>` unless a file is named. Heights are CSS px, measured in light mode (dark mode
has the same geometry).

**Screenshots:** `~/.local/share/melissa-design/settings-audit-2026-11-23/`. The contact sheet is `index.html`; the
raw numbers are in `measure.json`. Files are named `<screen>--<width>-<dark|light>.png`, and `<screen>~2`, `~3`, …
are the following scrolled pages of a long section. Widths are 1440, 768, 390 and 320. There are 336 shots of 25
screens, including the phone's level-1 list (`settings--390-*`), the in-page delete confirmation
(`dialog-delete-helper`), About with every disclosure open (`settings-about-open`), the "Add a schedule" form with
Advanced open (`settings-schedules-new`), and Settings opened as the modal from the chat
(`overlay-settings-updates`).

These were not shot: the Reset confirmation card (this install has no factory reset, so the page shows "Not
available"), a schedule's Edit form (no schedules in the demo), and a provider mid sign-in.

---

## 0. Top 10, ranked by visual impact

| # | Problem | Where | Fix in one line |
|---|---|---|---|
| 1 | **Double lines from disclosures inside setting cards.** `.disclosure` always draws its own top hairline (:5343). Inside a setting card, the card already draws one between rows (:4829). The result is two lines 5px apart (Updates "Details"), an inset line under every About row followed by the full-width line of the next row, and "Details" boxed between two lines in the provider detail. | Updates, About, Providers | Drop the disclosure's border and top padding when it sits in a card, an extra part or a detail pane. One divider per boundary. |
| 2 | **Provider detail cut off at tablet width.** At 768 the split card keeps two columns: the list stays 240–300px and the detail gets about 130px. The name, the version and "Remove provider" are clipped, and the card scrolls sideways by 33px. | Providers @768 (`settings-providers--768-*`) | Stack the split card whenever its own width is below about 640px, not only at 760px viewport width. |
| 3 | **The title is shown twice and the page starts 167px down.** The 56px head bar holds the breadcrumb "Settings / Updates". The body then adds 24px padding, a 1.75rem h1 "Updates", a lead line and a 24px gap. The phone already puts the title in the head bar and hides the body h1. | Every section, desktop | Desktop works like the phone: the title goes in the head bar (`.settings-here`, a bigger size), the body h1 goes, the lead goes or becomes optional. The first card then starts at about 80px. |
| 4 | **Cards that hold one field or one button.** Device name (166px), a helper's Name (141px), Delete (93px, one button), Icon (264px, its own Save), Change password (420px, three stacked fields). Each is a padded card with a full-width input and its own Save button. | Updates, helper page, Account | Make them rows in the existing card: title on the left, an inline field or button on the right (`inline_form` exists). Change password goes behind a row button. |
| 5 | **Rows taller than the spec.** A setting row is 48px with one line and 71–72px with a description. COMPONENT-SPEC §2.8 says 40 and 56. Long descriptions wrap to 2–3 lines (Developer: 92px rows; Fresh start: 72px). | All setting cards | Row padding `--space-2` vertically, row height `--size-row-md`/`--size-row-lg`, descriptions of one line (cut the copy). |
| 6 | **The disclosure summary stays 44px on desktop.** It is hard-coded `min-height: 44px` (:5351). Every other control is now 32px. Each "Details" costs about 57px (44 + 12 padding + 1 border). | Updates, Providers, About (3×), Schedules Advanced | `min-height: var(--size-control-md)`; under a coarse pointer the hit area grows to `--size-target-min`, as for buttons. |
| 7 | **Big forms that are always open.** "Add a schedule" (719px, 914px with Advanced) shows even with no schedules. Change password: 3 fields and 3 "Show password" boxes (420px). Developer "New agent task" (332px). Fields stretch to about 780px. | Schedules, Account, Developer | Keep the form behind one button or disclosure ("Add schedule", "Change password"). Cap form width at `24rem`, the value already used by `.danger-confirm form`, :5311. |
| 8 | **Two card systems and two page headers on one screen.** `card()` has its h2 inside, at `--font-size-lg`, with 24px bottom margin. `setting_card()` has its h2 outside, at `--font-size-md`. Developer, Schedules and Permissions mix them. `settings_head` (20px below) and `page_header` (24px below) also alternate between sections. | Developer, Schedules, Permissions, all heads | One container in Settings: `setting_card` with a heading outside. Tables and forms go inside it as rows or as an inset. One section header. |
| 9 | **Missing and misplaced dividers.** Developer → Chat has two rows with no line between them, because the rows are wrapped in a `region` div and the card's divider rule only sees direct children. Account → Change password: the fields end 24px short of the card's padding, because the form sits in a two-column `setting-row` with an empty `auto` column and a 24px column gap. | Developer, Account | Draw dividers on `.setting-row + .setting-row` (any depth), not on `> * + *`. A full-width row variant for forms. |
| 10 | **Phone: text and stacked controls make sections long.** The h1 is hidden, but its 2–3 line lead stays at the top of each section (Files 96px, Schedules 96px). Controls drop under the title, so a person row is 112px. The level-1 list rows are 32px under a finger (below 44), and four rows have no value ("Voice", "Apps", "Tailscale", "Updates"). With the About disclosures open, the section body scrolls sideways by 59px at 390 and 129px at 320. | Phone, all sections | Hide or shorten the lead on phones; keep one small button at the end of the row. List rows `--size-row-lg` 56 (COMPONENT-SPEC phone list). Fill or drop values consistently. Find the wide element in About's credits. |

Smaller items are in §3.

---

## 1. Inventory

### 1.1 Frame (every Settings page)

The frame comes from `components/panel.rs:219` (`settings_frame_at`). Its CSS is :4601–4808, the phone rules are
:5442–5528 and the two-level phone design is :7094–7290.

- **Modal.** A direct link also opens as the modal: melissa.js moves the page into `dialog.overlay`. It is
  `min(1120px, 100vw-2rem)` × `min(820px, 100dvh-2rem)`, and full screen at 760px or less.
- **Left column.** `nav.settings-nav` is 240px: the title "Settings", a search field, five group labels (Agents, Chat,
  System, People, Melissa) and 14 links of 32px with a 0.15rem gap. It is 818px tall at 1440, so it just fits the
  820px modal and scrolls on shorter screens.
- **Right column.** `.settings-main` holds `header.panel-head` (56px: a breadcrumb plus a close button, and on phones a
  back link plus `h1.settings-here`) and `.panel-body` (24px padding; 16px on a phone with no top padding; max width
  980px).
- **Phone level 1.** `GET /settings` shows the nav alone, as grouped cards of 32px rows with an icon, label, value and
  chevron (`list.rs`, :7114–7215). Shot: `settings--390-*`.
- **Phone level 2.** A section shows alone under a back link and the title. The body h1 is hidden (:7259), but the
  lead under it stays.

### 1.2 Sections and the components they use

Heights are the sum of the body's blocks plus the gaps between them (`measure.json`). At 1440 the head (56px) and
the body padding (24px top) come on top of that.

| Section (route) | Code | Components | 1440 | 390 |
|---|---|---|---|---|
| Providers (`/settings/providers[?p=]`, also `/settings` above 760px) | pages.rs:432–499, 551–716 | `settings_head`, `link_button`, `split_card` (`pick_list` + `toggle`, `detail_head`, `badge`, `disclosure` "Details" holding `facts`, `settings_form` ×2, `group`, `var_list`), `action_button` "Remove provider" | 382 | 443 |
| Add provider (`/settings/providers/add`) | add_provider.rs | `add_panel`, `stepper`, `provider_tiles`, `search_box`, `registry_list` | 248 | 272 |
| Agents (`/settings/agents`) | pages.rs:1203–1237 | `page_header`, `setting_card` of `setting_row_with` (`badge`, `action_button` Delete), two loose `p` | 423 | 608 |
| A helper (`/settings/agents/{id}`) | pages.rs:1273–1355 | `setting_card` (4 rows), `.setting-card.card-pad` + `settings_form` (Name), Icon card (`icon_picker` + form), Delete card (`action_button`) | 861 | 1098 |
| New helper (`/settings/agents/new`) | pages.rs:1378–1403 | `page_header`, hand-written `form.stack` + `label.field` + `icon_picker` + primary button | 365 | 443 |
| Permissions | pages.rs:890–916 | `page_header`, `card` + `table` (empty: `p.empty`) | 179 | 164 |
| Schedules | pages.rs:1051–1185 | `page_header` (2-line lead), `card` + `table`, `card` + `settings_form` (6 fields + `disclosure` Advanced), `setting_card` + `inline_form`/`inline_number` | 1090 (1285 with Advanced open) | 1139 |
| Voice | voice.rs page + pages.rs:1676–1757 | `settings_head` (from voice.rs), `live_region` with `setting_card` + `toggle` (+ `progress_bar`, `notice`) | 155 | 178 |
| Files | pages.rs:1419–1441 | `page_header` (3-line lead), `setting_card` + `inline_select`, `setting_card` "Boxes with their own setting" | 328 | 406 |
| Apps | pages.rs:1465–1486 | `page_header`, `setting_card` + `inline_select` | 160 | 153 |
| Tailscale | pages.rs:1508–1538 | `page_header`, `setting_card` + `actions` | 160 | 153 |
| Updates | pages.rs:785–868 | `settings_head`, `confirm_card` (when asked), `setting_card` (row + `command_block` + `.setting-more` > `disclosure` Details + `facts`), `device_name_region` (`.card-pad` form) | 538 | 537 |
| Developer (`/settings/developer`) | src/pages.rs:1315–1342 | `page_header`, `setting_card` of `bypass_region` + `full_access_region` (`toggle`, `notice`, `confirm_card`), `setting_card` of `chat_prefs_region` (`toggle`, `inline_select`), `card` New agent task (form), `card` Agent tasks (table) | 1035 | 1196 |
| Reset | pages.rs:1558–1593 | `page_header`, `setting_card` + `setting_row_with` + `danger_confirm` (`.approval.destructive` card holding `facts` + form) | 160 (not available here) | 129 |
| Account | components/people.rs:131–176 | `page_header`, `setting_card` (2 rows), `setting_card` holding a `div.setting-row` with `form.stack` (3 `credential_field`), `setting_card` of sessions | 826 | 923 |
| People | components/people.rs:208–258 | `page_header`, `setting_card` (new link: `command_block`), `setting_card` of people (`plain_action`, `action_button`), `setting_card` Invites | 499 | 672 |
| About (also members) | pages.rs:1799–1853 | `about_head` (mark + h1), `p.quiet`, `actions` of `new_tab_button` ×3, `setting_card` "Licenses" with 3–4 `setting_row_with` each holding a `disclosure` (`license_text`, `markdown_document`) | 605 (6389 open) | 693 (10426 open) |

Fixed head (56px) and body top padding (24px) come on top at 1440. At 390 the head is 56px and the body has no top
padding.

---

## 2. Double lines (every case found)

Each entry gives where it is, what you see, the rules that cause it, and the smallest fix.

### 2.1 Updates → "This device" → Details: two lines 5px apart

- **Shot:** `settings-updates--1440-*` (y ≈ 402/407), `overlay-settings-updates--*`.
- **Cause:** `.setting-card > * + *` (:4829) draws a full-width top line on `div.setting-more` (pages.rs:858).
  Inside it, `details.disclosure` (:5343) draws its own top line, inset by `.setting-more`'s 16px side padding
  (:6378). Above both, the `command_block` ends 12px higher. That makes three horizontal edges in 17px.
- **Fix:** `.setting-more > .disclosure { border-top: 0; padding-top: 0 }`. Better still is one rule for every
  disclosure that is not first in a free-standing flow: `.setting-card .disclosure, .setting-extra .disclosure,
  .split-detail > .disclosure { border-top: 0; padding-top: 0 }`.

### 2.2 About → Licenses: two lines per row

- **Shot:** `settings-about--1440-*`, `settings-about-open--*`.
- **Cause:** each row (pages.rs:1821–1848) puts a `disclosure` in `.setting-extra`. The disclosure's top line (:5343)
  sits under the title and is inset by the row's padding. The next row's full-width line (:4829) follows about 45px
  later. Three rows give six lines in about 230px. With a license open, the sunken `.license-text` box (:1971, its
  own border) ends 12px above the next row's line, so its bottom border and the row line read as a pair (measured
  gap 12px; 4px between the pre's top and the following row's top in the closed layout).
- **Fix:** the same rule as 2.1 (no disclosure border inside `.setting-extra`). Better: make each license row's
  disclosure the row's trailing control ("Show" as a link button), so the row stays one line.

### 2.3 Providers detail: "Details" boxed between two lines

- **Shot:** `settings-providers--1440-*` (lines at y ≈ 340 and 433, with only "Details" between them).
- **Cause:** `.disclosure` top line (:5343) plus `.remove-provider` (:6332–6336, `border-top`, 16px margin and 12px
  padding). Together with `.split-detail`'s 20px gap (:4979) the closed Details takes 93px between two hairlines.
- **Fix:** drop `.remove-provider`'s border (keep the space), and drop the disclosure border inside `.split-detail`.
  Space groups things and lines are the last resort (ui-craft §2).

### 2.4 Provider Details open: three kinds of line in one pane

- **Shot:** open "Details" in `settings-providers`.
- **Cause:** `.group` (:5172–5178) "Environment" has a top line and 1.1rem padding, inside the disclosure that already
  had one (2.3). Then `.vars` (:5206) is a bordered box with lines between its rows (:5222). It sits inside a
  bordered split card that sits inside the panel. That is a box in a box in a box.
- **Fix:** no top line on `.group` when it follows a form in a disclosure body (use `--space-5` of space). Keep
  `.vars`' own frame, because it is a list, and remove the outer one.

### 2.5 Boxes inside setting cards (nested borders that read as double lines)

These are not two hairlines on one edge. They are a bordered box inside a bordered card, which looks like a double
frame at the card's edge.

- **Updates:** `command_block` (`.md-code.command`, bordered) in the row's extra part. Its bottom edge is 12px above
  the `.setting-more` line (2.1).
- **Reset** (not shot: needs a container install): `danger_confirm` puts a bordered `.approval.destructive` card
  (:2664, radius 12, which is the same radius as its parent card and against ui-craft's "nested corners") inside
  `.setting-extra` of a setting card. Fix: the confirm body without its own border, or with `--radius-md`.
- **Developer:** the "Risky" notice in the bypass row is fine: it has no border, only a fill.
- **About:** `license-text` and the credits' `.md-table` grids sit inside the card. Keep them as insets, but without
  the disclosure line above them (2.2).

### 2.6 Table bottom line + card border

`th, td { border-bottom }` (:4491–4497) puts a hairline under the last row of a table in `card()` (Permissions,
Schedules, Developer → Agent tasks), 16px above the card's own bottom border. With the demo's empty tables this does
not show (`p.empty`), but it shows as soon as there are rules or schedules. Fix: `tr:last-child > td {
border-bottom: 0 }` inside a card.

### 2.7 Not double lines (checked)

- **Head bar and nav.** The head bar's bottom line (:4617) meets the nav's right line (:4665). That is a correct T.
- **Phone list.** The cards (:7155–7165) have a line between rows and none at the ends. Correct.
- **Phone head.** On phones `.settings-main .panel-head` drops its line (:7226). Correct.

---

## 3. Bulk (what makes sections tall) and other bugs

### 3.1 Spacing that adds up on every page (desktop)

| What | Now | Rule | Proposed |
|---|---|---|---|
| Head bar | 56px, breadcrumb only | `.panel-head` min 3.5rem (:4615) | Keep 56px, but it carries the title (`.settings-here`, `--font-size-lg`) and the close button; drop the "Settings /" crumb on desktop (the nav already says where you are) |
| Body padding | 24px all round | `.panel-body` `--space-5` (:4641) | `--space-5` sides, `--space-4` top |
| Section header | h1 1.75rem + lead + 20–24px below (62–87px) | `.page-header` (:1938), `.settings-head` (:4932, margin 1.25rem), pages.rs `page_header`/`settings_head` | Gone on desktop (title in the head bar). A lead only where it says something the rows don't, as one `--font-size-sm` muted line, `--space-3` below. "Checked … · Check again" moves to the head bar's right side |
| Gap between setting groups | 24px | `.setting-group + .setting-group` `--space-5` (:4815) | `--space-4` (16px) |
| Group heading | 1rem/650, 8px below | `.setting-heading` (:4818) | Keep the size; margin `0 0 var(--space-2)`; heading only when there are 2+ cards on the page |
| Setting row | 12px × 16px padding, 8/24px gap; one line → 48px, two → 71–72px | `.setting-row` (:4833–4839), `.setting-desc` margin 0.15rem (:4851) | `min-height: var(--size-row-md)` (40) for one line and `--size-row-lg` (56) for two; padding `var(--space-2) var(--space-4)`; gap `var(--space-1) var(--space-4)` |
| Description | `--font-size-sm`, max 60ch, wraps to 2–3 lines | `.setting-desc` (:4850) | One line (copy rule: "help is one short sentence at most"); long text goes behind the row's disclosure |
| `card()` in Settings | h2 `--font-size-lg` inside, 16px padding, 24px margin below, 12px grid gap | `.card` (:4466–4479), `.settings .card` (:5318) | Not used in Settings: `setting_card` with the heading outside (see 3.3) |
| Form gaps | `form.stack` 0.9rem (:4571), `.settings-form` 1.1rem (:5192), `.field` 0.3rem (:4581) | | `--space-3` between fields, `--field-gap` (`--space-1`) inside a field |
| Disclosure | summary 44px (:5351) + 12px padding (:5345) + line | `.disclosure` | Summary `--size-control-md` (32px), no line inside cards, `--space-2` padding |
| Split detail | 20px top padding, 20px gap (:4975, :4979) | `.split-detail` | `--space-4` padding and gap |

**Effect on a short page** (Apps, Tailscale, Reset, Voice). Today: head 56 + padding 24 + header 63 + gap 24 + one
row 73 = 240px for one setting. Proposed: 56 + 16 + 56 = 128px.

### 3.2 Per section: measured and target

Heights are at 1440 and 390 (see §1.2 for totals). The targets list only what changes.

- **Providers.**
  - Measured: the header is 62px, the Add provider row 32px plus 20px gap, and the split card 252px. In the detail,
    the head is 64px and the closed Details with Remove take about 150px.
  - Target: "Add provider" and "Check again" go into the head bar or one toolbar row (one row, 32px). Remove the
    disclosure and remove-provider lines (2.3). Pick rows are 60px; target `--size-row-lg` 56.
  - Bug 2 (tablet clipping): stack at a container width below about 640px.
  - At 320 the "Saved in `<path>`" code is 343px wide inside a closed Details. Give it `overflow-wrap: anywhere` or
    put it in a `command_block`.
- **Agents.**
  - Rows are 71px. Two loose paragraphs follow the card ("Change what an agent runs on…" and the "New helper" link,
    pages.rs:1233–1234).
  - Target: rows 56; "New helper" becomes a secondary button in the head bar or a last row of the card; drop the hint
    line, or make it the card's one-line footer.
- **A helper** (861px → target about 420px).
  - The first card (291px) duplicates the sidebar: the name with its icon, "Made", Pinned, Fresh start.
  - Name (141px), Icon (264px, Save icon) and Delete (93px) are each a card of their own (pages.rs:1323–1351,
    `.card-pad` :6383).
  - Target: one card with the rows Name (inline field + Save, as `inline_form`), Icon (the current icon plus a
    "Change" disclosure holding the picker), Pinned (a switch, not an "Unpin" button), Fresh start, then Delete as the
    last row in caution style.
  - Bug: the avatar sits 3px low next to the name (`setting_row_with` title, pages.rs:1292).
- **New helper** (365px). Fine in size. The icon picker's tiles are 40px with 6px gaps: keep.
- **Permissions** (179px). An empty `card()` "Always allowed" (92px) under a 63px header. Target: a `setting_card`
  with one muted row "Nothing yet. Every request asks you first." (40px).
- **Schedules** (1090px, 1285px with Advanced → target about 300px with no schedules).
  - The lead is 2 lines.
  - "Scheduled jobs" is an empty card (92px).
  - "Add a schedule" is always open (719px). It has six full-width fields; "Day (every week)" shows even for "Every
    day".
  - The wait setting's card heading "When a run needs your OK" repeats its row title.
  - Target: the list (or one empty row), an "Add schedule" button that opens the form, and fields capped at `24rem`
    with Repeats, Day and Time on one `.field-row`. The wait setting becomes a row with no heading.
- **Voice / Apps / Tailscale / Reset** (155–160px). Each is a header plus one row. Target: one row under the head bar
  (3.1).
  - Reset is the one place where the danger card (2.5) belongs: keep it, but without a nested border radius 12.
- **Files** (328px).
  - The lead is 3 lines.
  - "Boxes with their own setting" is a whole card that says there are none (100px).
  - Target: drop the lead to one line. Turn the second card into a muted line under the row, or a row only when there
    are overrides.
- **Updates** (538px → target about 220px).
  - The header is 62px.
  - The row puts the version in the description, and "Running from source…" and the command come as an extra part.
  - Details has the double line (2.1).
  - Device name is a 166px card with a full-width field, a hint and "Save name".
  - Target: "Check for updates" goes in the head bar. The row is "Melissa · Version 0.0.1 · badge" with the action on
    the right. Details carries no line. "Device name" becomes a second row in the same card (`inline_form` text field
    + Save).
- **Developer** (1035px).
  - The lead ("Try one agent with one prompt, outside the chat.") describes the task tool, not the page.
  - The rows are 92–112px because of 2–3 line descriptions and the "Risky" notice.
  - The Chat rows have no divider (bug 9).
  - Below them come two `card()`s (New agent task 332px, Agent tasks 92px).
  - Target: rows 56px with one-line descriptions. The notice appears only when bypass is on. The task form goes behind
    "New task".
- **Account** (826px → target about 330px).
  - Username and Role are two 48px rows that could be one ("owner · Owner").
  - Change password is a 420px always-open form: three fields, each with its own "Show password" checkbox, and it
    ends 24px short of the right edge (bug 9).
  - Target: one identity row; a "Change password" row with a button that opens the form (fields max `24rem`, one Show
    password for the form). Keep the sessions card.
- **People** (499px; 672px on a phone). Rows with buttons are 72px on desktop and 112px on a phone, where the buttons
  wrap under the name. Target: rows 56px; on a phone keep one button at the row's end, or put Reset password and
  Remove in a "⋯" menu (`agent_menu` pattern), rows 56px.
- **About** (605px; 6389px with everything open).
  - `p.quiet` "Pre-alpha…" (pages.rs:1814) repeats the global banner word for word.
  - The three outbound buttons wrap to two lines on a phone (72px).
  - Licenses: see 2.2. With credits open, the section body scrolls sideways by 59px at 390 and 129px at 320
    (`measure.json`, `panel-body` `scrollWidth`). The credits' tables scroll inside `.md-table` as intended, so the
    overflow comes from another element in the credits Markdown. Not pinned down; check in devtools.
  - Target: drop the duplicate line, Licenses rows one line each with "Show" on the right.
- **Add provider** (248px). Compact enough. Out of scope for "fat"; keep it.

### 3.3 One container, one header (consistency)

- Settings uses two cards: `card()` (components.rs:1979; heading inside at `--font-size-lg`) on Permissions,
  Schedules ×2 and Developer ×2, and `setting_card()` (panel.rs:329; heading outside at `--font-size-md`) everywhere
  else. On Developer and Schedules both kinds sit on one page, with different heading sizes and positions.
  - **Target:** `setting_card` only. Tables go in as rows or as an inset; a form goes in as a full-width row variant
    (`.setting-row.full`: one column, no empty `auto` column).
- Section headers: `settings_head` (header.settings-head, 20px below, with "Checked … · action") on Providers,
  Updates and Voice; `page_header` (24px below) elsewhere; a helper's page has none (title only in the
  breadcrumb), so it starts 80px higher than its siblings.
  - **Target:** the title in the head bar for all sections (3.1); one optional status/action slot on the head bar's
    right.
- Dividers: `.setting-card > * + *` (:4829) only sees direct children. Rows wrapped in `region()` or `.setting-more`
  break it (Developer → Chat has none; Updates gets an extra one).
  - **Target:** `.setting-row + .setting-row` plus an explicit `.setting-card > :not(:first-child)` only for known
    wrappers. Better: regions use `display: contents`, or the card itself is the swap target.

### 3.4 Controls (44px left over on desktop)

Measured at 1440: every button, input and select in Settings is 32px (28px for `sm`), which matches the proposal.
These are the exceptions:

- `details.disclosure > summary` is 44px (:5351). 17 instances across the shots.
- `button.btn-link` "Check again" is 18px tall. That is fine visually, but its hit area is under 24px. Give it the
  `--size-target-min` padding the buttons use.
- Phone level-1 list rows are 32px (`--size-control-md`, :7170) even under `pointer: coarse`. COMPONENT-SPEC §2.8
  gives phone lists `--size-row-lg` (56); at least 44px (`--size-target-touch`) under a finger.

### 3.5 Other visible bugs

1. **Focus ring on load.** When Settings opens from a link or loads directly, the current nav link shows the 3px
   focus ring (every 1440/768 shot). It looks like a heavy outline box, and to the owner it may read as one of the
   "double lines". Focus should land on the section title (`.settings-here`, which has no ring) on desktop too;
   melissa.js does that only when `settingsTitle()` finds a visible title (melissa.js:1939–1944, and the first-open
   path around :1826/:2301).
2. **Phone level 2: the lead stays when the h1 is hidden** (:7259 hides only `h1`). The section then starts with 2–3
   lines of muted text (Agents 72px, Files 96px, Schedules 96px). Hide `.page-header .lead` on phones too, or drop
   leads (3.1).
3. **Phone list values are uneven.** Voice, Apps, Tailscale and Updates show no value, while Developer shows "Off" and
   About "0.0.1" (list.rs:62–110). The Voice value is missing because voice is not available here; Updates could show
   "Up to date". Either every row has a value or the value is only for counts.
4. **Danger actions in secondary style.** A helper's "Delete helper" and Agents' "Delete" are `summary.btn-secondary`
   (components.rs:2247). The ui-craft rule calls for `danger-secondary` (caution text) for a destructive trigger.
   Reset's summary already has its own caution colour (:5301). Use one style for all three.
5. **Helper page title on phones is the helper's name** ("Updates" for the demo helper called Updates). With the
   section's own "Updates" also in the list, that is confusing. Consider "Agents / Updates" in the head, or
   "Helper: Updates". Copy decision for the owner.
6. **The nav is 818px tall** in an 820px modal (1440×900). On any shorter screen it scrolls inside the modal. With 3.4
   that is acceptable, but the 0.15rem gaps and the 12px group label margins (:4685, :4697) could go to `--space-half`
   / `--space-2` to keep it under 760px.
7. **About credits tables** at 390/320 are clipped at the card edge with no visible scroll cue (`settings-about-open~4
   --390-*`). Fine as a scroll box, but the right border of the grid is missing at the cut.

---

## 4. Slimmer targets in one place (for the Penpot board)

All values are existing tokens.

| Element | Target |
|---|---|
| Head bar | 56px; left: section title `--font-size-lg`/650 (phone: back link + title, unchanged); right: one status/action slot + close |
| Body | padding `--space-4` top, `--space-5` sides (phone `--space-4`), max width 980px (unchanged) |
| Lead | optional, one line, `--font-size-sm` muted, `--space-3` below; none on phones |
| Group heading | `--font-size-md`/650, `--space-2` below; only with 2+ groups |
| Between groups | `--space-4` |
| Setting card | `--color-bg-surface`, 1px `--color-border-default`, `--radius-lg`, no padding (rows pad themselves) |
| Setting row | padding `--space-2` `--space-4`; min height `--size-row-md` 40 (one line), `--size-row-lg` 56 (title + one-line description); title `--font-size-md`/600; description `--font-size-sm` muted, one line; control right, 32px (`--size-control-md`) |
| Row divider | one 1px `--color-border-default` between rows, never above the first or below the last, never a second one from a child |
| Disclosure in a card | no line; summary `--size-control-md`, muted, chevron; body `--space-2` above, `--space-3` gap |
| Form in a card | full-width row, fields max `24rem`, `--space-3` between fields, Save at the end (secondary unless it is the page's one action) |
| Inset (code, license, log) | `--color-bg-sunken`, `--radius-md`, `--space-3` padding; no inset directly next to a row divider (keep `--space-3` between) |
| Danger confirm | caution soft fill, `--radius-md` inside a card (not `--radius-lg`), no second border |
| Split card | stack below about 640px of its own width; detail padding `--space-4`; pick rows `--size-row-lg` |
| Phone list rows | `--size-row-lg` 56 (or 44 under `pointer: coarse` at least) |

Owner decisions this needs (please ask before building):

1. Desktop title in the head bar instead of the breadcrumb plus h1.
2. Leads dropped or reduced to one line.
3. Forms behind a button (Add schedule, Change password, New task).
4. Merging a helper's four cards into one.
5. Row heights 40/56.

Per AGENTS.md and the `melissa-ui` skill, nothing here is built until the owner has seen the Penpot redesign.

---

## 5. Reproducing

The harness and the shots run inside the dev shell. The wrapper used for this audit was kept outside the repo (a
scratch script). It subclasses `ui_shots.Shooter`, sets the widths 1440/768/390/320, adds the four extra screens
named in the method note, and runs one measuring `page.evaluate` per light shot. Its output is `measure.json` next
to the shots. To shoot only Settings with the stock harness: `devenv shell -- just ui-shots --only settings`
(1440/1024/390).
