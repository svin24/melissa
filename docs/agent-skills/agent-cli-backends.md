# Driving agent CLIs headlessly

**When:** adding or changing an agent backend (`crates/agent/src/cli.rs`, queen config).

## Known invocations (verified 2026-11-18 on the dev laptop, prompt "Reply with exactly one word: pong")
| Backend  | Command                         | Notes |
|----------|---------------------------------|-------|
| Claude   | `claude -p <prompt>`            | ~3.6 s. Plain text on stdout. `--output-format stream-json` exists for structured events. |
| Grok     | `grok -p <prompt>`              | ~4.6 s. `--output-format plain|json|streaming-json|streaming-messages-json`; `-m <model>`. |
| OpenCode | `opencode run <prompt>`         | ~4.2 s. Prints ANSI codes and a header line (`> build · <model>`). `-m provider/model`, `--format json`. Default model was not Gemma; set `-m` in config. |

Config (`~/.config/melissa/queen.toml`, optional):
```toml
[[backend]]
name = "gemma"
command = "opencode"
args = ["run", "-m", "<provider>/<gemma-model>", "{prompt}"]
```

## Rules
- Never run through a shell. Pass the prompt as one argv element (`{prompt}` placeholder).
- Reject prompts that start with `-` (after trim): `opencode run <message...>` would parse
  `--auto` as its auto-approve flag. Clap-based CLIs reject `-`-values after `-p`, but don't rely on it.
- Never pass auto-approve flags (`--always-approve`, `--auto`, `--dangerously-skip-permissions`) from
  config defaults. Tool use goes through Melissa's narrow tool API (planned).
- Set `NO_COLOR=1`, `TERM=dumb`, stdin null, and still strip ANSI escapes from output.
- `kill_on_drop(true)` so cancel/timeout kills the child. Unsandboxed, that is only the direct child;
  under the bwrap sandbox (default) it takes the whole PID namespace, grandchildren included.
- Working directory: `~/.local/state/melissa/work`, never the repo.

## Giving agents Melissa's tools (MCP) — verified with Claude, 2026-11-18
- The queen serves MCP (JSON-RPC over HTTP POST, JSON responses only) at `POST /mcp/<token>` on loopback.
  One session per task; the token is random (128-bit), written into `<workroot>/task-<id>/mcp.json` (mode 0600)
  and closed when the task finishes or is cancelled (old token → 404).
- `CliSpec.mcp_args` are appended only when the task has a config; `{mcp_config}` = path of that file.
- Claude: `claude -p <prompt> --tools "" --mcp-config <file> --strict-mcp-config --allowedTools mcp__melissa`.
  `--tools ""` removes all built-in tools (no Bash/Read); `--strict-mcp-config` ignores the user's own MCP
  servers. Verified: Claude calls `bee_info`, and reports it has no shell or file tools.
  Order matters: `--tools` and `--mcp-config` are variadic, so the prompt goes before them.
- Grok: no per-run MCP option for `grok -p` (only `grok mcp add`, persistent user config). Use the ACP backend
  `grok-acp` instead: it passes Melissa's MCP endpoint per session (verified 2026-11-18, see
  [acp-sessions.md](acp-sessions.md)).
- OpenCode: session backend `opencode-acp` (ACP flavor `opencode`) gets Melissa's MCP server through
  `OPENCODE_CONFIG_CONTENT` with `codemode: false` (verified 2026-11-18, see [acp-sessions.md](acp-sessions.md)).
  The one-shot `opencode run` backend still has no tools.
- Claude as a session: `claude-session` (`kind = "claude"`, stream-json, `--permission-prompt-tool stdio`, no
  `--allowedTools`: every MCP call is a permission prompt Melissa answers). See [acp-sessions.md](acp-sessions.md).
- Tool calls wait for confirmations inside the HTTP request; check each CLI's MCP tool-call timeout before
  relying on long waits (Claude: `claude-session` sets `MCP_TOOL_TIMEOUT=960000`, not measured).

## Sandbox (bubblewrap) — verified 2026-11-18 on the dev laptop (Kinoite 45, SELinux enforcing, bwrap 0.13.1)
Code: `crates/agent/src/sandbox.rs` (pure `command()` builds the argv; `SandboxPlan::resolve` does host lookups).
Every CLI backend runs under `bwrap` by default; the echo backend is in-process and has none.
- Inside: `/usr` and `/etc` read-only, `/bin` etc. as usr-merge symlinks, `/run/systemd/resolve` read-only
  (Fedora's `/etc/resolv.conf` points there), fresh `/proc`, minimal `/dev`, private `/tmp` and `/var/tmp`,
  `$HOME` an empty read-only tmpfs plus the preset's paths, the CLI binary (symlinks resolved, bound read-only,
  executed by its real path), and the task workdir (the only writable host path; `--chdir` into it).
- `--unshare-all --die-with-parent --new-session --cap-drop ALL --hostname melissa-agent --clearenv`.
  Env: HOME, PATH (`<binary dir>:/usr/bin`), NO_COLOR, TERM, plus USER/LOGNAME/LANG/LC_ALL/TZ if set and the
  names in `sandbox.env`.
- **Network (since 2026-11-18): own namespace, deny-by-default.** `melissa-netbridge` runs first and gives the
  CLI an HTTPS proxy on 127.0.0.1:3128 (allowlisted hosts only) and Melissa's MCP endpoint at the queen's own
  address; no resolver inside. Per-backend `network = "proxy"|"learn"|"none"|"host"` and `allow = [...]`.
  Details, verified host lists and how to add hosts: [agent-egress.md](agent-egress.md). (Before: `--share-net`
  and `/run/systemd/resolve` bound, so an agent could reach the LAN, the queen's whole HTTP API and host
  abstract Unix sockets.)
- What each CLI needed (all three verified through the queen; Claude also called `bee_info` and the audit log
  shows `tool.call`):

| Backend  | Binary bound (ro)                                  | Home paths                                                       |
|----------|----------------------------------------------------|------------------------------------------------------------------|
| Claude   | `~/.local/share/claude/versions/<ver>` (via `~/.local/bin/claude`) | tmpfs `~/.claude`; rw `~/.claude/.credentials.json`. Not needed: `~/.claude.json`, sessions, history. |
| Grok     | `~/.grok/downloads/grok-<ver>-linux-x86_64` (via `~/.grok/bin/grok`, static) | tmpfs `~/.grok`; rw `~/.grok/auth.json`; ro `~/.grok/config.toml`. |
| OpenCode | `~/.opencode/bin/opencode`                         | ro `~/.config/opencode/opencode.json{,c}` (since 2026-11-18 not the whole dir: it also holds `service.json`, a local server password, and plugins; tmpfs `~/.config/opencode` around them); tmpfs `~/.local/share/opencode`, `~/.local/state/opencode`, `~/.cache/opencode`; rw `~/.local/share/opencode/auth.json` if present (not present on the laptop: free default model). `opencode-acp`: same without the user's config files (Melissa writes the config). |

- Config: `[backend.sandbox]` with `enabled`, `tmpfs`, `ro`, `rw` (paths relative to `$HOME`, no `..`), `env`
  (names to pass through). Absent table = preset for the command's file name; a given table replaces the preset.
  `sandbox = { enabled = false }` runs the CLI unsandboxed.
- Integration test `crates/agent/tests/sandbox.rs`: fake HOME secret unreadable, writes outside the workdir fail,
  inside succeed, host env not leaked, cancel kills a `setsid` background grandchild. Skips if bwrap is missing.

- Headless box (since 2026-11-18): the CLIs live in the queen user's home `/var/lib/melissa/home`, installed by
  `sudo melissa-setup agent <name>` with the vendors' installers; same paths as above relative to that home.
  OpenCode 1.19.6 (the installer's current release) writes `~/.opencode/.gitignore` at start, so the preset makes
  `~/.opencode` a tmpfs (the binary is still bound read-only inside it).

### Gotchas
- `--remount-ro /` freezes only the root mount. A `$HOME` that is merely a directory inside another mount (e.g.
  under the `/tmp` tmpfs) stays writable. Make `$HOME` its own tmpfs and `--remount-ro` it too (found by the test).
- Without `--tmpfs` for a CLI's state dir, a read-only home fails with `EROFS ... mkdir '~/.local/state'`
  (OpenCode) — CLIs create their state dirs on every start.
- The CLIs ran with `XDG_*` cleared, so they use their default `~/.config` etc. paths even if the queen runs with
  custom XDG dirs; the presets assume those defaults.
- Unverified: OAuth token refresh while sandboxed. The credential file is bind-mounted; a CLI that refreshes by
  write-to-temp + rename gets `EBUSY` and may fail or lose the refresh. Watch for auth errors after token expiry.
- Lock files next to credentials (`~/.grok/auth.json.lock`) are on the sandbox tmpfs, so a sandboxed and an
  interactive grok do not coordinate refreshes.

## What each CLI contacts (verified 2026-11-18, dev laptop; full table in [agent-egress.md](agent-egress.md))
- Claude 2.1.327: `api.anthropic.com` (model, also its own event/metrics endpoints, so not separable by host),
  OAuth refresh at `platform.claude.com/v1/oauth/token` (from the binary). Telemetry `http-intake.logs.us5.
  datadoghq.com` denied without effect. Opens a new CONNECT per HTTP request (~25 per short prompt).
- Grok 1.0.89: `cli-chat-proxy.grok.com` (the chat model with a subscription login), `api.x.ai` (ACP mode),
  login issuer `auth.x.ai` (key in `auth.json`). Denied without effect: `grok.com`, `api.mixpanel.com`; both
  are retried ~40 times per prompt (collapsed in the audit).
- OpenCode 2.3.2 (free model `big-pickle`): `opencode.ai`, `models.opencode.ai` (catalogue; denied: still
  works, ~3 s slower).
- All three honour `HTTPS_PROXY`; none needed DNS inside the sandbox.

## Session backends (verified 2026-11-18 through the queen, sandboxed, `network = "proxy"`)
| Backend | Command | bee_info task | Queen ask + worker | Cancel | Resume |
|---|---|---|---|---|---|
| `grok-acp` | `grok --permission-mode default agent --no-leader stdio` | ~11 s | 23 s | yes | yes |
| `claude-session` | `claude -p --input-format stream-json … --tools "" --permission-prompt-tool stdio` | 3.1 s | 11.8 s | yes (interrupt) | yes (`--resume`) |
| `opencode-acp` | `opencode acp` + Melissa's config in env | 9.2 s | 23.6 s | yes | yes |
Queen default order: `grok-acp`, `claude-session`, `opencode-acp` (`[queen] backends`).
`kind = "claude"` config: `name`, `command`, `args` (only `--model`, `--fallback-model`, `--effort` + value),
`tools`, `sandbox`, `network`, `allow`, `record_dir`, `session_store` (default `.claude/projects`). Env set by
Melissa: `MCP_TOOL_TIMEOUT=960000`, `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`, `DISABLE_AUTOUPDATER=1`.
ACP opencode: `flavor = "opencode"`, optional `model = "provider/model"`.

## Next (ideas, unverified)
- Typed events: done for session backends (`crate::session`: ACP and Claude stream-json).
- seccomp filter (bwrap `--seccomp`) to drop rarely needed syscalls (ptrace, keyctl, mount APIs).
- Melissa-owned CLI logins (e.g. `CLAUDE_CONFIG_DIR` under `~/.local/share/melissa`) instead of binding the
  user's credential files.
- Check each CLI's terms allow being driven by Melissa (PLAN open question).
