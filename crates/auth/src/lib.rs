#![forbid(unsafe_code)]
//! Melissa's logins (PLAN 6c, step 1): accounts with username + password (Argon2id), the
//! owner's claim with a one-time setup code from the host, single-use invites and password
//! reset links, browser sessions and a recent-password check for sensitive actions.
//!
//! Everything lives in one file, `<state>/auth/auth.json` (directory 0700, file 0600): the
//! accounts with their password hashes, pending invites and the signed-in sessions. Tokens
//! (session cookies, invite links) are stored only as SHA-256 hashes. The setup code of a
//! box nobody has claimed yet sits next to it in `setup-code.json` (0600) until it is used
//! or expires, so a restart keeps the code the install printed. The failed sign-in counters
//! stay in memory.
//!
//! Hashing is slow on purpose: call the methods that take a password from a blocking
//! thread (`tokio::task::spawn_blocking`). The lock is never held while hashing.

mod limits;
mod password;
mod secret;

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

pub use limits::LOCKOUT_SECS;
pub use password::Cost;
pub use secret::constant_time_eq;

/// A session ends after this long without a request.
pub const SESSION_IDLE_SECS: u64 = 7 * 24 * 3600;
/// A session ends this long after sign-in, used or not.
pub const SESSION_MAX_SECS: u64 = 30 * 24 * 3600;
/// A password entered again counts for sensitive actions this long.
pub const REAUTH_SECS: u64 = 5 * 60;
/// A setup code works this long (a new one comes with a restart or `melissa-server setup-code`).
pub const SETUP_CODE_SECS: u64 = 3600;
/// Wrong setup codes before the code stops working.
pub const SETUP_CODE_TRIES: u32 = 10;
/// An invite link works this long.
pub const INVITE_SECS: u64 = 24 * 3600;
/// A password reset link from the owner works this long.
pub const RESET_SECS: u64 = 24 * 3600;
/// The host's owner reset link works this long.
pub const OWNER_RESET_SECS: u64 = 3600;
/// `last_seen` is written to disk at most this often per session.
const TOUCH_SECS: u64 = 300;
/// Passwords: at least this many characters (no other rules; NIST SP 800-63B).
pub const MIN_PASSWORD: usize = 8;
const MAX_PASSWORD: usize = 256;
const MAX_USERNAME: usize = 32;

const FILE_NAME: &str = "auth.json";
const SETUP_FILE_NAME: &str = "setup-code.json";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuthError {
    #[error("wrong username or password")]
    WrongCredentials,
    #[error("too many tries; wait {wait_secs} s")]
    SlowDown { wait_secs: u64 },
    #[error("too many failed tries; try again in {} min", wait_secs.div_ceil(60))]
    Locked { wait_secs: u64 },
    #[error("this box already has an owner")]
    AlreadyClaimed,
    #[error("wrong setup code")]
    WrongSetupCode,
    #[error("the setup code expired; get a new one on the host")]
    SetupCodeExpired,
    #[error("this link is used, expired or withdrawn")]
    InviteInvalid,
    #[error("that username is taken")]
    UsernameTaken,
    #[error("{0}")]
    UsernameInvalid(&'static str),
    #[error("{0}")]
    PasswordInvalid(&'static str),
    #[error("no such person")]
    NotFound,
    #[error("the owner can't be removed")]
    OwnerProtected,
    #[error("storage: {0}")]
    Io(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Exactly one; claimed the box; can't be removed or demoted.
    Owner,
    Member,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Member => "member",
        }
    }
}

/// A person as others see them (no hash).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Person {
    pub id: String,
    pub username: String,
    pub role: Role,
    /// Unix seconds.
    pub created: u64,
}

impl Person {
    pub fn is_owner(&self) -> bool {
        self.role == Role::Owner
    }
}

/// A signed-in session, as the web layer sees it on each request.
#[derive(Clone, Debug)]
pub struct SessionView {
    pub person: Person,
    /// The session's public id (for the session list), not its cookie.
    pub id: String,
    /// The CSRF token for this session's forms and htmx requests.
    pub csrf: String,
    /// The password was entered again within [`REAUTH_SECS`].
    pub reauth_fresh: bool,
}

/// What a successful sign-in returns: the cookie value (only here, never stored) and the view.
#[derive(Clone, Debug)]
pub struct NewSession {
    pub token: String,
    pub view: SessionView,
}

/// One of a person's sessions, for "Signed in on".
#[derive(Clone, Debug, Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub label: String,
    pub created: u64,
    pub last_seen: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InviteKind {
    /// Join as a member with a new username.
    Join,
    /// Set a new password for this account.
    Reset { account: String },
}

/// A pending invite or reset link (never its token).
#[derive(Clone, Debug, Serialize)]
pub struct InviteInfo {
    pub id: String,
    pub kind: InviteKind,
    /// For a reset: whose password.
    pub username: Option<String>,
    pub created: u64,
    pub expires: u64,
}

/// A new invite or reset link: the token goes in the link, shown once.
#[derive(Clone, Debug)]
pub struct NewInvite {
    pub token: String,
    pub info: InviteInfo,
}

/// Something worth an audit line. `person` is who did it (or tried: the username typed).
#[derive(Clone, Debug)]
pub struct AuthEvent {
    pub action: &'static str,
    pub person: String,
    pub source: String,
    pub detail: String,
}

pub type EventSink = Arc<dyn Fn(&AuthEvent) + Send + Sync>;
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

#[derive(Clone, Serialize, Deserialize)]
struct Account {
    id: String,
    username: String,
    role: Role,
    hash: String,
    created: u64,
}

impl Account {
    fn person(&self) -> Person {
        Person {
            id: self.id.clone(),
            username: self.username.clone(),
            role: self.role,
            created: self.created,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Invite {
    id: String,
    /// SHA-256 of the token.
    hash: String,
    kind: InviteKind,
    created: u64,
    expires: u64,
    /// Account id of who made it, or `host`.
    by: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct Session {
    id: String,
    /// SHA-256 of the cookie value.
    hash: String,
    account: String,
    created: u64,
    last_seen: u64,
    #[serde(default)]
    reauth: u64,
    csrf: String,
    label: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Data {
    #[serde(default)]
    accounts: Vec<Account>,
    #[serde(default)]
    invites: Vec<Invite>,
    #[serde(default)]
    sessions: Vec<Session>,
}

#[derive(Serialize, Deserialize)]
struct SetupCode {
    code: String,
    expires: u64,
    #[serde(default)]
    failures: u32,
}

struct Inner {
    data: Data,
    setup: Option<SetupCode>,
    limits: limits::Limits,
}

/// The login store. Cheap to clone; clones share state.
#[derive(Clone)]
pub struct Auth {
    inner: Arc<Mutex<Inner>>,
    /// `None`: in memory only (tests).
    path: Option<PathBuf>,
    clock: Clock,
    cost: Cost,
    events: Option<EventSink>,
}

fn system_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn io(e: impl std::fmt::Display) -> AuthError {
    AuthError::Io(e.to_string())
}

/// `auth.json`'s format (melissa-statefile). A newer Melissa's file (after a rollback) is read
/// as well as this version can and never written: existing sessions keep working, anything
/// that must be saved (a sign-in, a new password) fails with a message saying why.
///
/// So a change that takes access away (an account switched off, a narrower role) must not
/// rely on a new field an older Melissa would ignore: bump the format **and** make the file
/// unreadable to the older version (e.g. change a required field's type), so it refuses to
/// start instead of letting someone in.
pub const AUTH_FORMAT: melissa_statefile::JsonFormat = melissa_statefile::Format {
    name: "login store",
    current: 1,
    migrations: &[],
    v1_marker: true,
};

/// `setup-code.json`'s format.
pub const SETUP_CODE_FORMAT: melissa_statefile::JsonFormat = melissa_statefile::Format {
    name: "setup code",
    current: 1,
    migrations: &[],
    v1_marker: true,
};

/// Save `value` to `path` (atomically, 0600) in format `fmt`, unless a newer Melissa wrote it.
fn save_json<T: Serialize>(
    path: &Path,
    fmt: &melissa_statefile::JsonFormat,
    value: &T,
) -> Result<(), AuthError> {
    melissa_statefile::save_json(path, fmt, value, true).map_err(io)
}

impl Auth {
    /// Open (or start) the store in `dir` (created 0700; the file is 0600).
    pub fn open(dir: &Path) -> Result<Self, AuthError> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(io)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(io)?;
        let path = dir.join(FILE_NAME);
        let data = match std::fs::read(&path) {
            Ok(bytes) => {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                    .map_err(io)?;
                melissa_statefile::parse_json::<Data>(&path, &AUTH_FORMAT, &bytes)
                    .map_err(io)?
                    .value
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Data::default(),
            Err(e) => return Err(io(e)),
        };
        let claimed = data.accounts.iter().any(|a| a.role == Role::Owner);
        let auth = Self::with_data(data, Some(path));
        if claimed {
            auth.forget_setup_code()?;
        } else {
            // Kept even when expired: `setup_code` makes a new one then (the clock may be a
            // test's, set after opening).
            auth.lock().setup = auth.read_setup_code();
        }
        Ok(auth)
    }

    /// Where the unclaimed box's setup code is kept.
    fn setup_path(&self) -> Option<PathBuf> {
        self.path
            .as_ref()
            .map(|p| p.with_file_name(SETUP_FILE_NAME))
    }

    /// The saved setup code, if there is a readable one.
    fn read_setup_code(&self) -> Option<SetupCode> {
        let path = self.setup_path()?;
        match melissa_statefile::read_json(&path, &SETUP_CODE_FORMAT) {
            Ok(code) => code.map(|l| l.value),
            Err(e) => {
                tracing::warn!("{e}; a new setup code is made");
                None
            }
        }
    }

    /// Save the setup code (atomically, 0600), or remove the file for `None`.
    fn write_setup_code(&self, code: Option<&SetupCode>) -> Result<(), AuthError> {
        let Some(path) = self.setup_path() else {
            return Ok(());
        };
        let Some(code) = code else {
            return match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io(e)),
                _ => Ok(()),
            };
        };
        save_json(&path, &SETUP_CODE_FORMAT, code)
    }

    fn forget_setup_code(&self) -> Result<(), AuthError> {
        self.write_setup_code(None)
    }

    /// A store that is never written to disk (tests, previews).
    pub fn in_memory() -> Self {
        Self::with_data(Data::default(), None)
    }

    fn with_data(data: Data, path: Option<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                data,
                setup: None,
                limits: limits::Limits::default(),
            })),
            path,
            clock: Arc::new(system_now),
            cost: Cost::Standard,
            events: None,
        }
    }

    /// Use another clock (tests).
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Hash new passwords at this cost ([`Cost::Test`] in tests).
    pub fn with_cost(mut self, cost: Cost) -> Self {
        self.cost = cost;
        self
    }

    /// Where audit-worthy events go.
    pub fn with_events(mut self, sink: EventSink) -> Self {
        self.events = Some(sink);
        self
    }

    /// Seconds since the epoch on this store's clock (a test's own clock when set).
    pub fn now(&self) -> u64 {
        (self.clock)()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while holding the lock leaves plain data behind; keep serving.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn emit(&self, action: &'static str, person: &str, source: &str, detail: impl Into<String>) {
        if let Some(sink) = &self.events {
            sink(&AuthEvent {
                action,
                person: person.to_string(),
                source: source.to_string(),
                detail: detail.into(),
            });
        }
    }

    /// Drop what has expired and write the file (atomically, 0600).
    fn save(&self, inner: &mut Inner) -> Result<(), AuthError> {
        let now = self.now();
        inner.data.invites.retain(|i| i.expires > now);
        inner.data.sessions.retain(|s| session_alive(s, now));
        let Some(path) = &self.path else {
            return Ok(());
        };
        save_json(path, &AUTH_FORMAT, &inner.data)
    }

    fn hash(&self, password: &str) -> Result<String, AuthError> {
        password::hash(password, self.cost)
    }

    /// A hash to verify against when the username is unknown, so the answer takes as long.
    fn dummy_hash(&self) -> &'static str {
        static STANDARD: OnceLock<String> = OnceLock::new();
        static TEST: OnceLock<String> = OnceLock::new();
        let cell = match self.cost {
            Cost::Standard => &STANDARD,
            Cost::Test => &TEST,
        };
        cell.get_or_init(|| self.hash("not a real password").unwrap_or_default())
    }

    // --- Owner claim ----------------------------------------------------------------------

    /// Whether the box has its owner.
    pub fn has_owner(&self) -> bool {
        self.lock()
            .data
            .accounts
            .iter()
            .any(|a| a.role == Role::Owner)
    }

    /// The owner, once claimed.
    pub fn owner(&self) -> Option<Person> {
        self.lock()
            .data
            .accounts
            .iter()
            .find(|a| a.role == Role::Owner)
            .map(Account::person)
    }

    /// The current setup code and when it expires; a new one when there is none or it
    /// expired or was guessed at too often. `None` once the box has an owner.
    pub fn setup_code(&self) -> Result<Option<(String, u64)>, AuthError> {
        let now = self.now();
        let mut inner = self.lock();
        if inner.data.accounts.iter().any(|a| a.role == Role::Owner) {
            inner.setup = None;
            self.forget_setup_code()?;
            return Ok(None);
        }
        let usable = inner
            .setup
            .as_ref()
            .is_some_and(|s| s.expires > now && s.failures < SETUP_CODE_TRIES);
        if !usable {
            let fresh = SetupCode {
                code: secret::setup_code().map_err(io)?,
                expires: now + SETUP_CODE_SECS,
                failures: 0,
            };
            self.write_setup_code(Some(&fresh))?;
            inner.setup = Some(fresh);
        }
        Ok(inner.setup.as_ref().map(|s| (s.code.clone(), s.expires)))
    }

    /// Claim the box: the setup code, then the owner's username and password. Works once.
    pub fn claim(
        &self,
        code: &str,
        username: &str,
        password: &str,
        source: &str,
        label: &str,
    ) -> Result<NewSession, AuthError> {
        let username = check_username(username)?;
        check_password(password, &username)?;
        let now = self.now();
        {
            let mut inner = self.lock();
            if inner.data.accounts.iter().any(|a| a.role == Role::Owner) {
                return Err(AuthError::AlreadyClaimed);
            }
            let key = source_key(source);
            inner.limits.check(&key, limits::PER_SOURCE, now)?;
            let typed = secret::normalize_code(code);
            let outcome = match &mut inner.setup {
                None => Err(AuthError::SetupCodeExpired),
                Some(s) if s.expires <= now || s.failures >= SETUP_CODE_TRIES => {
                    Err(AuthError::SetupCodeExpired)
                }
                Some(s) => {
                    if constant_time_eq(
                        secret::normalize_code(&s.code).as_bytes(),
                        typed.as_bytes(),
                    ) {
                        Ok(())
                    } else {
                        s.failures += 1;
                        Err(AuthError::WrongSetupCode)
                    }
                }
            };
            if let Err(e) = outcome {
                if e == AuthError::WrongSetupCode {
                    // The count survives a restart too (best effort: the limit in memory holds).
                    if let Err(w) = self.write_setup_code(inner.setup.as_ref()) {
                        tracing::warn!("saving the setup code's tries: {w}");
                    }
                    inner.limits.fail(&key, limits::PER_SOURCE, now);
                    drop(inner);
                    self.emit("auth.claim_failed", &username, source, "wrong setup code");
                }
                return Err(e);
            }
        }
        let hash = self.hash(password)?;
        let mut inner = self.lock();
        // Checked again: another claim may have won while this one hashed.
        if inner.data.accounts.iter().any(|a| a.role == Role::Owner) {
            return Err(AuthError::AlreadyClaimed);
        }
        if inner.setup.is_none() {
            return Err(AuthError::SetupCodeExpired);
        }
        if inner.data.accounts.iter().any(|a| a.username == username) {
            return Err(AuthError::UsernameTaken);
        }
        let account = Account {
            id: secret::short_id("p").map_err(io)?,
            username: username.clone(),
            role: Role::Owner,
            hash,
            created: now,
        };
        inner.data.accounts.push(account.clone());
        inner.setup = None;
        let session = self.new_session(&mut inner, &account, label, now)?;
        self.save(&mut inner)?;
        drop(inner);
        // The box is claimed: the code is gone for good (a stale file would be ignored).
        if let Err(e) = self.forget_setup_code() {
            tracing::warn!("removing the used setup code: {e}");
        }
        self.emit("auth.claim", &username, source, "claimed the box as owner");
        Ok(session)
    }

    // --- Sign-in ----------------------------------------------------------------------------

    fn new_session(
        &self,
        inner: &mut Inner,
        account: &Account,
        label: &str,
        now: u64,
    ) -> Result<NewSession, AuthError> {
        let token = secret::token().map_err(io)?;
        let session = Session {
            id: secret::short_id("s").map_err(io)?,
            hash: secret::hash_token(&token),
            account: account.id.clone(),
            created: now,
            last_seen: now,
            // Signing in is entering the password: sensitive actions work right away.
            reauth: now,
            csrf: secret::token().map_err(io)?,
            label: short_label(label),
        };
        let view = SessionView {
            person: account.person(),
            id: session.id.clone(),
            csrf: session.csrf.clone(),
            reauth_fresh: true,
        };
        inner.data.sessions.push(session);
        Ok(NewSession { token, view })
    }

    /// Check a password against the account, with the failed-try limits. Returns the
    /// account on success.
    fn check_login(
        &self,
        username: &str,
        password: &str,
        source: &str,
    ) -> Result<Account, AuthError> {
        let username = username.trim().to_lowercase();
        let now = self.now();
        let (user_key, src_key) = (user_key(&username), source_key(source));
        let account = {
            let inner = self.lock();
            inner.limits.check(&user_key, limits::PER_USER, now)?;
            inner.limits.check(&src_key, limits::PER_SOURCE, now)?;
            inner
                .data
                .accounts
                .iter()
                .find(|a| a.username == username)
                .cloned()
        };
        let ok = match &account {
            Some(a) => password::verify(password, &a.hash),
            None => {
                password::verify(password, self.dummy_hash());
                false
            }
        };
        let mut inner = self.lock();
        match account {
            Some(a) if ok => {
                inner.limits.clear(&user_key);
                Ok(a)
            }
            _ => {
                let locked_user = inner.limits.fail(&user_key, limits::PER_USER, now);
                let locked_src = inner.limits.fail(&src_key, limits::PER_SOURCE, now);
                drop(inner);
                self.emit("auth.login_failed", &username, source, "wrong password");
                if locked_user || locked_src {
                    let what = if locked_user {
                        format!("username {username}")
                    } else {
                        format!("address {source}")
                    };
                    self.emit(
                        "auth.lockout",
                        &username,
                        source,
                        format!("{what} locked for {} min", LOCKOUT_SECS / 60),
                    );
                }
                Err(AuthError::WrongCredentials)
            }
        }
    }

    /// Sign in. `label` describes the browser ("Firefox on Linux").
    pub fn login(
        &self,
        username: &str,
        password: &str,
        source: &str,
        label: &str,
    ) -> Result<NewSession, AuthError> {
        let account = self.check_login(username, password, source)?;
        let now = self.now();
        let mut inner = self.lock();
        // Removed while the password was checked.
        if !inner.data.accounts.iter().any(|a| a.id == account.id) {
            return Err(AuthError::WrongCredentials);
        }
        let session = self.new_session(&mut inner, &account, label, now)?;
        self.save(&mut inner)?;
        drop(inner);
        self.emit("auth.login", &account.username, source, "signed in");
        Ok(session)
    }

    /// The session behind a cookie value, if it is still valid (and note it was used).
    pub fn session(&self, token: &str) -> Option<SessionView> {
        let hash = secret::hash_token(token);
        let now = self.now();
        let mut inner = self.lock();
        let idx = inner
            .data
            .sessions
            .iter()
            .position(|s| constant_time_eq(s.hash.as_bytes(), hash.as_bytes()))?;
        if !session_alive(&inner.data.sessions[idx], now) {
            inner.data.sessions.remove(idx);
            let _ = self.save(&mut inner);
            return None;
        }
        let s = inner.data.sessions[idx].clone();
        let account = inner
            .data
            .accounts
            .iter()
            .find(|a| a.id == s.account)?
            .clone();
        if now.saturating_sub(s.last_seen) >= TOUCH_SECS {
            inner.data.sessions[idx].last_seen = now;
            if let Err(e) = self.save(&mut inner) {
                tracing::warn!("saving the session list: {e}");
            }
        }
        Some(SessionView {
            person: account.person(),
            id: s.id,
            csrf: s.csrf,
            reauth_fresh: now.saturating_sub(s.reauth) < REAUTH_SECS,
        })
    }

    /// Sign out the session behind this cookie value.
    pub fn sign_out(&self, token: &str, source: &str) {
        let hash = secret::hash_token(token);
        let mut inner = self.lock();
        let Some(idx) = inner.data.sessions.iter().position(|s| s.hash == hash) else {
            return;
        };
        let s = inner.data.sessions.remove(idx);
        let who = username_of(&inner, &s.account);
        let _ = self.save(&mut inner);
        drop(inner);
        self.emit("auth.logout", &who, source, "signed out");
    }

    /// A person's sessions, most recently used first.
    pub fn sessions(&self, account: &str) -> Vec<SessionInfo> {
        let now = self.now();
        let inner = self.lock();
        let mut list: Vec<SessionInfo> = inner
            .data
            .sessions
            .iter()
            .filter(|s| s.account == account && session_alive(s, now))
            .map(|s| SessionInfo {
                id: s.id.clone(),
                label: s.label.clone(),
                created: s.created,
                last_seen: s.last_seen,
            })
            .collect();
        list.sort_by_key(|s| std::cmp::Reverse(s.last_seen));
        list
    }

    /// Sign out one of `account`'s sessions by its public id. False if there is none.
    pub fn revoke_session(&self, account: &str, id: &str, source: &str) -> bool {
        let mut inner = self.lock();
        let before = inner.data.sessions.len();
        inner
            .data
            .sessions
            .retain(|s| !(s.account == account && s.id == id));
        let removed = inner.data.sessions.len() < before;
        if removed {
            let who = username_of(&inner, account);
            let _ = self.save(&mut inner);
            drop(inner);
            self.emit(
                "auth.session_revoked",
                &who,
                source,
                format!("session {id}"),
            );
        }
        removed
    }

    /// Sign out every session of `account` ("Sign out everywhere"). Returns how many.
    pub fn revoke_all(&self, account: &str, source: &str) -> usize {
        let mut inner = self.lock();
        let before = inner.data.sessions.len();
        inner.data.sessions.retain(|s| s.account != account);
        let n = before - inner.data.sessions.len();
        let who = username_of(&inner, account);
        let _ = self.save(&mut inner);
        drop(inner);
        self.emit(
            "auth.session_revoked",
            &who,
            source,
            format!("signed out everywhere ({n})"),
        );
        n
    }

    /// Enter the password again: sensitive actions work for [`REAUTH_SECS`].
    pub fn reauth(&self, token: &str, password: &str, source: &str) -> Result<(), AuthError> {
        let view = self.session(token).ok_or(AuthError::WrongCredentials)?;
        self.check_login(&view.person.username, password, source)?;
        let now = self.now();
        let hash = secret::hash_token(token);
        let mut inner = self.lock();
        if let Some(s) = inner.data.sessions.iter_mut().find(|s| s.hash == hash) {
            s.reauth = now;
        }
        self.save(&mut inner)?;
        drop(inner);
        self.emit(
            "auth.reauth",
            &view.person.username,
            source,
            "entered the password again",
        );
        Ok(())
    }

    /// Change one's own password (the current one first). Other sessions are signed out.
    pub fn change_password(
        &self,
        token: &str,
        current: &str,
        new: &str,
        source: &str,
    ) -> Result<(), AuthError> {
        let view = self.session(token).ok_or(AuthError::WrongCredentials)?;
        check_password(new, &view.person.username)?;
        self.check_login(&view.person.username, current, source)?;
        let hash = self.hash(new)?;
        let now = self.now();
        let token_hash = secret::hash_token(token);
        let mut inner = self.lock();
        let account = inner
            .data
            .accounts
            .iter_mut()
            .find(|a| a.id == view.person.id)
            .ok_or(AuthError::NotFound)?;
        account.hash = hash;
        inner
            .data
            .sessions
            .retain(|s| s.account != view.person.id || s.hash == token_hash);
        if let Some(s) = inner
            .data
            .sessions
            .iter_mut()
            .find(|s| s.hash == token_hash)
        {
            s.reauth = now;
        }
        self.save(&mut inner)?;
        drop(inner);
        self.emit(
            "auth.password_changed",
            &view.person.username,
            source,
            "changed the password; other sessions signed out",
        );
        Ok(())
    }

    // --- Invites and resets -----------------------------------------------------------------

    fn add_invite(
        &self,
        kind: InviteKind,
        by: &str,
        secs: u64,
        source: &str,
    ) -> Result<NewInvite, AuthError> {
        let now = self.now();
        let token = secret::token().map_err(io)?;
        let mut inner = self.lock();
        let username = match &kind {
            InviteKind::Join => None,
            InviteKind::Reset { account } => {
                let name = inner
                    .data
                    .accounts
                    .iter()
                    .find(|a| &a.id == account)
                    .map(|a| a.username.clone())
                    .ok_or(AuthError::NotFound)?;
                // One reset link per person: a new one replaces the old.
                inner.data.invites.retain(|i| i.kind != kind);
                Some(name)
            }
        };
        let invite = Invite {
            id: secret::short_id("i").map_err(io)?,
            hash: secret::hash_token(&token),
            kind: kind.clone(),
            created: now,
            expires: now + secs,
            by: by.to_string(),
        };
        let info = InviteInfo {
            id: invite.id.clone(),
            kind,
            username: username.clone(),
            created: now,
            expires: invite.expires,
        };
        inner.data.invites.push(invite);
        let who = username_of(&inner, by);
        self.save(&mut inner)?;
        drop(inner);
        match &username {
            None => self.emit("auth.invite_created", &who, source, info.id.clone()),
            Some(name) => self.emit(
                "auth.reset_created",
                &who,
                source,
                format!("password reset link for {name} ({})", info.id),
            ),
        }
        Ok(NewInvite { token, info })
    }

    /// An invite link for a new member (`by`: the owner's account id).
    pub fn create_invite(&self, by: &str, source: &str) -> Result<NewInvite, AuthError> {
        self.add_invite(InviteKind::Join, by, INVITE_SECS, source)
    }

    /// A password reset link for `account` (by the owner).
    pub fn reset_link(
        &self,
        by: &str,
        account: &str,
        source: &str,
    ) -> Result<NewInvite, AuthError> {
        self.add_invite(
            InviteKind::Reset {
                account: account.to_string(),
            },
            by,
            RESET_SECS,
            source,
        )
    }

    /// The host's recovery: a reset link for the owner (`melissa-server reset-owner`).
    pub fn owner_reset(&self) -> Result<NewInvite, AuthError> {
        let owner = self.owner().ok_or(AuthError::NotFound)?;
        self.add_invite(
            InviteKind::Reset { account: owner.id },
            "host",
            OWNER_RESET_SECS,
            "host",
        )
    }

    /// Pending invites and reset links, newest first.
    pub fn invites(&self) -> Vec<InviteInfo> {
        let now = self.now();
        let inner = self.lock();
        let mut list: Vec<InviteInfo> = inner
            .data
            .invites
            .iter()
            .filter(|i| i.expires > now)
            .map(|i| invite_info(&inner, i))
            .collect();
        list.sort_by_key(|i| std::cmp::Reverse(i.created));
        list
    }

    /// Withdraw a pending invite or reset link.
    pub fn revoke_invite(&self, id: &str, by: &str, source: &str) -> bool {
        let mut inner = self.lock();
        let before = inner.data.invites.len();
        inner.data.invites.retain(|i| i.id != id);
        let removed = inner.data.invites.len() < before;
        if removed {
            let who = username_of(&inner, by);
            let _ = self.save(&mut inner);
            drop(inner);
            self.emit("auth.invite_revoked", &who, source, id.to_string());
        }
        removed
    }

    /// The invite behind a link, while it can be used.
    pub fn invite(&self, token: &str) -> Option<InviteInfo> {
        let hash = secret::hash_token(token);
        let now = self.now();
        let inner = self.lock();
        inner
            .data
            .invites
            .iter()
            .find(|i| constant_time_eq(i.hash.as_bytes(), hash.as_bytes()) && i.expires > now)
            .map(|i| invite_info(&inner, i))
    }

    /// Use an invite (a new member picks a username) or a reset link (`username` is ignored;
    /// every session of that person is signed out). Single use. Signs the person in.
    pub fn accept_invite(
        &self,
        token: &str,
        username: &str,
        password: &str,
        source: &str,
        label: &str,
    ) -> Result<NewSession, AuthError> {
        let now = self.now();
        let src_key = source_key(source);
        let info = {
            let inner = self.lock();
            inner.limits.check(&src_key, limits::PER_SOURCE, now)?;
            drop(inner);
            match self.invite(token) {
                Some(i) => i,
                None => {
                    self.lock().limits.fail(&src_key, limits::PER_SOURCE, now);
                    return Err(AuthError::InviteInvalid);
                }
            }
        };
        let username = match &info.kind {
            InviteKind::Join => check_username(username)?,
            InviteKind::Reset { .. } => info.username.clone().ok_or(AuthError::NotFound)?,
        };
        check_password(password, &username)?;
        let hash = self.hash(password)?;
        let mut inner = self.lock();
        // Single use: take it out now; a second request with the same link finds nothing.
        let Some(pos) = inner.data.invites.iter().position(|i| i.id == info.id) else {
            return Err(AuthError::InviteInvalid);
        };
        let account = match &info.kind {
            InviteKind::Join => {
                if inner.data.accounts.iter().any(|a| a.username == username) {
                    return Err(AuthError::UsernameTaken);
                }
                inner.data.invites.remove(pos);
                let account = Account {
                    id: secret::short_id("p").map_err(io)?,
                    username: username.clone(),
                    role: Role::Member,
                    hash,
                    created: now,
                };
                inner.data.accounts.push(account.clone());
                account
            }
            InviteKind::Reset { account } => {
                inner.data.invites.remove(pos);
                let id = account.clone();
                let a = inner
                    .data
                    .accounts
                    .iter_mut()
                    .find(|a| a.id == id)
                    .ok_or(AuthError::NotFound)?;
                a.hash = hash;
                let a = a.clone();
                inner.data.sessions.retain(|s| s.account != id);
                a
            }
        };
        let session = self.new_session(&mut inner, &account, label, now)?;
        self.save(&mut inner)?;
        drop(inner);
        match info.kind {
            InviteKind::Join => self.emit(
                "auth.invite_used",
                &username,
                source,
                format!("joined as a member ({})", info.id),
            ),
            InviteKind::Reset { .. } => self.emit(
                "auth.password_reset",
                &username,
                source,
                format!("set a new password from a reset link ({})", info.id),
            ),
        }
        Ok(session)
    }

    // --- People -----------------------------------------------------------------------------

    /// Everyone, the owner first, then by name.
    pub fn people(&self) -> Vec<Person> {
        let mut list: Vec<Person> = self
            .lock()
            .data
            .accounts
            .iter()
            .map(Account::person)
            .collect();
        list.sort_by(|a, b| {
            (a.role != Role::Owner, &a.username).cmp(&(b.role != Role::Owner, &b.username))
        });
        list
    }

    /// A person by account id.
    pub fn person(&self, id: &str) -> Option<Person> {
        self.lock()
            .data
            .accounts
            .iter()
            .find(|a| a.id == id)
            .map(Account::person)
    }

    /// Remove a member: their account, sessions and links. The owner can't be removed.
    pub fn remove_member(&self, id: &str, by: &str, source: &str) -> Result<Person, AuthError> {
        let mut inner = self.lock();
        let pos = inner
            .data
            .accounts
            .iter()
            .position(|a| a.id == id)
            .ok_or(AuthError::NotFound)?;
        if inner.data.accounts[pos].role == Role::Owner {
            return Err(AuthError::OwnerProtected);
        }
        let gone = inner.data.accounts.remove(pos);
        inner.data.sessions.retain(|s| s.account != id);
        inner
            .data
            .invites
            .retain(|i| !matches!(&i.kind, InviteKind::Reset { account } if account == id));
        let who = username_of(&inner, by);
        self.save(&mut inner)?;
        drop(inner);
        self.emit(
            "auth.member_removed",
            &who,
            source,
            format!("removed {}", gone.username),
        );
        Ok(gone.person())
    }
}

fn session_alive(s: &Session, now: u64) -> bool {
    now.saturating_sub(s.last_seen) < SESSION_IDLE_SECS
        && now.saturating_sub(s.created) < SESSION_MAX_SECS
}

fn username_of(inner: &Inner, account: &str) -> String {
    inner
        .data
        .accounts
        .iter()
        .find(|a| a.id == account)
        .map(|a| a.username.clone())
        .unwrap_or_else(|| account.to_string())
}

fn invite_info(inner: &Inner, i: &Invite) -> InviteInfo {
    InviteInfo {
        id: i.id.clone(),
        kind: i.kind.clone(),
        username: match &i.kind {
            InviteKind::Join => None,
            InviteKind::Reset { account } => Some(username_of(inner, account)),
        },
        created: i.created,
        expires: i.expires,
    }
}

fn user_key(username: &str) -> String {
    format!("u:{username}")
}

fn source_key(source: &str) -> String {
    format!("ip:{source}")
}

fn short_label(label: &str) -> String {
    let label: String = label.chars().filter(|c| !c.is_control()).take(80).collect();
    if label.trim().is_empty() {
        "Unknown browser".into()
    } else {
        label
    }
}

/// A username as stored: trimmed, lower case; 1–32 of `a-z 0-9 . _ -`, starting with a letter
/// or digit. `host` is reserved (the host's own actions are audited as `host`).
pub fn check_username(username: &str) -> Result<String, AuthError> {
    let u = username.trim().to_lowercase();
    if u.is_empty() {
        return Err(AuthError::UsernameInvalid("Enter a username."));
    }
    if u.chars().count() > MAX_USERNAME {
        return Err(AuthError::UsernameInvalid(
            "Use at most 32 characters for the username.",
        ));
    }
    if !u
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
        || !u.starts_with(|c: char| c.is_ascii_alphanumeric())
    {
        return Err(AuthError::UsernameInvalid(
            "Use letters, digits, dots, dashes and underscores, starting with a letter or digit.",
        ));
    }
    if u == "host" {
        return Err(AuthError::UsernameInvalid("That username is reserved."));
    }
    Ok(u)
}

/// A new password: 8 to 256 characters and not the username.
pub fn check_password(password: &str, username: &str) -> Result<(), AuthError> {
    let n = password.chars().count();
    if n < MIN_PASSWORD {
        return Err(AuthError::PasswordInvalid(
            "Use at least 8 characters for the password.",
        ));
    }
    if n > MAX_PASSWORD {
        return Err(AuthError::PasswordInvalid(
            "Use at most 256 characters for the password.",
        ));
    }
    if password.trim().to_lowercase() == username {
        return Err(AuthError::PasswordInvalid(
            "Use a password that isn't your username.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
