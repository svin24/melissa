# Melissa design system: component spec (target)

Status: research draft for the owner, 2026-11-22. Nothing here is built. It is the target the Penpot library is
drawn from first, then `components.rs` + `melissa.css` are moved onto. Binding rules (`melissa-ui` skill,
`ui-craft.md`) still apply: calm, colour-only hover, one primary per view, no browser dialogs, no inline
script/style (CSP), works without JS, offline.

Items marked **[owner]** need the owner's call (collected in section 8).

---

## 0. Ground rules for every component

| Rule | Detail |
|---|---|
| One source of markup | Each family is one Rust type in `components/<family>.rs` with `impl maud::Render`. Pages build the struct and write `(it)`. No `class: &str` parameters (today `mark(class)` and `avatar(a, class)` accept raw classes); variants are enums. |
| Variant axes = Penpot variant properties | CSS: family class + `data-*` per axis, named exactly like the Penpot variant property: `<button class="btn" data-variant="primary" data-size="md">`. Penpot component: `Button` with properties `variant`, `size`, `state` (or asset path `Button / primary / md / hover` if variants are not available in the local Penpot). |
| State comes from the platform | Hover `:hover` (inside `@media (hover: hover)`), `:focus-visible`, `:active`, `:checked`, `:disabled`, `[aria-current]`, `[aria-pressed]`, `[aria-expanded]`, `[aria-invalid]` / `:user-invalid`, htmx `.htmx-request`. No `.is-active` classes. |
| Native first | `button`, `a`, `input`, `select`, `textarea`, `fieldset`, `details`, `dialog`, `[popover]`, `progress`, `meter`, `table`. ARIA only where noted. No hand-built listbox, combobox or tablist. |
| Focus ring | `outline: var(--focus-width) solid var(--color-focus-ring); outline-offset: var(--focus-offset)`; flush rows use an inset ring (`outline-offset: calc(var(--focus-width) * -1)`). Never animated, never removed. |
| Hit area (touch) | Visual size stays thin. Under `@media (any-pointer: coarse)` `--target-min` becomes 44px and every control grows its hit area with a reserved `::after` (section 1.4). Spacing between adjacent controls grows so hit areas never overlap. |
| Motion | Colours and `opacity`/`transform` only, named properties, `--motion-fast` (150ms) for state changes, `--motion-medium` (200ms) for sheets; every transition inside the one `prefers-reduced-motion: no-preference` block. |
| Busy | A pending htmx request: htmx puts `.htmx-request` on the element; the button shows a spinner in its icon slot, keeps its width and label, `aria-disabled="true"`; forms use `hx-sync="this:drop"` against double posts (instead of `hx-disabled-elt`, which drops focus). A server-side job in progress is rendered as `busy: true` with the `-ing…` label ("Installing…"). |
| Disabled | Native `disabled` plus a visible reason next to it (never only `title`). Colours: `--color-text-disabled`, border `--color-border-default`, no hover. |
| Ids | Components that wire ARIA (`aria-describedby`, `aria-labelledby`, `popovertarget`) take an explicit `id` base and derive `-hint`, `-error`, `-menu`, `-title`. |

---

## 1. Tokens

### 1.1 Naming scheme

Three tiers. Penpot token path uses dots; CSS custom property is the same path with dashes.

| Tier | Penpot path | CSS | Where defined | Who reads it |
|---|---|---|---|---|
| Primitive (ref) | `ref.blue.600`, `ref.neutral.50`, `ref.honey.500` | `--ref-blue-600` | theme sets only (`:root` blocks) | semantic tokens only |
| Semantic (sys) | `color.bg.surface`, `space.3`, `radius.md` | `--color-bg-surface` | `:root` + theme/accent/density blocks | components |
| Component | `button.primary.bg.hover`, `field.height` | `--button-primary-bg-hover` | `:root`, as aliases of semantic tokens | that family only |

Component tokens exist only where a family has a variant × state matrix (button, field, chip, row, switch,
segmented). Everything else reads semantic tokens directly. A theme may override a component token, never a
primitive inside a component rule.

Penpot token sets (DTCG JSON): `core` (primitives, dimensions), `theme/light`, `theme/dark`, `accent/<name>`,
`density/compact`, `density/comfortable`. A Penpot theme = `core` + one `theme/*` + one `accent/*` + one
`density/*`, the same combinations the CSS allows.

### 1.2 Semantic colour tokens (map from today's names)

| Token | Today | Use |
|---|---|---|
| `color.bg.ground` | `--ground` | page |
| `color.bg.surface` | `--surface` | cards, rail, secondary buttons, fields |
| `color.bg.raised` | `--surface` | menus, dialogs, sheets, picker (one step lighter in dark) |
| `color.bg.sunken` | `--ground` | segmented track, progress/meter track, code, log |
| `color.bg.hover` | `--ground` | row/ghost hover on surface |
| `color.bg.active` | (new) | pressed row/ghost (one step past hover) |
| `color.bg.selected` | `--ground` | current nav row, selected segment |
| `color.text.default` | `--ink` | text, glyphs |
| `color.text.muted` | `--muted` | labels, meta, hints |
| `color.text.disabled` | `--muted` | disabled controls (contrast-exempt) |
| `color.text.link` | `--action` | links, link buttons |
| `color.border.default` | `--border` | hairlines, cards, dividers (decorative) |
| `color.border.control` | (new) **[owner]** | field, checkbox, switch, secondary-button boundaries; ≥ 3:1 on its ground (WCAG 1.4.11). Today's `--border` is ~1.3:1 on white, so fields read only by their fill |
| `color.border.strong` | `--muted` | hover border of secondary/fields |
| `color.action.bg` / `.bg-hover` / `.bg-active` | `--action` (+`filter`) | primary, checked, selected accents. Explicit hover/active values replace `filter: brightness` |
| `color.action.text` | `--action-ink` | text on `action.bg` |
| `color.action.soft` | (new) | info notice/badge fill, selected-row tint if wanted |
| `color.focus.ring` | `--focus` | focus outline (follows the accent) |
| `color.caution.bg` / `.bg-hover` / `.bg-active` / `.text` / `.soft` / `.on` | `--caution*` | danger buttons, errors (`.text` ≥ 4.5:1 on `.soft` and surface) |
| `color.ok.fg` / `.soft` / `.text` | `--ok` | success marks, ok badge |
| `color.attention.fg` / `.soft` / `.text` | `--honey`, `--honey-soft`, `--honey-text` | "look here", warn tone, approvals (brand honey) |
| `color.idle.fg` | `--idle` | stopped dots |
| `color.bubble.bg` / `.text` | `--bubble*` | user chat bubble |
| `color.backdrop`, `shadow.float` | same | modal backdrop, floating shadow |
| `color.avatar.<green…slate>` | `--icon-*` | helper icons only |

### 1.3 Dimension, type and motion tokens

| Token | Value (compact) | Note |
|---|---|---|
| `space.1..7` | 4, 8, 12, 16, 24, 32, 48 px (0.25 … 3rem) | the common steps in ui-craft §1; near-duplicates move here |
| `radius.xs` / `sm` / `md` / `lg` / `full` | 4 / 6 / 8 / 12 / 999 | sm controls 6, md controls + fields + menu items 8, cards/menus/dialogs 12 **[owner]**: today buttons/inputs are 10px; 8px suits 32px controls and the nested-corner rule (menu 12 = item 8 + padding 4) |
| `size.control.sm` / `md` / `lg` | 28 / 32 / 40 | buttons, fields, selects, segmented. lg only for the one focal action of setup/onboarding and the composer |
| `size.row.sm` / `md` / `lg` | 32 / 40 / 56 | steps rows / menu items, list rows / two-line rows |
| `size.icon.sm` / `md` / `lg` | 16 / 20 / 24 | 16 in sm/md controls, 20 in lg controls and composer |
| `size.target.min` | 24 (fine) / 44 (`any-pointer: coarse`) | WCAG 2.5.8 / 2.5.5 |
| `font.family.body` / `head` / `mono` | `--font` / `--font-head` / `--mono` | unchanged |
| `font.size.xs` / `sm` / `md` / `lg` / `xl` | 0.8 / 0.85 / 1 / 1.15 / 1.75 rem | nothing under xs; control text has its own tokens below |
| `font.size.control.sm` / `md` / `lg` | 0.85 / 0.9 / 1 rem | **[owner]** 1rem text in a 32px button is cramped; 0.9rem/600 reads well. Inputs stay 1rem (stops iOS zoom) |
| `font.weight.regular` / `strong` / `heading` / `brand` | 400 / 600 / 650 / 700 | |
| `line.height.tight` / `control` / `body` | 1.2 / 1.25 / 1.5 | |
| `border.width.hairline` | 1px | |
| `focus.width` / `focus.offset` | 3px / 2px | today's ring |
| `motion.duration.fast` / `medium` / `enter` | 150 / 200 / 180 ms | |
| `motion.ease.out` | `cubic-bezier(0.2, 0, 0, 1)` | never ease-in |
| `shadow.float` | `0 12px 32px var(--color-shadow)` | menus, dialogs, sheets, picker only |

### 1.4 Touch hit area (one rule for every control)

```css
:root { --target-min: 24px; }
@media (any-pointer: coarse) { :root { --target-min: 44px; } }   /* invisible, so any-pointer is safe */
.btn, .seg, .chip[data-interactive], .row-link, .check, .switch { position: relative; }
.btn::after { /* reserved pseudo-element: hit area only */
  content: ""; position: absolute;
  /* top/bottom % resolve against the control's height, left/right % against its width:
     grows only the sides that are under --target-min, by exactly the missing amount */
  inset: min(0px, calc((100% - var(--target-min)) / 2));
}
@media (pointer: coarse) { :root { --gap-control: var(--space-3); } }  /* ≥ 12px so 32px controls' 44px areas don't overlap */
```

Checkbox/radio/switch: the wrapping `<label>` is the hit area (min-block-size `--target-min`). Text fields are
tapped via their full-width box plus the label above them; no extra area. Links inside sentences are exempt.

---

## 2. Families

Size columns: **sm** 28px, **md** 32px (default), **lg** 40px.

### 2.1 Button

| Variant | Use | Default look (tokens) | Hover / Active |
|---|---|---|---|
| `primary` | the one main action per view/card | bg `action.bg`, text `action.text`, border transparent 1px | `action.bg-hover` / `action.bg-active` |
| `secondary` | other actions | bg `bg.surface`, text `text.default`, border `border.control` | border `border.strong`, bg `bg.hover` / `bg.active` |
| `ghost` | toolbars, low-weight actions, icon-only | bg transparent, text `text.default`, border transparent | `bg.hover` / `bg.active` |
| `danger` | the confirming step of a destructive action | bg `caution.bg`, text `caution.on` | `caution.bg-hover` / `caution.bg-active` |
| `danger-quiet` (ghost + caution text) | the trigger that opens a destructive confirm, menu "Delete" | text `caution.text`, transparent | `caution.soft` |
| `link` | inline low-emphasis action ("Show details") | text `text.link`, underline, no padding, height = line | underline thickness 2px |
| modifier `icon_only` | any variant; square `size.control.*`, glyph `size.icon.sm` | needs `label` → `aria-label` + tooltip | — |

Anatomy: [icon 16] label [trailing icon (chevron for a menu trigger)]. Padding-inline: sm `space.2`+2, md `space.3`, lg `space.4`. Radius sm `radius.sm`, md/lg `radius.md`. Text `font.size.control.*`/600.

| State | Look |
|---|---|
| focus-visible | ring (section 0) |
| pressed toggle | `aria-pressed="true"`: bg `bg.selected`, border `border.strong` (secondary/ghost only) |
| disabled | text `text.disabled`, bg unchanged for secondary/ghost, primary/danger bg `bg.sunken`; no hover; reason shown beside |
| busy | spinner replaces the icon slot (or precedes label), label kept, `aria-disabled="true"`; reduced motion: still dotted ring |

Keyboard/SR: native `button` (Enter/Space). Navigation is always `<a class="btn">` (same look), never a button. Menu trigger: `popovertarget` gives `aria-expanded` natively. Icon-only: `aria-label`; glyph `aria-hidden`.

No JS: `type="submit"` in a real `<form method="post">` (+CSRF), or `<a href>`. htmx attributes added on top.

```rust
pub enum ButtonVariant { Primary, Secondary, Ghost, Danger, DangerQuiet, Link }
pub enum Size { Sm, Md, Lg }            // shared by every family
pub enum Does<'a> {
    Submit,                              // inside an enclosing form
    SubmitValue { name: &'a str, value: &'a str },
    Href(&'a str),                       // renders <a class="btn">
    Popover(&'a str),                    // popovertarget=id
    ShowModal(&'a str),                  // commandfor=id command="show-modal" (+ href fallback, §2.12)
    Close,                               // inside dialog/popover: command="close" / popovertargetaction="hide"
}
pub struct Button<'a> {
    pub label: &'a str, pub variant: ButtonVariant, pub size: Size, pub does: Does<'a>,
    pub icon: Option<Glyph>, pub trailing: Option<Glyph>, pub icon_only: bool,
    pub pressed: Option<bool>, pub busy: bool, pub disabled: bool,
}
impl<'a> Button<'a> {
    pub fn primary(label: &'a str, does: Does<'a>) -> Self;   // + secondary/ghost/danger/link
    pub fn size(self, s: Size) -> Self; pub fn icon(self, g: Glyph) -> Self; pub fn icon_only(self) -> Self;
}
impl maud::Render for Button<'_> { … }
```

Component tokens: `button.{variant}.{bg|text|border}.{default|hover|active|disabled}`, `button.{size}.{height|padding-x|font-size|radius|icon}`.

### 2.2 Text input, textarea, search

| Part | Look |
|---|---|
| Field box | height `field.height` (= `size.control.md`; sm in dense tables/inline forms), bg `bg.surface`, border 1px `border.control`, radius `radius.md`, padding-inline `space.3`, text 1rem `text.default` |
| Placeholder | `text.muted`, generic stand-ins only (`example.com`); never the only label |
| Textarea | min 3 rows; `field-sizing: content` where supported (progressive), `max-block-size` 16 rows; mono variant for code/config |
| Search | `input type=search` inside `<form role="search">` with a visible or visually-hidden label; leading search glyph; native clear |

| State | Look |
|---|---|
| hover | border `border.strong` |
| focus-visible | ring (`:focus-visible` also fires for text fields; border stays) |
| invalid | `aria-invalid="true"` (server) or `:user-invalid` (client): border `caution.fg`, error line under (§2.17) |
| disabled / readonly | disabled: bg `bg.sunken`, text `text.disabled`; readonly: bg `bg.sunken`, text `text.default`, still focusable/selectable |

Keyboard/SR: native. `autocomplete`, `inputmode`, `type`, `spellcheck` set per field kind. Search with live
filtering: `hx-get` + `hx-trigger="input changed delay:250ms, search"`, results region `aria-live="polite"`
announcing a count; without JS Enter submits a GET and the page renders filtered.

```rust
pub enum InputKind { Text, Email, Url, Number { min: i64, max: i64 }, Password, Search, Tel }
pub struct TextInput<'a> {
    pub name: &'a str, pub value: &'a str, pub kind: InputKind, pub size: Size,
    pub placeholder: Option<&'a str>, pub autocomplete: &'a str, pub inputmode: Option<&'a str>,
    pub max_len: u32, pub required: bool, pub readonly: bool, pub disabled: bool,
}
pub struct TextArea<'a> { pub name: &'a str, pub value: &'a str, pub rows: u8, pub mono: bool, pub max_len: u32, pub required: bool }
pub struct SearchBox<'a> { pub action: &'a str, pub name: &'a str, pub value: &'a str, pub label: &'a str, pub live: Option<Hx<'a>> }
```

Tokens: `field.{bg|text|border|placeholder}.{default|hover|invalid|disabled}`, `field.height`, `field.padding-x`, `field.radius`.

### 2.3 Choosing a value: select, segmented, radio list, picker popover, menu

**Decision table** (one rule for every page):

| Need | Component | Why |
|---|---|---|
| One value from a short plain-text list in a form (≤ ~15, e.g. "Keep files for", provider) | **native `<select>`** | free keyboard, SR, no-JS, phone OS picker |
| One of 2–5 short options people compare at a glance (theme, scope) | **segmented** (radios, §2.6) | all options visible |
| One of N with a description, icon or status per option | **radio list** (`pick_list` rows with radios) | rich rows without a custom listbox |
| Several related choices at once behind a chip (the model picker: model + reasoning + access) | **picker popover**: `[popover]` holding a `<form>` of radio groups | native radios inside a native popover; existing pattern |
| Commands on a thing ("⋯": Rename, Fresh start, Delete) | **menu** (§2.4) | actions, not values |
| Many options (> ~30) or typed lookup | search input + radio list (server-filtered) | no combobox |
| Several values | checkbox list | |

**No custom `role="listbox"`/combobox**: a hand-built one owes the full contract (ui-craft §7) for no gain.

**Native select styling.** Closed control looks like a field (`field.*` tokens) with a chevron. Progressive
enhancement with the customizable select (`appearance: base-select`; Chrome/Edge 135+, Safari 27, not
Firefox, which falls back to the normal OS dropdown):

```css
@media (pointer: fine) and (hover: hover) {               /* phones keep the OS picker (guide: base-select opts out of it) */
  select, ::picker(select) { appearance: base-select; }
  ::picker(select) { background: var(--color-bg-raised); border: 1px solid var(--color-border-default);
                     border-radius: var(--radius-lg); box-shadow: var(--shadow-float); padding: var(--space-1); }
  option { min-block-size: var(--size-row-sm); border-radius: var(--radius-sm); }
  option:hover, option:focus-visible { background: var(--color-bg-hover); }
  option:checked { font-weight: 600; }  option::checkmark { color: var(--color-action-bg); }
}
```

Options stay text-only (older engines strip markup in `<option>`). Without base-select the closed control
still gets the field look (`appearance: none` + a border-drawn chevron on a wrapper `span.select::after`, which
survives forced colors; not a background image). Selects that save on change: `hx-trigger="change"` with a
Save button shown only under `@media (scripting: none)` (today's `inline_select` pattern).

```rust
pub struct Choice<'a> { pub value: &'a str, pub label: &'a str, pub enabled: bool }   // exists
pub struct Select<'a> { pub name: &'a str, pub choices: &'a [Choice<'a>], pub selected: Option<&'a str>,
                        pub size: Size, pub required: bool, pub save_on_change: Option<Hx<'a>> }
```

### 2.4 Menu (popover)

| Part | Look |
|---|---|
| Trigger | `Button` ghost/secondary, icon-only "⋯" or label + chevron; `popovertarget` |
| Surface | `[popover]` (auto): bg `bg.raised`, border `border.default`, radius `radius.lg`, padding `space.1`, `shadow.float`, min 13rem |
| Item | `button`/`a`, height `size.row.md` (40), glyph 16, text 0.9rem, radius `radius.md`; hover `bg.hover`, active `bg.active`; destructive item `caution.text` |
| Separator | 1px `border.default`, `space.1` margin |
| Phone (< 760px) | the same element styled as an action sheet (exists) |

Keyboard/SR (**decision**): Melissa's menus are short lists of commands people Tab through, so they use the
**disclosure-popover pattern**: no `role="menu"`/`menuitem`/`aria-haspopup`. Tab/Shift+Tab move through items,
Esc and click-outside close (native `popover=auto`), focus returns to the trigger (native). melissa.js may add
Up/Down/Home/End as a convenience. Opening moves focus to the first item (JS; without JS the next Tab reaches it).
If a menu ever needs arrow-key-only navigation it switches to full menu roles everywhere at once.

Placement: CSS anchor positioning where `CSS.supports("position-area", "top")`, flipping with
`position-try-fallbacks: flip-block`; otherwise melissa.js places it (as today). No JS: popover opens centred
(native default), still usable.

Reduced motion: `open` (opacity + scale 0.98, 160ms) becomes opacity-only.

```rust
pub enum MenuItem<'a> {
    Link { label: &'a str, href: &'a str, glyph: Glyph },
    Post { label: &'a str, action: &'a str, hx: Option<Hx<'a>>, glyph: Glyph, caution: bool },
    Confirm { label: &'a str, confirm: Confirm<'a> },   // opens the in-page confirm
    Separator,
}
pub struct Menu<'a> { pub id: &'a str, pub trigger: Button<'a>, pub items: &'a [MenuItem<'a>], pub sheet_title: Option<&'a str> }
```

### 2.5 Checkbox, radio, switch

| | Checkbox | Radio | Switch |
|---|---|---|---|
| Element | `input type=checkbox` | `input type=radio` in `fieldset` + `legend` | `input type=checkbox role="switch"` (+ `switch` attribute: native in Safari, harmless elsewhere) |
| Look | native, `accent-color: var(--color-action-bg)`, 16px | native, same | custom (`appearance: none`): track 32×18 (md) / 28×16 (sm), bg `bg.sunken`, border `border.control`; knob 14/12 `text.muted`; checked: track `action.bg`, knob `action.text` |
| Label | wrapping `<label class="check">`; row min-height `--target-min` | same | row label is the accessible name; hint via `aria-describedby` |
| Hover | label row → border `border.strong` | same | track border `border.strong` |
| Disabled | native + `text.disabled` label + reason | same | track `bg.sunken`, knob `text.disabled`, reason visible (today it's only `title`) |
| Invalid | `aria-invalid` + error under the group | error under the fieldset | n/a |
| Motion | none | none | knob `transform` 150ms; reduced: none |

Switch semantics: acts immediately (saves on change). Checkbox: part of a form saved by a button. Without JS
the switch's form shows a "Save" button under `@media (scripting: none)` (exists).

```rust
pub struct Check<'a> { pub name: &'a str, pub value: &'a str, pub label: &'a str, pub hint: Option<&'a str>, pub checked: bool, pub disabled: Option<&'a str> /* reason */ }
pub struct RadioGroup<'a> { pub name: &'a str, pub legend: &'a str, pub options: &'a [Choice<'a>], pub selected: Option<&'a str>, pub hint: Option<&'a str> }
pub struct Switch<'a> { pub id: &'a str, pub label: &'a str, pub on: bool, pub hx: Hx<'a>, pub locked: Option<&'a str> }  // replaces Toggle
```

Tokens: `switch.track.{bg|border}.{off|on|disabled}`, `switch.knob.{off|on|disabled}`, `switch.{sm|md}.{width|height}`.

### 2.6 Segmented control and tabs

| Kind | Element | Use |
|---|---|---|
| **Segmented** (choose a value) | `fieldset.segmented` > `label.seg` > `input type=radio` (visually hidden) + text | 2–5 options of one setting |
| **Section switcher** (go to a view) | `nav` > links with `aria-current="page"`, same look as segmented or underline | Settings sections, Files/Activity views. Never `role="tablist"` for navigation |
| **In-page tabs** | not provided | add only when a real need appears (needs JS; no-JS fallback = panels stacked) |

Segmented look: track `bg.sunken`, border `border.default`, radius `radius.md`, padding 2px; segment height
`size.control.*` − 4, radius `radius.sm`, text control size/600 `text.muted`; hover `text.default`;
selected (`:has(:checked)` / `[aria-current]`) bg `bg.surface`, border `border.control`, text `text.default`;
focus ring on the label via `:has(:focus-visible)`; disabled option `text.disabled`. Danger option (exists):
selected uses `caution.soft`/`caution.text`. Arrow keys move the selection (native radios); with htmx the
form saves on change, without JS a Save button appears. Replace today's `.seg[aria-pressed]` buttons.

```rust
pub struct Segmented<'a> { pub name: &'a str, pub legend: &'a str, pub legend_visible: bool, pub options: &'a [Choice<'a>], pub selected: &'a str, pub size: Size, pub save: Hx<'a> }
pub struct SectionNav<'a> { pub label: &'a str, pub items: &'a [(&'a str /*href*/, &'a str /*label*/, Option<Glyph>)], pub current: &'a str }
```

### 2.7 Badge, status chip, chip

| Kind | Interactive | Anatomy | Look |
|---|---|---|---|
| **Badge** | no | text (+ optional 6px dot) | height 20, padding-inline `space.2`, 0.8rem/600, radius `full`, bg `{tone}.soft`, text `{tone}.text`, 1px transparent border (shows in forced colors) |
| **Status chip** | no | dot or spinner + text ("Working", "Needs you", "Stopped") | badge look; dot colour = tone; spinner for working |
| **Chip** (suggestion / filter) | yes | text (+ icon) | sm button with radius `full`; filter chip = checkbox/radio styled as chip, checked → `action.soft` + `border.control` |
| **Count** | no | number | plain text, tabular figures, no pill (existing rule) |

Tones: `neutral` (bg.sunken / text.muted), `info` (action.soft / text.link), `ok`, `attention` (honey), `caution`.
Colour never carries the meaning alone: the word is always there. A chip that updates lives in a region that
announces (`role=status`), not each chip.

```rust
pub enum Tone { Neutral, Info, Ok, Attention, Caution }   // rename of today's Tone (Muted→Neutral, Warn→Attention, Error→Caution)
pub struct Badge<'a> { pub label: &'a str, pub tone: Tone, pub mark: Option<Mark> }   // Mark::Dot | Mark::Spin
pub struct Chip<'a> { pub label: &'a str, pub does: Does<'a>, pub icon: Option<Glyph> }
```

Tokens: `chip.{tone}.{bg|text|border}`, `chip.height`, `chip.font-size`.

### 2.8 List rows

| Variant | Element | Anatomy | Trailing |
|---|---|---|---|
| **Navigation row** | `a.row` (whole row) | lead (avatar/icon 18) · title (0.9rem/600) · sub (0.8rem muted) | optional badge / count |
| **Value row** (settings → subpage) | `a.row` | title · hint | value (muted) + chevron; accessible name = title + value (both inside the link) |
| **Switch row** | `div.row` with `label` | title · hint | `Switch`; the title is the switch's `<label for>`, hint `aria-describedby` |
| **Control row** | `div.row` | title · hint | select/segmented/button |
| **Static row** (facts) | `div.row` / `dl` | term · value | — |

Sizes: `size.row.md` 40 (one line), `size.row.lg` 56 (two lines); padding-inline `space.3`; hairline between
rows only inside cards, never around the list (space groups).

| State | Look |
|---|---|
| hover (links only) | `bg.hover` |
| active | `bg.active` |
| current | `aria-current="page"`: `bg.selected`, title weight 650 |
| attention | tone modifier: `attention.soft` + 3px `attention.fg` inline-start edge (exists) |
| focus | inset ring |
| disabled | `text.disabled`, reason as the sub line, not a link |

Lists: `ul` > `li` > row; a titled group is `section` with `aria-labelledby` on its heading.

```rust
pub enum Lead<'a> { None, Glyph(Glyph), Avatar(&'a Avatar<'a>) }
pub enum Trail<'a> { None, Chevron, Value(&'a str), Badge(Badge<'a>), Count(usize), Switch(Switch<'a>), Control(Markup) }
pub enum RowKind<'a> { Link { href: &'a str, current: bool }, Static }
pub struct Row<'a> { pub kind: RowKind<'a>, pub lead: Lead<'a>, pub title: &'a str, pub sub: Option<&'a str>, pub trail: Trail<'a>, pub tone: Option<Tone> }
```

`Trail::Control(Markup)` takes only other library components (typed wrappers preferred if it grows).

Tokens: `row.{bg}.{hover|active|current}`, `row.height.{md|lg}`, `row.padding-x`.

### 2.9 Card, panel, modal dialog, bottom sheet

| Kind | Element | Look | Notes |
|---|---|---|---|
| **Card** | `section.card` + h2/h3 | bg `bg.surface`, 1px `border.default`, radius `lg`, padding `space.4` (< 760) / `space.5` | optional header actions (right), footer actions; tone `attention` / `caution` (permission cards). One card level per region |
| **Panel** | the details column / settings pane | bg `bg.surface`, hairline on one side, no radius | layout, not nested in a card |
| **Modal dialog** | `dialog` + `showModal()` | bg `bg.raised`, radius `lg`, `shadow.float`, backdrop `color.backdrop`; widths sm 24rem (confirm), md 32rem (form), lg 48rem (logs) | head: title h2 (`aria-labelledby`) + icon-only close; body scrolls; footer actions right, primary last |
| **Bottom sheet** | same `dialog`, `data-sheet` under 760px | full width, top radius `lg`, max-block 85dvh, handle (decorative) | full screen under 40rem (exists) |

Behaviour: Esc closes (native); click-outside via `closedby="any"` (Chrome 134+/Firefox 141+) plus the guide's
fallback (melissa.js: close when the click target is the dialog itself; Safari lacks `closedby`). Focus moves
into the dialog (`autofocus` on the first field or the least destructive button), returns to the opener
(native). Background inert (native for modal). Confirmations of single actions stay **in-page** (`details`-based
`inline_confirm`), not dialogs.

No JS: (a) dialogs whose content is a page (app details, logs) are opened from `<a href>` to that full page,
htmx upgrades to the dialog (exists); (b) static dialogs in the page use invoker commands
`<button commandfor="id" command="show-modal">` (Baseline since 2025-12) which need no script; give those a
`details` or page fallback where older browsers matter **[owner: browser policy]**.

Motion: dialog/sheet `open` 160ms opacity + scale 0.98 / sheet translate 200ms; reduced: opacity only.

```rust
pub struct Card<'a> { pub title: Option<&'a str>, pub level: HeadingLevel, pub actions: Option<Markup>, pub tone: Option<Tone>, pub body: Markup }
pub enum DialogSize { Sm, Md, Lg }
pub struct Dialog<'a> { pub id: &'a str, pub title: &'a str, pub size: DialogSize, pub body: Markup, pub footer: Option<Markup>, pub sheet_on_phone: bool }
```

### 2.10 Notice, alert, toast

| Kind | Element / ARIA | Look | When |
|---|---|---|---|
| **Notice** (inline) | `p.notice` / `div.notice`; no role when present at page load | glyph (16, tone) + text (+ one link/button); bg `{tone}.soft`, text `text.default`, glyph `{tone}.fg`, radius `md`, padding `space.2` `space.3` | facts about the region ("Couldn't read device details yet") |
| **Form status** | one `div role="status"` per form, empty at load; errors and success are swapped into it | notice look | the answer to a submit |
| **Alert** | `role="alert"` | notice look, caution tone | only for an error that blocks and arrives without user action (lost connection). Rare |
| **Toast** | `[popover=manual]` items inside one persistent `role="status"` region, bottom-end | bg `bg.raised`, `shadow.float`, close button; persistent until closed, max 3 | **[owner]** only for results whose origin is no longer on screen ("Update downloaded" while in a chat). Default: don't use; prefer inline |

Fix to note: today's `notice()` always sets `role="status"`, which re-announces notices rendered at page load.

```rust
pub struct Notice<'a> { pub tone: Tone, pub text: &'a str, pub action: Option<Button<'a>>, pub detail: Option<&'a str> /* disclosure with raw detail */ }
pub fn form_status(id: &str, content: Option<Notice<'_>>) -> Markup;
```

### 2.11 Table

| Part | Look |
|---|---|
| Wrapper | `div.table-wrap` overflow-x auto; when it can scroll: `role="region" aria-label tabindex="0"` so keyboard users can scroll |
| Header | `th scope=col`, 0.85rem/600 `text.muted`, bottom hairline |
| Rows | height `size.row.md`, hairline between, no zebra; numbers right-aligned, `tabular-nums` |
| Row link | first cell holds the `a`; the row highlights `bg.hover` via `tr:has(a:hover)` |
| Sorting | server-sorted; header is a link, `aria-sort` on the `th` |
| Empty | `EmptyState` instead of the table |
| Phone | scrolls inside its box; no card transformation |

```rust
pub enum Align { Start, End }
pub struct Column<'a> { pub label: &'a str, pub align: Align, pub sort: Option<(&'a str /*href*/, Option<SortDir>)> }
pub struct Table<'a> { pub label: &'a str, pub columns: &'a [Column<'a>], pub rows: Vec<Vec<Markup>>, pub empty: EmptyState<'a> }
```

### 2.12 Tooltip and toggletip

| Kind | Use | Implementation |
|---|---|---|
| **Tooltip** | only the visible name of an icon-only button (duplicates `aria-label`) | a `span.tip aria-hidden="true"` child, shown on `:hover` (hover: hover) and `:focus-visible`, after no delay, hoverable, Esc hides (melissa.js), positioned above (anchor positioning where available). `popover="hint"` + `interestfor` later, when Firefox/Safari ship it |
| **Toggletip** | short help ("What is a provider?") | a `button` (ghost, icon "i", label "About …") with `popovertarget` to a small `[popover]`; works without JS, Esc/outside closes |

Never: `title` as the only carrier (not keyboard/touch), essential info or a disabled reason in a tooltip.

```rust
pub struct Toggletip<'a> { pub id: &'a str, pub about: &'a str, pub text: &'a str }
// tooltips are rendered by Button when icon_only = true
```

### 2.13 Avatar / agent icon

| Size | px | Use |
|---|---|---|
| xs | 16 | inline, menus |
| sm | 18 | sidebar rows, lists |
| md | 22 | chat/agent headers |
| lg | 32 | sheet heads |
| xl | 44 | detail heads, onboarding |

Anatomy: hexagon/shape (`color.avatar.*` stroke, line style) or provider mark (letter in hexagon); optional
status dot (6–8px, ring in `bg.surface`). Decorative (`aria-hidden`) when the name is next to it, else
`role="img" aria-label`. Forced colors: strokes are `currentColor`; status dot hides, the status word carries it.

```rust
pub enum AvatarSize { Xs, Sm, Md, Lg, Xl }
pub struct AvatarView<'a> { pub avatar: &'a Avatar<'a>, pub size: AvatarSize, pub status: Option<Tone>, pub label: Option<&'a str> }   // replaces avatar(a, class)
```

### 2.14 Progress and meter

| Kind | Element | Look |
|---|---|---|
| Progress (a job) | `<progress value max>`; indeterminate without `value` | track `bg.sunken`, fill `action.bg`, height 6, radius full; label above ("Downloading update"), text "1.2 GB of 4 GB" (tabular) |
| Meter (a level: disk, memory) | `<meter low high optimum value>` | fill `ok.fg` / `attention.fg` / `caution.fg` from the native ranges |
| Spinner | `spin` glyph | `text.muted`; reduced motion: still dotted ring (exists) |

Labelled via `<label for>` or `aria-labelledby`; the number in text is the truth. Indeterminate animation stops
under reduced motion. Forced colors: if styled with `appearance: none`, use `forced-color-adjust: none` with
`Highlight` fill and `CanvasText` border.

```rust
pub struct Progress<'a> { pub id: &'a str, pub label: &'a str, pub text: &'a str, pub done: Option<(u64, u64)> }
pub struct Gauge<'a> { pub id: &'a str, pub label: &'a str, pub text: &'a str, pub value: f64, pub low: f64, pub high: f64 }
```

### 2.15 Empty state

Anatomy: optional title (h3, 1rem/650) · one line (`text.muted`) "what will appear and how" · at most one action
(secondary; primary only when it is the view's focal action). Left-aligned on the region's edge, padding
`space.5` block. No illustration.

```rust
pub struct EmptyState<'a> { pub title: Option<&'a str>, pub text: &'a str, pub action: Option<Button<'a>> }
```

### 2.16 Form field (wrapper for 2.2, 2.3, 2.5)

| Part | Look / wiring |
|---|---|
| Label | above, 0.9rem/600 `text.default`, `<label for=id>` (or `legend` for groups) |
| Optional mark | "(optional)" muted after the label; required fields carry native `required` and no mark **[owner]** |
| Control | the input/select/textarea |
| Hint | 0.85rem `text.muted`, id `{id}-hint` |
| Error | 0.85rem `caution.text` + fail glyph, id `{id}-error`, replaces nothing (hint stays) |
| Wiring | `aria-describedby="{id}-hint {id}-error"`, `aria-invalid="true"` when error |
| Gap | label→control `space.1`, control→hint `space.1`, field→field `space.4` |

Validation: the server decides. Client side only native constraints (`required`, `type`, `min/max`,
`maxlength`) styled with `:user-invalid` (Baseline widely available: no red before interaction). After an htmx
submit with errors the returned fragment puts `autofocus` on the first invalid field and the form status says
what failed. Typed values are kept. Without JS the same fragment arrives in the full page.

```rust
pub enum Control<'a> { Text(TextInput<'a>), Area(TextArea<'a>), Select(Select<'a>), Check(Check<'a>), Radios(RadioGroup<'a>), Segmented(Segmented<'a>) }
pub struct Field<'a> { pub id: &'a str, pub label: &'a str, pub hint: Option<&'a str>, pub error: Option<&'a str>, pub optional: bool, pub control: Control<'a> }
pub struct Form<'a> { pub action: &'a str, pub hx: Option<Hx<'a>>, pub fields: &'a [Field<'a>], pub submit: Button<'a>, pub status: Option<Notice<'a>> }
pub struct Hx<'a> { pub post: &'a str, pub target: &'a str, pub swap: Swap, pub trigger: Option<&'a str> }   // shared, adds CSRF + hx-sync
```

Tokens: `field.label.*`, `field.hint.text`, `field.error.text`, `field.gap`.

---

## 3. Keyboard and screen-reader summary

| Family | Keys | ARIA beyond native |
|---|---|---|
| Button | Enter/Space | `aria-label` (icon-only), `aria-pressed` (toggle), `aria-disabled` while busy |
| Field / search | native | `aria-describedby`, `aria-invalid`; search results `aria-live="polite"` count |
| Select | native | none |
| Menu | Tab, Esc; (JS) Up/Down/Home/End | none (disclosure pattern) |
| Checkbox / radio | Space / arrows | none |
| Switch | Space | `role="switch"` |
| Segmented | arrows (native radios) | none; section nav `aria-current="page"` |
| Rows | Tab to links | `aria-current="page"` |
| Dialog / sheet | Esc, Tab cycles inside | `aria-labelledby` |
| Notice | — | `role="status"` only for inserted ones |
| Table | Tab to scroll region | `role="region"` + label when scrollable, `aria-sort` |
| Tooltip / toggletip | Esc | tip `aria-hidden` (name is `aria-label`) |
| Progress / meter | — | label association |

## 4. Theming

**Mode.** `<html>` carries `data-theme="light" | "dark"` only when the person chose one; no attribute = follow
the system. Rendered by the server from a cookie/preference (like `looks.rs`), so no flash and no script.
Colour tokens are written once with `light-dark()` (Baseline since 2024-05) and the attribute only sets
`color-scheme`, which also themes native controls and scrollbars:

```css
:root { color-scheme: light dark;
  --color-bg-ground: light-dark(var(--ref-neutral-50), var(--ref-neutral-950)); … }
:root[data-theme="light"] { color-scheme: light; }
:root[data-theme="dark"]  { color-scheme: dark; }
```

(Fallback if older engines matter: the classic duplicate block, `@media (prefers-color-scheme: dark) {
:root:not([data-theme="light"]) {…} }` plus `:root[data-theme="dark"] {…}`.) The Settings control is a
3-option segmented "System / Light / Dark".

**Accent.** Per-person accent = a preset name, `data-accent="blue|teal|violet|green|rose|orange|slate"`
(server-rendered). Each preset defines only `color.action.{bg,bg-hover,bg-active,text,soft}`,
`color.text.link` and `color.focus.ring`, light and dark, each pair checked for 4.5:1 (text) and 3:1 (ring)
**[owner]**. No free colour picker: the CSP forbids inline `style`, and presets keep contrast guaranteed.
Honey stays the brand/attention colour and is not an accent option.

**High contrast.**
- `@media (prefers-contrast: more)`: `border.default` → `border.control`, `text.muted` → closer to `text.default`,
  `focus.width` 3 → 4.
- `@media (forced-colors: active)`: the system palette wins. Rules that keep it working: every control keeps a
  1px (transparent) border so it shows as `ButtonBorder`; focus is an `outline`; state never only by
  background or shadow: selected segment/row gets `outline: 2px solid Highlight` (or a border), disabled uses
  `GrayText`; the switch, status dots and custom progress use `forced-color-adjust: none` with `ButtonText`/
  `Highlight`/`HighlightText`; select chevrons are borders, not images. Don't use `forced-color-adjust: none`
  for looks.

**Density.** `data-density="compact"` (default) | `"comfortable"` on `<html>` changes only tokens:

| Token | compact | comfortable |
|---|---|---|
| `size.control.sm/md/lg` | 28 / 32 / 40 | 32 / 40 / 48 |
| `size.row.sm/md/lg` | 32 / 40 / 56 | 40 / 48 / 64 |
| `space` inside cards | `space.4` | `space.5` |

Touch devices keep compact visuals with 44px hit areas (§1.4); comfortable is a personal setting **[owner]**.

## 5. Without JS (summary)

Everything renders and posts as plain HTML: buttons are form submits or links; selects/switches/segmented
show a Save button under `@media (scripting: none)`; menus and toggletips are native popovers; dialogs are full
pages (or invoker commands); confirmations are `details`. JS only adds: htmx swaps, menu placement and arrow
keys, Esc for tooltips, `closedby` fallback, focus after swaps.

## 6. Migration notes (for the build phase, not now)

- Old token names become aliases (`--ground: var(--color-bg-ground)`) until every rule is moved, then removed.
- `button.ghost` today is a full-width secondary; the new `ghost` is transparent. Rename the old one first.
- Replace `filter: brightness(1.08)` hover with `action.bg-hover`.
- `Toggle` → `Switch` (reason visible, not `title`); `.seg[aria-pressed]` → radios; `notice()` role fix;
  `avatar(a, class)`/`mark(class)` → size enums; positional-`&str` helpers (`action_button(action, target,
  label, confirm)`) → structs.
- `min-height: 44px` on all `button` goes away; hit areas come from §1.4.

## 7. Penpot library layout (suggested)

Pages: `Foundations` (colour, type, space, radius, motion, density tables bound to tokens) · `Controls`
(Button, Field, Select, Check/Radio/Switch, Segmented, Chip) · `Navigation & lists` (Row, Menu, SectionNav) ·
`Containers` (Card, Dialog, Sheet, Notice, Table, Empty state) · `Media` (Avatar, Progress, Meter). Each
component: variant properties = the `data-*` axes above, plus `state` (default, hover, focus, active, selected,
disabled, busy, invalid), shown in light and dark token sets.

## 8. Owner decisions needed

1. **Control heights and text:** md 32 / sm 28 / lg 40, button text 0.9rem/600 (inputs stay 1rem).
2. **Radius:** controls 8px (sm 6px) instead of today's 10px.
3. **Control border contrast:** add `color.border.control` at ≥ 3:1 for fields, checkboxes, switches (fields get a visibly darker outline).
4. **Toasts:** none (inline only), or persistent toasts for off-screen results.
5. **Required vs optional marking:** mark optional fields only.
6. **Accent presets:** which colours, and whether accent is per person or per Melissa.
7. **Comfortable density:** offer as a setting now or later.
8. **Browser policy** (for AGENTS.md): e.g. "Baseline newly available is fine when it degrades to working HTML". It decides `light-dark()` vs duplicate dark block, invoker commands for dialogs, and `base-select`.
9. **Token source of truth:** hand-written `melissa.css` tokens + a Rust test checking parity with a Penpot-exported `tokens.json`, versus generating the `:root` block (a build step, which the rules avoid).
