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
- OpenCode: reads `opencode.json` (with an `mcp` section) from its working directory — next to wire.
- Tool calls wait for confirmations inside the HTTP request; check each CLI's MCP tool-call timeout before
  relying on long waits (Claude: `MCP_TOOL_TIMEOUT` env, unverified).

## Sandbox (bubblewrap) — verified 2026-11-18 on the dev laptop (Kinoite 45, SELinux enforcing, bwrap 0.13.1)
Code: `crates/agent/src/sandbox.rs` (pure `command()` builds the argv; `SandboxPlan::resolve` does host lookups).
Every CLI backend runs under `bwrap` by default; the echo backend is in-process and has none.
- Inside: `/usr` and `/etc` read-only, `/bin` etc. as usr-merge symlinks, `/run/systemd/resolve` read-only
  (Fedora's `/etc/resolv.conf` points there), fresh `/proc`, minimal `/dev`, private `/tmp` and `/var/tmp`,
  `$HOME` an empty read-only tmpfs plus the preset's paths, the CLI binary (symlinks resolved, bound read-only,
  executed by its real path), and the task workdir (the only writable host path; `--chdir` into it).
- `--unshare-all --share-net --die-with-parent --new-session --cap-drop ALL --hostname melissa-agent --clearenv`.
  Env: HOME, PATH (`<binary dir>:/usr/bin`), NO_COLOR, TERM, plus USER/LOGNAME/LANG/LC_ALL/TZ if set and the
  names in `sandbox.env`. Host network is shared (model API + queen MCP on 127.0.0.1).
- What each CLI needed (all three verified through the queen; Claude also called `bee_info` and the audit log
  shows `tool.call`):

| Backend  | Binary bound (ro)                                  | Home paths                                                       |
|----------|----------------------------------------------------|------------------------------------------------------------------|
| Claude   | `~/.local/share/claude/versions/<ver>` (via `~/.local/bin/claude`) | tmpfs `~/.claude`; rw `~/.claude/.credentials.json`. Not needed: `~/.claude.json`, sessions, history. |
| Grok     | `~/.grok/downloads/grok-<ver>-linux-x86_64` (via `~/.grok/bin/grok`, static) | tmpfs `~/.grok`; rw `~/.grok/auth.json`; ro `~/.grok/config.toml`. |
| OpenCode | `~/.opencode/bin/opencode`                         | ro `~/.config/opencode`; tmpfs `~/.local/share/opencode`, `~/.local/state/opencode`, `~/.cache/opencode`; rw `~/.local/share/opencode/auth.json` if present (not present on the laptop: free default model). |

- Config: `[backend.sandbox]` with `enabled`, `tmpfs`, `ro`, `rw` (paths relative to `$HOME`, no `..`), `env`
  (names to pass through). Absent table = preset for the command's file name; a given table replaces the preset.
  `sandbox = { enabled = false }` runs the CLI unsandboxed.
- Integration test `crates/agent/tests/sandbox.rs`: fake HOME secret unreadable, writes outside the workdir fail,
  inside succeed, host env not leaked, cancel kills a `setsid` background grandchild. Skips if bwrap is missing.

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

## Next (ideas, unverified)
- Typed events: done for ACP agents (`crate::session`). For Claude (no ACP mode) use
  `--input-format stream-json --output-format stream-json`; OpenCode has `opencode acp` (ACP v1, HTTP MCP).
- Deny-by-default egress: run the sandbox with `--unshare-net` and give it only a Unix socket to an egress proxy
  in the queen that allows the CLI's model API host(s) and forwards MCP. Needs per-CLI proxy support
  (`HTTPS_PROXY`) checks; not built yet.
- seccomp filter (bwrap `--seccomp`) to drop rarely needed syscalls (ptrace, keyctl, mount APIs).
- Melissa-owned CLI logins (e.g. `CLAUDE_CONFIG_DIR` under `~/.local/share/melissa`) instead of binding the
  user's credential files.
- Check each CLI's terms allow being driven by Melissa (PLAN open question).
