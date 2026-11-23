# Sandbox escape tests

The adversarial escape suite: real escape attempts, run inside the real agent sandbox, each of which must fail.
It is the **gate for image updates** (docs/PLAN.md, "Escape suite"): an image, bwrap or kernel change ships only
when the suite passes with no failures, on the dev host and inside the shipped artefact.

## Running it

```sh
devenv shell -- just escape-test          # on this host (about 3 s)
devenv shell -- just escape-test-image    # inside the built image (localhost/melissa:dev), needs sudo
nix build .#checks.x86_64-linux.melissa-nixos -L   # on NixOS, in the VM test (KVM, about 10 min)
```

It prints a table (probe, result, detail) and a count line, and fails on any `FAIL`. `just escape-test` sets
`MELISSA_ESCAPE_REQUIRED=1`, so a host without bwrap, without unprivileged user namespaces or without the probe
helper **fails**; under plain `just check` / `cargo nextest run` the suite skips there like the other sandbox
tests. `just escape-test` also runs `seccomp_off_backend_is_reported_not_passed` (below).

- **Image** (`packaging/container/escape-test.sh`): builds the Containerfile's `escape-build` stage (the test
  binary and `melissa-escape-probe`, same builder and sources as the image's binaries), adds them under
  `/opt/melissa-escape` to a throwaway copy of the image (`Containerfile.escape-test`, k3s and the server masked),
  boots it rootful with `--privileged --systemd=always` like the Quadlet, and runs the suite as `melissa` in a
  transient unit with the server unit's hardening. The image's own `bwrap`, `melissa-netbridge` and `/usr` are
  what the probes face. Log: `target/escape-test-image.log`. A pass tags the image
  `localhost/melissa:escape-tested` only when the image's revision label is the commit the suite was built
  from (and `crates/` has no local changes); any run on that image first removes the tag. Publish only that tag.
- **NixOS**: the VM test (`packaging/nix/tests/vm.nix`) runs `melissa-it-escape` as `melissa` with
  `MELISSA_ESCAPE_REQUIRED=1`; the helper comes from `melissa-integration-tests` (`bin/melissa-escape-probe`).

Results:

- `pass`: the attempt was blocked (or a control worked).
- `FAIL`: an escape (`ESCAPED: ...`), a broken control (`broken: ...`), a host-side check that saw the effect,
  or no verdict at all (the script crashed, the sandbox did not start, it timed out). **Never** weaken a probe
  to make a `FAIL` go away: fix the sandbox, or record it in PLAN's Open questions and tell the owner.
- `skip`: the probe cannot run here (`tool missing: socat`, no LAN address), or it is kernel surface that only
  seccomp closes and the argv has no `--seccomp` (a backend with `seccomp = false`, or an architecture without
  a filter). Those kernel probes are **always** skipped then, blocked or not: the detail starts with
  `seccomp off: not judged` and says what the probe saw (`without the filter: ESCAPED: ...`). They never pass
  without the filter.

## How it works

- `crates/agent/tests/escape.rs` is the runner; `crates/agent/tests/escape/probes.rs` is the data.
- One sandbox per probe, built the way agents get theirs: an `EgressProxy` lease (proxy with web read on, DNS
  sinkhole, MCP through the bridge), `SandboxPlan::resolve` from a `SandboxSpec` with `network = proxy`,
  `with_rw_dirs` (the agent's own file box), `with_env`, `with_bridge`, then `sandbox::spawn_command()`: the one
  spawn path the backends use too (through `cli::sandboxed_command`): bwrap argv, the seccomp program at fd 3
  (`--seccomp 3`), empty environment. `proc.die-with-parent` needs an intermediate parent it can kill, so it
  builds the same argv, fd and empty environment around an `sh`. Change how agents spawn the sandbox only in
  `spawn_command`, or the suite tests something agents do not run.
- The scene (`scene()`): a host home with a canary, Melissa's state dir in it (`permissions.json`,
  `files/boxes/<a1|a2>`, `files/index/<a2>.json`), an unbound "outside" dir, a host process with a marked argv,
  HTTP servers on the host's loopback and LAN address, a host abstract Unix socket. Every canary file holds a
  random token (`$ESC_TOKEN`). The scene lives under cargo's `CARGO_TARGET_TMPDIR`, **not** `/tmp`: the
  sandbox has its own `/tmp` tmpfs, so a path under the host's `/tmp` would be writable inside and the
  in-sandbox verdict would mislead (host checks still catch it). Sockets stay under `/tmp` (path length).
- Seccomp detection: the argv contains `--seccomp` or `--add-seccomp-fd`. The table header says
  `seccomp: on` or `seccomp: OFF ... kernel probes are skipped, not passed`.
- **Probe helper** (`crates/escape-probe`, binary `melissa-escape-probe`): the kernel probes call
  `helper <probe>` (`ptrace`, `keyctl`, `bpf`, `perf-event-open`, `abstract-connect <name>`), and the helper prints
  the verdict line itself. It uses safe crates only (`nix` for ptrace, `linux-keyutils`, `perf-event2`, `aya`
  for the bpf map; `std` for abstract sockets), so no python3 anywhere. It is its own workspace with its own
  `Cargo.lock`: never in the main lockfile, the license report, the image or the Nix package. `just test` and
  `just escape-test` build it (`just escape-probe`, to `target/debug/`); the suite finds it there or at
  `MELISSA_ESCAPE_PROBE`, and binds it read-only into each probe's sandbox (`spawn_command`'s `extra_ro`). Missing
  helper: a skip normally, a failure with `MELISSA_ESCAPE_REQUIRED=1`.
- `seccomp_off_backend_is_reported_not_passed` runs every `.seccomp()` probe with `SandboxSpec { seccomp: false }`
  and checks they all come back as skips with the reason. The server's startup summary prints
  `Seccomp:  OFF for <backend> (sandbox.seccomp = false)` and logs a warning for each such backend.

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
  `getent`, `util-linux` (unshare, mount) are in the image; **python3 is not**, so do not use it. A raw syscall
  or socket call goes into the probe helper: a function in `crates/escape-probe/src/main.rs` printing a
  `Verdict`, a name in its `PROBES` list, and `helper <name>` in the script. Safe crates only (the helper keeps
  `#![forbid(unsafe_code)]`). Never let a missing tool pass: use `need`.
- Kernel surface that only seccomp closes: add `.seccomp()`. With `--seccomp` (the default) blocked passes and
  an escape fails; without it the probe is a skip whatever it saw.
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
- 2026-11-23, probe fix (not an escape): the first NixOS VM run failed `fs.chown-root` as "broken": it copied
  `/usr/bin/id`, which NixOS does not have. It now copies `$(command -v id)`. Prefer tools from `PATH` over
  `/usr/bin` paths in probes.
