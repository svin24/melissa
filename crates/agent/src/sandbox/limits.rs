//! Resource limits for every agent sandbox (cgroup v2): memory, processes, CPU, optionally IO
//! weight. One runaway or hostile agent (fork bomb, memory hog, busy loop) then cannot starve the
//! box, the web UI or the other agents.
//!
//! How (docs/DECISIONS.md, 2026-11-23): `melissa-server.service` has `Delegate=`, so systemd
//! hands the service's cgroup to the `melissa` user. At startup ([`init`]) the server moves
//! itself into a `server` leaf (cgroup v2 allows processes only in leaves once controllers are
//! enabled), enables the controllers and creates `sandboxes/`. Each sandbox gets its own child
//! there ([`Tree::create`], limits written as plain files). bwrap starts with `--block-fd`: it
//! creates its namespaces and then waits, before it forks or executes anything inside; the
//! spawner moves bwrap and its one child into the cgroup and only then lets it go on
//! ([`super::SandboxCommand::spawn`]). Nothing inside the sandbox ever runs outside its cgroup.
//! When the sandbox ends ([`SandboxCgroup`] dropped), whatever is left is killed
//! (`cgroup.kill`) and the cgroup removed; a hit limit (OOM kill, `pids.max`) is logged.
//!
//! Without delegation (a plain `cargo run`, cgroup v1, a service without `Delegate=`) agents
//! run without limits: [`init`] logs why once and the startup summary says so.

use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        OnceLock,
    },
    time::Duration,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Where cgroup v2 is mounted.
pub const CGROUP_FS: &str = "/sys/fs/cgroup";
/// The leaf the server process moves into.
const SERVER_LEAF: &str = "server";
/// The parent of the per-sandbox cgroups.
const SANDBOXES: &str = "sandboxes";
/// `cpu.max` period (the kernel default).
const CPU_PERIOD_US: u64 = 100_000;

const KIB: u64 = 1024;
const GIB: u64 = KIB * KIB * KIB;

/// Per-backend limits (`[backend.sandbox.limits]` in the server config). Every key is optional;
/// the defaults suit the Node-based agent CLIs (see [`Limits::default`]).
///
/// ```toml
/// [backend.sandbox.limits]
/// memory = "3G"     # memory.max; "max" = no limit
/// swap = "0"        # memory.swap.max
/// pids = 1024       # pids.max (processes and threads)
/// cpus = 1.5        # cpu.max as CPUs; "max" = no limit
/// io_weight = 50    # io.weight (1-10000), only where the io controller is delegated
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Limits {
    /// Apply the limits (where the host allows it). Off: this agent runs without them.
    pub enabled: bool,
    /// Memory (`memory.max`): RAM the sandbox may use, page cache and tmpfs files included.
    pub memory: Bytes,
    /// Swap (`memory.swap.max`) on top of `memory`.
    pub swap: Bytes,
    /// Processes and threads (`pids.max`).
    pub pids: Pids,
    /// CPU time (`cpu.max`), in CPUs.
    pub cpus: Cpus,
    /// IO weight (`io.weight`); unset: the kernel's default (100).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub io_weight: Option<IoWeight>,
}

impl Default for Limits {
    /// 2 GiB memory: Claude Code, Gemini CLI and Codex run on Node, idle at 300-600 MB and spike
    /// past 1 GB on long sessions; the tools they run (builds, tests) need room too. No swap: the
    /// limit means what it says, and a hog cannot push the box into swapping. 512 processes and
    /// threads: Node alone starts about a dozen threads; a build with parallel jobs stays far
    /// below; a fork bomb stops there. 2 CPUs: enough for a CLI plus a compiler, and on a box
    /// with more cores the rest stays free for the UI, k3s and other agents.
    fn default() -> Self {
        Self {
            enabled: true,
            memory: Bytes(Some(2 * GIB)),
            swap: Bytes(Some(0)),
            pids: Pids(Some(512)),
            cpus: Cpus(Some(2000)),
            io_weight: None,
        }
    }
}

impl Limits {
    /// The cgroup files and values for these limits, given the controllers the host delegates.
    /// Memory and pids are always there (see [`Tree::prepare`]).
    fn files(&self, c: Controllers) -> Vec<(&'static str, String)> {
        let mut files = vec![
            ("memory.max", self.memory.cgroup_value()),
            ("memory.swap.max", self.swap.cgroup_value()),
            ("pids.max", self.pids.cgroup_value()),
        ];
        if c.cpu {
            files.push(("cpu.max", self.cpus.cgroup_value()));
        }
        if let (true, Some(w)) = (c.io, self.io_weight) {
            files.push(("io.weight", format!("default {}", w.0)));
        }
        files
    }
}

/// `[backend.sandbox.limits]` as written in the config: only the keys given change the
/// backend's limits ([`LimitsOverride::apply`]); the others keep their preset values.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<Bytes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swap: Option<Bytes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pids: Option<Pids>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<Cpus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io_weight: Option<IoWeight>,
}

impl LimitsOverride {
    /// `base` with the keys this override sets replaced.
    pub fn apply(&self, base: Limits) -> Limits {
        Limits {
            enabled: self.enabled.unwrap_or(base.enabled),
            memory: self.memory.unwrap_or(base.memory),
            swap: self.swap.unwrap_or(base.swap),
            pids: self.pids.unwrap_or(base.pids),
            cpus: self.cpus.unwrap_or(base.cpus),
            io_weight: self.io_weight.or(base.io_weight),
        }
    }
}

impl fmt::Display for Limits {
    /// Short form for the startup summary: `2 GiB memory, 512 processes, 2 CPUs`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.enabled {
            return f.write_str("off");
        }
        write!(f, "{} memory, ", self.memory)?;
        match self.pids.0 {
            Some(n) => write!(f, "{n} processes, ")?,
            None => f.write_str("any number of processes, ")?,
        }
        match self.cpus.0 {
            Some(m) => write!(f, "{} CPUs", fmt_millis(m)),
            None => f.write_str("all CPUs"),
        }
    }
}

fn fmt_millis(m: u32) -> String {
    if m.is_multiple_of(1000) {
        (m / 1000).to_string()
    } else {
        format!("{}", f64::from(m) / 1000.0)
    }
}

/// A config value as TOML gives it: a number or a string.
#[derive(Deserialize)]
#[serde(untagged)]
enum Raw {
    Int(u64),
    Float(f64),
    Str(String),
}

fn is_max(s: &str) -> bool {
    s.trim().eq_ignore_ascii_case("max")
}

/// A byte count (`2G`, `512M`, `2147483648`) or `"max"` (no limit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bytes(pub Option<u64>);

impl Bytes {
    /// `"max"`, or digits with an optional binary unit: `K`, `M`, `G`, `T` (also `KiB`, `MB`,
    /// ...; all powers of 1024).
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if is_max(s) {
            return Ok(Self(None));
        }
        let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        let (digits, unit) = s.split_at(split);
        let n: u64 = digits
            .parse()
            .map_err(|_| format!("`{s}` is not a size (e.g. \"2G\", \"512M\" or \"max\")"))?;
        let unit = unit.trim().to_ascii_uppercase();
        let unit = unit.strip_suffix('B').unwrap_or(&unit);
        let unit = unit.strip_suffix('I').unwrap_or(unit);
        let shift = match unit {
            "" => 0,
            "K" => 10,
            "M" => 20,
            "G" => 30,
            "T" => 40,
            _ => return Err(format!("`{s}`: unknown unit (K, M, G or T)")),
        };
        n.checked_mul(1u64 << shift)
            .map(|b| Self(Some(b)))
            .ok_or_else(|| format!("`{s}` is too large"))
    }

    fn cgroup_value(self) -> String {
        self.0.map_or_else(|| "max".into(), |b| b.to_string())
    }
}

impl fmt::Display for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(b) = self.0 else {
            return f.write_str("no limit on");
        };
        for (shift, unit) in [(40, "TiB"), (30, "GiB"), (20, "MiB"), (10, "KiB")] {
            if b >= 1 << shift && b.is_multiple_of(1 << shift) {
                return write!(f, "{} {unit}", b >> shift);
            }
        }
        write!(f, "{b} bytes")
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Raw::deserialize(d)? {
            Raw::Int(n) => Ok(Self(Some(n))),
            Raw::Str(s) => Self::parse(&s).map_err(serde::de::Error::custom),
            Raw::Float(_) => Err(serde::de::Error::custom(
                "a size is whole bytes or a string like \"1536M\"",
            )),
        }
    }
}

impl Serialize for Bytes {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Some(b) => s.serialize_u64(b),
            None => s.serialize_str("max"),
        }
    }
}

/// A process count (at least 1) or `"max"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pids(pub Option<u32>);

impl Pids {
    fn cgroup_value(self) -> String {
        self.0.map_or_else(|| "max".into(), |n| n.to_string())
    }
}

impl<'de> Deserialize<'de> for Pids {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let bad = || serde::de::Error::custom("pids is a whole number from 1, or \"max\"");
        match Raw::deserialize(d)? {
            Raw::Str(s) if is_max(&s) => Ok(Self(None)),
            Raw::Int(n) if n >= 1 => u32::try_from(n).map(|n| Self(Some(n))).map_err(|_| bad()),
            _ => Err(bad()),
        }
    }
}

impl Serialize for Pids {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Some(n) => s.serialize_u32(n),
            None => s.serialize_str("max"),
        }
    }
}

/// CPU time in thousandths of a CPU (from a number such as `2` or `0.5`), or `"max"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cpus(pub Option<u32>);

impl Cpus {
    /// From a number of CPUs: at least 0.01 (the kernel's smallest quota), at most 4096.
    pub fn from_cpus(cpus: f64) -> Result<Self, String> {
        if !(0.01..=4096.0).contains(&cpus) {
            return Err(format!("cpus = {cpus}: use 0.01 to 4096, or \"max\""));
        }
        // In range above, so the rounded value fits a u32.
        Ok(Self(Some((cpus * 1000.0).round() as u32)))
    }

    fn cgroup_value(self) -> String {
        match self.0 {
            Some(m) => format!("{} {CPU_PERIOD_US}", u64::from(m) * CPU_PERIOD_US / 1000),
            None => format!("max {CPU_PERIOD_US}"),
        }
    }
}

impl<'de> Deserialize<'de> for Cpus {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let cpus = match Raw::deserialize(d)? {
            Raw::Str(s) if is_max(&s) => return Ok(Self(None)),
            Raw::Str(s) => s.trim().parse::<f64>().map_err(|_| {
                serde::de::Error::custom(format!("cpus = \"{s}\": a number or \"max\""))
            })?,
            // Small config numbers; precision is not a concern.
            Raw::Int(n) => n as f64,
            Raw::Float(f) => f,
        };
        Self::from_cpus(cpus).map_err(serde::de::Error::custom)
    }
}

impl Serialize for Cpus {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Some(m) => s.serialize_f64(f64::from(m) / 1000.0),
            None => s.serialize_str("max"),
        }
    }
}

/// `io.weight`, 1 to 10000 (the kernel's range; 100 is the default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoWeight(pub u16);

impl<'de> Deserialize<'de> for IoWeight {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Raw::deserialize(d)? {
            Raw::Int(n @ 1..=10000) => Ok(Self(n as u16)),
            _ => Err(serde::de::Error::custom("io_weight is 1 to 10000")),
        }
    }
}

impl Serialize for IoWeight {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u16(self.0)
    }
}

/// The controllers Melissa uses, as available in a cgroup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Controllers {
    pub memory: bool,
    pub pids: bool,
    pub cpu: bool,
    pub io: bool,
}

impl Controllers {
    /// From `cgroup.controllers` (`cpu io memory pids`).
    pub fn parse(text: &str) -> Self {
        let has = |name: &str| text.split_whitespace().any(|c| c == name);
        Self {
            memory: has("memory"),
            pids: has("pids"),
            cpu: has("cpu"),
            io: has("io"),
        }
    }

    /// What to write to `cgroup.subtree_control` to enable them (`+memory +pids ...`).
    fn enable_line(self) -> String {
        [
            (self.memory, "+memory"),
            (self.pids, "+pids"),
            (self.cpu, "+cpu"),
            (self.io, "+io"),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, c)| *c)
        .collect::<Vec<_>>()
        .join(" ")
    }
}

/// The process's cgroup v2 path (`/system.slice/melissa-server.service`) from
/// `/proc/self/cgroup`. `None` on a cgroup v1 (or hybrid without the v2 entry) host.
pub fn own_cgroup(proc_self_cgroup: &str) -> Option<&str> {
    proc_self_cgroup
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .map(str::trim)
        .filter(|p| p.starts_with('/'))
}

/// The cgroup directory for a cgroup path under `fs` (the cgroup2 mount).
pub fn cgroup_dir(fs: &Path, cgroup: &str) -> PathBuf {
    fs.join(cgroup.trim_start_matches('/'))
}

/// Whether limits can be applied on this host, and where.
#[derive(Debug, Clone)]
pub enum Status {
    Active(Tree),
    /// Why not (shown in the log and the startup summary).
    Off(String),
}

static STATUS: OnceLock<Status> = OnceLock::new();

/// Set up limits for this process's sandboxes (the server, once at startup, before it starts any
/// other process): take over the delegated cgroup this process runs in. Logs the outcome once.
/// Later calls return the first result.
pub fn init() -> &'static Status {
    STATUS.get_or_init(|| log(take_over_own_cgroup(Path::new(CGROUP_FS))))
}

/// Like [`init`], but use `base`, an existing delegated cgroup that holds no processes (tests:
/// the escape suite runs inside one, `just escape-test`). Safe to call from several processes.
pub fn init_at(base: &Path) -> &'static Status {
    STATUS.get_or_init(|| {
        let available = fs::read_to_string(base.join("cgroup.controllers"))
            .map(|t| Controllers::parse(&t))
            .unwrap_or_default();
        log(Tree::prepare(base, available).map_or_else(
            |e| Status::Off(format!("cannot use {}: {e}", base.display())),
            Status::Active,
        ))
    })
}

/// The status, if [`init`] or [`init_at`] ran.
pub fn status() -> Option<&'static Status> {
    STATUS.get()
}

/// Where sandboxes get their cgroups, when limits are active.
pub fn tree() -> Option<&'static Tree> {
    match STATUS.get() {
        Some(Status::Active(tree)) => Some(tree),
        _ => None,
    }
}

fn log(status: Status) -> Status {
    match &status {
        Status::Active(tree) => {
            tracing::info!(
                cgroup = %tree.sandboxes.display(),
                cpu = tree.controllers.cpu,
                io = tree.controllers.io,
                "agent sandboxes get resource limits"
            );
            if !tree.controllers.cpu {
                tracing::warn!("the cpu controller is not delegated: no CPU limit for agents");
            }
        }
        Status::Off(why) => tracing::warn!(
            "agent sandboxes run WITHOUT resource limits (memory, processes, CPU): {why}. \
             They need cgroup delegation: melissa-server.service (Delegate=), or the binary \
             alone in `systemd-run --user --scope -p Delegate=yes` (docs/DEV-ENV.md)"
        ),
    }
    let active = matches!(status, Status::Active(_));
    if let Some(risk) = oom_panic_risk(read_panic_on_oom(), active) {
        tracing::warn!("{risk}. Fix: {OOM_PANIC_FIX}");
    }
    status
}

/// The kernel's OOM panic setting.
pub const PANIC_ON_OOM: &str = "/proc/sys/vm/panic_on_oom";

/// How to make the host safe again (startup log and summary).
pub const OOM_PANIC_FIX: &str =
    "`sysctl -w vm.panic_on_oom=0`, and `vm.panic_on_oom = 0` in /etc/sysctl.d/ to keep it";

/// `vm.panic_on_oom` (Documentation/admin-guide/sysctl/vm.rst).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OomPanic {
    /// 0: the OOM killer kills a process; no panic.
    Never,
    /// 1: panic on a system-wide OOM only. An OOM confined by a memory cgroup, mempolicy or
    /// cpuset still just kills a process, so an agent at its own limit is safe.
    SystemWide,
    /// 2: panic on every OOM, a memory cgroup's included.
    Always,
}

/// Parse `/proc/sys/vm/panic_on_oom`. Unknown values count as [`OomPanic::Always`] (the
/// cautious reading); unreadable text is `None`.
pub fn parse_panic_on_oom(text: &str) -> Option<OomPanic> {
    match text.trim().parse::<i64>().ok()? {
        0 => Some(OomPanic::Never),
        1 => Some(OomPanic::SystemWide),
        _ => Some(OomPanic::Always),
    }
}

/// This host's setting, if readable.
pub fn read_panic_on_oom() -> Option<OomPanic> {
    fs::read_to_string(PANIC_ON_OOM)
        .ok()
        .and_then(|t| parse_panic_on_oom(&t))
}

/// Why an agent's memory use could panic the whole host, or `None` when it cannot.
/// `limits_active`: agent sandboxes get their own memory cgroup ([`Status::Active`]).
///
/// With 2 every OOM panics, so an agent that reaches its `memory.max` takes the host down.
/// With 1 a cgroup OOM is safe, but without per-agent limits an agent that eats all memory
/// causes a system-wide OOM, which panics. The memory limit stays on in both cases: without
/// it an agent could still exhaust the host's memory (a panic under 1 and 2 alike, or the OOM
/// killer hitting k3s or the server under 0), and the process and CPU limits are unaffected.
pub fn oom_panic_risk(setting: Option<OomPanic>, limits_active: bool) -> Option<&'static str> {
    match (setting?, limits_active) {
        (OomPanic::Always, true) => Some(
            "vm.panic_on_oom = 2: an agent that hits its memory limit panics the whole host",
        ),
        (OomPanic::Always, false) => {
            Some("vm.panic_on_oom = 2: any out-of-memory, also one caused by an agent, panics the whole host")
        }
        (OomPanic::SystemWide, false) => Some(
            "vm.panic_on_oom = 1 and no per-agent memory limit: an agent that uses up the memory panics the whole host",
        ),
        (OomPanic::SystemWide, true) | (OomPanic::Never, _) => None,
    }
}

/// [`init`]: the server's own cgroup must hold only this process (a systemd service with
/// `Delegate=`); move into `server/` and prepare the tree.
fn take_over_own_cgroup(fs_root: &Path) -> Status {
    let me = std::process::id();
    let Some(own) = fs::read_to_string("/proc/self/cgroup")
        .ok()
        .and_then(|t| own_cgroup(&t).map(str::to_string))
    else {
        return Status::Off("no cgroup v2 (unified hierarchy) on this host".into());
    };
    let base = cgroup_dir(fs_root, &own);
    let procs = fs::read_to_string(base.join("cgroup.procs")).unwrap_or_default();
    tracing::debug!(cgroup = %own, "the server's cgroup");
    if procs.split_whitespace().any(|p| p != me.to_string()) {
        return Status::Off("the server shares its cgroup with other processes".into());
    }
    let available = fs::read_to_string(base.join("cgroup.controllers"))
        .map(|t| Controllers::parse(&t))
        .unwrap_or_default();
    if !(available.memory && available.pids) {
        return Status::Off("the memory and pids controllers are not delegated".into());
    }
    let leaf = base.join(SERVER_LEAF);
    let moved = mkdir(&leaf).and_then(|()| write(&leaf.join("cgroup.procs"), &me.to_string()));
    if let Err(e) = moved {
        tracing::debug!(cgroup = %own, "cannot create the server leaf: {e}");
        return Status::Off("the server's cgroup is not delegated".into());
    }
    match Tree::prepare(&base, available) {
        Ok(tree) => {
            tree.sweep();
            Status::Active(tree)
        }
        Err(e) => {
            // Back where systemd put it; best effort.
            let _ = write(&base.join("cgroup.procs"), &me.to_string());
            let _ = fs::remove_dir(&leaf);
            Status::Off(format!("cannot enable controllers in {own}: {e}"))
        }
    }
}

fn mkdir(dir: &Path) -> io::Result<()> {
    match fs::create_dir(dir) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => Err(e),
        _ => Ok(()),
    }
}

/// Write a cgroup file: one `write`, no trailing newline (some kernels reject a pid with one).
fn write(path: &Path, value: &str) -> io::Result<()> {
    fs::write(path, value).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))
}

/// The delegated subtree: `sandboxes/` with the controllers enabled.
#[derive(Debug, Clone)]
pub struct Tree {
    sandboxes: PathBuf,
    controllers: Controllers,
}

static NEXT: AtomicU64 = AtomicU64::new(1);

impl Tree {
    /// Enable the wanted controllers in `base` (which must hold no processes by now), create
    /// `base/sandboxes` and enable them there too. Memory and pids are required.
    pub fn prepare(base: &Path, available: Controllers) -> io::Result<Self> {
        if !(available.memory && available.pids) {
            return Err(io::Error::other(
                "the memory and pids controllers are not available",
            ));
        }
        let line = available.enable_line();
        write(&base.join("cgroup.subtree_control"), &line)?;
        let sandboxes = base.join(SANDBOXES);
        mkdir(&sandboxes)?;
        write(&sandboxes.join("cgroup.subtree_control"), &line)?;
        Ok(Self {
            sandboxes,
            controllers: available,
        })
    }

    /// The parent of the per-sandbox cgroups.
    pub fn path(&self) -> &Path {
        &self.sandboxes
    }

    pub fn controllers(&self) -> Controllers {
        self.controllers
    }

    /// Remove cgroups left from an earlier run (only empty ones: removing a populated cgroup
    /// fails, so a running sandbox of another process sharing the tree is never touched).
    fn sweep(&self) {
        for entry in fs::read_dir(&self.sandboxes)
            .into_iter()
            .flatten()
            .flatten()
        {
            if entry.path().is_dir() {
                let _ = fs::remove_dir(entry.path());
            }
        }
    }

    /// A new, empty cgroup for one sandbox with `limits` written. `label` names it in logs.
    pub fn create(&self, limits: &Limits, label: &str) -> io::Result<SandboxCgroup> {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = self
            .sandboxes
            .join(format!("agent-{}-{n}", std::process::id()));
        fs::create_dir(&path)?;
        let cgroup = SandboxCgroup {
            path,
            label: label.to_string(),
        };
        for (file, value) in limits.files(self.controllers) {
            match write(&cgroup.path.join(file), &value) {
                // No swap accounting on this kernel: there is no swap to limit.
                Err(e) if file == "memory.swap.max" && e.kind() == io::ErrorKind::NotFound => {}
                other => other?,
            }
        }
        Ok(cgroup)
    }
}

/// What a sandbox ran into, from its cgroup's event counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LimitEvents {
    /// Processes the OOM killer killed (`memory.events` `oom_kill`).
    pub oom_kills: u64,
    /// Times usage reached `memory.max` (`memory.events` `max`).
    pub memory_max: u64,
    /// Forks refused at `pids.max` (`pids.events` `max`).
    pub pids_max: u64,
    /// Periods the CPU quota throttled it (`cpu.stat` `nr_throttled`).
    pub cpu_throttled: u64,
}

impl LimitEvents {
    /// From the three files' contents (missing ones as empty strings).
    pub fn parse(memory_events: &str, pids_events: &str, cpu_stat: &str) -> Self {
        let field = |text: &str, key: &str| {
            text.lines()
                .filter_map(|l| l.split_once(' '))
                .find(|(k, _)| *k == key)
                .and_then(|(_, v)| v.trim().parse().ok())
                .unwrap_or(0)
        };
        Self {
            oom_kills: field(memory_events, "oom_kill"),
            memory_max: field(memory_events, "max"),
            pids_max: field(pids_events, "max"),
            cpu_throttled: field(cpu_stat, "nr_throttled"),
        }
    }

    /// A limit stopped something (a process killed or a fork refused).
    pub fn hit(&self) -> bool {
        self.oom_kills > 0 || self.pids_max > 0
    }
}

/// One sandbox's cgroup. Dropping it kills what is left inside and removes it.
#[derive(Debug)]
pub struct SandboxCgroup {
    path: PathBuf,
    label: String,
}

impl SandboxCgroup {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Move `pid` (the bwrap process) and its descendants in. bwrap waits on `--block-fd` with
    /// exactly one child, so nothing forks while this runs; children cloned before `pid` moved
    /// are found through `/proc/<pid>/task/*/children`, later ones inherit the cgroup.
    pub(crate) fn adopt(&self, pid: u32) -> io::Result<()> {
        let procs = self.path.join("cgroup.procs");
        write(&procs, &pid.to_string())?;
        let mut todo = children(pid)?;
        while let Some(child) = todo.pop() {
            match write(&procs, &child.to_string()) {
                // Gone meanwhile: nothing to move.
                Err(e) if e.raw_os_error() == Some(3) => continue, // ESRCH
                other => other?,
            }
            todo.extend(children(child)?);
        }
        Ok(())
    }

    /// The event counters so far.
    pub fn events(&self) -> LimitEvents {
        let read = |f: &str| fs::read_to_string(self.path.join(f)).unwrap_or_default();
        LimitEvents::parse(
            &read("memory.events"),
            &read("pids.events"),
            &read("cpu.stat"),
        )
    }
}

/// Direct children of `pid` (all its threads'). An exited process has none.
fn children(pid: u32) -> io::Result<Vec<u32>> {
    let tasks = match fs::read_dir(format!("/proc/{pid}/task")) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for task in tasks.flatten() {
        let text = match fs::read_to_string(task.path().join("children")) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // The task exited, or the kernel lacks CONFIG_PROC_CHILDREN.
                if task.path().exists() {
                    return Err(io::Error::other("/proc/<pid>/task/<tid>/children missing"));
                }
                continue;
            }
            Err(e) => return Err(e),
        };
        out.extend(
            text.split_whitespace()
                .filter_map(|p| p.parse::<u32>().ok()),
        );
    }
    Ok(out)
}

impl Drop for SandboxCgroup {
    fn drop(&mut self) {
        let events = self.events();
        if events.hit() {
            tracing::warn!(
                sandbox = %self.label,
                oom_kills = events.oom_kills,
                forks_refused = events.pids_max,
                "agent sandbox hit its resource limits"
            );
        }
        // Whatever is still running inside (normally nothing: bwrap is gone).
        // (Never creates the file: kernels before 5.14 have none.)
        if let Ok(mut kill) = fs::OpenOptions::new()
            .write(true)
            .open(self.path.join("cgroup.kill"))
        {
            let _ = io::Write::write_all(&mut kill, b"1");
        }
        if fs::remove_dir(&self.path).is_ok() {
            return;
        }
        // Killed processes take a moment to leave; retry off the caller's thread.
        let path = std::mem::take(&mut self.path);
        let spawned = std::thread::Builder::new()
            .name("melissa-cgroup-cleanup".into())
            .spawn(move || {
                for _ in 0..100 {
                    std::thread::sleep(Duration::from_millis(100));
                    if fs::remove_dir(&path).is_ok() || !path.exists() {
                        return;
                    }
                }
                tracing::warn!(path = %path.display(), "cannot remove an agent sandbox cgroup");
            });
        if let Err(e) = spawned {
            tracing::warn!("cannot start the cgroup cleanup thread: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_2_gib_no_swap_512_pids_2_cpus() {
        let l = Limits::default();
        assert!(l.enabled);
        assert_eq!(l.memory, Bytes(Some(2 << 30)));
        assert_eq!(l.swap, Bytes(Some(0)));
        assert_eq!(l.pids, Pids(Some(512)));
        assert_eq!(l.cpus, Cpus(Some(2000)));
        assert_eq!(l.io_weight, None);
        assert_eq!(l.to_string(), "2 GiB memory, 512 processes, 2 CPUs");
        // An empty table is the defaults.
        let parsed: Limits = toml::from_str("").unwrap();
        assert_eq!(parsed, l);
    }

    #[test]
    fn sizes_parse_with_binary_units_and_max() {
        let ok = |s: &str| Bytes::parse(s).unwrap().0;
        assert_eq!(ok("0"), Some(0));
        assert_eq!(ok("1536M"), Some(1536 << 20));
        assert_eq!(ok("2G"), Some(2 << 30));
        assert_eq!(ok("2GiB"), Some(2 << 30));
        assert_eq!(ok("2 gb"), Some(2 << 30));
        assert_eq!(ok("64k"), Some(64 << 10));
        assert_eq!(ok("1T"), Some(1 << 40));
        assert_eq!(ok("max"), None);
        assert_eq!(ok(" MAX "), None);
        for bad in ["", "G", "1.5G", "2X", "-1", "99999999999T"] {
            assert!(Bytes::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn config_table_overrides_each_key() {
        let l: Limits = toml::from_str(
            r#"
            memory = "3G"
            swap = "max"
            pids = 1024
            cpus = 1.5
            io_weight = 50
            "#,
        )
        .unwrap();
        assert_eq!(l.memory, Bytes(Some(3 << 30)));
        assert_eq!(l.swap, Bytes(None));
        assert_eq!(l.pids, Pids(Some(1024)));
        assert_eq!(l.cpus, Cpus(Some(1500)));
        assert_eq!(l.io_weight, Some(IoWeight(50)));
        assert_eq!(l.to_string(), "3 GiB memory, 1024 processes, 1.5 CPUs");

        let l: Limits =
            toml::from_str("memory = 1073741824\ncpus = \"max\"\npids = \"max\"").unwrap();
        assert_eq!(l.memory, Bytes(Some(1 << 30)));
        assert_eq!(l.cpus, Cpus(None));
        assert_eq!(l.pids, Pids(None));
        assert_eq!(
            l.to_string(),
            "1 GiB memory, any number of processes, all CPUs"
        );
        let off: Limits = toml::from_str("enabled = false").unwrap();
        assert!(!off.enabled);
        assert_eq!(off.memory, Limits::default().memory);
    }

    #[test]
    fn limits_override_changes_only_given_keys() {
        let base = Limits {
            memory: Bytes(Some(3 * GIB)),
            io_weight: Some(IoWeight(50)),
            ..Limits::default()
        };
        let o: LimitsOverride = toml::from_str("pids = 64").unwrap();
        let l = o.apply(base.clone());
        assert_eq!(l.pids, Pids(Some(64)));
        assert_eq!(
            l,
            Limits {
                pids: Pids(Some(64)),
                ..base.clone()
            }
        );
        assert_eq!(LimitsOverride::default().apply(base.clone()), base);
        let o: LimitsOverride = toml::from_str("memory = \"max\"\nenabled = false").unwrap();
        let l = o.apply(base.clone());
        assert_eq!(
            (l.enabled, l.memory, l.cpus),
            (false, Bytes(None), base.cpus)
        );
        assert!(toml::from_str::<LimitsOverride>("memroy = \"1G\"").is_err());
        assert!(toml::from_str::<LimitsOverride>("pids = 0").is_err());
    }

    #[test]
    fn panic_on_oom_is_read_and_judged() {
        assert_eq!(parse_panic_on_oom("0\n"), Some(OomPanic::Never));
        assert_eq!(parse_panic_on_oom("1\n"), Some(OomPanic::SystemWide));
        assert_eq!(parse_panic_on_oom("2\n"), Some(OomPanic::Always));
        assert_eq!(parse_panic_on_oom("7"), Some(OomPanic::Always));
        assert_eq!(parse_panic_on_oom(""), None);
        assert_eq!(parse_panic_on_oom("x"), None);

        // 2: every OOM panics, the memory cgroup's included, with or without limits.
        assert!(oom_panic_risk(Some(OomPanic::Always), true)
            .unwrap()
            .contains("hits its memory limit"));
        assert!(oom_panic_risk(Some(OomPanic::Always), false).is_some());
        // 1: a cgroup OOM only kills; without limits an agent can cause a system-wide OOM.
        assert_eq!(oom_panic_risk(Some(OomPanic::SystemWide), true), None);
        assert!(oom_panic_risk(Some(OomPanic::SystemWide), false).is_some());
        assert_eq!(oom_panic_risk(Some(OomPanic::Never), true), None);
        assert_eq!(oom_panic_risk(Some(OomPanic::Never), false), None);
        // Unreadable (no /proc): nothing to report.
        assert_eq!(oom_panic_risk(None, true), None);
    }

    #[test]
    fn bad_limits_are_rejected() {
        for bad in [
            "pids = 0",
            "pids = -1",
            "pids = 1.5",
            "cpus = 0",
            "cpus = 0.001",
            "cpus = -2",
            "cpus = \"lots\"",
            "memory = 1.5",
            "memory = \"2 apples\"",
            "io_weight = 0",
            "io_weight = 10001",
            "nonsense = 1",
        ] {
            assert!(toml::from_str::<Limits>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn limits_round_trip_through_serde() {
        let l = Limits {
            swap: Bytes(None),
            cpus: Cpus(Some(500)),
            io_weight: Some(IoWeight(10)),
            ..Limits::default()
        };
        let text = toml::to_string(&l).unwrap();
        assert_eq!(toml::from_str::<Limits>(&text).unwrap(), l);
    }

    #[test]
    fn cgroup_files_follow_the_limits_and_the_controllers() {
        let all = Controllers::parse("cpuset cpu io memory hugetlb pids");
        assert_eq!(
            all,
            Controllers {
                memory: true,
                pids: true,
                cpu: true,
                io: true
            }
        );
        let l = Limits {
            io_weight: Some(IoWeight(50)),
            ..Limits::default()
        };
        assert_eq!(
            l.files(all),
            vec![
                ("memory.max", "2147483648".to_string()),
                ("memory.swap.max", "0".to_string()),
                ("pids.max", "512".to_string()),
                ("cpu.max", "200000 100000".to_string()),
                ("io.weight", "default 50".to_string()),
            ]
        );
        // Without cpu/io delegated, only memory and pids; "max" spelled the kernel's way.
        let some = Controllers::parse("memory pids");
        let unlimited = Limits {
            memory: Bytes(None),
            pids: Pids(None),
            cpus: Cpus(None),
            ..l
        };
        let files = unlimited.files(some);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0], ("memory.max", "max".to_string()));
        assert_eq!(files[2], ("pids.max", "max".to_string()));
        assert_eq!(Cpus(None).cgroup_value(), "max 100000");
        assert_eq!(Cpus(Some(250)).cgroup_value(), "25000 100000");
        assert_eq!(some.enable_line(), "+memory +pids");
        assert_eq!(all.enable_line(), "+memory +pids +cpu +io");
    }

    #[test]
    fn own_cgroup_is_the_unified_entry() {
        assert_eq!(
            own_cgroup("0::/system.slice/melissa-server.service\n"),
            Some("/system.slice/melissa-server.service")
        );
        // Hybrid: v1 lines first.
        assert_eq!(
            own_cgroup("12:pids:/system.slice/x\n1:name=systemd:/x\n0::/system.slice/x\n"),
            Some("/system.slice/x")
        );
        // Pure v1: no unified entry.
        assert_eq!(own_cgroup("12:pids:/x\n1:name=systemd:/x\n"), None);
        assert_eq!(
            cgroup_dir(Path::new("/sys/fs/cgroup"), "/system.slice/m.service"),
            Path::new("/sys/fs/cgroup/system.slice/m.service")
        );
        assert_eq!(
            cgroup_dir(Path::new("/sys/fs/cgroup"), "/"),
            Path::new("/sys/fs/cgroup/")
        );
    }

    #[test]
    fn events_parse_the_counters() {
        let e = LimitEvents::parse(
            "low 0\nhigh 0\nmax 12\noom 2\noom_kill 1\noom_group_kill 0\n",
            "max 7\n",
            "usage_usec 10\nnr_periods 5\nnr_throttled 3\nthrottled_usec 9\n",
        );
        assert_eq!(
            e,
            LimitEvents {
                oom_kills: 1,
                memory_max: 12,
                pids_max: 7,
                cpu_throttled: 3
            }
        );
        assert!(e.hit());
        let none = LimitEvents::parse("", "", "");
        assert_eq!(none, LimitEvents::default());
        assert!(!none.hit());
    }

    /// A temp directory standing in for a delegated cgroup: the same files, plain ones.
    struct FakeBase(PathBuf);

    impl FakeBase {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for FakeBase {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fake_base() -> FakeBase {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("melissa-cg-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("cgroup.controllers"), "cpu io memory pids").unwrap();
        FakeBase(dir)
    }

    #[test]
    fn prepare_enables_controllers_and_creates_the_sandboxes_dir() {
        let base = fake_base();
        let available = Controllers::parse("cpu memory pids");
        let tree = Tree::prepare(base.path(), available).unwrap();
        assert_eq!(tree.path(), base.path().join("sandboxes"));
        let read = |p: &Path| fs::read_to_string(p).unwrap();
        assert_eq!(
            read(&base.path().join("cgroup.subtree_control")),
            "+memory +pids +cpu"
        );
        assert_eq!(
            read(&base.path().join("sandboxes/cgroup.subtree_control")),
            "+memory +pids +cpu"
        );
        // Again (a restart, another test process): fine.
        Tree::prepare(base.path(), available).unwrap();
        // Memory and pids are required.
        assert!(Tree::prepare(base.path(), Controllers::parse("cpu io")).is_err());
    }

    #[test]
    fn each_sandbox_gets_its_own_cgroup_with_the_limits_written() {
        let base = fake_base();
        let tree = Tree::prepare(base.path(), Controllers::parse("memory pids cpu")).unwrap();
        let limits = Limits {
            pids: Pids(Some(64)),
            ..Limits::default()
        };
        let a = tree.create(&limits, "test").unwrap();
        let b = tree.create(&limits, "test").unwrap();
        assert_ne!(a.path(), b.path());
        assert!(a.path().starts_with(tree.path()));
        let name = a.path().file_name().unwrap().to_str().unwrap().to_string();
        assert!(
            name.starts_with(&format!("agent-{}-", std::process::id())),
            "{name}"
        );
        let read = |f: &str| fs::read_to_string(a.path().join(f)).unwrap();
        assert_eq!(read("memory.max"), "2147483648");
        assert_eq!(read("memory.swap.max"), "0");
        assert_eq!(read("pids.max"), "64");
        assert_eq!(read("cpu.max"), "200000 100000");
        assert!(!a.path().join("io.weight").exists());

        // Counters as the kernel would write them.
        fs::write(a.path().join("pids.events"), "max 3\n").unwrap();
        assert_eq!(a.events().pids_max, 3);

        // Dropping removes the cgroup (here: once the fake files are gone, as the kernel's are).
        for c in [&a, &b] {
            for f in fs::read_dir(c.path()).unwrap() {
                fs::remove_file(f.unwrap().path()).unwrap();
            }
        }
        let (pa, pb) = (a.path().to_path_buf(), b.path().to_path_buf());
        drop(a);
        drop(b);
        assert!(!pa.exists() && !pb.exists());
    }

    #[test]
    fn sweep_removes_only_empty_leftovers() {
        let base = fake_base();
        let tree = Tree::prepare(base.path(), Controllers::parse("memory pids")).unwrap();
        let empty = tree.path().join("agent-1-1");
        let busy = tree.path().join("agent-1-2");
        fs::create_dir(&empty).unwrap();
        fs::create_dir(&busy).unwrap();
        // A real populated cgroup refuses rmdir; a non-empty directory does too.
        fs::write(busy.join("cgroup.procs"), "42").unwrap();
        tree.sweep();
        assert!(!empty.exists());
        assert!(busy.exists());
    }
}
