//! Adversarial sandbox escape suite: real escape attempts, run inside the real sandbox.
//!
//! Every probe in `escape/probes.rs` runs as its own sandboxed "agent": bubblewrap plus the
//! egress namespace and `melissa-netbridge`, built with the crate's own `SandboxPlan` /
//! `sandbox::spawn_command()`, the agent backends' one spawn path. Each attempt must
//! fail; controls must work (so a dead proxy or read-only workdir cannot make the denials
//! pass vacuously). Results: pass, fail or skip, printed as a table; any fail fails the test.
//!
//! `just escape-test` runs this with `MELISSA_ESCAPE_REQUIRED=1`, which turns a missing
//! bwrap (or user namespaces being off, or a missing probe helper) into a failure instead of
//! a skip: it is the gate for image updates. `just escape-test-image` runs the same suite inside
//! the built container image, the NixOS VM test runs it on NixOS. Plain `cargo nextest run`
//! skips it on hosts without bwrap, like the other sandbox tests.
//!
//! The kernel-surface probes (ptrace, keyctl, bpf, perf_event_open) call
//! `melissa-escape-probe` (crates/escape-probe, test-only, never installed), found next to
//! this test binary or at `MELISSA_ESCAPE_PROBE`, and bound read-only into each probe's
//! sandbox. How to add probes: docs/agent-skills/sandbox-escape-tests.md.
//!
//! Resource limits (`limits.*` probes) need a delegated cgroup: `just escape-test` runs the suite
//! in a `systemd-run --user --scope -p Delegate=yes` scope and names it in `MELISSA_TEST_CGROUP`.
//! Without one those probes skip (they never run without limits: one is a fork bomb).
//!
//! Agent users (`users.*` probes) need subordinate ids for the user running the suite
//! (`/etc/subuid`, `/etc/subgid`) and newuidmap; the suite keeps its id table in
//! `<scene base>/melissa-escape-users`. Without them those probes skip with the reason.

// Test-only file: panicking on setup failure is the intended behaviour.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "escape/probes.rs"]
mod probes;

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    os::{
        linux::net::SocketAddrExt,
        unix::{
            net::{SocketAddr as UnixAddr, UnixListener, UnixStream},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use melissa_agent::{
    egress::{EgressProxy, EgressRequest, HostRule, WebRead},
    mcp::McpServer,
    sandbox::{
        self, acl,
        limits::{self, LimitEvents, Limits},
        users, Mount, SandboxChild, SandboxCommand, SandboxPlan,
    },
    tools::{DeviceInfo, TaskContext, Tool, ToolRegistry},
    NetworkMode, SandboxSpec,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
};

/// A host-side check of what an attempt would have changed; `Err` = it escaped.
pub type HostCheck = fn(&Ctx) -> Result<(), String>;

/// A check of the sandbox's cgroup counters after a limit probe; `Err` = the limit did not hold.
pub type LimitCheck = fn(&LimitEvents) -> Result<(), String>;

/// How a probe runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The script runs to its verdict line.
    Script,
    /// The script starts a detached heartbeat (`./beat`); the runner kills the sandbox's
    /// parent and the probe fails if the heartbeat keeps going.
    ParentDies,
}

/// One escape attempt.
#[derive(Clone, Copy)]
pub struct Probe {
    pub name: &'static str,
    /// What it tries.
    pub tries: &'static str,
    /// The expected outcome.
    pub expect: &'static str,
    /// POSIX sh, run after the shared preamble (see probes.rs for the verdict words).
    pub script: &'static str,
    /// Kernel surface that only seccomp closes: while the sandbox's argv has no seccomp
    /// filter, an escape is reported as skipped; once it has one, an escape fails.
    pub requires_seccomp: bool,
    pub host: Option<HostCheck>,
    /// A resource-limit probe: runs only when the sandbox has a cgroup with limits, then this
    /// checks its counters.
    pub limits: Option<LimitCheck>,
    /// An agent-user probe: judged only when sandboxes run as their own host users; skipped with
    /// the reason otherwise.
    pub requires_users: bool,
    /// Bind the other agent's box read-write into this sandbox (a hypothetical bug or escape):
    /// only the host permissions stand in the way.
    pub bind_other_box: bool,
    pub kind: Kind,
}

impl Probe {
    pub const fn new(
        name: &'static str,
        tries: &'static str,
        expect: &'static str,
        script: &'static str,
    ) -> Self {
        Self {
            name,
            tries,
            expect,
            script,
            requires_seccomp: false,
            host: None,
            limits: None,
            requires_users: false,
            bind_other_box: false,
            kind: Kind::Script,
        }
    }

    pub const fn users(mut self) -> Self {
        self.requires_users = true;
        self
    }

    pub const fn bind_other_box(mut self) -> Self {
        self.bind_other_box = true;
        self
    }

    pub const fn seccomp(mut self) -> Self {
        self.requires_seccomp = true;
        self
    }

    pub const fn limits(mut self, check: LimitCheck) -> Self {
        self.limits = Some(check);
        self
    }

    pub const fn host(mut self, check: HostCheck) -> Self {
        self.host = Some(check);
        self
    }

    pub const fn kind(mut self, kind: Kind) -> Self {
        self.kind = kind;
        self
    }
}

/// Helpers every probe script can use (see probes.rs).
const PREAMBLE: &str = r#"
escaped() { echo "ESCAPED: $*"; exit 0; }
blocked() { echo "BLOCKED: $*"; exit 0; }
works() { echo "WORKS: $*"; exit 0; }
broken() { echo "BROKEN: $*"; exit 0; }
skip() { echo "SKIP: $*"; exit 0; }
need() { for t in "$@"; do command -v "$t" >/dev/null 2>&1 || skip "tool missing: $t"; done; }
# Run the probe helper (melissa-escape-probe <probe>), which prints the verdict itself.
helper() {
  if [ -z "${ESC_PROBE:-}" ] || [ ! -x "$ESC_PROBE" ]; then
    [ "${ESC_REQUIRED:-0}" = 1 ] && broken "probe helper missing (melissa-escape-probe)"
    skip "probe helper missing (build it: just escape-probe)"
  fi
  "$ESC_PROBE" "$@" || broken "probe helper exited with status $?"
  exit 0
}
leaked() { [ "$(cat "$1" 2>/dev/null)" = "$ESC_TOKEN" ]; }
# The host uid of in-sandbox uid $1 (/proc/self/uid_map: inside outside count), empty if unmapped.
host_uid() {
  while read -r i o n; do
    [ "$1" -ge "$i" ] && [ "$1" -lt $((i + n)) ] && { echo $((o + $1 - i)); return; }
  done < /proc/self/uid_map
}
direct() { curl -sS -o /dev/null --noproxy '*' --connect-timeout 3 --max-time 5 "$1" 2>/dev/null; }
viaproxy() { curl -sS -o /dev/null --max-time 5 -x "$HTTPS_PROXY" --noproxy '' -w '%{http_connect}' "$@" 2>/dev/null; }
proxy_verdict() {
  case "$1" in
    403) blocked "proxy refused $2 (403)";;
    000|'') broken "proxy not reachable";;
    *) escaped "proxy accepted $2 (CONNECT answered $1)";;
  esac
}
"#;

/// The host-side scene the probes attack.
pub struct Ctx {
    pub root: PathBuf,
    sockets: PathBuf,
    /// The host user's home as the sandbox plan sees it (`$HOME` inside is an empty tmpfs).
    pub home: PathBuf,
    pub outside: PathBuf,
    /// `$XDG_STATE_HOME/melissa` inside `home`.
    pub state: PathBuf,
    pub own_box: PathBuf,
    pub other_box: PathBuf,
    pub token: String,
    pub canary_pid: u32,
    pub tmp_name: String,
    port: u16,
    lan: Option<IpAddr>,
    abstract_name: String,
    mcp_addr: SocketAddr,
    mcp_token: String,
    /// `melissa-escape-probe`, bound read-only into every probe's sandbox.
    probe_helper: Option<PathBuf>,
    /// `MELISSA_ESCAPE_REQUIRED=1`: nothing may be skipped for a missing tool of ours.
    required: bool,
    /// The suite's own uid (the "server").
    pub server_uid: u32,
    /// The host uids of this agent (`own_box`'s) and of the other one, when agent users are on.
    pub own_uid: Option<u32>,
    pub other_uid: Option<u32>,
}

impl Ctx {
    /// The workdir of one probe (each probe gets its own).
    pub fn work_of(&self, probe: &str) -> PathBuf {
        self.root.join("work").join(probe)
    }

    /// The variables set inside each sandbox (`SandboxPlan::with_env`).
    fn vars(&self) -> Vec<(String, String)> {
        let boxes = self.own_box.parent().unwrap();
        let other_key = self.other_box.file_name().unwrap().to_str().unwrap();
        let s = |p: &Path| p.display().to_string();
        [
            ("ESC_TOKEN", self.token.clone()),
            ("ESC_HOME_CANARY", s(&self.home.join("secret-canary.txt"))),
            ("ESC_OUTSIDE", s(&self.outside)),
            ("ESC_STATE", s(&self.state)),
            ("ESC_OWN_BOX", s(&self.own_box)),
            ("ESC_OTHER_BOX", s(&self.other_box)),
            (
                "ESC_OTHER_INDEX",
                s(&self.state.join(format!("files/index/{other_key}.json"))),
            ),
            ("ESC_BOXES", s(boxes)),
            ("ESC_HOST_PID", self.canary_pid.to_string()),
            ("ESC_PORT", self.port.to_string()),
            (
                "ESC_LAN",
                self.lan.map(|ip| ip.to_string()).unwrap_or_default(),
            ),
            ("ESC_ABSTRACT", self.abstract_name.clone()),
            ("ESC_MCP", format!("http://{}", self.mcp_addr)),
            ("ESC_MCP_TOKEN", self.mcp_token.clone()),
            ("ESC_TMP_NAME", self.tmp_name.clone()),
            (
                "ESC_PROBE",
                self.probe_helper.as_deref().map(s).unwrap_or_default(),
            ),
            ("ESC_REQUIRED", u8::from(self.required).to_string()),
            ("ESC_SERVER_UID", self.server_uid.to_string()),
            (
                "ESC_OTHER_UID",
                self.other_uid.map(|u| u.to_string()).unwrap_or_default(),
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
    }
}

/// A probe's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pass(String),
    Fail(String),
    Skip(String),
}

/// Read the verdict from a probe's stdout. No verdict is a failure, never a pass. A probe of
/// kernel surface only seccomp closes, run without the filter (`sandbox.seccomp = false`, or an
/// architecture without one), is skipped with what it saw: it neither passes nor fails.
fn verdict(stdout: &str, requires_seccomp: bool, seccomp: bool) -> Outcome {
    let line = stdout.lines().find_map(|l| {
        ["ESCAPED", "BLOCKED", "WORKS", "BROKEN", "SKIP"]
            .iter()
            .find_map(|w| l.strip_prefix(&format!("{w}: ")).map(|rest| (*w, rest)))
    });
    let unfiltered = requires_seccomp && !seccomp;
    match line {
        Some(("BLOCKED", why)) if unfiltered => Outcome::Skip(format!(
            "seccomp off: not judged (without the filter: blocked: {why})"
        )),
        Some(("ESCAPED", what)) if unfiltered => Outcome::Skip(format!(
            "seccomp off: not judged (without the filter: ESCAPED: {what})"
        )),
        Some(("BLOCKED", why)) => Outcome::Pass(format!("blocked: {why}")),
        Some(("WORKS", what)) => Outcome::Pass(format!("works: {what}")),
        Some(("SKIP", why)) => Outcome::Skip(why.to_string()),
        Some(("ESCAPED", what)) => Outcome::Fail(format!("ESCAPED: {what}")),
        Some((_, why)) => Outcome::Fail(format!("broken: {why}")),
        None => Outcome::Fail("no verdict (probe did not run to the end)".into()),
    }
}

/// A fresh directory named for this run under `base`.
fn scratch(base: &Path) -> PathBuf {
    let dir = base.join(format!("melissa-escape-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Where the scene lives: cargo's target tmp dir, i.e. not under /tmp, so "outside the
/// workdir" really is a host path the sandbox has no mount for (its /tmp is a private tmpfs,
/// where a write succeeds harmlessly). `MELISSA_ESCAPE_DIR` overrides it.
fn scene_base() -> PathBuf {
    let base = std::env::var_os("MELISSA_ESCAPE_DIR")
        .map(PathBuf::from)
        .or_else(|| option_env!("CARGO_TARGET_TMPDIR").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    // Cargo names CARGO_TARGET_TMPDIR but doesn't create it, so a fresh checkout or a
    // cleaned target/ has none; create it so the suite never fails on a missing folder.
    std::fs::create_dir_all(&base).expect("create the escape suite's base dir");
    base
}

/// A host program: `/usr/bin/<name>` (Fedora) or the first `<name>` on `PATH` (NixOS).
fn host_program(name: &str) -> Option<PathBuf> {
    let usr = Path::new("/usr/bin").join(name);
    if usr.is_file() {
        return Some(usr);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// The suite's own run: `MELISSA_ESCAPE_REQUIRED=1` (the image gate).
fn required() -> bool {
    std::env::var_os("MELISSA_ESCAPE_REQUIRED").is_some_and(|v| v == "1")
}

/// `melissa-escape-probe`: `MELISSA_ESCAPE_PROBE`, else where `cargo build -p
/// melissa-escape-probe` puts it: `target/<profile>/` (found from `CARGO_TARGET_TMPDIR`,
/// `target/tmp`, since test binaries may live in a separate build dir), or beside the
/// test binary's `deps/` directory.
fn probe_helper() -> Option<PathBuf> {
    const NAME: &str = "melissa-escape-probe";
    if let Some(p) = std::env::var_os("MELISSA_ESCAPE_PROBE") {
        return Some(PathBuf::from(p)).filter(|p| p.is_file());
    }
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let from_target = option_env!("CARGO_TARGET_TMPDIR")
        .and_then(|t| Path::new(t).parent())
        .map(|target| target.join(profile).join(NAME));
    let from_exe = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join(NAME)));
    from_target
        .into_iter()
        .chain(from_exe)
        .find(|p| p.is_file())
}

fn netbridge() -> PathBuf {
    std::env::var_os("MELISSA_NETBRIDGE")
        .map(PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_melissa-netbridge").into())
}

/// bwrap and namespaces available? `MELISSA_ESCAPE_REQUIRED=1` (the image gate) makes
/// "no" a failure rather than a skip.
fn sandbox_available() -> Option<(PathBuf, PathBuf)> {
    let required = required();
    let bwrap = sandbox::find_bwrap();
    let sh = host_program("sh");
    let ok = match (&bwrap, &sh) {
        (Some(b), Some(s)) => std::process::Command::new(b)
            .args(["--unshare-all", "--ro-bind", "/", "/", "--"])
            .arg(s)
            .args(["-c", "true"])
            .status()
            .is_ok_and(|st| st.success()),
        _ => false,
    };
    if !ok {
        let why = "bwrap missing or it cannot create namespaces here";
        assert!(!required, "escape suite required but cannot run: {why}");
        eprintln!("SKIP: {why}");
        return None;
    }
    assert!(
        !required || probe_helper().is_some(),
        "escape suite required but the probe helper is missing: \
         `just escape-probe`, or set MELISSA_ESCAPE_PROBE"
    );
    Some((bwrap?, sh?))
}

/// The host's LAN address, if it has one (no packet is sent).
fn lan_address() -> Option<IpAddr> {
    let s = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    s.connect(("203.0.113.1", 9)).ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}

/// A tiny HTTP server answering every request with `body`.
fn http_server(listener: TcpListener, body: &'static str) {
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf).await;
                let reply = format!(
                    "HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(reply.as_bytes()).await;
            });
        }
    });
}

/// Kills the host canary (its whole process group: `sh` and its `sleep`) when the suite
/// ends, also on panic.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", self.0.id())])
            .status();
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The file box keys of the suite's agent and of the other agent whose box it attacks.
const OWN_KEY: &str = "d-0a1b2c3d.a1";
const OTHER_KEY: &str = "d-0a1b2c3d.a2";

/// Set up agent users with the suite's own id table (kept across runs, so the agents keep their
/// ids). Off (with the reason) where this user has no subordinate ids or newuidmap.
fn init_users() {
    users::init(&scene_base().join("melissa-escape-users"));
}

fn users_off_reason() -> String {
    match users::status() {
        Some(users::Status::Off(why)) => why.clone(),
        Some(users::Status::Active(_)) => "not used for this sandbox".into(),
        None => "not set up".into(),
    }
}

/// Everything the suite holds for its length.
struct Scene {
    ctx: Ctx,
    proxy: EgressProxy,
    allow: HostRule,
    bwrap: PathBuf,
    sh: PathBuf,
    _canary: KillOnDrop,
    _abstract: UnixListener,
}

async fn scene(bwrap: PathBuf, sh: PathBuf) -> Scene {
    let root = scratch(&scene_base());
    // Unix socket paths are short (107 bytes): the egress sockets live under /tmp.
    let sockets = scratch(&std::env::temp_dir());
    let token = format!(
        "melissa-escape-token-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let home = root.join("home");
    let outside = root.join("outside");
    let state = home.join(".local/state/melissa");
    let boxes = state.join("files/boxes");
    let own_box = boxes.join(OWN_KEY);
    let other_box = boxes.join(OTHER_KEY);
    for d in [&outside, &own_box, &other_box, &state.join("files/index")] {
        std::fs::create_dir_all(d).unwrap();
    }
    // Boxes are private, as `FileBoxes::ensure_dir` makes them.
    for d in [&own_box, &other_box] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    for f in [
        home.join("secret-canary.txt"),
        outside.join("canary.txt"),
        state.join("permissions.json"),
        state.join("files/index/d-0a1b2c3d.a2.json"),
        other_box.join("secret.txt"),
    ] {
        std::fs::write(f, &token).unwrap();
    }

    // Agent users: both agents' host uids; the other agent's box as its own runs leave it (its
    // user let in, nobody else).
    let server_uid = {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata("/proc/self").unwrap().uid()
    };
    let (own_uid, other_uid) = match users::active() {
        Some(setup) => {
            let own = setup.user(OWN_KEY).unwrap();
            let other = setup.user(OTHER_KEY).unwrap();
            let follow = acl::Follow::No;
            acl::grant(&other_box, other.uid, acl::RWX, follow).unwrap();
            acl::set_default(
                &other_box,
                &acl::Acl::shared_default(&[other.uid, server_uid]),
                follow,
            )
            .unwrap();
            acl::grant(
                &other_box.join("secret.txt"),
                other.uid,
                acl::R | acl::W,
                follow,
            )
            .unwrap();
            (Some(own.uid), Some(other.uid))
        }
        None => (None, None),
    };

    // A host process the sandbox must neither see nor signal.
    let mark = format!("melissa-escape-canary-{}", std::process::id());
    let canary = std::process::Command::new(&sh)
        .args(["-c", "sleep 600; :", &mark])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let canary_pid = canary.id();

    // Servers on the host's loopback and LAN address (same port where possible).
    let lo = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = lo.local_addr().unwrap().port();
    http_server(lo, "allowed-body");
    let lan = match lan_address() {
        Some(ip) => match TcpListener::bind((ip, port)).await {
            Ok(l) => {
                http_server(l, "lan-body");
                Some(ip)
            }
            Err(_) => None,
        },
        None => None,
    };

    // A host abstract Unix socket; checked reachable from the host so the probe is not vacuous.
    let abstract_name = format!("melissa-escape-{}", std::process::id());
    let addr = UnixAddr::from_abstract_name(abstract_name.as_bytes()).unwrap();
    let abstract_listener = UnixListener::bind_addr(&addr).unwrap();
    UnixStream::connect_addr(&addr).expect("host reaches its own abstract socket");

    // Melissa's MCP endpoint through the bridge, as for agents.
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(DeviceInfo::new())];
    let mcp = McpServer::new(ToolRegistry::new(tools, None));
    let mcp_token = mcp.open_session(7, TaskContext::default(), None).unwrap();
    let mcp_addr: SocketAddr = "127.0.0.1:18080".parse().unwrap();
    let proxy = EgressProxy::new(sockets.clone(), None)
        .unwrap()
        .with_netbridge(netbridge())
        .with_mcp(mcp, mcp_addr)
        .with_web_read(WebRead::on())
        .with_resolver_override(
            "allowed.test.example",
            vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
        )
        .with_resolver_override("denied.test.example", vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);
    let allow = HostRule::parse(&format!("allowed.test.example:{port}")).unwrap();

    Scene {
        ctx: Ctx {
            root,
            sockets,
            home,
            outside,
            state,
            own_box,
            other_box,
            token,
            canary_pid,
            tmp_name: format!("melissa-escape-tmp-{}", std::process::id()),
            port,
            lan,
            abstract_name,
            mcp_addr,
            mcp_token,
            probe_helper: probe_helper(),
            required: required(),
            server_uid,
            own_uid,
            other_uid,
        },
        proxy,
        allow,
        bwrap,
        sh,
        _canary: KillOnDrop(canary),
        _abstract: abstract_listener,
    }
}

/// Run one probe in its own sandbox, with the backend setting `sandbox.seccomp = seccomp_on`;
/// returns the outcome and whether the argv really had the filter.
async fn run(scene: &Scene, probe: &Probe, seccomp_on: bool) -> (Outcome, bool) {
    let ctx = &scene.ctx;
    let work = ctx.work_of(probe.name);
    std::fs::create_dir_all(&work).unwrap();
    // The agents' path: an egress lease (proxy, DNS sinkhole, MCP), a plan from a spec,
    // the own file box, fixed variables, the bridge, then `sandbox::spawn_command` (the
    // backends' one spawn path: bwrap argv, seccomp fd, empty environment).
    let lease = scene
        .proxy
        .open(EgressRequest {
            backend: "escape-test".into(),
            task: 7,
            mode: NetworkMode::Proxy,
            allow: vec![scene.allow.clone()],
            deny: Vec::new(),
            mcp_tokens: vec![ctx.mcp_token.clone()],
        })
        .unwrap();
    let spec = SandboxSpec {
        network: NetworkMode::Proxy,
        seccomp: seccomp_on,
        limits: suite_limits(),
        ..SandboxSpec::default()
    };
    let mut plan = SandboxPlan::resolve(&spec, &scene.bwrap, Some(&ctx.home), &scene.sh)
        .unwrap()
        .with_rw_dirs(std::slice::from_ref(&ctx.own_box))
        .unwrap()
        .with_env(&ctx.vars())
        .unwrap()
        .with_bridge(lease.bridge().cloned())
        .with_user(OWN_KEY)
        .unwrap();
    if probe.bind_other_box {
        // Added by hand, as a bug would: no grant comes with it.
        plan.mounts.push(Mount::ReadWrite(ctx.other_box.clone()));
    }
    let script = format!("{PREAMBLE}\n{}\n", probe.script);
    let args = ["-c".to_string(), script];
    // The probe helper, bound read-only (`extra_ro`, like a backend's extra files).
    let helper: Vec<PathBuf> = ctx.probe_helper.iter().cloned().collect();
    let cmd = sandbox::spawn_command(&plan, &work, &helper, &args).unwrap();
    let seccomp = cmd
        .command()
        .as_std()
        .get_args()
        .any(|a| a == "--seccomp" || a == "--add-seccomp-fd");

    let outcome = match probe.kind {
        // Without agent users the hypothetical bind would simply work: not judged.
        _ if probe.requires_users && plan.user.is_none() => {
            drop(cmd);
            Outcome::Skip(format!("agent users off: {}", users_off_reason()))
        }
        // A limit probe never runs without limits (one is a fork bomb).
        Kind::Script if probe.limits.is_some() && plan.limits.is_none() => {
            drop(cmd);
            Outcome::Skip(format!("resource limits off: {}", limits_off_reason()))
        }
        // With vm.panic_on_oom = 2 any cgroup OOM panics the kernel (the NixOS test VMs default to
        // it): the memory probe would take the whole host down, so it does not run.
        Kind::Script if probe.name == "limits.memory-hog" && panic_on_any_oom() => {
            drop(cmd);
            Outcome::Skip("vm.panic_on_oom = 2: a cgroup OOM would panic this host's kernel".into())
        }
        Kind::Script => {
            let (outcome, events) = run_script(cmd, &work, probe, seccomp).await;
            match (outcome, probe.limits, events) {
                (Outcome::Pass(p), Some(check), Some(events)) => match check(&events) {
                    Ok(()) => Outcome::Pass(p),
                    Err(e) => Outcome::Fail(format!("ESCAPED (cgroup check): {e}")),
                },
                (Outcome::Pass(_), Some(_), None) => {
                    Outcome::Fail("broken: the sandbox got no cgroup".into())
                }
                (o, _, _) => o,
            }
        }
        Kind::ParentDies => {
            drop(cmd);
            // Built by hand around an `sh` parent, without the cgroup gate or an agent user
            // (bwrap makes its own user namespace, as before agent users).
            let plan = SandboxPlan {
                limits: None,
                user: None,
                ..plan.clone()
            };
            run_parent_dies(&scene.sh, &plan, &args, &work).await
        }
    };
    drop(lease);
    let outcome = match (outcome, probe.host) {
        (Outcome::Fail(f), _) => Outcome::Fail(f),
        // Not run (or not judged): nothing to look at on the host.
        (Outcome::Skip(s), _) if probe.requires_users => Outcome::Skip(s),
        (o, Some(check)) => match check(ctx) {
            Ok(()) => o,
            Err(e) => Outcome::Fail(format!("ESCAPED (host check): {e}")),
        },
        (o, None) => o,
    };
    (outcome, seccomp)
}

/// `cmd` is from `sandbox::spawn_command`; the rest as the backends set it (cli.rs). Also
/// returns the sandbox cgroup's counters, when it had one.
async fn run_script(
    mut cmd: SandboxCommand,
    work: &Path,
    probe: &Probe,
    seccomp: bool,
) -> (Outcome, Option<LimitEvents>) {
    cmd.command_mut()
        .current_dir(work)
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let SandboxChild { child, cgroup } = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return (
                Outcome::Fail(format!("cannot start the sandbox: {e}")),
                None,
            )
        }
    };
    let out = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output()).await;
    let events = cgroup.as_ref().map(|c| c.events());
    let out = match out {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return (Outcome::Fail(format!("sandbox failed: {e}")), events),
        Err(_) => return (Outcome::Fail("timed out after 30 s".into()), events),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let outcome = verdict(&stdout, probe.requires_seccomp, seccomp);
    if let Outcome::Fail(why) = &outcome {
        if why.starts_with("no verdict") {
            let err = String::from_utf8_lossy(&out.stderr);
            let tail: String = err.lines().rev().take(3).collect::<Vec<_>>().join(" | ");
            return (
                Outcome::Fail(format!("{why}; {}; stderr: {tail}", out.status)),
                events,
            );
        }
    }
    (outcome, events)
}

/// The limits every suite sandbox gets: small, so the limit probes are quick and light on the
/// host, and enough for the other probes (sh, curl).
fn suite_limits() -> Limits {
    Limits {
        memory: limits::Bytes(Some(256 << 20)),
        pids: limits::Pids(Some(128)),
        cpus: limits::Cpus::from_cpus(0.5).unwrap(),
        ..Limits::default()
    }
}

/// Set up limits when the suite runs in a delegated cgroup (`just escape-test`).
fn init_limits() {
    if let Some(base) = std::env::var_os("MELISSA_TEST_CGROUP") {
        limits::init_at(Path::new(&base));
    }
}

/// `vm.panic_on_oom = 2`: the kernel panics on any OOM, also one confined to a memory cgroup.
fn panic_on_any_oom() -> bool {
    std::fs::read_to_string("/proc/sys/vm/panic_on_oom").is_ok_and(|s| s.trim() == "2")
}

fn limits_off_reason() -> String {
    match limits::status() {
        Some(limits::Status::Off(why)) => why.clone(),
        Some(limits::Status::Active(_)) => "turned off for this sandbox".into(),
        None => "not in a delegated cgroup (MELISSA_TEST_CGROUP unset; `just escape-test` sets it)"
            .into(),
    }
}

/// Kill the sandbox's parent; the detached heartbeat inside must stop. The parent is an
/// intermediate `sh` (so it can be killed alone), given what `sandbox::spawn_command` gives
/// bwrap: the same argv, the seccomp fd (inherited by bwrap) and an empty environment.
async fn run_parent_dies(sh: &Path, plan: &SandboxPlan, args: &[String], work: &Path) -> Outcome {
    let (program, argv) = sandbox::command(plan, work, &[], args);
    let mut parent = tokio::process::Command::new(sh);
    parent.env_clear();
    if let Err(e) = sandbox::pass_seccomp(&mut parent, plan) {
        return Outcome::Fail(format!("cannot pass the seccomp program: {e}"));
    }
    parent
        .args([
            "-c",
            r#""$@" </dev/null >/dev/null 2>&1 & echo $!; wait"#,
            "parent",
        ])
        .arg(&program)
        .args(&argv)
        .current_dir(work)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = match parent.spawn() {
        Ok(c) => c,
        Err(e) => return Outcome::Fail(format!("cannot start the parent: {e}")),
    };
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let bwrap_pid = lines.next_line().await.ok().flatten().unwrap_or_default();
    let beat = work.join("beat");
    let read = || std::fs::read_to_string(&beat).unwrap_or_default();
    let mut started = false;
    for _ in 0..100 {
        if !read().is_empty() {
            started = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if !started {
        return Outcome::Fail("broken: heartbeat never started".into());
    }
    let _ = child.kill().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let before = read();
    // Six heartbeats' worth: a survivor would have rewritten the file.
    tokio::time::sleep(Duration::from_millis(300)).await;
    if read() != before {
        // Clean up what escaped before failing.
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &bwrap_pid])
            .status();
        return Outcome::Fail("ESCAPED: heartbeat kept going after the parent died".into());
    }
    Outcome::Pass("blocked: sandbox died with its parent".into())
}

/// The table's first line: says plainly when the run had no seccomp filter.
fn header(seccomp: bool) -> String {
    if seccomp {
        "Sandbox escape suite (seccomp: on, filter in the bwrap argv)".into()
    } else {
        "Sandbox escape suite (seccomp: OFF, sandbox.seccomp = false or no filter for this \
         architecture; kernel probes are skipped, not passed)"
            .into()
    }
}

fn print_table(rows: &[(&Probe, Outcome)], seccomp: bool) {
    let width = rows.iter().map(|(p, _)| p.name.len()).max().unwrap_or(5);
    let limits = match limits::tree() {
        Some(t) => format!("on, {} ({})", suite_limits(), t.path().display()),
        None => format!("off ({})", limits_off_reason()),
    };
    println!();
    let agent_users = match users::active() {
        Some(s) => {
            let (uid, _) = s.server();
            let id = |k| {
                s.existing(k)
                    .map_or_else(|| "?".into(), |u| u.uid.to_string())
            };
            format!(
                "on (this agent uid {}, the other agent {}, the suite {uid})",
                id(OWN_KEY),
                id(OTHER_KEY)
            )
        }
        None => format!("off ({})", users_off_reason()),
    };
    println!("{}", header(seccomp));
    println!("Resource limits: {limits}");
    println!("Agent users: {agent_users}");
    println!("{:width$}  {:6}  DETAIL", "PROBE", "RESULT");
    for (probe, outcome) in rows {
        let (word, detail) = match outcome {
            Outcome::Pass(d) => ("pass", d.as_str()),
            Outcome::Fail(d) => ("FAIL", d.as_str()),
            Outcome::Skip(d) => ("skip", d.as_str()),
        };
        println!("{:width$}  {word:6}  {detail}", probe.name);
    }
    let count = |f: fn(&Outcome) -> bool| rows.iter().filter(|(_, o)| f(o)).count();
    println!(
        "{} probes: {} pass, {} fail, {} skip",
        rows.len(),
        count(|o| matches!(o, Outcome::Pass(_))),
        count(|o| matches!(o, Outcome::Fail(_))),
        count(|o| matches!(o, Outcome::Skip(_))),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sandbox_escape_suite() {
    let Some((bwrap, sh)) = sandbox_available() else {
        return;
    };
    init_limits();
    init_users();
    let scene = scene(bwrap, sh).await;
    let mut rows = Vec::new();
    let mut seccomp = false;
    for probe in probes::PROBES {
        let (outcome, s) = run(&scene, probe, true).await;
        seccomp |= s;
        rows.push((probe, outcome));
    }
    print_table(&rows, seccomp);
    let failed: Vec<String> = rows
        .iter()
        .filter_map(|(p, o)| match o {
            Outcome::Fail(why) => Some(format!(
                "{}: tried to {}; expected {}; got {why}",
                p.name, p.tries, p.expect
            )),
            _ => None,
        })
        .collect();
    let _ = users::remove_dir_all(&scene.ctx.root, OWN_KEY);
    let _ = std::fs::remove_dir_all(&scene.ctx.sockets);
    assert!(failed.is_empty(), "sandbox escapes:\n{}", failed.join("\n"));
}

/// A backend configured with `sandbox.seccomp = false`: the kernel-surface probes run without
/// the filter and are reported as skipped (never passed), and the header says seccomp is off.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seccomp_off_backend_is_reported_not_passed() {
    let Some((bwrap, sh)) = sandbox_available() else {
        return;
    };
    init_limits();
    init_users();
    let scene = scene(bwrap, sh).await;
    let mut rows = Vec::new();
    for probe in probes::PROBES.iter().filter(|p| p.requires_seccomp) {
        let (outcome, in_argv) = run(&scene, probe, false).await;
        assert!(
            !in_argv,
            "{}: seccomp = false still put a filter in the argv",
            probe.name
        );
        rows.push((probe, outcome));
    }
    print_table(&rows, false);
    let _ = users::remove_dir_all(&scene.ctx.root, OWN_KEY);
    let _ = std::fs::remove_dir_all(&scene.ctx.sockets);
    assert!(header(false).contains("seccomp: OFF"));
    assert!(!rows.is_empty());
    for (probe, outcome) in &rows {
        match outcome {
            Outcome::Skip(why) => assert!(
                why.starts_with("seccomp off") || why.starts_with("tool missing"),
                "{}: skipped without saying why: {why}",
                probe.name
            ),
            other => panic!(
                "{}: expected a skip with seccomp off, got {other:?}",
                probe.name
            ),
        }
    }
}

#[test]
fn probe_names_are_unique_and_described() {
    let mut names = std::collections::HashSet::new();
    for p in probes::PROBES {
        assert!(names.insert(p.name), "duplicate probe {}", p.name);
        assert!(!p.tries.is_empty() && !p.expect.is_empty(), "{}", p.name);
    }
}

#[test]
fn a_probe_without_a_verdict_never_passes() {
    assert!(matches!(verdict("", false, false), Outcome::Fail(_)));
    assert!(matches!(verdict("noise\n", false, false), Outcome::Fail(_)));
    assert!(matches!(
        verdict("BLOCKED: x", false, false),
        Outcome::Pass(_)
    ));
    assert!(matches!(
        verdict("BROKEN: x", false, false),
        Outcome::Fail(_)
    ));
    assert!(matches!(
        verdict("SKIP: tool missing: x", false, false),
        Outcome::Skip(_)
    ));
    // Kernel surface without the filter (seccomp = false): skipped with the reason, whatever
    // it saw; with the filter, an escape is a real failure.
    assert!(matches!(
        verdict("ESCAPED: x", true, false),
        Outcome::Skip(why) if why.starts_with("seccomp off")
    ));
    assert!(matches!(
        verdict("BLOCKED: x", true, false),
        Outcome::Skip(why) if why.starts_with("seccomp off")
    ));
    assert!(matches!(
        verdict("BLOCKED: x", true, true),
        Outcome::Pass(_)
    ));
    // A broken kernel probe fails even without the filter.
    assert!(matches!(
        verdict("BROKEN: x", true, false),
        Outcome::Fail(_)
    ));
    assert!(matches!(verdict("", true, false), Outcome::Fail(_)));
    assert!(matches!(
        verdict("ESCAPED: x", true, true),
        Outcome::Fail(_)
    ));
    assert!(matches!(
        verdict("ESCAPED: x", false, false),
        Outcome::Fail(_)
    ));
    // The first verdict counts.
    assert!(matches!(
        verdict("ESCAPED: a\nBLOCKED: b", false, false),
        Outcome::Fail(_)
    ));
}
