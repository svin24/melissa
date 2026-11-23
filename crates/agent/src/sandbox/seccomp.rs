//! The seccomp filter every agent sandbox runs under (bwrap `--seccomp`).
//!
//! A deny-list: everything an agent CLI (Node.js, Bun, Go or Rust binaries, a shell and the
//! usual tools) needs stays allowed; syscalls no agent CLI uses that widen the kernel's attack
//! surface or help an escape fail with `EPERM` (never a kill, so a CLI that probes for a
//! feature degrades instead of dying). `clone3` fails with `ENOSYS` instead: its flags sit
//! behind a pointer the filter cannot read, and on `ENOSYS` glibc, Go and Rust fall back to
//! `clone`, whose flags the filter does check (Docker's default profile does the same).
//!
//! The program is classic BPF, written here in safe Rust (`seccompiler` would build one
//! architecture per program and add a crate for what is a short list of jumps). It covers
//! x86_64 and aarch64 in one program: the
//! first instructions branch on the architecture the kernel reports for the syscall. Any
//! other ABI — 32-bit x86 or ARM compat calls, x32 calls on x86_64 — gets `EPERM` for every
//! syscall, so a 32-bit binary cannot use its ABI's different syscall numbers to get around
//! the list. On a host of another architecture the sandbox runs without the filter and the
//! lead agent logs a warning ([`host_supported`]).
//!
//! bwrap installs the program last, right before it executes the CLI, so bwrap's own
//! namespace and mount setup is unaffected.

use std::{
    fs::File,
    io::{self, Seek, SeekFrom, Write},
    os::fd::OwnedFd,
};

/// `struct seccomp_data` offsets: syscall number, architecture, low half of the first argument
/// (both supported architectures are little-endian).
const NR: u32 = 0;
const ARCH: u32 = 4;
const ARG0_LO: u32 = 16;

/// `AUDIT_ARCH_*` values the kernel reports (`EM_*` | 64-bit | little-endian).
pub const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;
pub const AUDIT_ARCH_AARCH64: u32 = 0xc000_00b7;

/// Filter results.
pub const RET_ALLOW: u32 = 0x7fff_0000;
const RET_ERRNO: u32 = 0x0005_0000;
const EPERM: u32 = 1;
const ENOSYS: u32 = 38;
pub const RET_EPERM: u32 = RET_ERRNO | EPERM;
pub const RET_ENOSYS: u32 = RET_ERRNO | ENOSYS;

/// x86_64 syscall numbers with this bit set are x32 ABI calls.
const X32_SYSCALL_BIT: u32 = 0x4000_0000;

/// Namespace flags of `clone` and `unshare`: `CLONE_NEWNS`, `NEWCGROUP`, `NEWUTS`, `NEWIPC`,
/// `NEWUSER`, `NEWPID`, `NEWNET`. New namespaces (above all a user namespace) expose kernel
/// code that is otherwise privileged; the sandbox already has its own of each.
const CLONE_NAMESPACES: u32 = 0x7e02_0000;
/// `CLONE_NEWTIME`, valid only for `unshare` (in `clone` this bit is part of the exit signal).
const CLONE_NEWTIME: u32 = 0x80;

/// `personality` values allowed (the same as Docker's default profile): `PER_LINUX`,
/// `PER_LINUX32`, `UNAME26`, `PER_LINUX32|UNAME26` and the query `0xffffffff`. Anything else,
/// e.g. `ADDR_NO_RANDOMIZE` (turns ASLR off), fails.
const PERSONALITY_ALLOWED: [u32; 5] = [0, 0x8, 0x2_0000, 0x2_0008, 0xffff_ffff];

/// Syscall numbers for one architecture.
#[derive(Debug, Clone, Copy)]
pub struct Arch {
    pub name: &'static str,
    pub audit: u32,
    /// Always `EPERM`, with why each is denied.
    pub denied: &'static [(&'static str, u32)],
    pub clone: u32,
    pub clone3: u32,
    pub unshare: u32,
    pub personality: u32,
}

/// Why each syscall is on the list (the same for both architectures):
/// - `ptrace`, `process_vm_readv`/`writev`, `pidfd_getfd`: read or change another process
///   (with no other users inside, this mostly matters for kernel bugs and the bridge process).
/// - `mount`, `umount2`, `pivot_root`, `chroot` and the new mount API (`open_tree`,
///   `move_mount`, `fsopen`, `fsconfig`, `fsmount`, `fspick`, `mount_setattr`): change the
///   sandbox's file tree.
/// - `setns`: join another namespace.
/// - `kexec_load`, `kexec_file_load`, `init_module`, `finit_module`, `delete_module`,
///   `reboot`, `swapon`, `swapoff`, `acct`, `quotactl`, `quotactl_fd`, `syslog`,
///   `settimeofday`, `clock_settime`, `adjtimex`, `clock_adjtime` (and x86 `iopl`, `ioperm`):
///   host administration; never allowed without capabilities anyway, denied to keep the
///   kernel code out of reach.
/// - `bpf`, `perf_event_open`, `userfaultfd`, `io_uring_setup`/`enter`/`register`: large,
///   often exploited kernel interfaces. Node's libuv and Bun fall back to epoll and threads.
/// - `keyctl`, `add_key`, `request_key`: the kernel keyring (not namespaced per sandbox).
/// - `lookup_dcookie`, `name_to_handle_at`, `open_by_handle_at`: profiling and file-handle
///   lookups that bypass path checks.
/// - `uselib`, `_sysctl` (x86_64): obsolete.
pub const X86_64: Arch = Arch {
    name: "x86_64",
    audit: AUDIT_ARCH_X86_64,
    denied: &[
        ("ptrace", 101),
        ("process_vm_readv", 310),
        ("process_vm_writev", 311),
        ("pidfd_getfd", 438),
        ("mount", 165),
        ("umount2", 166),
        ("pivot_root", 155),
        ("chroot", 161),
        ("open_tree", 428),
        ("move_mount", 429),
        ("fsopen", 430),
        ("fsconfig", 431),
        ("fsmount", 432),
        ("fspick", 433),
        ("mount_setattr", 442),
        ("setns", 308),
        ("kexec_load", 246),
        ("kexec_file_load", 320),
        ("init_module", 175),
        ("finit_module", 313),
        ("delete_module", 176),
        ("reboot", 169),
        ("swapon", 167),
        ("swapoff", 168),
        ("acct", 163),
        ("quotactl", 179),
        ("quotactl_fd", 443),
        ("syslog", 103),
        ("settimeofday", 164),
        ("clock_settime", 227),
        ("adjtimex", 159),
        ("clock_adjtime", 305),
        ("iopl", 172),
        ("ioperm", 173),
        ("bpf", 321),
        ("perf_event_open", 298),
        ("userfaultfd", 323),
        ("io_uring_setup", 425),
        ("io_uring_enter", 426),
        ("io_uring_register", 427),
        ("keyctl", 250),
        ("add_key", 248),
        ("request_key", 249),
        ("lookup_dcookie", 212),
        ("name_to_handle_at", 303),
        ("open_by_handle_at", 304),
        ("uselib", 134),
        ("_sysctl", 156),
    ],
    clone: 56,
    clone3: 435,
    unshare: 272,
    personality: 135,
};

/// aarch64 (the generic syscall table; no `iopl`/`ioperm`/`uselib`/`_sysctl`).
pub const AARCH64: Arch = Arch {
    name: "aarch64",
    audit: AUDIT_ARCH_AARCH64,
    denied: &[
        ("ptrace", 117),
        ("process_vm_readv", 270),
        ("process_vm_writev", 271),
        ("pidfd_getfd", 438),
        ("mount", 40),
        ("umount2", 39),
        ("pivot_root", 41),
        ("chroot", 51),
        ("open_tree", 428),
        ("move_mount", 429),
        ("fsopen", 430),
        ("fsconfig", 431),
        ("fsmount", 432),
        ("fspick", 433),
        ("mount_setattr", 442),
        ("setns", 268),
        ("kexec_load", 104),
        ("kexec_file_load", 294),
        ("init_module", 105),
        ("finit_module", 273),
        ("delete_module", 106),
        ("reboot", 142),
        ("swapon", 224),
        ("swapoff", 225),
        ("acct", 89),
        ("quotactl", 60),
        ("quotactl_fd", 443),
        ("syslog", 116),
        ("settimeofday", 170),
        ("clock_settime", 112),
        ("adjtimex", 171),
        ("clock_adjtime", 266),
        ("bpf", 280),
        ("perf_event_open", 241),
        ("userfaultfd", 282),
        ("io_uring_setup", 425),
        ("io_uring_enter", 426),
        ("io_uring_register", 427),
        ("keyctl", 219),
        ("add_key", 217),
        ("request_key", 218),
        ("lookup_dcookie", 18),
        ("name_to_handle_at", 264),
        ("open_by_handle_at", 265),
    ],
    clone: 220,
    clone3: 435,
    unshare: 97,
    personality: 92,
};

/// The architectures the program covers.
pub const ARCHES: [Arch; 2] = [X86_64, AARCH64];

/// Does the filter cover the architecture this binary was built for (and so the CLIs it runs)?
pub fn host_supported() -> bool {
    cfg!(any(target_arch = "x86_64", target_arch = "aarch64"))
}

/// One classic BPF instruction (`struct sock_filter`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Insn {
    pub code: u16,
    pub jt: u8,
    pub jf: u8,
    pub k: u32,
}

/// `BPF_LD | BPF_W | BPF_ABS`.
pub const LD_ABS: u16 = 0x20;
/// `BPF_JMP | BPF_JEQ | BPF_K`.
pub const JEQ: u16 = 0x15;
/// `BPF_JMP | BPF_JGE | BPF_K`.
pub const JGE: u16 = 0x35;
/// `BPF_JMP | BPF_JSET | BPF_K`.
pub const JSET: u16 = 0x45;
/// `BPF_RET | BPF_K`.
pub const RET: u16 = 0x06;

/// Jump targets in the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Label {
    Arch(usize),
    Allow,
    Eperm,
    Enosys,
    Clone,
    Unshare,
    Personality,
}

/// An instruction before jump offsets are known. Conditional jumps go to `then` when true and
/// fall through otherwise.
enum Op {
    Load(u32),
    JumpIf(u16, u32, Label),
    Ret(u32),
    Here(Label),
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("seccomp jump to {0:?} is too far ({1} instructions)")]
    TooFar(String, usize),
    #[error("seccomp jump to {0:?} has no target")]
    NoTarget(String),
}

fn ops() -> Vec<Op> {
    use Op::*;
    let mut ops = vec![Load(ARCH)];
    for (i, arch) in ARCHES.iter().enumerate() {
        ops.push(JumpIf(JEQ, arch.audit, Label::Arch(i)));
    }
    // Any other ABI (i386 or arm32 compat calls, a big-endian kernel): nothing.
    ops.push(Ret(RET_EPERM));
    for (i, arch) in ARCHES.iter().enumerate() {
        ops.push(Here(Label::Arch(i)));
        ops.push(Load(NR));
        if arch.audit == AUDIT_ARCH_X86_64 {
            ops.push(JumpIf(JGE, X32_SYSCALL_BIT, Label::Eperm));
        }
        ops.push(JumpIf(JEQ, arch.clone, Label::Clone));
        ops.push(JumpIf(JEQ, arch.unshare, Label::Unshare));
        ops.push(JumpIf(JEQ, arch.personality, Label::Personality));
        ops.push(JumpIf(JEQ, arch.clone3, Label::Enosys));
        for &(_, nr) in arch.denied {
            ops.push(JumpIf(JEQ, nr, Label::Eperm));
        }
        ops.push(Ret(RET_ALLOW));
    }
    // Argument checks, shared by both architectures (same argument layout).
    ops.push(Here(Label::Clone));
    ops.push(Load(ARG0_LO));
    ops.push(JumpIf(JSET, CLONE_NAMESPACES, Label::Eperm));
    ops.push(Ret(RET_ALLOW));
    ops.push(Here(Label::Unshare));
    ops.push(Load(ARG0_LO));
    ops.push(JumpIf(JSET, CLONE_NAMESPACES | CLONE_NEWTIME, Label::Eperm));
    ops.push(Ret(RET_ALLOW));
    ops.push(Here(Label::Personality));
    ops.push(Load(ARG0_LO));
    for value in PERSONALITY_ALLOWED {
        ops.push(JumpIf(JEQ, value, Label::Allow));
    }
    ops.push(Ret(RET_EPERM));
    ops.push(Here(Label::Allow));
    ops.push(Ret(RET_ALLOW));
    ops.push(Here(Label::Eperm));
    ops.push(Ret(RET_EPERM));
    ops.push(Here(Label::Enosys));
    ops.push(Ret(RET_ENOSYS));
    ops
}

/// The filter as instructions, jump offsets resolved.
pub fn program() -> Result<Vec<Insn>, BuildError> {
    let ops = ops();
    // Labels take no slot: each one marks the index of the next instruction.
    let mut at = Vec::new();
    let mut pc = 0;
    for op in &ops {
        match op {
            Op::Here(label) => at.push((*label, pc)),
            _ => pc += 1,
        }
    }
    let target = |label: Label| {
        at.iter()
            .find(|(l, _)| *l == label)
            .map(|(_, pc)| *pc)
            .ok_or_else(|| BuildError::NoTarget(format!("{label:?}")))
    };
    let mut out = Vec::with_capacity(pc);
    for op in ops {
        let insn = match op {
            Op::Here(_) => continue,
            Op::Load(offset) => Insn {
                code: LD_ABS,
                jt: 0,
                jf: 0,
                k: offset,
            },
            Op::Ret(k) => Insn {
                code: RET,
                jt: 0,
                jf: 0,
                k,
            },
            Op::JumpIf(code, k, label) => {
                // Offsets are relative to the next instruction and only jump forward.
                let to = target(label)?;
                let distance = to
                    .checked_sub(out.len() + 1)
                    .ok_or_else(|| BuildError::TooFar(format!("{label:?}"), 0))?;
                let jt = u8::try_from(distance)
                    .map_err(|_| BuildError::TooFar(format!("{label:?}"), distance))?;
                Insn { code, jt, jf: 0, k }
            }
        };
        out.push(insn);
    }
    Ok(out)
}

/// The program as bwrap reads it: `struct sock_filter` entries in native byte order.
pub fn program_bytes() -> Result<Vec<u8>, BuildError> {
    Ok(program()?
        .iter()
        .flat_map(|i| {
            let mut b = [0u8; 8];
            b[..2].copy_from_slice(&i.code.to_ne_bytes());
            b[2] = i.jt;
            b[3] = i.jf;
            b[4..].copy_from_slice(&i.k.to_ne_bytes());
            b
        })
        .collect())
}

/// A sealed, read-only memfd holding the program, positioned at its start, for bwrap's
/// `--seccomp FD`. It is close-on-exec here; the caller maps it to the child's fd number.
pub fn memfd() -> io::Result<OwnedFd> {
    use rustix::fs::{memfd_create, MemfdFlags, SealFlags};
    let bytes = program_bytes().map_err(io::Error::other)?;
    let fd = memfd_create(
        "melissa-seccomp",
        MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
    )?;
    let mut file = File::from(fd);
    file.write_all(&bytes)?;
    rustix::fs::fcntl_add_seals(
        &file,
        SealFlags::SHRINK | SealFlags::GROW | SealFlags::WRITE | SealFlags::SEAL,
    )?;
    file.seek(SeekFrom::Start(0))?;
    Ok(OwnedFd::from(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the program the way the kernel does, for one syscall.
    fn run(prog: &[Insn], arch: u32, nr: u32, arg0: u64) -> u32 {
        let mut data = [0u8; 64];
        data[0..4].copy_from_slice(&nr.to_le_bytes());
        data[4..8].copy_from_slice(&arch.to_le_bytes());
        data[16..24].copy_from_slice(&arg0.to_le_bytes());
        let mut acc = 0u32;
        let mut pc = 0usize;
        loop {
            let i = prog[pc];
            pc += 1;
            match i.code {
                LD_ABS => {
                    let o = i.k as usize;
                    acc = u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
                }
                JEQ | JGE | JSET => {
                    let taken = match i.code {
                        JEQ => acc == i.k,
                        JGE => acc >= i.k,
                        _ => acc & i.k != 0,
                    };
                    pc += usize::from(if taken { i.jt } else { i.jf });
                }
                RET => return i.k,
                other => panic!("unexpected opcode {other:#x}"),
            }
        }
    }

    fn nr(arch: &Arch, name: &str) -> u32 {
        arch.denied.iter().find(|(n, _)| *n == name).unwrap().1
    }

    #[test]
    fn both_arches_deny_the_listed_syscalls_and_allow_the_rest() {
        let prog = program().unwrap();
        assert!(prog.len() < 200, "{}", prog.len());
        for arch in ARCHES {
            for &(name, n) in arch.denied {
                assert_eq!(
                    run(&prog, arch.audit, n, 0),
                    RET_EPERM,
                    "{} {name}",
                    arch.name
                );
            }
            // Everything a CLI does day to day.
            for n in [0, 1, 2, 3, 9, 56, 57, 59, 63, 202, 231, 257, 435 - 1] {
                let listed = arch.denied.iter().any(|&(_, d)| d == n)
                    || [arch.clone, arch.clone3, arch.unshare, arch.personality].contains(&n);
                if !listed {
                    assert_eq!(run(&prog, arch.audit, n, 0), RET_ALLOW, "{} {n}", arch.name);
                }
            }
        }
        // Spot checks on the tables.
        assert_eq!(nr(&X86_64, "ptrace"), 101);
        assert_eq!(nr(&AARCH64, "ptrace"), 117);
        assert_eq!(nr(&AARCH64, "mount"), 40);
        for name in ["iopl", "ioperm", "uselib"] {
            assert!(AARCH64.denied.iter().all(|(n, _)| *n != name));
        }
    }

    #[test]
    fn namespaces_personality_and_clone3() {
        let prog = program().unwrap();
        // Flags glibc's pthread_create and fork use, and a vfork-style spawn.
        let thread = 0x003d_0f00u64;
        let fork = 0x0120_0011u64;
        let vfork = 0x4111u64;
        for arch in ARCHES {
            let a = arch.audit;
            for flags in [thread, fork, vfork] {
                assert_eq!(run(&prog, a, arch.clone, flags), RET_ALLOW);
            }
            for flag in [0x1000_0000u64, 0x2000_0000, 0x4000_0000, 0x0002_0000] {
                assert_eq!(run(&prog, a, arch.clone, fork | flag), RET_EPERM);
                assert_eq!(run(&prog, a, arch.unshare, flag), RET_EPERM);
            }
            assert_eq!(run(&prog, a, arch.unshare, CLONE_NEWTIME.into()), RET_EPERM);
            // CLONE_FILES, CLONE_FS: harmless.
            assert_eq!(run(&prog, a, arch.unshare, 0x400 | 0x200), RET_ALLOW);
            assert_eq!(run(&prog, a, arch.clone3, 0), RET_ENOSYS);
            assert_eq!(run(&prog, a, arch.personality, 0xffff_ffff), RET_ALLOW);
            assert_eq!(run(&prog, a, arch.personality, 0), RET_ALLOW);
            // ADDR_NO_RANDOMIZE.
            assert_eq!(run(&prog, a, arch.personality, 0x4_0000), RET_EPERM);
        }
    }

    #[test]
    fn other_abis_get_nothing() {
        let prog = program().unwrap();
        const AUDIT_ARCH_I386: u32 = 0x4000_0003;
        const AUDIT_ARCH_ARM: u32 = 0x4000_0028;
        for arch in [AUDIT_ARCH_I386, AUDIT_ARCH_ARM, 0] {
            assert_eq!(run(&prog, arch, 0, 0), RET_EPERM);
        }
        // x32 calls on x86_64 (`read` with the x32 bit).
        assert_eq!(run(&prog, AUDIT_ARCH_X86_64, X32_SYSCALL_BIT, 0), RET_EPERM);
    }

    #[test]
    fn bytes_are_sock_filter_entries() {
        let prog = program().unwrap();
        let bytes = program_bytes().unwrap();
        assert_eq!(bytes.len(), prog.len() * 8);
        assert_eq!(u16::from_ne_bytes([bytes[0], bytes[1]]), LD_ABS);
        assert_eq!(u32::from_ne_bytes(bytes[4..8].try_into().unwrap()), ARCH);
    }

    #[test]
    fn memfd_holds_the_program() {
        use std::io::Read;
        let mut file = File::from(memfd().unwrap());
        let mut read = Vec::new();
        file.read_to_end(&mut read).unwrap();
        assert_eq!(read, program_bytes().unwrap());
        // Sealed: nobody can change the filter after it is built.
        assert!(file.write_all(b"x").is_err());
    }

    /// The host architecture's table matches libc's syscall numbers.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn host_table_matches_libc() {
        let arch = if cfg!(target_arch = "x86_64") {
            X86_64
        } else {
            AARCH64
        };
        let expect = |name: &str| -> Option<libc::c_long> {
            Some(match name {
                "ptrace" => libc::SYS_ptrace,
                "process_vm_readv" => libc::SYS_process_vm_readv,
                "process_vm_writev" => libc::SYS_process_vm_writev,
                "pidfd_getfd" => libc::SYS_pidfd_getfd,
                "mount" => libc::SYS_mount,
                "umount2" => libc::SYS_umount2,
                "pivot_root" => libc::SYS_pivot_root,
                "chroot" => libc::SYS_chroot,
                "open_tree" => libc::SYS_open_tree,
                "move_mount" => libc::SYS_move_mount,
                "fsopen" => libc::SYS_fsopen,
                "fsconfig" => libc::SYS_fsconfig,
                "fsmount" => libc::SYS_fsmount,
                "fspick" => libc::SYS_fspick,
                "mount_setattr" => libc::SYS_mount_setattr,
                "setns" => libc::SYS_setns,
                "kexec_load" => libc::SYS_kexec_load,
                "kexec_file_load" => libc::SYS_kexec_file_load,
                "init_module" => libc::SYS_init_module,
                "finit_module" => libc::SYS_finit_module,
                "delete_module" => libc::SYS_delete_module,
                "reboot" => libc::SYS_reboot,
                "swapon" => libc::SYS_swapon,
                "swapoff" => libc::SYS_swapoff,
                "acct" => libc::SYS_acct,
                "quotactl" => libc::SYS_quotactl,
                "quotactl_fd" => libc::SYS_quotactl_fd,
                "syslog" => libc::SYS_syslog,
                "settimeofday" => libc::SYS_settimeofday,
                "clock_settime" => libc::SYS_clock_settime,
                "adjtimex" => libc::SYS_adjtimex,
                "clock_adjtime" => libc::SYS_clock_adjtime,
                "bpf" => libc::SYS_bpf,
                "perf_event_open" => libc::SYS_perf_event_open,
                "userfaultfd" => libc::SYS_userfaultfd,
                "io_uring_setup" => libc::SYS_io_uring_setup,
                "io_uring_enter" => libc::SYS_io_uring_enter,
                "io_uring_register" => libc::SYS_io_uring_register,
                "keyctl" => libc::SYS_keyctl,
                "add_key" => libc::SYS_add_key,
                "request_key" => libc::SYS_request_key,
                "lookup_dcookie" => libc::SYS_lookup_dcookie,
                "name_to_handle_at" => libc::SYS_name_to_handle_at,
                "open_by_handle_at" => libc::SYS_open_by_handle_at,
                #[cfg(target_arch = "x86_64")]
                "iopl" => libc::SYS_iopl,
                #[cfg(target_arch = "x86_64")]
                "ioperm" => libc::SYS_ioperm,
                #[cfg(target_arch = "x86_64")]
                "uselib" => libc::SYS_uselib,
                #[cfg(target_arch = "x86_64")]
                "_sysctl" => libc::SYS__sysctl,
                _ => return None,
            })
        };
        for &(name, n) in arch.denied {
            assert_eq!(expect(name), Some(libc::c_long::from(n)), "{name}");
        }
        assert_eq!(libc::c_long::from(arch.clone), libc::SYS_clone);
        assert_eq!(libc::c_long::from(arch.clone3), libc::SYS_clone3);
        assert_eq!(libc::c_long::from(arch.unshare), libc::SYS_unshare);
        assert_eq!(libc::c_long::from(arch.personality), libc::SYS_personality);
    }
}
