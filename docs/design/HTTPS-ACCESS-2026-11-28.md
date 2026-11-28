# HTTPS access from other devices (2026-11-28)

MVP blocker #1: open the web UI from a phone or laptop without sending passwords over plain HTTP.
Owner decisions: DECISIONS 2026-11-28 "HTTPS access from other devices". Open items: PLAN Q22, Q85b, Q87a,
Q87f, Q110. Names and addresses below are placeholders (`melissa-box.example-tailnet.ts.net`, `100.64.0.x`).

## Summary

- **Phase 1 (built): Tailscale serve.** Melissa's own tailscaled terminates HTTPS with a Let's Encrypt
  certificate for `<node>.<tailnet>.ts.net` and proxies to a second loopback listener in the server. The owner
  turns it on from Settings → Tailscale, with the password. Only tailnet devices reach it.
- **Phase 2 (not built): LAN HTTPS.** A root-held, name-constrained local CA issues a certificate for
  `<host>.local`, served on port 7383. Devices that trust the CA get HTTPS on the LAN without Tailscale.
- **Later: a confirm service on its own hostname.** Root's on-screen confirmations (PLAN Q108) can't live on
  the ts.net origin. `ts.net` is on the Public Suffix List, so every node is its own site, but the `melissa`
  uid (and so an unsandboxed agent, Q85h) can rewrite Melissa's serve config and put anything on that origin.
  The confirm service needs a hostname and a listener the `melissa` uid can't touch.

## Phase 1: how it works

```
phone (tailnet) --HTTPS--> tailscaled (in the container, :443 on tailscale0)
                              --HTTP--> 127.0.0.1:8082  [web] proxy_listen, trusts the proxy
host 127.0.0.1:UI_PORT --> container :8081 (socket proxy)
                              --HTTP--> 127.0.0.1:8080  listen, trusts nothing (by default)
```

- **Turning it on.** Settings → Tailscale shows a row "Open Melissa from your other devices" once Melissa's
  Tailscale is connected. Turn on opens a form: a notice that the device name goes into public certificate
  logs, the name (default the node name; it can be changed first, `tailscale set --hostname`), and the
  owner's password (`/ui/settings/tailscale/serve`, a password path, see below). Melissa runs
  `tailscale serve --bg --https=443 http://127.0.0.1:8082` as the server's uid (already tailscaled's
  operator). Audited `tailscale.serve_on`; a rename is audited separately.
- **HTTPS off for the tailnet.** The CLI prints a `https://login.tailscale.com/f/serve?...` page and waits.
  Melissa shows "Allow HTTPS in Tailscale" (opens in a new tab), keeps the CLI waiting in the background and
  polls; Cancel stops it. Same shape as the sign-in URL flow.
- **Every start.** When the setting is on, the server applies the target again (`serve reset`, then serve).
  When the status shows a config that isn't exactly Melissa's (only TCP 443 HTTPS, one handler `/` to the
  target, no Funnel), it applies it again.
- **Turning it off.** Turn off (no confirmation: easy to undo) saves the setting as off, then runs
  `tailscale serve reset`; Disable does that first and says the HTTPS address stops working. Audited
  `tailscale.serve_off`.
- **Something served while it is off.** Melissa's tailscaled serves her UI alone. While the setting is
  off, every read of the state (the Settings page, `tailscale_status`) checks `tailscale serve status` and
  resets anything configured (e.g. an agent, as the `melissa` uid, serving an app port on the node's ts.net
  name), audited `tailscale.serve_off` by `melissa`. A config with Tailscale Services is never "ours".
- **Agents.** `tailscale_status` says whether it is on and the address; there is no tool to turn it on.
- **The host runs its own Tailscale** (`HOST_TAILSCALE=yes`): Melissa doesn't touch it. The row shows
  `sudo tailscale serve --bg http://127.0.0.1:<UI port>` with Copy (an approved exception to "no host commands
  in the UI"); install.sh prints the same.

## Security

- **Two listeners.** Only the second listener (`[web] proxy_listen`, loopback, must differ from `listen`)
  believes `X-Forwarded-Proto/For/Host`. Every listener strips forwarded and Tailscale identity headers first
  (`crates/web/src/front.rs`); the trusting one puts back `X-Forwarded-Proto: https` and the client address.
- **`https()`** is now only `X-Forwarded-Proto: https` after that layer. The old fallback (an `https://`
  Origin) is gone: a browser on plain HTTP could not send one, but any non-browser client could.
- **Cookies.** Over HTTPS only `__Host-melissa_session` (Secure) counts; the plain cookie is ignored, since
  anything that can answer on the host name over HTTP could set it.
- **Passwords over plain HTTP** (`/login`, `/claim`, `/reauth`, `/invite/*`, `/ui/account/password`,
  `/ui/settings/tailscale/serve`) are refused with 403 unless the Host is loopback (this device or an SSH
  tunnel). The form says to open the HTTPS address.
- **Host names** (against DNS rebinding): loopback names, this node's MagicDNS name, `[web] hosts`, and on a
  trusting main listener `*.ts.net` over HTTPS. Anything else gets 421.
- **HSTS** only on the ts.net origin through the serve listener: `max-age=86400`, no `includeSubDomains`, no
  `preload`.
- **Sign-in limits** (Q87a) count per client address (the last `X-Forwarded-For` entry) only on a trusting
  listener; elsewhere every request still comes from loopback. A full table (10,000 keys) drops stale
  counters and addresses first and never a username's counter that still has failures; only at a hard cap
  (100,000) do usernames go, those with the fewest failures first, so flooding it with made-up names or
  addresses can't reset the counter of a username being guessed.
- **`[web] trust_forwarded`** (the main listener believes `X-Forwarded-*`) is off by default. install.sh
  sets it only when the host runs Tailscale and `UI_BIND` is loopback (the host's `tailscale serve` in
  front of the UI), marked as its own line, and removes that line again when either stops being true. The
  server ignores it when the container publishes the UI beyond loopback (`MELISSA_UI_BIND`): every
  connection through the UI forward arrives as 127.0.0.1, so a LAN client could name any address and never
  fill the per-address limit. Local users on the host can still do that with the setting on (accepted:
  the per-username limit still holds).
- **Tailscale identity headers** are shown with the browser's name in Account → sessions and audited, never
  used to sign in.
- **Apps are never served over HTTPS on the UI hostname.** HSTS would turn their `http://<name>:<port>`
  links into HTTPS, so app links leave out the MagicDNS name while serve is on, and pages loaded through serve
  don't offer the Host they came in on. Apps stay plain HTTP on their ports (Q56).

## Migration

- **Host `tailscale serve` → 127.0.0.1:UI_PORT** keeps working after running install.sh again (it writes
  `[web] trust_forwarded = true` when the host runs Tailscale and `UI_BIND` is loopback). An image update
  alone doesn't run install.sh: until then, or on a Nix/host install, set it by hand (`[web]
  trust_forwarded = true` in server.toml; NixOS: `services.melissa.settings.web.trust_forwarded = true`).
  The main listener accepts `*.ts.net` names only when the proxy says HTTPS.
- **Plain-HTTP LAN access** (`UI_BIND` not loopback) now gets 421 for names Melissa doesn't know, and sign-in
  is refused over plain HTTP. Add the name to `[web] hosts` to browse; sign in through SSH, Tailscale serve
  or (later) phase 2. **Owner accepted (2026-11-28)** that signing in over plain HTTP from another device on
  the LAN is refused; LAN HTTPS (phase 2) is the fix (PLAN Q110 (i)).

## Not done (security review 2026-11-28)

- The serve listener (`proxy_listen`) is bound at every start when Melissa runs Tailscale, not only while
  serve is on. It is loopback only; anything that reaches it (root, the `melissa` uid, an unsandboxed
  agent) can already do more. Binding it on demand needs the listener to follow the setting.
- The main listener with the host's own Tailscale accepts any `*.ts.net` name over HTTPS, not only the
  host's node name: the container can't read the host's tailscaled.

## Unverified (needs a real tailnet)

- The exact CLI output and waiting behaviour when HTTPS is off for the tailnet.
- That tailscaled intercepts :443 for serve before the container's nft `input` chain (which drops all but
  the app ports from `tailscale0`).
- That operator rights suffice for `serve` and `set --hostname`.
- That `tailscale serve` sends `X-Forwarded-Proto: https` and appends to `X-Forwarded-For` (Q87f).
- The certificate is fetched on the first visit; the first page load can take a few seconds.

## Phase 2 sketch (not built)

Root holds a local CA with a name constraint (`<host>.local` and the device's LAN addresses only), so a
leaked CA key can't sign for other names. The server gets a leaf certificate from root and listens with TLS
on 7383. Devices install the CA once (a download page plus its fingerprint shown on screen). Android apps and
some browsers refuse user-installed CAs: the Android app (6b) pins the CA instead. Open: Q22.
