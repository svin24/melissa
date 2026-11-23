//! Agent users: every agent's sandbox runs as its own host user, the rootless-podman way.
//!
//! The server's user (`melissa`) owns a range of subordinate ids (`/etc/subuid`,
//! `/etc/subgid`). Each agent gets a slot of two uids and two gids from it, kept for good in
//! `<state>/agent-users.json` ([`Setup::user`]; never handed to another agent): the agent's own
//! id, and a setup id that only bwrap runs as while it builds the sandbox. Inside, the agent is
//! an ordinary user ([`AgentUser::inner_uid`], 1000 unless the server has that id) without
//! capabilities or supplementary groups, so whatever it creates is owned by its own id on the
//! host, and an agent that escapes its namespaces is a host user that owns nothing: not the
//! server's state, not another agent's processes or file box.
//!
//! How a sandbox starts ([`Setup::launch`], [`super::spawn_command`], [`super::command`]):
//!
//! 1. A holder process (`unshare --user cat`) makes an empty user namespace.
//! 2. `newuidmap`/`newgidmap` (shadow-utils' audited setuid/file-capability helpers, as rootless
//!    podman uses) map in it: root to the setup id, the agent's inside id to its host id, and the
//!    server's own id to itself (so the setup can reach the server's files it binds). Melissa
//!    runs no root code.
//! 3. `nsenter` joins that namespace as its root. That clears the server's supplementary groups
//!    (e.g. `melissa-kube`, which may read the cluster's kubeconfig) and keeps full capabilities
//!    *inside that namespace* for one `exec`: bwrap's, which makes the other namespaces and the
//!    mounts as before (no `--unshare-user`: the namespace is there), drops every capability but
//!    the three `setpriv` needs, sets `no_new_privs` and installs the seccomp filter.
//! 4. `setpriv` (util-linux), the first program inside, becomes the agent's inside uid/gid with
//!    no groups, an empty bounding set and no capabilities, then runs the bridge or the CLI.
//!    The holder exits once bwrap is in; nothing else ever runs as the setup id.
//!
//! Files ([`Setup::grant`]): the paths a sandbox binds stay the server's; POSIX ACLs let the
//! agent's uid in. Its working directory, file box and session store get the agent's uid on
//! the directory and, as the default ACL, on everything created in them (the agent's files
//! then name the server's uid, so the server reads and removes them). The first time, the whole
//! existing tree is granted (deepest first). Credential and config files bound from `$HOME`
//! get a grant on the file alone; directories bound from `$HOME` are not granted (they are read
//! with their "other" bits). An agent's files that it locked down itself are removed through
//! a namespace where the server is root and the agent's id is mapped ([`remove_dir_all`]).
//!
//! Off ([`Status::Off`], logged once, `Agent users: off: <why>` in the startup summary) when the
//! server runs as root, has no subordinate ids, lacks the helpers, runs with `no_new_privs`
//! (`NoNewPrivileges=` stops newuidmap), its state directory has no ACLs, or a trial mapping
//! fails. Sandboxes then run as the server's user, as before this existed.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{self, Read as _, Write as _},
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Mutex, OnceLock},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use super::acl::{self, Acl, Follow, R, RWX, W, X};

/// The key of sandboxes that belong to no agent (one-shot runs, readiness probes).
pub const SHARED: &str = "shared";

/// The allocation table, in the state directory.
const TABLE: &str = "agent-users.json";

/// `MELISSA_AGENT_USERS=off` turns agent users off (e.g. running from source as your own
/// user, whose CLI logins would otherwise get ACL entries for the agents' ids).
pub const OFF_VAR: &str = "MELISSA_AGENT_USERS";

/// Where setuid helpers live on NixOS (`security.wrappers`); searched before `/usr/bin`.
const WRAPPERS: &str = "/run/wrappers/bin";

/// A range of subordinate ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: u32,
    pub count: u32,
}

impl Range {
    /// The last id in the range.
    pub fn last(&self) -> u32 {
        self.start.saturating_add(self.count.saturating_sub(1))
    }

    fn contains(&self, id: u32) -> bool {
        id >= self.start && id <= self.last()
    }
}

/// The first range for `name` (or its numeric `uid`) in `/etc/subuid` or `/etc/subgid` text
/// (`name:start:count` lines).
pub fn subid_range(text: &str, name: Option<&str>, uid: u32) -> Option<Range> {
    let uid = uid.to_string();
    text.lines().find_map(|line| {
        let mut parts = line.trim().split(':');
        let (who, start, count) = (parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() || !(Some(who) == name || who == uid) {
            return None;
        }
        let range = Range {
            start: start.parse().ok()?,
            count: count.parse().ok()?,
        };
        (range.count > 0 && range.start.checked_add(range.count - 1).is_some()).then_some(range)
    })
}

/// The user name of `uid` in `/etc/passwd` text.
pub fn passwd_name(text: &str, uid: u32) -> Option<String> {
    let uid = uid.to_string();
    text.lines().find_map(|line| {
        let mut parts = line.split(':');
        let name = parts.next()?;
        let id = parts.nth(1)?;
        (id == uid).then(|| name.to_string())
    })
}

/// One agent's ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentUser {
    /// Whose: the file box key (`lead`, an agent id) or [`SHARED`].
    pub key: String,
    /// The agent's host uid and gid: what its processes and files are on the host.
    pub uid: u32,
    pub gid: u32,
    /// The setup ids: root of the sandbox's user namespace, which only bwrap runs as.
    pub root_uid: u32,
    pub root_gid: u32,
    /// The agent's uid and gid inside the sandbox.
    pub inner_uid: u32,
    pub inner_gid: u32,
    /// `setpriv`, the sandbox's first program (switches to the inside ids).
    pub setpriv: PathBuf,
    /// `/etc/passwd` and `/etc/group` for inside, naming the inside ids
    /// ([`Setup::identity`]); None: the host's.
    pub passwd: Option<PathBuf>,
    pub group: Option<PathBuf>,
}

impl AgentUser {
    /// `setpriv` and its options: become the agent, keep nothing.
    pub fn setpriv_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec![self.setpriv.clone().into()];
        args.extend(
            [
                "--reuid".to_string(),
                self.inner_uid.to_string(),
                "--regid".into(),
                self.inner_gid.to_string(),
                "--clear-groups".into(),
                "--bounding-set".into(),
                "-all".into(),
                "--inh-caps".into(),
                "-all".into(),
                "--".into(),
            ]
            .map(OsString::from),
        );
        args
    }
}

/// The capabilities bwrap keeps for `setpriv` (it drops everything else).
pub const SETPRIV_CAPS: [&str; 3] = ["CAP_SETUID", "CAP_SETGID", "CAP_SETPCAP"];

/// The agent's id inside: 1000, or the next one the server's own ids leave free (they are
/// mapped too).
pub fn inner_id(server_uid: u32, server_gid: u32) -> u32 {
    (1000..)
        .find(|id| *id != server_uid && *id != server_gid)
        .unwrap_or(1000)
}

/// Errors from agent users.
#[derive(Debug, thiserror::Error)]
pub enum UsersError {
    #[error("no host user left for `{0}`: every id in the server's subordinate range is taken")]
    Full(String),
    #[error("cannot save the agent user table {path}: {source}")]
    Save {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot start the user namespace holder: {0}")]
    Holder(String),
    #[error("{tool} failed: {why}")]
    Map { tool: String, why: String },
    #[error("cannot write the sandbox's passwd and group files: {0}")]
    Identity(#[source] io::Error),
    #[error("cannot let the agent's user into {path}: {source}")]
    Grant {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// The programs agent users need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tools {
    pub newuidmap: PathBuf,
    pub newgidmap: PathBuf,
    pub unshare: PathBuf,
    pub nsenter: PathBuf,
    pub cat: PathBuf,
    pub rm: PathBuf,
    /// Run inside the sandbox: a path its mounts show (`/usr`, or the Nix store).
    pub setpriv: PathBuf,
}

/// `name` in `dirs`, else on `PATH`.
fn find(name: &str, dirs: &[&str]) -> Option<PathBuf> {
    let fixed = dirs.iter().map(|d| Path::new(d).join(name));
    let path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    fixed
        .chain(path.into_iter().map(|d| d.join(name)))
        .find(|p| p.is_file())
}

impl Tools {
    fn find() -> Result<Self, String> {
        let need = |name: &str, dirs: &[&str]| {
            find(name, dirs).ok_or_else(|| {
                format!("`{name}` not found (shadow-utils' newuidmap/newgidmap, util-linux)")
            })
        };
        Ok(Self {
            newuidmap: need("newuidmap", &[WRAPPERS, "/usr/bin"])?,
            newgidmap: need("newgidmap", &[WRAPPERS, "/usr/bin"])?,
            unshare: need("unshare", &["/usr/bin"])?,
            nsenter: need("nsenter", &["/usr/bin"])?,
            cat: need("cat", &["/usr/bin"])?,
            rm: need("rm", &["/usr/bin"])?,
            setpriv: need("setpriv", &["/usr/bin", "/run/current-system/sw/bin"])?
                .canonicalize()
                .map_err(|e| format!("setpriv: {e}"))?,
        })
    }
}

/// The table: which slot of the range each key has. Slots are never handed to another key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Table {
    uid_start: u32,
    gid_start: u32,
    slots: BTreeMap<String, u32>,
}

/// Agent users are on: the server's ids, its subordinate ranges, the helpers, the table.
#[derive(Debug)]
pub struct Setup {
    server_uid: u32,
    server_gid: u32,
    /// The agent's ids inside ([`inner_id`]).
    inner: u32,
    /// `<state>/agent-users`: the passwd and group files for inside.
    dir: PathBuf,
    uids: Range,
    gids: Range,
    tools: Tools,
    table_path: PathBuf,
    table: Mutex<Table>,
}

/// Whether sandboxes get agent users.
#[derive(Debug)]
pub enum Status {
    Active(Box<Setup>),
    /// Why not (logged once, shown in the startup summary).
    Off(String),
}

static STATUS: OnceLock<Status> = OnceLock::new();

/// Set up agent users for this process's sandboxes (the server, once at startup; tests with
/// their own state directory). Logs the outcome once; later calls return the first result.
pub fn init(state: &Path) -> &'static Status {
    STATUS.get_or_init(|| {
        log(Setup::probe(state).map_or_else(Status::Off, |s| Status::Active(Box::new(s))))
    })
}

/// The status, if [`init`] ran.
pub fn status() -> Option<&'static Status> {
    STATUS.get()
}

/// The setup, when agent users are on.
pub fn active() -> Option<&'static Setup> {
    match STATUS.get() {
        Some(Status::Active(s)) => Some(s),
        _ => None,
    }
}

fn log(status: Status) -> Status {
    match &status {
        Status::Active(s) => tracing::info!(
            uids = %format!("{}-{}", s.uids.start, s.uids.last()),
            gids = %format!("{}-{}", s.gids.start, s.gids.last()),
            "agent sandboxes run as their own host users"
        ),
        Status::Off(why) => tracing::warn!(
            "agent sandboxes run as the server's own user (agent users off): {why}. They need \
             subordinate ids for the server's user (/etc/subuid, /etc/subgid) and newuidmap; \
             see docs/DEV-ENV.md"
        ),
    }
    status
}

fn lock(m: &Mutex<Table>) -> std::sync::MutexGuard<'_, Table> {
    // A panic while holding the lock leaves a consistent table (it is only replaced whole).
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// This process's uid and gid: the owner of its `/proc/self`.
fn own_ids() -> io::Result<(u32, u32)> {
    let meta = std::fs::metadata("/proc/self")?;
    Ok((meta.uid(), meta.gid()))
}

/// `no_new_privs` is set for this process (systemd `NoNewPrivileges=yes`): newuidmap's file
/// capabilities or setuid bit then do nothing.
fn no_new_privs() -> bool {
    std::fs::read_to_string("/proc/self/status").is_ok_and(|s| {
        s.lines().any(|l| {
            l.split_once(':')
                .is_some_and(|(k, v)| k == "NoNewPrivs" && v.trim() == "1")
        })
    })
}

impl Setup {
    /// Look at the host and try one mapping; `Err(why)` turns agent users off.
    fn probe(state: &Path) -> Result<Self, String> {
        if std::env::var_os(OFF_VAR).is_some_and(|v| v == "off") {
            return Err(format!("turned off ({OFF_VAR}=off)"));
        }
        let (uid, gid) = own_ids().map_err(|e| format!("cannot read this process's ids: {e}"))?;
        if uid == 0 || gid == 0 {
            return Err("the server runs as root".into());
        }
        let name = std::fs::read_to_string("/etc/passwd")
            .ok()
            .and_then(|t| passwd_name(&t, uid))
            .or_else(|| std::env::var("USER").ok());
        let who = name.clone().unwrap_or_else(|| uid.to_string());
        let range = |file: &str| {
            let text = std::fs::read_to_string(file).unwrap_or_default();
            subid_range(&text, name.as_deref(), uid)
                .ok_or_else(|| format!("no subordinate ids for {who} in {file}"))
        };
        let uids = range("/etc/subuid")?;
        let gids = range("/etc/subgid")?;
        if uids.contains(uid) || gids.contains(gid) {
            return Err(format!("{who}'s subordinate ids include its own id"));
        }
        let tools = Tools::find()?;
        if no_new_privs() {
            return Err(
                "this process has no_new_privs (NoNewPrivileges=yes), which stops \
                        newuidmap"
                    .into(),
            );
        }
        std::fs::create_dir_all(state)
            .map_err(|e| format!("cannot create {}: {e}", state.display()))?;
        acl_works(state, uids.start)?;
        let table_path = state.join(TABLE);
        let table = load_table(&table_path, uids, gids)?;
        let dir = state.join("agent-users");
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let setup = Self {
            server_uid: uid,
            server_gid: gid,
            inner: inner_id(uid, gid),
            dir,
            uids,
            gids,
            tools,
            table_path,
            table: Mutex::new(table),
        };
        // One real mapping, run to the end: proves newuidmap may use the range here.
        let probe = setup.at("probe", 0);
        let launch = setup.launch(&probe).map_err(|e| e.to_string())?;
        let out = Command::new(&launch.program)
            .args(&launch.args)
            .arg(&setup.tools.cat)
            .arg("/dev/null")
            .env_clear()
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run nsenter: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "nsenter into a mapped namespace failed: {}",
                first_line(&out.stderr)
            ));
        }
        Ok(setup)
    }

    /// The subordinate uid range.
    pub fn uids(&self) -> Range {
        self.uids
    }

    /// The subordinate gid range.
    pub fn gids(&self) -> Range {
        self.gids
    }

    /// How many agents can have a user (two ids each).
    pub fn capacity(&self) -> u32 {
        self.uids.count.min(self.gids.count) / 2
    }

    /// The host user of `key` (the file box key: `lead` or an agent id; [`SHARED`] for
    /// sandboxes without an owner), allocated on first use and kept for good.
    pub fn user(&self, key: &str) -> Result<AgentUser, UsersError> {
        let mut table = lock(&self.table);
        if let Some(slot) = table.slots.get(key) {
            return Ok(self.at(key, *slot));
        }
        let slot =
            next_free(&table, self.capacity()).ok_or_else(|| UsersError::Full(key.into()))?;
        let mut next = table.clone();
        next.slots.insert(key.to_string(), slot);
        save_table(&self.table_path, &next).map_err(|source| UsersError::Save {
            path: self.table_path.clone(),
            source,
        })?;
        *table = next;
        Ok(self.at(key, slot))
    }

    /// The host user of `key`, if it has one already (nothing is allocated).
    pub fn existing(&self, key: &str) -> Option<AgentUser> {
        lock(&self.table)
            .slots
            .get(key)
            .map(|slot| self.at(key, *slot))
    }

    fn at(&self, key: &str, slot: u32) -> AgentUser {
        AgentUser {
            key: key.to_string(),
            uid: self.uids.start + 2 * slot + 1,
            gid: self.gids.start + 2 * slot + 1,
            root_uid: self.uids.start + 2 * slot,
            root_gid: self.gids.start + 2 * slot,
            inner_uid: self.inner,
            inner_gid: self.inner,
            setpriv: self.tools.setpriv.clone(),
            passwd: None,
            group: None,
        }
    }

    /// `/etc/passwd` and `/etc/group` for a sandbox whose `$HOME` is `home`: the host's, with
    /// the agent's inside ids as user and group `agent` (git, `id` and `whoami` look them up).
    /// Written once per content (the name carries a hash of it), mode 0644.
    pub fn identity(&self, home: &Path) -> Result<(PathBuf, PathBuf), UsersError> {
        let host = |p: &str| std::fs::read_to_string(p).unwrap_or_default();
        let user_line = format!(
            "agent:x:{0}:{0}:Melissa agent:{1}:/bin/sh",
            self.inner,
            home.display()
        );
        let passwd = identity_file(&host("/etc/passwd"), self.inner, &user_line);
        let group_line = format!("agent:x:{}:", self.inner);
        let group = identity_file(&host("/etc/group"), self.inner, &group_line);
        let write = |name: &str, text: &str| -> io::Result<PathBuf> {
            use std::{
                hash::{Hash, Hasher},
                os::unix::fs::PermissionsExt,
            };
            let mut h = std::collections::hash_map::DefaultHasher::new();
            text.hash(&mut h);
            let path = self.dir.join(format!("{name}-{:016x}", h.finish()));
            if !path.exists() {
                let tmp = path.with_extension("tmp");
                std::fs::write(&tmp, text)?;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))?;
                std::fs::rename(&tmp, &path)?;
            }
            Ok(path)
        };
        let p = write("passwd", &passwd).map_err(UsersError::Identity)?;
        let g = write("group", &group).map_err(UsersError::Identity)?;
        Ok((p, g))
    }

    /// The server's own uid and gid.
    pub fn server(&self) -> (u32, u32) {
        (self.server_uid, self.server_gid)
    }

    /// A user namespace for `user`'s sandbox, and the `nsenter` command line that runs a
    /// program in it as that user, with the capabilities bwrap needs to set up (inside the
    /// namespace only) and no supplementary groups.
    pub fn launch(&self, user: &AgentUser) -> Result<Launch, UsersError> {
        let (uids, gids) = sandbox_maps(user, self.server_uid, self.server_gid);
        self.enter(&uids, &gids)
    }

    fn enter(&self, uids: &[[u32; 3]], gids: &[[u32; 3]]) -> Result<Launch, UsersError> {
        let holder = Holder::start(&self.tools)?;
        holder.map(&self.tools.newuidmap, uids)?;
        holder.map(&self.tools.newgidmap, gids)?;
        let mut ns = OsString::from("--user=");
        ns.push(holder.ns_path());
        let args = vec![
            ns,
            "--setuid".into(),
            "0".into(),
            "--setgid".into(),
            "0".into(),
            "--".into(),
        ];
        Ok(Launch {
            program: self.tools.nsenter.clone(),
            args,
            holder,
        })
    }

    /// Let `user` into what `plan` binds (see the module docs). Called by
    /// [`super::spawn_command`] for every sandbox, before it starts.
    pub fn grant(
        &self,
        plan: &super::SandboxPlan,
        user: &AgentUser,
        workdir: &Path,
        extra_ro: &[PathBuf],
    ) -> Result<(), UsersError> {
        let server = self.server_uid;
        let agent = user.uid;
        let err = |path: &Path| {
            let path = path.to_path_buf();
            move |source| UsersError::Grant { path, source }
        };
        // Its own trees: the working directory, the file box, the session store.
        let trees = std::iter::once(workdir.to_path_buf())
            .chain(plan.rw_dirs.iter().cloned())
            .chain(plan.mounts.iter().filter_map(|m| match m {
                super::Mount::BindAt { src, .. } => Some(src.clone()),
                _ => None,
            }));
        for root in trees {
            share_tree(&root, agent, server).map_err(err(&root))?;
        }
        // bwrap changes into the working directory as the setup id, after its setup.
        acl::grant(workdir, user.root_uid, X, Follow::No).map_err(err(workdir))?;
        // Single files from $HOME (credentials, settings), the CLI binary, extra files.
        let mut files: Vec<(PathBuf, u16, Follow)> = Vec::new();
        for m in &plan.mounts {
            match m {
                super::Mount::ReadWrite(p) => files.push((p.clone(), R | W, Follow::Yes)),
                super::Mount::ReadOnly(p) => files.push((p.clone(), R, Follow::Yes)),
                super::Mount::Tmpfs(_) | super::Mount::BindAt { .. } => {}
            }
        }
        files.push((plan.program.clone(), R, Follow::Yes));
        for f in extra_ro {
            // Inside the agent's own workdir a path may be the agent's symlink: never follow.
            let follow = if f.starts_with(workdir) {
                Follow::No
            } else {
                Follow::Yes
            };
            files.push((f.clone(), R, follow));
        }
        for (path, perm, follow) in files {
            share_file(&path, agent, server, perm, follow).map_err(err(&path))?;
        }
        // The bridge's sockets (connecting needs write access) and its resolv.conf.
        if let super::Network::Isolated(Some(bridge)) = &plan.network {
            let dir = &bridge.socket_dir;
            acl::grant(dir, agent, R | X, Follow::No).map_err(err(dir))?;
            for entry in std::fs::read_dir(dir).map_err(err(dir))? {
                let entry = entry.map_err(err(dir))?;
                let socket = entry.file_type().is_ok_and(|t| {
                    use std::os::unix::fs::FileTypeExt;
                    t.is_socket()
                });
                let perm = if socket { R | W } else { R };
                let path = entry.path();
                share_file(&path, agent, server, perm, Follow::No).map_err(err(&path))?;
            }
        }
        Ok(())
    }
}

/// The lowest slot no key has.
fn next_free(table: &Table, capacity: u32) -> Option<u32> {
    let used: std::collections::BTreeSet<u32> = table.slots.values().copied().collect();
    (0..capacity).find(|s| !used.contains(s))
}

/// The id maps of a sandbox's namespace (`[inside, host, count]`): root is the setup id, the
/// agent's inside id its host id, and the server's own ids map to themselves, so bwrap's setup
/// (privileged inside the namespace only) can reach the server's files it binds.
pub fn sandbox_maps(
    user: &AgentUser,
    server_uid: u32,
    server_gid: u32,
) -> (Vec<[u32; 3]>, Vec<[u32; 3]>) {
    (
        vec![
            [0, user.root_uid, 1],
            [user.inner_uid, user.uid, 1],
            [server_uid, server_uid, 1],
        ],
        vec![
            [0, user.root_gid, 1],
            [user.inner_gid, user.gid, 1],
            [server_gid, server_gid, 1],
        ],
    )
}

/// The host's passwd or group `text` without lines for id `id` (third field) or the name
/// `agent`, plus `line`.
fn identity_file(text: &str, id: u32, line: &str) -> String {
    let id = id.to_string();
    let mut out: String = text
        .lines()
        .filter(|l| {
            let mut f = l.split(':');
            let name = f.next().unwrap_or_default();
            name != "agent" && f.nth(1) != Some(id.as_str())
        })
        .flat_map(|l| [l, "\n"])
        .collect();
    out.push_str(line);
    out.push('\n');
    out
}

/// The id maps of the clean-up namespace: root is the server, 1 is the agent.
pub fn cleanup_maps(
    user: &AgentUser,
    server_uid: u32,
    server_gid: u32,
) -> (Vec<[u32; 3]>, Vec<[u32; 3]>) {
    (
        vec![[0, server_uid, 1], [1, user.uid, 1]],
        vec![[0, server_gid, 1], [1, user.gid, 1]],
    )
}

fn load_table(path: &Path, uids: Range, gids: Range) -> Result<Table, String> {
    let fresh = Table {
        uid_start: uids.start,
        gid_start: gids.start,
        slots: BTreeMap::new(),
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(fresh),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let table: Table =
        serde_json::from_str(&text).map_err(|e| format!("{} is invalid: {e}", path.display()))?;
    let fits = table.uid_start == uids.start
        && table.gid_start == gids.start
        && table
            .slots
            .values()
            .all(|s| *s < uids.count.min(gids.count) / 2);
    if !fits {
        return Err(format!(
            "the subordinate id range changed since {} was written (files the agents made \
             belong to the old ids); restore the old range in /etc/subuid and /etc/subgid",
            path.display()
        ));
    }
    Ok(table)
}

fn save_table(path: &Path, table: &Table) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let body = serde_json::to_vec_pretty(table).map_err(io::Error::other)?;
    // One temporary name per process: two processes sharing a state dir (the escape suite's
    // parallel tests) would otherwise rename each other's file away.
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(&body)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// The state directory takes ACLs (a throw-away file, granted to an id of the range).
fn acl_works(state: &Path, uid: u32) -> Result<(), String> {
    let file = state.join(".agent-users-acl-check");
    let _ = std::fs::remove_file(&file);
    let res = std::fs::write(&file, b"")
        .and_then(|()| acl::grant(&file, uid, R, Follow::No))
        .and_then(|_| acl::access(&file, Follow::No))
        .map(|a| a.grants(uid, R));
    let _ = std::fs::remove_file(&file);
    match res {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!("{} kept no ACL", state.display())),
        Err(e) => Err(format!("{} has no POSIX ACLs: {e}", state.display())),
    }
}

/// Owner rights of `mode`, as ACL bits.
fn owner_bits(mode: u32) -> u16 {
    ((mode >> 6) & 7) as u16
}

/// Let `agent` into the tree at `root` (owned by `server`): the root and its direct entries
/// every time (files the server just made with a strict mode), everything the first time;
/// directories also get the shared default ACL. Never follows symlinks: the agent can write
/// in here.
fn share_tree(root: &Path, agent: u32, server: u32) -> io::Result<()> {
    let default = Acl::shared_default(&[agent, server]);
    let first = !acl::access(root, Follow::No)?.grants(agent, RWX);
    // Deepest first, the root last: the agent gets into a directory only once all of it is done.
    walk(
        root,
        agent,
        server,
        &default,
        if first { usize::MAX } else { 1 },
    )?;
    acl::set_default(root, &default, Follow::No)?;
    acl::grant(root, agent, RWX, Follow::No)?;
    Ok(())
}

fn walk(dir: &Path, agent: u32, server: u32, default: &Acl, depth: usize) -> io::Result<()> {
    if depth == 0 {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        // Only the server's own entries: the agent's are its own already; symlinks carry no ACL.
        if meta.uid() != server || meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            walk(&path, agent, server, default, depth - 1)?;
            acl::set_default(&path, default, Follow::No)?;
        }
        acl::grant(&path, agent, owner_bits(meta.mode()), Follow::No)?;
    }
    Ok(())
}

/// Let `agent` have `perm` (plus execute where the owner has it) on one file of `server`'s.
/// Files of anyone else keep their own permissions (only the owner can set an ACL), and so do
/// directories (a bound directory is read with its "other" bits).
fn share_file(path: &Path, agent: u32, server: u32, perm: u16, follow: Follow) -> io::Result<()> {
    let meta = match follow {
        Follow::Yes => std::fs::metadata(path),
        Follow::No => std::fs::symlink_metadata(path),
    };
    let meta = match meta {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if meta.uid() != server || meta.file_type().is_symlink() || meta.is_dir() {
        return Ok(());
    }
    let exec = if owner_bits(meta.mode()) & X != 0 {
        X
    } else {
        0
    };
    let want = perm | exec;
    // Everyone has that already (a CLI binary, a world-readable config): leave the file alone.
    let other = (meta.mode() & 7) as u16;
    if other & want == want && acl::access(path, follow)?.user(agent).is_none() {
        return Ok(());
    }
    acl::grant(path, agent, want, follow).map(|_| ())
}

/// A process that only holds a fresh, empty user namespace (`unshare --user cat`) until the
/// sandbox has joined it. Killed when dropped.
#[derive(Debug)]
pub struct Holder {
    child: Child,
}

impl Holder {
    fn start(tools: &Tools) -> Result<Self, UsersError> {
        let mut child = Command::new(&tools.unshare)
            .arg("--user")
            .arg(&tools.cat)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| UsersError::Holder(format!("{}: {e}", tools.unshare.display())))?;
        // `cat` echoes the byte once it runs, i.e. once unshare has made the namespace.
        let echoed = match (child.stdin.as_mut(), child.stdout.as_mut()) {
            (Some(i), Some(o)) => i.write_all(b"x").and_then(|()| {
                let mut b = [0u8; 1];
                o.read(&mut b)
            }),
            _ => Ok(0),
        };
        let mut holder = Self { child };
        if !matches!(echoed, Ok(1)) {
            let mut err = String::new();
            let _ = holder.child.kill();
            if let Some(e) = holder.child.stderr.as_mut() {
                let _ = e.read_to_string(&mut err);
            }
            return Err(UsersError::Holder(first_line(err.as_bytes())));
        }
        Ok(holder)
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// `/proc/<pid>/ns/user` of the holder.
    pub fn ns_path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/{}/ns/user", self.pid()))
    }

    /// Which namespace that is (`user:[inode]`).
    fn ns_id(&self) -> Option<PathBuf> {
        std::fs::read_link(self.ns_path()).ok()
    }

    fn map(&self, tool: &Path, lines: &[[u32; 3]]) -> Result<(), UsersError> {
        let out = Command::new(tool)
            .arg(self.pid().to_string())
            .args(lines.iter().flatten().map(u32::to_string))
            .env_clear()
            .stdin(Stdio::null())
            .output()
            .map_err(|e| UsersError::Map {
                tool: tool.display().to_string(),
                why: e.to_string(),
            })?;
        if !out.status.success() {
            return Err(UsersError::Map {
                tool: tool.display().to_string(),
                why: first_line(&out.stderr),
            });
        }
        Ok(())
    }

    /// Drop this holder once process `pid` has joined its namespace (or is gone), from a
    /// short-lived thread: the sandbox's own processes keep the namespace alive from then on.
    pub fn release_when_joined(self, pid: Option<u32>) {
        let Some(pid) = pid else { return };
        let Some(ns) = self.ns_id() else { return };
        let joined = move || {
            let link = std::fs::read_link(format!("/proc/{pid}/ns/user"));
            !matches!(link, Ok(l) if l != ns)
        };
        if joined() {
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("melissa-userns".into())
            .spawn(move || {
                let holder = self;
                for _ in 0..500 {
                    if joined() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                drop(holder);
            });
        if let Err(e) = spawned {
            tracing::warn!("cannot wait for the sandbox to join its user namespace: {e}");
        }
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// How to start a program in a sandbox's user namespace: `program args.. <the program>`.
#[derive(Debug)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub holder: Holder,
}

fn first_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.is_empty() {
        "no error message".into()
    } else {
        line.to_string()
    }
}

/// Remove the tree at `dir` that agent `key` may have written into. Plain removal first; if
/// the agent locked some of it (its own files with no rights for the server), the rest goes in
/// a namespace where the server is root and the agent's id is mapped (`rm -rf` as that root:
/// it may remove what either of them owns). A missing `dir` is fine.
pub fn remove_dir_all(dir: &Path, key: &str) -> io::Result<()> {
    let denied = match std::fs::remove_dir_all(dir) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => e,
        Err(e) => return Err(e),
    };
    let Some((setup, user)) = active().and_then(|s| Some((s, s.existing(key)?))) else {
        return Err(denied);
    };
    let (uids, gids) = cleanup_maps(&user, setup.server_uid, setup.server_gid);
    let launch = setup.enter(&uids, &gids).map_err(io::Error::other)?;
    let out = Command::new(&launch.program)
        .args(&launch.args)
        .arg(&setup.tools.rm)
        .args(["-rf", "--one-file-system", "--"])
        .arg(dir)
        .env_clear()
        .stdin(Stdio::null())
        .output()?;
    drop(launch);
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "rm in the agent's namespace failed: {}",
            first_line(&out.stderr)
        )));
    }
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        _ => Err(denied),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subid_ranges_by_name_or_uid() {
        let text = "alice:100000:65536\n# x\nmelissa:1879048192:65536\n990:5:3\nbad:1\n";
        assert_eq!(
            subid_range(text, Some("melissa"), 990),
            Some(Range {
                start: 1879048192,
                count: 65536
            })
        );
        // By number when the name has no line.
        assert_eq!(
            subid_range(text, Some("other"), 990),
            Some(Range { start: 5, count: 3 })
        );
        assert_eq!(subid_range(text, Some("bob"), 7), None);
        assert_eq!(subid_range("x:1:0\n", Some("x"), 1), None, "empty range");
        assert_eq!(
            subid_range("x:4294967295:2\n", Some("x"), 1),
            None,
            "overflowing range"
        );
        let r = Range {
            start: 10,
            count: 5,
        };
        assert_eq!(r.last(), 14);
        assert!(r.contains(10) && r.contains(14) && !r.contains(15) && !r.contains(9));
    }

    #[test]
    fn passwd_names() {
        let text = "root:x:0:0:root:/root:/bin/sh\nmelissa:x:990:990::/var/lib/melissa/home:/sbin/nologin\n";
        assert_eq!(passwd_name(text, 990).as_deref(), Some("melissa"));
        assert_eq!(passwd_name(text, 0).as_deref(), Some("root"));
        assert_eq!(passwd_name(text, 5), None);
    }

    #[test]
    fn maps_give_root_to_the_setup_id_and_keep_the_server() {
        let user = AgentUser {
            key: "d.a1".into(),
            uid: 1_879_048_193,
            gid: 1_879_048_194,
            root_uid: 1_879_048_192,
            root_gid: 1_879_048_195,
            inner_uid: 1000,
            inner_gid: 1000,
            setpriv: "/usr/bin/setpriv".into(),
            passwd: None,
            group: None,
        };
        let (u, g) = sandbox_maps(&user, 990, 991);
        assert_eq!(
            u,
            vec![
                [0, 1_879_048_192, 1],
                [1000, 1_879_048_193, 1],
                [990, 990, 1]
            ]
        );
        assert_eq!(
            g,
            vec![
                [0, 1_879_048_195, 1],
                [1000, 1_879_048_194, 1],
                [991, 991, 1]
            ]
        );
        let args: Vec<String> = user
            .setpriv_args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args.first().map(String::as_str), Some("/usr/bin/setpriv"));
        assert_eq!(args.last().map(String::as_str), Some("--"));
        for flag in ["--clear-groups", "--reuid", "--regid", "--bounding-set"] {
            assert!(args.iter().any(|a| a == flag), "{flag}: {args:?}");
        }
        let (u, g) = cleanup_maps(&user, 990, 991);
        assert_eq!(u, vec![[0, 990, 1], [1, 1_879_048_193, 1]]);
        assert_eq!(g, vec![[0, 991, 1], [1, 1_879_048_194, 1]]);
    }

    #[test]
    fn slots_are_never_reused_and_the_table_checks_the_range() {
        let mut t = Table::default();
        assert_eq!(next_free(&t, 3), Some(0));
        t.slots.insert("lead".into(), 0);
        t.slots.insert("d.a2".into(), 2);
        assert_eq!(next_free(&t, 3), Some(1));
        t.slots.insert("d.a3".into(), 1);
        assert_eq!(next_free(&t, 3), None);

        let dir = std::env::temp_dir().join(format!("melissa-users-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(TABLE);
        // Two ids per slot: three slots.
        let r = Range {
            start: 1000,
            count: 6,
        };
        assert_eq!(load_table(&path, r, r).unwrap().slots.len(), 0);
        let t = Table {
            uid_start: 1000,
            gid_start: 1000,
            ..t
        };
        save_table(&path, &t).unwrap();
        assert_eq!(load_table(&path, r, r).unwrap(), t);
        // Another range: refused, the files the agents made belong to the old ids.
        let moved = Range {
            start: 2000,
            count: 6,
        };
        assert!(load_table(&path, moved, moved).is_err());
        let small = Range {
            start: 1000,
            count: 4,
        };
        assert!(load_table(&path, small, small).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_inside_id_avoids_the_servers() {
        assert_eq!(inner_id(990, 990), 1000);
        assert_eq!(inner_id(1000, 1000), 1001);
        assert_eq!(inner_id(1001, 1000), 1002);
    }

    #[test]
    fn identity_files_name_the_agent_once() {
        let passwd =
            "root:x:0:0::/root:/bin/sh\nme:x:1000:1000::/home/me:/bin/sh\nagent:x:5:5::/:/bin/sh\n";
        let out = identity_file(passwd, 1000, "agent:x:1000:1000:Melissa agent:/h:/bin/sh");
        assert_eq!(
            out,
            "root:x:0:0::/root:/bin/sh\nagent:x:1000:1000:Melissa agent:/h:/bin/sh\n"
        );
        let group = identity_file("wheel:x:10:me\nme:x:1000:\n", 1000, "agent:x:1000:");
        assert_eq!(group, "wheel:x:10:me\nagent:x:1000:\n");
    }

    #[test]
    fn owner_bits_are_the_acl_rights() {
        assert_eq!(owner_bits(0o750), RWX);
        assert_eq!(owner_bits(0o640), R | W);
        assert_eq!(owner_bits(0o100), X);
    }
}
