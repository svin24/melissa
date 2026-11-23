# Sandbox escape tests

The adversarial escape suite: real escape attempts, run inside the real agent sandbox, each of which must fail.
It is the **gate for image updates** (docs/PLAN.md, "Escape suite"): an image, bwrap or kernel change ships only
when `just escape-test` passes with no failures.

## Running it

```sh
devenv shell -- just escape-test
```

It prints a table (probe, result, detail) and a count line, and fails on any `FAIL`. `just escape-test` sets
`MELISSA_ESCAPE_REQUIRED=1`, so a host without bwrap or unprivileged user namespaces **fails**; under plain
`just check` / `cargo nextest run` the suite skips there like the other sandbox tests. Takes about 3 seconds
(about 8 with the resource-limit probes).

Results:

- `pass`: the attempt was blocked (or a control worked).
- `FAIL`: an escape (`ESCAPED: ...`), a broken control (`broken: ...`), a host-side check that saw the effect,
  or no verdict at all (the script crashed, the sandbox did not start, it timed out). **Never** weaken a probe
  to make a `FAIL` go away: fix the sandbox, or record it in PLAN's Open questions and tell the owner.
- `skip`: the probe cannot run here (`tool missing: python3`, no LAN address), or it is kernel surface that only
  seccomp closes and the argv has no `--seccomp` (e.g. a backend with `seccomp = false`; the detail says what
  got through), or it is a `limits.*` probe and the sandbox has no cgroup (`resource limits off: ...`).

Resource limits: `just escape-test` runs the suite inside `systemd-run --user --scope -p Delegate=yes` when
systemd's user manager is there; the shell moves itself into a `runner` leaf and passes the scope's cgroup in
`MELISSA_TEST_CGROUP`, which the suite hands to `sandbox::limits::init_at`. Every suite sandbox then gets small
limits (`suite_limits()`: 256 MiB, 128 processes, 0.5 CPUs) and the header says `resource limits: on`. Elsewhere
(a container build, plain `cargo nextest run`) the `limits.*` probes skip and never run: one is a fork bomb.

## How it works

- `crates/agent/tests/escape.rs` is the runner; `crates/agent/tests/escape/probes.rs` is the data.
- One sandbox per probe, built the way agents get theirs: an `EgressProxy` lease (proxy with web read on, DNS
  sinkhole, MCP through the bridge), `SandboxPlan::resolve` from a `SandboxSpec` with `network = proxy`,
  `with_rw_dirs` (the agent's own file box), `with_env`, `with_bridge`, then `sandbox::spawn_command()`: the one
  spawn path the backends use too (through `cli::sandboxed_command`): bwrap argv, the seccomp program at fd 3
  (`--seccomp 3`), the cgroup gate at fd 4 (`--block-fd 4`, when limits are on), empty environment. `proc.die-with-parent` needs an intermediate parent it can kill, so it
  builds the same argv, fd and empty environment around an `sh`. Change how agents spawn the sandbox only in
  `spawn_command`, or the suite tests something agents do not run.
- The scene (`scene()`): a host home with a canary, Melissa's state dir in it (`permissions.json`,
  `files/boxes/<a1|a2>`, `files/index/<a2>.json`), an unbound "outside" dir, a host process with a marked argv,
  HTTP servers on the host's loopback and LAN address, a host abstract Unix socket. Every canary file holds a
  random token (`$ESC_TOKEN`). The scene lives under cargo's `CARGO_TARGET_TMPDIR`, **not** `/tmp`: the
  sandbox has its own `/tmp` tmpfs, so a path under the host's `/tmp` would be writable inside and the
  in-sandbox verdict would mislead (host checks still catch it). Sockets stay under `/tmp` (path length).
- Seccomp detection: the argv contains `--seccomp` or `--add-seccomp-fd`. The table header says which.

## Adding a probe

One entry in `PROBES`, a few lines:

```rust
Probe::new(
    "fs.write-boot",                       // area.what, unique
    "create a file in /boot",              // what it tries
    "blocked (not bound)",                 // expected outcome
    r#"touch /boot/x 2>/dev/null && escaped "created /boot/x"
       blocked "not writable""#,
)
.host(|_| absent("/boot/x")),             // optional: check the effect from the host
```

- The script is POSIX `sh` after a preamble with these helpers: `escaped "what"`, `blocked "why"`, `works`,
  `broken` (controls), `skip "why"`, `need tool...` (skips with `tool missing: tool`), `leaked file` (true if
  the file's content is the canary token), `direct url` (curl around the proxy), `viaproxy args` (curl through the
  proxy, prints the CONNECT code), `proxy_verdict code label`. Each verdict word exits the script; the first
  verdict line counts.
- The variables listed at the top of `probes.rs` (`$ESC_OUTSIDE`, `$ESC_STATE`, `$ESC_OTHER_BOX`, ...) point at
  the scene. Add a new one in `Ctx::vars()`.
- Use only tools inside the sandbox: `/usr` is the host's (or the image's). Prefer sh + coreutils; `curl`,
  `getent`, `util-linux` (unshare, mount) are in the image; **python3 is not** (it is on Fedora hosts), so
  ctypes-based syscall probes (`py_prelude!()`) skip in the image. Never let a missing tool pass: use `need`.
- Kernel surface that only seccomp closes: add `.seccomp()`. If it is blocked anyway it passes; if it
  gets through while the argv has no `--seccomp` it is a skip; with `--seccomp` (the default) it is a failure.
- Resource limits: add `.limits(|events| ...)`. The probe runs only when the sandbox has a cgroup; afterwards
  the check gets the cgroup's counters (`LimitEvents`: `oom_kills`, `memory_max`, `pids_max`, `cpu_throttled`)
  and decides, since a script's own verdict could be a coincidence. Keep such scripts bounded anyway (a loop
  count, `timeout`). The OOM killer may take more than one process: put the hog in a subshell with
  `oom_score_adj` 1000 so the verdict-printing shell survives.
- Something that would only look blocked because nothing works (dead proxy, read-only workdir) needs a control:
  the `control.*` probes run first.
- Never print secrets: report names, counts or paths, not values (see `proc.environ-leak`).
- Avoid false positives from the sandbox's own state: `/proc/1/cmdline` (bwrap) contains the whole script and
  every `--setenv` value, so grep patterns like `'melissa-escape-canar[y]'` (the brackets keep the pattern from
  matching itself).

## Findings so far

- 2026-11-23, fixed: bwrap's **own** environment (inherited from `melissa-server`) was readable inside through
  `/proc/1/environ` (bwrap is PID 1 in the sandbox; `--clearenv` only clears the child's). 201 host variables
  in a dev-shell run. Fix: `sandbox::spawn_command()` spawns bwrap with an empty environment.
- 2026-11-23, closed by seccomp: the sandbox keeps the launcher's **session keyring** (before the filter, a key
  added outside was readable inside, and the sandbox could link new keys into it); nested user namespaces
  (`unshare -U`, then `mount`), `ptrace` of its own children, `perf_event_open` on itself. All refused with
  `EPERM` now. Without seccomp (`seccomp = false`) they are open again.
