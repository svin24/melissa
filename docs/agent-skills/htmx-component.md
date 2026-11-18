# Adding or using a UI component (htmx + maud)

**When:** any change under `crates/web`.

## Rules
- All markup lives in `crates/web/src/components.rs`. Pages (`pages.rs`) only call component functions.
  A new component is a human-reviewed change; agents never generate markup.
- All text goes through maud escaping. No `PreEscaped` with data. Agent/model output is untrusted:
  render with `log_view` (escaped, monospaced), never as HTML or markdown.
- Styles only in `assets/melissa.css` (embedded with rust-embed). No CDN, no inline `style=`.

## Patterns
- Live updates: `live_region(id, url, secs, live, content)`. The fragment endpoint returns the same
  region; when `live` is false the returned region has no `hx-get`, so polling stops by itself.
- Forms: `post_form(action, target, label, fields)` works with htmx (`HX-Request: true` → return the
  fragment) and without JS (plain POST → 303 redirect). Handle both in the handler.
- htmx does not swap 4xx/5xx responses by default; for htmx requests return 200 with an error `notice`.

## Security (verified by tests in `crates/web/src/lib.rs`)
- CSP `default-src 'self'`, no inline scripts/styles. htmx needs
  `<meta name="htmx-config" content='{"includeIndicatorStyles":false,"allowEval":false}'>` so it
  doesn't inject inline styles or use eval (`hx-on`, `js:` values won't work; don't use them).
- Mutating requests must be same-origin (`Sec-Fetch-Site` or `Origin` vs `Host`). Real CSRF tokens and
  `__Host-` session cookies come with login.

## Testing
- Router tests: `tower::ServiceExt::oneshot` + `http_body_util::BodyExt::collect` (dev-deps).
