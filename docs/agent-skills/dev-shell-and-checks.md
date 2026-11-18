# Dev shell and checks

**When:** any Rust work in this repo; before every commit.

## Steps
- Enter the shell: `direnv allow` (once) or prefix commands: `devenv shell -- <cmd>`.
- Full gate: `devenv shell -- just check` (fmt, clippy `-D warnings`, nextest, `just licenses`).
- `just licenses`: cargo-about report into `target/THIRD-PARTY-LICENSES.md`; fails on a crate license not in the
  `about.toml` allowlist. Network only for `cargo fetch` (first run); about 10 s wall, but ~2.5 CPU-min of text
  matching. Adding a license to the allowlist needs review and a note in ACKNOWLEDGMENTS.md.
- Single crate: `devenv shell -- cargo nextest run -p melissa-agent`.
- Commit from inside the shell: `devenv shell -- git commit -F msg.txt`.

## Gotchas (verified 2026-11-18)
- The host (Kinoite, immutable) has no `cargo` and no `cc`. The pre-commit clippy hook fails with
  ``linker `cc` not found`` if `git commit` runs outside the dev shell. Always commit inside it.
- `cargo nextest run` exits 4 when there are zero tests ("no tests to run").
- Workspace lints warn on `unwrap`/`expect`; `clippy.toml` allows them in tests only. Non-test code
  must handle errors (poisoned mutex: `lock().unwrap_or_else(|p| p.into_inner())`).
- tokio features are set once in the root `[workspace.dependencies]`; `tokio::fs` needs `"fs"`.
- `pkill -f <pattern>` inside a Bash tool call can match the calling shell's own command line and kill
  it (exit 144). Use `pkill -x melissa-queen` or a pidfile.
- Long first builds inside a tool call may be cut off; run them with `run_in_background`.
- `/tmp` (including the agent scratchpad) is wiped on reboot. Keep anything durable in the repo or in
  `~/.local/share/melissa`.
- sudo from an agent: the owner provides the password via a secret file; use `sudo -S -p '' <cmd> < "$file"`.
  Never pipe data into a `sudo -S` command (the password and data share stdin and sudo counts failed attempts;
  this happened once on 2026-11-18). Write data to a file first (`podman save -o` then `sudo podman load -i`).
  Check `faillock --user <user>` if sudo starts refusing.
