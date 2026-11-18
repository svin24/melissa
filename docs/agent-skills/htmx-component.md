# Adding or using a UI component (htmx + maud)

**When:** any change under `crates/web`. Load the `melissa-ui` skill first (its rules are binding).

## Layout of the crate
- `components.rs`: the fixed component library, the only place with markup and CSS classes.
- `pages.rs`: pages and fragments composed from components (no markup of its own).
- `feed.rs`: folds a conversation's event log (the Queen's or a bee's: same shape) into chat items and holds
  **every mapping from internal names to UI words** (tool names, event types, backend names, approval texts, how
  Melissa frames jobs/messages relayed into a bee's conversation). The UI says "helper" for a bee and "app" for a
  cell. Renames of tools/events only touch this module (and `state.rs` for `list_apps`/`cluster_health`/
  `bee_info`). Both `bee_info` and `comb_info` are mapped while that rename lands. A `Lookup` trait gives the fold
  confirmation arguments and bee names (implemented by `ui::Ui`).
- `state.rs`: apps and devices for the UI, read through the tool registry (audited `tool.call`, task 0,
  `Scope::Nothing`, read-only tools) plus who looks after each cell (`queen.bees().cells()`, i.e. the
  `melissa.io/owner-bee` labels), cached 30 s; the first request waits, later ones refresh in the background.
- `ui.rs`: the UI routes (`/`, `/hive`, `/approvals`, `/bees/{bee}`, `/ui/...`); `pages::Who` picks the
  conversation (Queen or a bee) for chat pages, streams, the composer and Stop; `api.rs`: the JSON API; `lib.rs`: router,
  task pages, assets, security middleware.

## Components (2026-11-18)
Shell: `app_page` (banner + rail + main), `page` (no rail: task pages, errors), `banner`, `mark`, `rail`
(nav, conversations: Melissa and each helper, devices), `waiting_count`, `page_header`, `section`, `empty_state`.
Chat: `feed` (live list), `feed_entry` (stable `li` id), `oob_inner`, `user_message`, `queen_message`,
`relayed_message` (a job or message Melissa passed into a helper's chat),
`plain_text`, `activity_row`, `note`, `approval_card` (pending / approved / denied / withdrawn; destructive and
"Can't be undone" variants), `composer`, `composer_status`.
Apps and devices: `app_cell` (hexagon), `add_app_cell`, `honeycomb`, `app_strip` (mini cells, polls itself),
`device_card` (facts + `<meter>`).
Task pages: `card`, `table`, `badge`, `notice`, `code`, `link`, `truncated`, `live_region`, `log_view`,
`select_field`, `textarea_field`, `post_form`, `action_button`.

## Rules
- All markup lives in `components.rs`. Pages only call component functions. A new component is a
  human-reviewed change; agents never generate markup.
- All text goes through maud escaping. No `PreEscaped` with data. Agent/model output is untrusted: render it with
  `plain_text` (paragraphs, `- ` lists and backtick code by splitting text; never HTML or markdown) or `log_view`.
- Agent-supplied ids (e.g. ACP tool-call ids) never become DOM ids or selectors: `feed.rs` maps them to its own
  keys (`t1`, `t2`, ...).
- Styles only in `assets/melissa.css` (tokens on `:root`, dark mode via `prefers-color-scheme`). No CDN, no
  inline `style=`. Our only script is `assets/melissa.js` (Enter-to-send, scrolling, reload after a restart).

## Live updates (chat and approvals)
- `GET /ui/stream?since=<seq>&view=chat|approvals` (the Queen) and `GET /ui/bees/{bee}/stream?since=` (a helper)
  are SSE, fed by the conversation's `EventLog` (same logs as the long-poll API). Confirmations are only in the
  Queen's log, so approvals show in her chat and on `/approvals`, not in a helper's chat. The page renders the feed folded up to `last_seq` and connects with `since=last_seq`.
- Each SSE `feed` event is server-rendered HTML: new `feed_entry` items (the htmx SSE extension appends them,
  `sse-swap="feed" hx-swap="beforeend"`) plus `hx-swap-oob` replacements for changed entries (a growing reply, a
  tool call that finished, a decided approval), the composer status and the waiting count. One SSE event per
  batch of log events; its `id` is the last seq, so a reconnect (`Last-Event-ID`) resumes exactly.
- The stream re-folds the log up to its cursor before streaming, so keys match the DOM the page rendered.
  A cursor newer than the log (daemon restarted) gets a `reload` event; `melissa.js` reloads the page.
- Why SSE + the extension and not polling: one connection, low latency for streamed reply text, no
  re-rendering of the whole conversation (which would also steal focus from approval buttons).
- Apps change slowly: `app_strip` and the hive's apps region poll (`/ui/strip`, `/ui/hive/apps`, 15 s).

## Patterns
- `live_region(id, url, secs, live, content)`: a region that polls itself; `live = false` stops polling.
- Forms work with htmx (`HX-Request: true` → return the fragment) and without JS (plain POST → 303 redirect).
  Handle both in the handler (`/ui/messages`, `/ui/confirmations/{id}/approve|deny`, `/ui/cancel`, `/tasks`).
- htmx does not swap 4xx/5xx responses by default; for htmx requests return 200 with an error text (the composer
  keeps the typed text and shows why it wasn't sent; a decided approval returns an "Already decided" note).
- `?draft=` on `/` pre-fills the composer (e.g. "Add an app" → `/?draft=Install%20`); it is never sent by itself.

## Security (verified by tests in `crates/web/src/lib.rs`)
- CSP `default-src 'self'`, no inline scripts/styles. htmx needs
  `<meta name="htmx-config" content='{"includeIndicatorStyles":false,"allowEval":false}'>` so it
  doesn't inject inline styles or use eval (`hx-on`, `js:` values and `hx-trigger` filters like
  `keydown[key=='Enter']` won't work; put such behaviour in `melissa.js`).
- Mutating requests must be same-origin (`Sec-Fetch-Site` or `Origin` vs `Host`), including all `/ui/*` POSTs.
  Real CSRF tokens and `__Host-` session cookies come with login.

## Gotchas
- OOB with `outerHTML:#id` replaces the target with the *wrapper* element; use `innerHTML:#id` on a stable
  container (`feed_entry`'s `li`), or `hx-swap-oob="true"` on an element that carries the id itself
  (`waiting_count`).
- `.feed-empty:not(:only-child)` hides the empty state once entries exist; maud emits no whitespace text nodes.
- Grok's tool calls change title while running: `search_tool` → `Search tools: "<query>"` (hidden),
  `use_tool` → `melissa__<tool>` (shown as "Using a tool", then the tool's words). Claude titles are
  `mcp__melissa__<tool>`. Check `feed.rs` tests when a backend changes its titles.
- rust-embed reads assets from disk in debug builds (no rebuild needed for CSS/JS changes); release builds
  embed them. CSS/JS are `Cache-Control: no-cache` so a new binary's styles show at once; fonts are immutable.
- The event log is in memory: after a daemon restart the chat starts empty (the transcript keeps history).
- Screenshots without the T3 preview: `firefox --headless --profile <fresh dir> --no-remote
  --window-size=390,844 --screenshot out.png http://127.0.0.1:<port>/` (a fresh profile avoids cached CSS;
  `user_pref("ui.systemUsesDarkTheme", 1)` in its `user.js` for dark mode).

## Testing
- Router tests: `tower::ServiceExt::oneshot` + `http_body_util::BodyExt::collect` (dev-deps). For SSE, keep the
  response body and read frames with `body.frame()` under a timeout (`the_stream_sends_rendered_updates`).
- The Queen test app registers fake read-only `list_apps`/`cluster_health`/`bee_info` tools (`Fixed`).
