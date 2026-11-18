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
- `kill_on_drop(true)` so cancel/timeout kills the child. It kills only the direct child; grandchildren
  may survive (TODO: process group or a podman sandbox).
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
- Grok: MCP via `grok mcp add` (persistent user config, not per run) — find a per-run option before wiring.
- OpenCode: reads `opencode.json` (with an `mcp` section) from its working directory — next to wire.
- Tool calls wait for confirmations inside the HTTP request; check each CLI's MCP tool-call timeout before
  relying on long waits (Claude: `MCP_TOOL_TIMEOUT` env, unverified).

## Next (ideas, unverified)
- Use each CLI's JSON streaming mode to get typed events (text, tool call, usage) instead of lines.
- Run each CLI in a rootless podman sandbox: `--network=none`, read-only root, `/workspace` only,
  model/tool access through per-agent Unix sockets.
- Check each CLI's terms allow being driven by Melissa (PLAN open question).
