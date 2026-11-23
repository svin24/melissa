#![forbid(unsafe_code)]
//! `melissa-escape-probe`: the kernel-surface (and abstract-socket) attempts of the sandbox escape suite
//! (crates/agent/tests/escape.rs), as one small binary instead of python3 ctypes, so the
//! suite runs the same on a Fedora host, inside the container image (no python3) and on NixOS.
//!
//! Test-only: never installed. The image builds it only in its `escape-test` stage, the Nix
//! package does not build it (`-p` list), and the suite bind-mounts it read-only into each
//! probe's sandbox. Usage: `melissa-escape-probe <probe>`; it prints exactly one verdict line
//! (`ESCAPED: …`, `BLOCKED: …`, `SKIP: …` or `BROKEN: …`, the words the suite reads) and exits 0.
//! Exit 2 = usage error.
//!
//! The raw calls go through crates with safe APIs, so Melissa's code stays free of `unsafe`:
//! `nix` (ptrace), `linux-keyutils` (keyctl), `perf-event2` (perf_event_open) and `aya`
//! (bpf map creation); abstract sockets need only the standard library.

use std::{
    io,
    process::{Command, ExitCode, Stdio},
};

/// The probes this helper knows, with what each tries.
const PROBES: &[(&str, &str)] = &[
    ("ptrace", "PTRACE_ATTACH to a child process"),
    (
        "keyctl",
        "reach the session keyring (keyctl KEYCTL_GET_KEYRING_ID)",
    ),
    ("bpf", "create a BPF array map (bpf BPF_MAP_CREATE)"),
    (
        "perf-event-open",
        "open a software perf event on itself (perf_event_open)",
    ),
    (
        "abstract-connect",
        "connect to the abstract Unix socket named by the argument",
    ),
];

/// A probe's result, printed as the suite's verdict line.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Escaped(String),
    Blocked(String),
    Broken(String),
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Verdict::Escaped(s) => write!(f, "ESCAPED: {s}"),
            Verdict::Blocked(s) => write!(f, "BLOCKED: {s}"),
            Verdict::Broken(s) => write!(f, "BROKEN: {s}"),
        }
    }
}

/// A failed call: EPERM, EACCES and ENOSYS mean the call was refused (seccomp answers EPERM,
/// or ENOSYS for some calls); any other error means the kernel looked at the arguments, so the
/// surface is reachable.
fn judge_errno(call: &str, errno: Option<i32>) -> Verdict {
    match errno {
        Some(e) if e == nix::libc::EPERM => Verdict::Blocked(format!("{call}: EPERM")),
        Some(e) if e == nix::libc::EACCES => Verdict::Blocked(format!("{call}: EACCES")),
        Some(e) if e == nix::libc::ENOSYS => Verdict::Blocked(format!("{call}: ENOSYS")),
        Some(e) => Verdict::Escaped(format!(
            "{call} reachable ({})",
            nix::errno::Errno::from_raw(e)
        )),
        None => Verdict::Escaped(format!("{call} reachable (error without errno)")),
    }
}

fn judge_io(call: &str, e: &io::Error) -> Verdict {
    judge_errno(call, e.raw_os_error())
}

fn ptrace() -> Verdict {
    use nix::{sys::ptrace, unistd::Pid};
    // A child of ours (Yama's ptrace_scope 1 allows tracing descendants); a plain process
    // instead of fork(), which `nix` only offers as `unsafe`.
    let mut child = match Command::new("sleep")
        .arg("5")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Verdict::Broken(format!("cannot start a child (sleep): {e}")),
    };
    let Ok(raw) = i32::try_from(child.id()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Verdict::Broken("child pid out of range".into());
    };
    let pid = Pid::from_raw(raw);
    let result = ptrace::attach(pid);
    if result.is_ok() {
        // Best effort: the child is killed next anyway.
        let _ = nix::sys::wait::waitpid(pid, None);
        let _ = ptrace::detach(pid, None);
    }
    let _ = child.kill();
    let _ = child.wait();
    match result {
        Ok(()) => Verdict::Escaped("ptrace(PTRACE_ATTACH) succeeded".into()),
        Err(e) => judge_errno("ptrace(PTRACE_ATTACH)", Some(e as i32)),
    }
}

fn keyctl() -> Verdict {
    use linux_keyutils::{KeyError, KeyRing, KeyRingIdentifier};
    let errno = |e: &KeyError| match e {
        KeyError::PermissionDenied => Some(nix::libc::EPERM),
        KeyError::AccessDenied => Some(nix::libc::EACCES),
        KeyError::Unknown(n) => Some(*n),
        // Any other mapped error means the kernel processed the call.
        _ => Some(nix::libc::EINVAL),
    };
    match KeyRing::from_special_id(KeyRingIdentifier::Session, false) {
        Ok(ring) => {
            let name = ring
                .metadata()
                .map(|m| m.get_description().to_string())
                .unwrap_or_else(|_| "?".into());
            let keys = ring.get_links(1024).map(|l| l.len()).unwrap_or(0);
            Verdict::Escaped(format!(
                "session keyring '{name}' reachable, {keys} keys linked"
            ))
        }
        Err(e) => judge_errno("keyctl", errno(&e)),
    }
}

fn bpf() -> Verdict {
    use aya::maps::{Array, MapData, MapError};
    match Array::<MapData, u32>::create(1, 0) {
        Ok(_) => Verdict::Escaped("bpf(BPF_MAP_CREATE) succeeded".into()),
        Err(MapError::CreateError { io_error, .. }) => judge_io("bpf(BPF_MAP_CREATE)", &io_error),
        Err(e) => Verdict::Broken(format!("bpf map setup failed before the call: {e}")),
    }
}

fn perf_event_open() -> Verdict {
    use perf_event::{events::Software, Builder};
    let mut builder = Builder::new(Software::CPU_CLOCK);
    builder
        .observe_self()
        .any_cpu()
        .exclude_kernel(true)
        .exclude_hv(true);
    match builder.build() {
        Ok(_counter) => Verdict::Escaped("perf_event_open succeeded".into()),
        Err(e) => judge_io("perf_event_open", &e),
    }
}

/// Abstract sockets belong to a network namespace: the sandbox's own one must not reach the
/// host's (the X11/D-Bus escape class).
fn abstract_connect(name: &str) -> Verdict {
    use std::os::{
        linux::net::SocketAddrExt,
        unix::net::{SocketAddr, UnixStream},
    };
    if name.is_empty() {
        return Verdict::Broken("no socket name given".into());
    }
    let addr = match SocketAddr::from_abstract_name(name.as_bytes()) {
        Ok(a) => a,
        Err(e) => return Verdict::Broken(format!("bad abstract name: {e}")),
    };
    match UnixStream::connect_addr(&addr) {
        Ok(_) => Verdict::Escaped("connected to the host abstract socket".into()),
        Err(e) => Verdict::Blocked(format!("connect refused: {e}")),
    }
}

fn run(probe: &str, arg: &str) -> Option<Verdict> {
    Some(match probe {
        "ptrace" => ptrace(),
        "keyctl" => keyctl(),
        "bpf" => bpf(),
        "perf-event-open" => perf_event_open(),
        "abstract-connect" => abstract_connect(arg),
        _ => return None,
    })
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let probe = args.next().unwrap_or_default();
    let arg = args.next().unwrap_or_default();
    match run(&probe, &arg) {
        Some(v) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("usage: melissa-escape-probe <probe> [arg]\nprobes:");
            for (name, tries) in PROBES {
                eprintln!("  {name:16} {tries}");
            }
            println!("BROKEN: unknown probe {probe:?}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_block_and_other_errors_escape() {
        for e in [nix::libc::EPERM, nix::libc::EACCES, nix::libc::ENOSYS] {
            assert!(matches!(judge_errno("x", Some(e)), Verdict::Blocked(_)));
        }
        assert!(matches!(
            judge_errno("x", Some(nix::libc::EINVAL)),
            Verdict::Escaped(_)
        ));
        assert!(matches!(judge_errno("x", None), Verdict::Escaped(_)));
    }

    #[test]
    fn verdict_lines_use_the_suites_words() {
        assert_eq!(Verdict::Blocked("a".into()).to_string(), "BLOCKED: a");
        assert_eq!(Verdict::Escaped("a".into()).to_string(), "ESCAPED: a");
        assert_eq!(Verdict::Broken("a".into()).to_string(), "BROKEN: a");
    }

    #[test]
    fn every_listed_probe_runs_and_unknown_ones_do_not() {
        for (name, _) in PROBES {
            assert!(run(name, "").is_some(), "{name}");
        }
        assert!(run("nope", "").is_none());
        assert!(matches!(
            run("abstract-connect", ""),
            Some(Verdict::Broken(_))
        ));
    }
}
