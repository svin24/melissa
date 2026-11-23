//! The escape probes: one entry per attempt. See docs/agent-skills/sandbox-escape-tests.md.
//!
//! Each probe is a POSIX `sh` script run as the sandboxed "agent" (bwrap, egress namespace,
//! the same `SandboxPlan`/`command()` path agents use). It ends with exactly one verdict:
//!
//! - `blocked "why"`: the attempt failed (pass);
//! - `escaped "what"`: the attempt worked (fail);
//! - `works "what"` / `broken "why"`: for controls, which prove a probe class is not vacuous
//!   (e.g. the proxy really answers, the workdir really is writable);
//! - `skip "why"`: the probe cannot run here (`need tool` prints `skip "tool missing: tool"`).
//!
//! A script that prints no verdict (crashed, sandbox did not start) fails the suite.
//!
//! Inside the sandbox the scripts get these variables (see `Ctx::vars` in escape.rs):
//! `ESC_TOKEN` (the content of every host canary file), `ESC_HOME_CANARY` (a file in the
//! host home, which the sandbox's empty home hides), `ESC_OUTSIDE` (an unbound host directory
//! with `canary.txt`), `ESC_STATE` (Melissa's state dir in the host home: `permissions.json`,
//! `files/`), `ESC_OWN_BOX` (this agent's file box, bound), `ESC_OTHER_BOX` / `ESC_OTHER_INDEX`
//! (another agent's box and its index), `ESC_BOXES` (the boxes' parent), `ESC_HOST_PID` (a
//! host process whose argv ends in `melissa-escape-canary-<pid>`), `ESC_PORT` (an HTTP server
//! on the host's loopback and, as `ESC_LAN`, on its LAN address if it has one), `ESC_ABSTRACT`
//! (a host abstract Unix socket), `ESC_MCP` / `ESC_MCP_TOKEN` (Melissa's MCP endpoint through
//! the bridge and this sandbox's token), `ESC_TMP_NAME` (a file name checked in the host's /tmp).

use super::{Kind, Probe};

/// The probes, in report order. Adding one: a `Probe::new(name, tries, expect, script)`
/// entry (plus `.seccomp()` for kernel surface only seccomp will close, `.host(check)` for a
/// host-side check of the effect). Keep scripts POSIX sh + coreutils where possible.
pub const PROBES: &[Probe] = &[
    // ---- Controls: prove the probe classes below are not vacuous. ----
    Probe::new(
        "control.workdir-writable",
        "write a file in the task workdir",
        "works (the workdir is the agent's)",
        r#"echo ok > ./control.txt && [ "$(cat ./control.txt)" = ok ] && works "workdir writable"
           broken "cannot write the workdir""#,
    ),
    Probe::new(
        "control.own-box-writable",
        "write a file in the agent's own file box",
        "works",
        r#"echo ok > "$ESC_OWN_BOX/control.txt" && works "own file box writable"
           broken "cannot write the own file box""#,
    ),
    Probe::new(
        "control.proxy-allowlisted",
        "fetch an allowlisted host through the egress proxy",
        "works (the proxy path is live)",
        r#"need curl
           body=$(curl -sS --max-time 5 -x "$HTTPS_PROXY" --noproxy '' -p "http://allowed.test.example:$ESC_PORT/" 2>&1)
           [ "$body" = allowed-body ] && works "allowlisted host reached through the proxy"
           broken "proxy path not working: $body""#,
    ),
    Probe::new(
        "control.mcp-own-token",
        "list Melissa's tools with this sandbox's MCP token",
        "works",
        r#"need curl
           out=$(curl -sS --max-time 5 -X POST -H 'Content-Type: application/json' \
             -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' "$ESC_MCP/mcp/$ESC_MCP_TOKEN" 2>&1)
           case "$out" in *device_info*) works "MCP answers through the bridge";; esac
           broken "MCP not reachable: $out""#,
    ),
    // ---- Filesystem: writes ----
    Probe::new(
        "fs.write-usr",
        "create a file in /usr",
        "blocked (read-only bind)",
        r#"touch /usr/.melissa-escape-probe 2>/dev/null && escaped "created /usr/.melissa-escape-probe"
           blocked "/usr is read-only""#,
    )
    .host(|_| absent("/usr/.melissa-escape-probe")),
    Probe::new(
        "fs.write-etc",
        "create a file in /etc",
        "blocked (read-only bind)",
        r#"touch /etc/.melissa-escape-probe 2>/dev/null && escaped "created /etc/.melissa-escape-probe"
           blocked "/etc is read-only""#,
    )
    .host(|_| absent("/etc/.melissa-escape-probe")),
    Probe::new(
        "fs.write-home",
        "create a file in $HOME (the host user's home path)",
        "blocked (empty read-only tmpfs)",
        r#"touch "$HOME/.melissa-escape-probe" 2>/dev/null && escaped "created a file in \$HOME"
           blocked "\$HOME is read-only""#,
    )
    .host(|c| absent(c.home.join(".melissa-escape-probe"))),
    Probe::new(
        "fs.write-outside-workdir",
        "create a file in a host directory outside the workdir",
        "blocked (not bound; root is read-only)",
        r#"mkdir -p "$ESC_OUTSIDE" 2>/dev/null; echo x > "$ESC_OUTSIDE/escaped.txt" 2>/dev/null
           [ -f "$ESC_OUTSIDE/escaped.txt" ] && escaped "wrote $ESC_OUTSIDE/escaped.txt (check the host)"
           blocked "path not writable""#,
    )
    .host(|c| absent(c.outside.join("escaped.txt"))),
    Probe::new(
        "fs.write-host-tmp",
        "write the host's /tmp",
        "blocked (private tmpfs; nothing reaches the host)",
        r#"echo x > "/tmp/$ESC_TMP_NAME" 2>/dev/null; blocked "wrote only the sandbox's own /tmp""#,
    )
    .host(|c| absent(std::path::Path::new("/tmp").join(&c.tmp_name))),
    Probe::new(
        "fs.write-state",
        "overwrite Melissa's permissions.json",
        "blocked (the state dir is not in the sandbox)",
        r#"echo '{"bypass":true}' > "$ESC_STATE/permissions.json" 2>/dev/null \
             && escaped "wrote permissions.json"
           blocked "permissions.json not writable""#,
    )
    .host(|c| unchanged(c.state.join("permissions.json"), &c.token)),
    Probe::new(
        "fs.write-other-box",
        "plant a file in another agent's file box or beside the own box",
        "blocked",
        r#"echo x > "$ESC_OTHER_BOX/planted.txt" 2>/dev/null
           echo x > "$ESC_BOXES/planted.txt" 2>/dev/null
           blocked "writes did not reach the host (checked there)""#,
    )
    .host(|c| {
        absent(c.other_box.join("planted.txt"))?;
        absent(c.other_box.parent().unwrap_or(&c.other_box).join("planted.txt"))
    }),
    // ---- Filesystem: reads ----
    Probe::new(
        "fs.read-host-home",
        "read a canary file in the host user's home",
        "blocked (home is an empty tmpfs)",
        r#"leaked "$ESC_HOME_CANARY" && escaped "read the home canary"
           ls -A "$HOME" 2>/dev/null | grep -q secret-canary && escaped "home canary name visible"
           blocked "home canary not visible""#,
    ),
    Probe::new(
        "fs.read-outside",
        "read a canary in an unbound host directory",
        "blocked (not bound)",
        r#"leaked "$ESC_OUTSIDE/canary.txt" && escaped "read the outside canary"
           blocked "not visible""#,
    ),
    Probe::new(
        "fs.read-permissions",
        "read Melissa's permissions.json (standing approvals)",
        "blocked",
        r#"leaked "$ESC_STATE/permissions.json" && escaped "read permissions.json"
           blocked "not visible""#,
    ),
    Probe::new(
        "fs.read-files-index",
        "read another agent's file box index",
        "blocked",
        r#"leaked "$ESC_OTHER_INDEX" && escaped "read the other agent's index"
           ls "$ESC_STATE/files/index" 2>/dev/null | grep -q . && escaped "index names visible"
           blocked "not visible""#,
    ),
    Probe::new(
        "fs.read-other-box",
        "read another agent's file box, or see its name",
        "blocked (only the own box is bound)",
        r#"leaked "$ESC_OTHER_BOX/secret.txt" && escaped "read the other agent's file"
           for n in $(ls -A "$ESC_BOXES" 2>/dev/null); do
             [ "$ESC_BOXES/$n" = "$ESC_OWN_BOX" ] || escaped "other box name visible: $n"
           done
           blocked "only the own box is visible""#,
    ),
    Probe::new(
        "fs.symlink-out",
        "follow symlinks and .. out of the workdir to host canaries",
        "blocked (targets are not in the sandbox's mount namespace)",
        r#"ln -s "$ESC_HOME_CANARY" l1; ln -s "$ESC_OUTSIDE/canary.txt" l2; ln -s / l3
           for f in l1 l2 "l3$ESC_OUTSIDE/canary.txt" "$PWD/../../outside/canary.txt"; do
             leaked "$f" && escaped "read a host canary via $f"
           done
           blocked "no symlink or .. path leads out""#,
    ),
    Probe::new(
        "fs.proc-root",
        "reach host files through /proc/<pid>/root and /cwd",
        "blocked (every visible process is in the sandbox)",
        r#"for p in /proc/[0-9]*; do
             for f in "$p/root$ESC_OUTSIDE/canary.txt" "$p/root$ESC_HOME_CANARY" \
                      "$p/cwd/../../outside/canary.txt"; do
               leaked "$f" && escaped "read a host canary via $f"
             done
           done
           blocked "no /proc path leads out""#,
    ),
    Probe::new(
        "fs.chown-root",
        "make a root-owned setuid file in the workdir",
        "blocked (host root is not mapped)",
        r#"cp /usr/bin/id ./suid 2>/dev/null || broken "cannot copy a binary into the workdir"
           chown 0:0 ./suid 2>/dev/null && chmod 4755 ./suid 2>/dev/null \
             && escaped "created a root-owned setuid file"
           blocked "chown to root refused""#,
    )
    .host(|c| not_root_owned(c.work_of("fs.chown-root").join("suid"))),
    Probe::new(
        "fs.nosuid-workdir",
        "check that setuid bits on the writable workdir are ignored",
        "blocked (workdir mounted nosuid)",
        r#"line=$(grep -F " $PWD " /proc/self/mountinfo | tail -n1)
           [ -n "$line" ] || broken "workdir mount not found"
           case "$line" in *nosuid*) blocked "workdir is nosuid";; esac
           escaped "workdir is not mounted nosuid""#,
    ),
    // ---- Processes ----
    Probe::new(
        "proc.host-processes",
        "list host processes in /proc",
        "blocked (own PID namespace: only the sandbox's processes)",
        r#"n=0
           for p in /proc/[0-9]*; do
             n=$((n + 1))
             tr '\0' ' ' < "$p/cmdline" 2>/dev/null | grep -q 'melissa-escape-canar[y]' \
               && escaped "host canary process visible as ${p#/proc/}"
           done
           [ "$n" -gt 10 ] && escaped "$n processes visible"
           blocked "$n processes, all the sandbox's own""#,
    ),
    Probe::new(
        "proc.signal-host",
        "SIGKILL a host process by its host PID",
        "blocked (PID not in the sandbox's namespace)",
        r#"kill -KILL "$ESC_HOST_PID" 2>/dev/null && escaped "signal to PID $ESC_HOST_PID delivered"
           blocked "no such process here""#,
    )
    .host(|c| alive(c.canary_pid)),
    Probe::new(
        "proc.environ-leak",
        "read the launcher's environment through /proc/*/environ (e.g. bwrap as PID 1)",
        "blocked (only the sandbox's own variables)",
        r#"own=/tmp/esc-own; all=/tmp/esc-all
           { tr '\0' '\n' < /proc/$$/environ; echo NO_COLOR=1; echo TERM=dumb; } | sort -u > "$own"
           for p in /proc/[0-9]*; do tr '\0' '\n' < "$p/environ" 2>/dev/null; done | sort -u > "$all"
           # Report names only (never values: they may be secrets, and multi-line ones
           # split into several lines here).
           new=$(comm -23 "$all" "$own" | grep -avE '^(PWD|OLDPWD|SHLVL|_)=')
           [ -n "$new" ] || blocked "no variables beyond the sandbox's own"
           names=$(printf '%s\n' "$new" | grep -aoE '^[A-Za-z_][A-Za-z0-9_]*=' | tr -d = | sort -u)
           n=$(printf '%s\n' "$names" | grep -c .)
           shown=$(printf '%s\n' "$names" | head -n 8 | tr '\n' ' ')
           [ "$n" -gt 0 ] && escaped "$n host variables readable, e.g. $shown"
           escaped "unexpected environment entries readable""#,
    ),
    Probe::new(
        "proc.die-with-parent",
        "keep a detached grandchild running after the sandbox's parent is killed",
        "blocked (--die-with-parent + PID namespace kill everything)",
        r#"setsid sh -c 'while :; do date +%s%N > beat; sleep 0.05; done' </dev/null >/dev/null 2>&1 &
           sleep 600"#,
    )
    .kind(Kind::ParentDies),
    Probe::new(
        "proc.hostname",
        "read the host's name",
        "blocked (own UTS namespace, fixed name)",
        r#"[ "$(cat /proc/sys/kernel/hostname)" = melissa-agent ] && blocked "sees only melissa-agent"
           escaped "sees a host name other than melissa-agent""#,
    ),
    Probe::new(
        "proc.sysrq",
        "write /proc/sysrq-trigger",
        "blocked",
        r#"printf h > /proc/sysrq-trigger 2>/dev/null && escaped "wrote sysrq-trigger"
           blocked "refused""#,
    ),
    // ---- Privileges ----
    Probe::new(
        "priv.capabilities",
        "hold any capability (CapInh/Prm/Eff/Bnd/Amb)",
        "blocked (all sets 0)",
        r#"bad=$(grep -E '^Cap(Inh|Prm|Eff|Bnd|Amb):' /proc/self/status | grep -v '0000000000000000$' | tr '\t\n' ' ;')
           [ -n "$bad" ] && escaped "capabilities: $bad"
           blocked "all capability sets empty""#,
    ),
    Probe::new(
        "priv.no-new-privs",
        "run without no_new_privs",
        "blocked (NoNewPrivs: 1)",
        r#"grep -qE '^NoNewPrivs:[[:space:]]+1$' /proc/self/status && blocked "NoNewPrivs is 1"
           escaped "NoNewPrivs is not set""#,
    ),
    Probe::new(
        "priv.not-root",
        "run as uid 0",
        "blocked (the host user's uid)",
        r#"[ "$(id -u)" = 0 ] && escaped "runs as uid 0"
           blocked "uid $(id -u)""#,
    ),
    Probe::new(
        "priv.setuid-binary",
        "gain root through a setuid binary (sudo -n)",
        "blocked (no_new_privs; root unmapped)",
        r#"need sudo
           [ "$(sudo -n id -u 2>/dev/null)" = 0 ] && escaped "sudo gave uid 0"
           blocked "sudo refused""#,
    ),
    // ---- Network ----
    Probe::new(
        "net.interfaces-routes",
        "find a network interface or route besides loopback",
        "blocked (own netns: lo only, no routes)",
        r#"ifs=$(tail -n +3 /proc/net/dev | cut -d: -f1 | tr -d ' ' | grep -vx lo | tr '\n' ' ')
           [ -n "$ifs" ] && escaped "interfaces besides lo: $ifs"
           n=$(tail -n +2 /proc/net/route | grep -c .)
           [ "$n" -gt 0 ] && escaped "$n IPv4 routes"
           n6=$(grep -vc ' lo$' /proc/net/ipv6_route 2>/dev/null)
           [ "${n6:-0}" -gt 0 ] && escaped "$n6 IPv6 routes off loopback"
           blocked "lo only, no routes""#,
    ),
    Probe::new(
        "net.direct-host-loopback",
        "connect straight to a server on the host's loopback",
        "blocked (sandbox loopback is its own)",
        r#"need curl
           if direct "http://127.0.0.1:$ESC_PORT/"; then escaped "reached the host loopback server"
           else blocked "curl exit $?"; fi"#,
    ),
    Probe::new(
        "net.direct-host-lan",
        "connect straight to a server on the host's LAN address",
        "blocked (no route)",
        r#"need curl
           [ -n "$ESC_LAN" ] || skip "host has no LAN address"
           if direct "http://$ESC_LAN:$ESC_PORT/"; then escaped "reached the host LAN server"
           else blocked "curl exit $?"; fi"#,
    ),
    Probe::new(
        "net.direct-public",
        "connect straight to a public address (203.0.113.1:80)",
        "blocked (no route)",
        r#"need curl
           if direct "http://203.0.113.1/"; then escaped "reached 203.0.113.1"
           else blocked "curl exit $?"; fi"#,
    ),
    Probe::new(
        "net.dns-leak",
        "resolve an arbitrary name through the system resolver",
        "blocked (sinkhole placeholder only, nothing forwarded)",
        r#"need getent
           ans=$(getent hosts melissa-escape-probe.invalid 2>/dev/null | head -n1 | cut -d' ' -f1)
           [ -z "$ans" ] && blocked "no answer"
           [ "$ans" = 192.88.99.53 ] && blocked "sinkhole placeholder only"
           escaped "resolved to a real answer: $ans""#,
    ),
    Probe::new(
        "net.proxy-denied-host",
        "CONNECT to a host not on the allowlist through the proxy",
        "blocked (403)",
        r#"need curl
           code=$(viaproxy -p "http://denied.test.example:$ESC_PORT/")
           proxy_verdict "$code" denied.test.example"#,
    ),
    Probe::new(
        "net.proxy-host-loopback",
        "CONNECT to the host's loopback (127.0.0.1:443) through the proxy",
        "blocked (403: non-public address)",
        r#"need curl
           proxy_verdict "$(viaproxy -p https://127.0.0.1/)" 127.0.0.1:443"#,
    ),
    Probe::new(
        "net.proxy-localhost-name",
        "CONNECT to localhost:443 through the proxy",
        "blocked (403: local name)",
        r#"need curl
           proxy_verdict "$(viaproxy -p https://localhost/)" localhost:443"#,
    ),
    Probe::new(
        "net.proxy-host-lan",
        "CONNECT to the host's LAN address on 443 through the proxy",
        "blocked (403: non-public address)",
        r#"need curl
           [ -n "$ESC_LAN" ] || skip "host has no LAN address"
           proxy_verdict "$(viaproxy -p "https://$ESC_LAN/")" "LAN:443""#,
    ),
    Probe::new(
        "net.proxy-get-loopback",
        "plain HTTP GET of 127.0.0.1:80 through the proxy (web read)",
        "blocked (403)",
        r#"need curl
           code=$(curl -sS -o /dev/null --max-time 5 -x "$HTTPS_PROXY" --noproxy '' -w '%{http_code}' http://127.0.0.1/ 2>/dev/null)
           case "$code" in 403) blocked "proxy refused ($code)";; 000|'') broken "proxy not reachable";; esac
           escaped "proxy forwarded the request ($code)""#,
    ),
    Probe::new(
        "net.mcp-other-api",
        "reach the lead agent's other API routes through the MCP bridge",
        "blocked (MCP only)",
        r#"need curl
           code=$(curl -sS -o /dev/null --max-time 5 -w '%{http_code}' -X POST "$ESC_MCP/api/lead/messages" 2>/dev/null)
           case "$code" in 2??) escaped "lead API answered $code";; 000|'') broken "bridge not reachable";; esac
           blocked "refused ($code)""#,
    ),
    Probe::new(
        "net.mcp-wrong-token",
        "use MCP with a token that is not this sandbox's",
        "blocked",
        r#"need curl
           out=$(curl -sS --max-time 5 -X POST -H 'Content-Type: application/json' \
             -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' "$ESC_MCP/mcp/not-$ESC_MCP_TOKEN" 2>/dev/null)
           case "$out" in *device_info*) escaped "tools listed with a foreign token";; esac
           blocked "refused""#,
    ),
    // ---- IPC ----
    Probe::new(
        "ipc.abstract-socket",
        "connect to a host abstract Unix socket (the X11/D-Bus escape class)",
        "blocked (abstract sockets are per network namespace)",
        r#"helper abstract-connect "$ESC_ABSTRACT""#,
    ),
    Probe::new(
        "ipc.host-sockets",
        "find the host's service sockets (D-Bus, Wayland, X11, container runtimes, systemd)",
        "blocked (not bound)",
        r#"for s in /run/dbus/system_bus_socket /run/user/*/bus /run/user/*/wayland-* \
                    /tmp/.X11-unix/X* /run/docker.sock /var/run/docker.sock /run/podman/podman.sock \
                    /run/containerd/containerd.sock /run/k3s/containerd/containerd.sock \
                    /run/systemd/private /run/systemd/journal/socket /run/user/*/podman/podman.sock; do
             [ -S "$s" ] && escaped "host socket visible: $s"
           done
           blocked "none visible""#,
    ),
    // ---- Devices ----
    Probe::new(
        "dev.minimal",
        "find device nodes beyond bwrap's minimal /dev",
        "blocked (null zero full random urandom tty ptmx pts shm fd std* core only)",
        r#"extra=$(ls -A /dev | grep -vxE 'null|zero|full|random|urandom|tty|ptmx|pts|shm|fd|stdin|stdout|stderr|core' | tr '\n' ' ')
           [ -n "$extra" ] && escaped "extra /dev entries: $extra"
           blocked "minimal /dev""#,
    ),
    Probe::new(
        "dev.block-devices",
        "find a raw block device",
        "blocked (none)",
        r#"b=$(find /dev -type b 2>/dev/null | head -n3 | tr '\n' ' ')
           [ -n "$b" ] && escaped "block devices: $b"
           blocked "no block devices""#,
    ),
    Probe::new(
        "dev.kernel-memory",
        "read kernel memory or the kernel log (/proc/kcore, /dev/core, /proc/kmsg, /dev/mem)",
        "blocked",
        r#"for f in /proc/kcore /dev/core /proc/kmsg /dev/mem /dev/kmsg; do
             timeout 2 head -c 1 "$f" >/dev/null 2>&1 && escaped "read $f"
           done
           blocked "all refused""#,
    ),
    Probe::new(
        "dev.sysfs",
        "browse /sys (hardware, kernel and cgroup details)",
        "blocked (not mounted)",
        r#"[ -n "$(ls -A /sys 2>/dev/null)" ] && escaped "/sys is mounted"
           blocked "/sys absent or empty""#,
    ),
    // ---- Kernel surface: only seccomp closes these; skipped (never passed) without it. ----
    // `helper <probe>` runs melissa-escape-probe (crates/escape-probe), bound read-only.
    Probe::new(
        "kern.unshare-user",
        "create a nested user namespace (unshare -U)",
        "blocked by seccomp",
        r#"need unshare
           unshare -U true 2>/dev/null && escaped "nested user namespace created"
           blocked "unshare refused""#,
    )
    .seccomp(),
    Probe::new(
        "kern.mount",
        "mount a tmpfs in the workdir",
        "blocked (no CAP_SYS_ADMIN; seccomp later)",
        r#"need mount
           mkdir -p m
           mount -t tmpfs none "$PWD/m" 2>/dev/null && escaped "mounted a tmpfs"
           blocked "mount refused""#,
    )
    .seccomp(),
    Probe::new(
        "kern.userns-mount",
        "mount after gaining root in a nested user namespace (unshare -Urm)",
        "blocked by seccomp",
        r#"need unshare
           mkdir -p m
           unshare -Urm sh -c 'mount -t tmpfs none "$1"' x "$PWD/m" 2>/dev/null \
             && escaped "mounted inside a nested user namespace"
           blocked "refused""#,
    )
    .seccomp(),
    Probe::new(
        "kern.ptrace",
        "PTRACE_ATTACH to a child process",
        "blocked by seccomp",
        r#"helper ptrace"#,
    )
    .seccomp(),
    Probe::new(
        "kern.keyctl",
        "reach the launcher's session keyring (bwrap keeps it: keys readable and writable)",
        "blocked by seccomp",
        r#"helper keyctl"#,
    )
    .seccomp(),
    Probe::new(
        "kern.bpf",
        "create a BPF map (bpf BPF_MAP_CREATE)",
        "blocked (unprivileged bpf off; seccomp later)",
        r#"helper bpf"#,
    )
    .seccomp(),
    Probe::new(
        "kern.perf-event-open",
        "open a software perf event on itself (perf_event_open)",
        "blocked by seccomp",
        r#"helper perf-event-open"#,
    )
    .seccomp(),
];

// ---- Host-side checks: the effect an attempt would have had, seen from outside. ----

fn absent(path: impl AsRef<std::path::Path>) -> Result<(), String> {
    let path = path.as_ref();
    if path.symlink_metadata().is_ok() {
        // Do not leave the planted file behind for the next run.
        let _ = std::fs::remove_file(path);
        return Err(format!("{} appeared on the host", path.display()));
    }
    Ok(())
}

fn unchanged(path: std::path::PathBuf, token: &str) -> Result<(), String> {
    match std::fs::read_to_string(&path) {
        Ok(s) if s == token => Ok(()),
        _ => Err(format!("{} changed on the host", path.display())),
    }
}

fn not_root_owned(path: std::path::PathBuf) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    match path.metadata() {
        Ok(m) if m.uid() == 0 => Err(format!("{} is root-owned on the host", path.display())),
        _ => Ok(()),
    }
}

fn alive(pid: u32) -> Result<(), String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    // Field 3 (after the parenthesised name) is the state; Z = killed, not yet reaped.
    match stat
        .rsplit_once(") ")
        .map(|(_, rest)| rest.starts_with('Z'))
    {
        Some(false) => Ok(()),
        _ => Err(format!("host canary process {pid} was killed")),
    }
}
