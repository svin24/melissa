# Adding or using a UI component (htmx + maud)

**When:** any change under `crates/web`. Load the `melissa-ui` skill first (its rules are binding).

## Layout of the crate
- `components.rs`: the fixed component library, the only place with markup and CSS classes.
- `pages.rs`: pages and fragments composed from components (no markup of its own).
- `feed.rs`: folds a conversation's event log (the lead agent's or an agent's: same shape) into chat items and holds
  **every mapping from internal names to UI words** (tool names, event types, backend names, approval texts, how
  Melissa frames jobs/messages relayed into an agent's conversation). The UI says "helper" for an agent and "app" for a
  workspace. Renames of tools/events only touch this module (and `state.rs` for `list_apps`/`cluster_health`/
  `device_info`). `agent_info` is still mapped for old transcripts. A `Lookup` trait gives the fold
  confirmation arguments and agent names (implemented by `ui::Ui`). `Feed::for_helper(true)` words things for a
  helper's chat; `Feed::archived()` folds a past chat (no confirmation lookups); `settle()` marks what was still
  running as stopped and pending approvals as withdrawn.
- `state.rs`: apps and devices for the UI, read through the tool registry (audited `tool.call`, task 0,
  `Scope::Nothing`, read-only tools) plus who looks after each workspace (`lead.agents().workspaces()`, i.e. the
  `melissa.io/owner-agent` labels), cached 30 s; the first request waits, later ones refresh in the background.
- `settings/`: the Settings pages, forms and `/api/settings/...` (see [settings-ui.md](settings-ui.md)).
- `ui.rs`: the UI routes (`/`, `/cluster`, `/approvals`, `/agents/{agent}`, `/chats/{id}` (a past chat, read-only),
  `/ui/...`, `POST /ui/new-chat`); `pages::Who` picks the
  conversation (Lead agent or an agent) for chat pages, streams, the composer and Stop; `api.rs`: the JSON API; `lib.rs`: router,
  task pages, assets, security middleware.

## Components (2026-11-18)
Shell: `app_page` (banner + rail + main), `page` (no rail: errors, and the task pages when there is no lead agent), `banner`, `mark`, `rail`
(nav, conversations: Melissa, each helper, "New chat" (a plain form to `/ui/new-chat`), the last 5 past chats;
devices; no task link: Agent tasks live in Settings → Developer), `waiting_count`, `page_header`, `section`, `empty_state`.
Chat: `feed` (live list), `feed_archive` (the same list, read-only, no stream), `chat_divider` ("Earlier",
"Melissa restarted"), `feed_entry` (stable `li` id), `oob_inner`, `user_message`, `lead_message`,
`relayed_message` (a job or message Melissa passed into a helper's chat),
`markdown` (agent prose), `plain_text` (what the user typed), `activity_row` (its detail is Markdown for a
helper's job, plain for a tool), `note` (tones quiet, caution, guard: the honey "read text from outside" note),
`approval_card` (pending / approved / denied / withdrawn; full honey border, caution border when destructive,
"Can't be undone" variant), `composer` (2026-11-20: grows with its text; paperclip/paste/drop files, shown as chips cloned from its
`file-chip` template by melissa.js; since the model picker one rounded box: text on top, a bar below with the
paperclip left and `model_chip`, the mic and a round Send right; placeholder "Message <name>"), `user_message_with` + `SentFile` (a sent message's thumbnails and download chips),
`composer_status`, `file_panel` (2026-11-20, owner decision: the chat's file box beside it, a `details`
collapsed by default and remembered per browser by melissa.js (`data-remember`), a section above the chat below
1280 px; rows with download link and a trash button (`hx-confirm`), "Keep files for…" select; `file_panel_body`
is what delete/retention return, `file_panel_inner` + `file_count` what the stream swaps on `Change::Files`;
`SentFile.href = None` renders an "expired" chip), `model_chip` + `model_picker`
(2026-11-20, owner-requested; replaced the 2026-11-19 `agent_menu`): the chip ("Opus 5.5 · High", the backend's
`provider_mark`) is a `popovertarget` button in the composer bar (slot `#model-chip`, swapped by the stream on
`Change::Status`); the picker is a `div[popover]` after the composer with its own form (`choice` = `<backend>[ <model>]`,
or `effort` + hidden `current`) posting to `/ui/backend` or `/ui/agents/{id}/backend`; htmx swaps the picker (closed)
and the chip out of band; a refusal comes back `data-error` and melissa.js reopens it. Rail of usable backends
(Favorites first), search, model rows (mark, name, check, star, Ctrl+1..9 hints), "Show more", a Reasoning column
only when the current model reports levels; built from `BackendView`s in `agents.rs`. Without script every model
shows grouped by backend and each pick is a plain form post. A footer (`.picker-footer`, left out when empty) holds
the option groups that apply: Speed (`.segmented` Standard / Fast, `fast=off|on`) only when the current model
reports a fast mode (`ModelOption::fast`; a fast model itself reports none), Access (`access=ask|full`) only with
the developer option "Full access per agent"; Full access asks in place (`details.seg-confirm`, no browser dialog).
The chip adds `span.chip-tag` "Fast" and `span.chip-warn` (warning icon) on Full access. Never add a group a
backend doesn't report.
Apps and devices: `app_tile` (hexagon), `add_app_tile`, `app_grid`, `app_strip` (mini workspaces, polls itself),
`device_card` (`ul.device-facts` pills, memory and storage `<meter>`s, an optional warning, e.g. storage almost full).
Settings (2026-11-19, owner-requested; details in [settings-ui.md](settings-ui.md)): `settings_frame`,
`settings_frame_at` (a page below a section: Settings / Section / page),
`settings_head`, `split_card`, `pick_list`, `provider_mark`, `toggle`, `detail_head`, `facts` (`dl.facts`), `group`,
`text_field`, `lines_field`, `settings_form`, `var_list`, `command_block`, `confirm_card`, `plain_action`,
`actions`, `region`; rail link "Settings" (`Nav::Settings`).
Task pages (2026-11-19; since the providers integration in Settings → Developer: `/settings/developer` is the
list, `GET /tasks` redirects there, `/tasks/{id}` uses `settings_frame_at`; bare pages without a lead agent; the built-in `echo` is not offered, a configured
echo is; the reply (stdout) is `markdown`, stderr a `log_view` "Agent log"; htmx "Run" answers `HX-Redirect` to the
new task): `card`, `table`, `badge`, `notice`, `code`, `link`, `truncated`, `live_region`, `log_view`,
`select_field`, `textarea_field`, `post_form`, `action_button`, `crumbs` (breadcrumb, Settings' look), `facts`.

## Rules
- All markup lives in `components.rs`. Pages only call component functions. A new component is a
  human-reviewed change; agents never generate markup.
- All text goes through maud escaping. No `PreEscaped` with data. Agent/model output is untrusted: prose (replies,
  relayed jobs, a helper's job detail) goes through `markdown`; user text through `plain_text` (paragraphs, `- `
  lists, backtick code, bare URLs linked); logs through `log_view`.
- Agent-supplied ids (e.g. ACP tool-call ids) never become DOM ids or selectors: `feed.rs` maps them to its own
  keys (`t1`, `t2`, ...).
- Styles only in `assets/melissa.css` (tokens on `:root`, dark mode via `prefers-color-scheme`). No CDN, no
  inline `style=`. Our only script is `assets/melissa.js` (Enter-to-send, scrolling, reload after a restart).

## Agent Markdown (`markdown`, 2026-11-19)
- Two steps: `src/markdown.rs` parses with `pulldown-cmark` (no default features; tables, strikethrough, task lists)
  into a small `Node` tree, no markup; `components::markdown` renders the tree with maud, so every text is escaped.
  We never use pulldown-cmark's HTML writer (it passes raw HTML through).
- Safety: `Html`/`InlineHtml` events become text; `safe_href` passes only `http(s)://host` and `mailto:` (Email
  autolinks get `mailto:`), everything else keeps just the link text; images become a link "Image: <alt>"; all
  links get `target="_blank" rel="noopener noreferrer nofollow"`; nesting capped at `MAX_DEPTH` (24).
- `markdown::linkify` links bare `http(s)://` URLs in text (not in code or inside links); GFM-like trailing
  punctuation and unbalanced `)` trimming. `plain_text` (user messages) uses it too.
- Chat choices: soft line breaks render as `<br>` (agents and users mean them); `#`..`###+` become `h4`/`h5`/`h6`
  (the message has a hidden `h3`); tables scroll in `div.md-table`; code blocks scroll horizontally.
- Copy button on code blocks: `button.copy-code`, hidden by CSS unless `melissa.js` set `html.can-copy` (needs
  `navigator.clipboard`, i.e. a secure context: localhost or HTTPS; on plain-HTTP LAN it stays hidden).
- Tests: `components::tests::markdown_xss_inputs_are_inert` checks every emitted tag and attribute against an
  allowlist and every `href` scheme; add new XSS inputs there.

## Live updates (chat and approvals)
- `GET /ui/stream?since=<seq>&view=chat|approvals` (the lead agent) and `GET /ui/agents/{agent}/stream?since=` (a helper)
  are SSE, fed by the conversation's `EventLog` (same logs as the long-poll API). Confirmations are only in the
  Lead agent's log, so approvals show in its chat and on `/approvals`, not in a helper's chat. The page renders the feed folded up to `last_seq` and connects with `since=last_seq`.
- Each SSE `feed` event is server-rendered HTML: new `feed_entry` items (the htmx SSE extension appends them,
  `sse-swap="feed" hx-swap="beforeend"`) plus `hx-swap-oob` replacements for changed entries (a growing reply, a
  tool call that finished, a decided approval), the composer status and the waiting count. One SSE event per
  batch of log events; its `id` is the last seq, so a reconnect (`Last-Event-ID`) resumes exactly.
- The stream re-folds the log up to its cursor before streaming, so keys match the DOM the page rendered.
  Cursors carry the log's epoch (page: `?since=<seq>&epoch=<epoch>`, SSE ids `<epoch>-<seq>`); a cursor from
  another epoch (daemon restarted) or newer than the log gets a `reload` event, and so does every open page
  when the user starts a new chat (`new_chat` event); `melissa.js` reloads the page.
- History after a restart: each log starts with the last 30 turns of the current chat, restored from the
  transcript between `earlier_start`/`earlier_end` (memory only). The feed shows them between "Earlier" and
  "Melissa restarted" dividers under keys prefixed `e` (confirmation and agent tool-call ids repeat after a
  restart); what was running then shows stopped, pending approvals withdrawn.
- Taint: the first `tainted` event of a conversation becomes a `guard` note at that point ("Melissa read text
  from outside (Nextcloud's logs). Changes now need your OK until you start a new chat."); no "trust again"
  button. A new chat is the way to clear it.
- Disk: `device_info` reports `disks`; `state.rs` takes `/var` (else `/`) as the data disk; at 85% used or more
  the app strip and the device card warn ("Storage almost full (1.7 GB left)").
- Why SSE + the extension and not polling: one connection, low latency for streamed reply text, no
  re-rendering of the whole conversation (which would also steal focus from approval buttons).
- Files: a box change pushes `files_changed` (memory only) into that conversation's log; the stream answers
  with the panel's body and count out of band, and re-renders sent messages so removed files show as expired.
- Apps change slowly: `app_strip` and the cluster's apps region poll (`/ui/strip`, `/ui/cluster/apps`, 15 s).

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
- Screenshots without the T3 preview: `firefox --headless --profile <fresh dir> --no-remote
  --window-size=390,844 --screenshot out.png http://127.0.0.1:<port>/` (a fresh profile avoids cached CSS;
  `user_pref("ui.systemUsesDarkTheme", 1)` in its `user.js` for dark mode).

## Testing
- Router tests: `tower::ServiceExt::oneshot` + `http_body_util::BodyExt::collect` (dev-deps). For SSE, keep the
  response body and read frames with `body.frame()` under a timeout (`the_stream_sends_rendered_updates`).
- The lead agent test app registers fake read-only `list_apps`/`cluster_health`/`device_info` tools (`Fixed`; its
  `/var` is 91% used, so the disk warning shows) and keeps a transcript (new chat archives it).
