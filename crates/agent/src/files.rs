//! File boxes: every agent (and the lead agent) has its own box of files.
//!
//! - **What lands there.** Files the owner sends in an agent's chat ([`FileBoxes::store_uploads`])
//!   and files the agent saves itself, with its own tools or the `my_files` tool
//!   ([`MyFilesTool`]). The owner sees the box in the chat's file panel and can download
//!   or delete each file.
//! - **Isolation.** A box is a directory Melissa owns, `<root>/boxes/<key>` (`key` = `lead` or
//!   the agent's id). Only that agent's sandbox binds it, read-write, at the same path
//!   ([`crate::sandbox::SandboxPlan::with_rw_dirs`]); no other agent's sandbox sees it. What
//!   Melissa knows about each file (its id, who added it and when) is kept in
//!   `<root>/index/<key>.json`, outside every sandbox, so an agent cannot change it.
//! - **Untrusted contents.** The agent can put anything in its box, symlinks included. Melissa
//!   only looks at regular files directly in the box (no subdirectories), opens them without
//!   following symlinks ([`rustix::fs::openat`] with `NOFOLLOW` from the box directory) and
//!   serves them by a generated id, never by a name from a request. Owner uploads keep the
//!   attachment rules: limits, type sniffed from the bytes, a cleaned name ([`safe_name`]).
//! - **Retention.** Files are removed once they are older (since added) than the box's
//!   [`Keep`]: the owner's default (5 days) or the agent's override; [`Keep::Forever`] keeps
//!   them. [`FileBoxes::prune`] does it (audited `files.pruned`); messages that named a
//!   removed file show it as expired.
//! - **Size.** At most [`MAX_BOX_BYTES`] of owner uploads per box (agent files are counted but
//!   can't be refused: the agent writes them directly).

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    attachments::{safe_name, sniff, validate, Attachment, AttachmentError, Upload},
    tools::permissions::Principal,
    AuditLog, AuditRecord,
};

/// Owner uploads are refused once a box holds this much.
pub const MAX_BOX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Most files Melissa tracks in one box (more are ignored until some are removed).
pub const MAX_BOX_FILES: usize = 1000;
/// How long files are kept unless the owner says otherwise.
pub const DEFAULT_KEEP_DAYS: u32 = 5;
/// The longest retention in days (ten years); longer is "forever".
pub const MAX_KEEP_DAYS: u32 = 3650;
const DAY: u64 = 86_400;

/// How long a box keeps its files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Keep {
    /// Removed this many days after they were added.
    Days(u32),
    /// Never removed by Melissa.
    Forever,
}

impl Default for Keep {
    fn default() -> Self {
        Self::Days(DEFAULT_KEEP_DAYS)
    }
}

impl Keep {
    /// `Days(n)` checked against the bounds.
    pub fn days(n: u32) -> Result<Self, FileBoxError> {
        if (1..=MAX_KEEP_DAYS).contains(&n) {
            Ok(Self::Days(n))
        } else {
            Err(FileBoxError::Invalid(format!(
                "keep files for 1 to {MAX_KEEP_DAYS} days, or forever"
            )))
        }
    }

    /// From a form or API value: a number of days or `forever`.
    pub fn parse(value: &str) -> Result<Self, FileBoxError> {
        let v = value.trim();
        if v.eq_ignore_ascii_case("forever") {
            return Ok(Self::Forever);
        }
        v.parse::<u32>()
            .map_err(|_| FileBoxError::Invalid("a number of days, or forever".into()))
            .and_then(Self::days)
    }

    /// The value [`Keep::parse`] reads back: `5` or `forever`.
    pub fn value(self) -> String {
        match self {
            Self::Days(n) => n.to_string(),
            Self::Forever => "forever".into(),
        }
    }

    /// In words: `1 day`, `5 days`, `forever`.
    pub fn words(self) -> String {
        match self {
            Self::Days(1) => "1 day".into(),
            Self::Days(n) => format!("{n} days"),
            Self::Forever => "forever".into(),
        }
    }

    /// Days, None for forever (the JSON API's `keep_days`).
    pub fn as_days(self) -> Option<u32> {
        match self {
            Self::Days(n) => Some(n),
            Self::Forever => None,
        }
    }

    /// When a file added at `added` (Unix seconds) expires; None: never.
    pub fn expires(self, added: u64) -> Option<u64> {
        match self {
            Self::Days(n) => Some(added.saturating_add(u64::from(n) * DAY)),
            Self::Forever => None,
        }
    }
}

/// The owner's retention settings: a default and per-box overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Retention {
    #[serde(default)]
    pub default: Keep,
    /// Box key (`lead` or an agent id) → its own retention.
    #[serde(default)]
    pub overrides: BTreeMap<String, Keep>,
}

/// Who put a file in a box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddedBy {
    /// The owner, with a chat message.
    Owner,
    /// The agent whose box it is.
    Agent,
}

/// A file in a box.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoxFile {
    /// 32 lowercase hex digits, made by Melissa; how the UI and API name the file.
    pub id: String,
    /// Its name in the box (also shown to the owner).
    pub name: String,
    /// Sniffed from the bytes.
    pub mime: String,
    pub size: u64,
    /// When Melissa first saw it (Unix seconds); retention counts from here.
    pub added: u64,
    pub by: AddedBy,
    /// Last modification (Unix seconds), to notice an agent rewriting a file.
    #[serde(default)]
    pub modified: i64,
}

impl BoxFile {
    pub fn is_image(&self) -> bool {
        matches!(
            self.mime.as_str(),
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FileBoxError {
    #[error("no such file")]
    NotFound,
    #[error("not a file box")]
    BadOwner,
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Io(String),
}

impl From<io::Error> for FileBoxError {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::NotFound {
            Self::NotFound
        } else {
            Self::Io(e.to_string())
        }
    }
}

/// The key of `owner`'s box: `lead`, or the agent's id (checked, so it is a safe path
/// component).
pub fn box_key(owner: &Principal) -> Option<String> {
    match owner {
        Principal::Lead => Some("lead".into()),
        Principal::Agent(id) if crate::device::valid_agent_id(id) => Some(id.clone()),
        Principal::Agent(_) => None,
    }
}

/// The owner of a box key (the reverse of [`box_key`]).
pub fn owner_of_key(key: &str) -> Option<Principal> {
    if key == "lead" {
        Some(Principal::Lead)
    } else if crate::device::valid_agent_id(key) {
        Some(Principal::Agent(key.to_string()))
    } else {
        None
    }
}

/// The id part of a message attachment's stored name (`<id>.<ext>`).
pub fn attachment_id(file: &str) -> &str {
    file.split_once('.').map_or(file, |(id, _)| id)
}

fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Called with the box whose files changed (for live updates).
pub type ChangeHook = Arc<dyn Fn(&Principal) + Send + Sync>;
/// "Now" in Unix seconds (a fake clock in tests).
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

struct Inner {
    root: PathBuf,
    /// Loaded indexes: box key → files.
    boxes: Mutex<HashMap<String, Vec<BoxFile>>>,
    retention: Mutex<Retention>,
    clock: Clock,
    audit: Option<AuditLog>,
    on_change: Mutex<Option<ChangeHook>>,
}

/// Every file box on this device. Cheap to clone; clones share state.
#[derive(Clone)]
pub struct FileBoxes {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for FileBoxes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileBoxes")
            .field("root", &self.inner.root)
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned lock only means a panic elsewhere; the data is still usable.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn system_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 128 random bits as 32 lowercase hex digits (from the OS).
fn random_id() -> io::Result<String> {
    let mut buf = [0u8; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// Write a file that must not exist yet (mode 0600). `create_new` never follows a symlink.
fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// Is `name` one Melissa looks at in a box: one plain path component, not hidden?
fn listable(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.contains(['/', '\\', '\0'])
        && name.chars().count() <= 255
}

/// Open `name` directly inside `dir` without following a symlink at either end; only a
/// regular file is returned.
fn open_in(dir: &Path, name: &str) -> io::Result<fs::File> {
    use rustix::fs::{openat, Mode, OFlags, CWD};
    if !listable(name) {
        return Err(io::ErrorKind::NotFound.into());
    }
    let dirfd = openat(
        CWD,
        dir,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let fd = openat(
        &dirfd,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )?;
    let file = fs::File::from(fd);
    if !file.metadata()?.is_file() {
        return Err(io::ErrorKind::NotFound.into());
    }
    Ok(file)
}

/// The first bytes of a file, to sniff its type.
fn head_of(file: &mut fs::File) -> Vec<u8> {
    let mut head = Vec::with_capacity(8192);
    let _ = file.take(8192).read_to_end(&mut head);
    head
}

/// `name`, or `stem-2.ext`, `stem-3.ext`, ... until nothing in `taken` or on disk has it.
fn unique_name(dir: &Path, name: &str, taken: &HashSet<String>) -> String {
    let free = |n: &str| !taken.contains(n) && fs::symlink_metadata(dir.join(n)).is_err();
    if free(name) {
        return name.to_string();
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, Some(e)),
        _ => (name, None),
    };
    for n in 2..10_000 {
        let candidate = match ext {
            Some(e) => format!("{stem}-{n}.{e}"),
            None => format!("{stem}-{n}"),
        };
        if free(&candidate) {
            return candidate;
        }
    }
    format!("{stem}-{}", system_now())
}

impl FileBoxes {
    /// Boxes under `root` (created on first use). The retention settings are read from
    /// `<root>/retention.json` (missing or broken: the default, 5 days).
    pub fn open(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let retention = match fs::read(root.join("retention.json")) {
            Ok(data) => serde_json::from_slice(&data).unwrap_or_else(|e| {
                tracing::error!(
                    "{}: {e}; keeping files for the default time",
                    root.display()
                );
                Retention::default()
            }),
            Err(_) => Retention::default(),
        };
        Self {
            inner: Arc::new(Inner {
                root,
                boxes: Mutex::new(HashMap::new()),
                retention: Mutex::new(retention),
                clock: Arc::new(system_now),
                audit: None,
                on_change: Mutex::new(None),
            }),
        }
    }

    fn rebuild(self, f: impl FnOnce(&mut Inner)) -> Self {
        let mut inner = Arc::try_unwrap(self.inner).unwrap_or_else(|shared| Inner {
            root: shared.root.clone(),
            boxes: Mutex::new(lock(&shared.boxes).clone()),
            retention: Mutex::new(lock(&shared.retention).clone()),
            clock: shared.clock.clone(),
            audit: shared.audit.clone(),
            on_change: Mutex::new(lock(&shared.on_change).clone()),
        });
        f(&mut inner);
        Self {
            inner: Arc::new(inner),
        }
    }

    /// Use `clock` for "now" (tests).
    pub fn with_clock(self, clock: Clock) -> Self {
        self.rebuild(|i| i.clock = clock)
    }

    /// Audit deletions, prunes and retention changes to `audit`.
    pub fn with_audit(self, audit: Option<AuditLog>) -> Self {
        self.rebuild(|i| i.audit = audit)
    }

    /// Call `hook` whenever a box's files change (set once by the lead agent).
    pub fn on_change(&self, hook: ChangeHook) {
        *lock(&self.inner.on_change) = Some(hook);
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    fn now(&self) -> u64 {
        (self.inner.clock)()
    }

    fn changed(&self, owner: &Principal) {
        let hook = lock(&self.inner.on_change).clone();
        if let Some(hook) = hook {
            hook(owner);
        }
    }

    fn key(owner: &Principal) -> Result<String, FileBoxError> {
        box_key(owner).ok_or(FileBoxError::BadOwner)
    }

    /// The directory of `owner`'s box (may not exist yet).
    pub fn dir(&self, owner: &Principal) -> Option<PathBuf> {
        box_key(owner).map(|k| self.inner.root.join("boxes").join(k))
    }

    /// The directory of `owner`'s box, created (mode 0700) if missing.
    pub fn ensure_dir(&self, owner: &Principal) -> io::Result<PathBuf> {
        use std::os::unix::fs::DirBuilderExt;
        let dir = self
            .dir(owner)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a file box"))?;
        let mut b = fs::DirBuilder::new();
        b.recursive(true).mode(0o700);
        b.create(&dir)?;
        Ok(dir)
    }

    fn index_path(&self, key: &str) -> PathBuf {
        self.inner.root.join("index").join(format!("{key}.json"))
    }

    /// The loaded index of box `key` (read from disk the first time).
    fn index<'a>(
        &self,
        boxes: &'a mut HashMap<String, Vec<BoxFile>>,
        key: &str,
    ) -> &'a mut Vec<BoxFile> {
        boxes.entry(key.to_string()).or_insert_with(|| {
            fs::read(self.index_path(key))
                .ok()
                .and_then(|d| serde_json::from_slice(&d).ok())
                .unwrap_or_default()
        })
    }

    fn save_index(&self, key: &str, files: &[BoxFile]) {
        let path = self.index_path(key);
        let result = serde_json::to_vec_pretty(files)
            .map_err(io::Error::other)
            .and_then(|data| crate::device::write_private(&path, &data));
        if let Err(e) = result {
            tracing::error!(path = %path.display(), "saving a file box index: {e}");
        }
    }

    fn audit(&self, action: &str, actor: &str, detail: &str) {
        if let Some(log) = &self.inner.audit {
            if let Err(e) = log.append(&AuditRecord::now(action, 0, "", detail).by(actor)) {
                tracing::error!(path = %log.path().display(), "audit log write failed: {e}");
            }
        }
    }

    /// Store one message's files in `owner`'s box. All or nothing: on an error, files already
    /// written for this message are removed.
    pub fn store_uploads(
        &self,
        owner: &Principal,
        uploads: Vec<Upload>,
    ) -> Result<Vec<Attachment>, AttachmentError> {
        validate(&uploads)?;
        if uploads.is_empty() {
            return Ok(Vec::new());
        }
        let store_err = |name: &str, reason: String| AttachmentError::Store {
            name: name.to_string(),
            reason,
        };
        let key = Self::key(owner).map_err(|e| store_err("files", e.to_string()))?;
        let dir = self
            .ensure_dir(owner)
            .map_err(|e| store_err("files", e.to_string()))?;
        let now = self.now();
        let mut boxes = lock(&self.inner.boxes);
        let index = self.index(&mut boxes, &key);
        let used: u64 = index.iter().map(|f| f.size).sum();
        let adding: u64 = uploads.iter().map(|u| u.bytes.len() as u64).sum();
        if used.saturating_add(adding) > MAX_BOX_BYTES {
            return Err(AttachmentError::BoxFull);
        }
        let mut taken: HashSet<String> = index.iter().map(|f| f.name.clone()).collect();
        let mut added: Vec<(BoxFile, Attachment)> = Vec::new();
        for u in uploads {
            let shown = safe_name(&u.name);
            let (mime, ext) = sniff(&u.bytes);
            let name = unique_name(&dir, &shown, &taken);
            let result = random_id().and_then(|id| {
                write_new(&dir.join(&name), &u.bytes)?;
                Ok(id)
            });
            match result {
                Ok(id) => {
                    taken.insert(name.clone());
                    let size = u.bytes.len() as u64;
                    added.push((
                        BoxFile {
                            id: id.clone(),
                            name: name.clone(),
                            mime: mime.to_string(),
                            size,
                            added: now,
                            by: AddedBy::Owner,
                            modified: modified_of(&dir.join(&name)),
                        },
                        Attachment {
                            file: format!("{id}.{ext}"),
                            name,
                            mime: mime.to_string(),
                            size,
                        },
                    ));
                }
                Err(e) => {
                    for (f, _) in &added {
                        let _ = fs::remove_file(dir.join(&f.name));
                    }
                    return Err(store_err(&shown, e.to_string()));
                }
            }
        }
        index.extend(added.iter().map(|(f, _)| f.clone()));
        let snapshot = index.clone();
        drop(boxes);
        self.save_index(&key, &snapshot);
        self.changed(owner);
        Ok(added.into_iter().map(|(_, a)| a).collect())
    }

    /// Remove a refused message's files again.
    pub fn remove_uploads(&self, owner: &Principal, files: &[Attachment]) {
        let (Ok(key), Some(dir)) = (Self::key(owner), self.dir(owner)) else {
            return;
        };
        let ids: HashSet<&str> = files.iter().map(|a| attachment_id(&a.file)).collect();
        let mut boxes = lock(&self.inner.boxes);
        let index = self.index(&mut boxes, &key);
        index.retain(|f| {
            let gone = ids.contains(f.id.as_str());
            if gone {
                let _ = fs::remove_file(dir.join(&f.name));
            }
            !gone
        });
        let snapshot = index.clone();
        drop(boxes);
        self.save_index(&key, &snapshot);
        self.changed(owner);
    }

    /// Bring `owner`'s index in line with the box directory: files the agent added are
    /// recorded (by the agent, added now), rewritten ones get their new size and type, and
    /// removed ones are forgotten. True if anything changed (the change hook is called too).
    pub fn sync(&self, owner: &Principal) -> bool {
        let (Ok(key), Some(dir)) = (Self::key(owner), self.dir(owner)) else {
            return false;
        };
        let Ok(entries) = fs::read_dir(&dir) else {
            return false;
        };
        let mut seen: Vec<(String, u64, i64)> = Vec::new();
        for entry in entries.flatten() {
            if seen.len() >= MAX_BOX_FILES {
                break;
            }
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            // `DirEntry::metadata` does not follow symlinks: only real files count.
            let Ok(meta) = entry.metadata() else { continue };
            if !listable(&name) || !meta.is_file() {
                continue;
            }
            seen.push((name, meta.len(), mtime(&meta)));
        }
        let now = self.now();
        let mut boxes = lock(&self.inner.boxes);
        let index = self.index(&mut boxes, &key);
        let mut changed = false;
        let before = index.len();
        index.retain(|f| seen.iter().any(|(n, ..)| *n == f.name));
        changed |= index.len() != before;
        for (name, size, modified) in seen {
            match index.iter_mut().find(|f| f.name == name) {
                Some(f) if f.size == size && f.modified == modified => {}
                Some(f) => {
                    f.size = size;
                    f.modified = modified;
                    if let Ok(mut file) = open_in(&dir, &name) {
                        f.mime = sniff(&head_of(&mut file)).0.to_string();
                    }
                    changed = true;
                }
                None => {
                    let Ok(id) = random_id() else { continue };
                    let mime = match open_in(&dir, &name) {
                        Ok(mut file) => sniff(&head_of(&mut file)).0.to_string(),
                        Err(_) => continue,
                    };
                    index.push(BoxFile {
                        id,
                        name,
                        mime,
                        size,
                        added: now,
                        by: AddedBy::Agent,
                        modified,
                    });
                    changed = true;
                }
            }
        }
        let snapshot = index.clone();
        drop(boxes);
        if changed {
            self.save_index(&key, &snapshot);
            self.changed(owner);
        }
        changed
    }

    /// `owner`'s files, newest first (looks at the box directory first).
    pub fn list(&self, owner: &Principal) -> Vec<BoxFile> {
        self.sync(owner);
        let mut files = self.indexed(owner);
        files.sort_by(|a, b| b.added.cmp(&a.added).then_with(|| a.name.cmp(&b.name)));
        files
    }

    /// `owner`'s files as last recorded (no look at the directory).
    pub fn indexed(&self, owner: &Principal) -> Vec<BoxFile> {
        let Ok(key) = Self::key(owner) else {
            return Vec::new();
        };
        let mut boxes = lock(&self.inner.boxes);
        self.index(&mut boxes, &key).clone()
    }

    /// The ids of `owner`'s files as last recorded (which message attachments still exist).
    pub fn ids(&self, owner: &Principal) -> HashSet<String> {
        self.indexed(owner).into_iter().map(|f| f.id).collect()
    }

    /// One file by id, as last recorded.
    pub fn get(&self, owner: &Principal, id: &str) -> Option<BoxFile> {
        if !valid_id(id) {
            return None;
        }
        self.indexed(owner).into_iter().find(|f| f.id == id)
    }

    /// Number of files and their total size.
    pub fn usage(&self, owner: &Principal) -> (usize, u64) {
        let files = self.indexed(owner);
        (files.len(), files.iter().map(|f| f.size).sum())
    }

    /// Open a file by id for reading (a regular file in the box; never through a symlink).
    pub fn open_file(
        &self,
        owner: &Principal,
        id: &str,
    ) -> Result<(BoxFile, fs::File), FileBoxError> {
        let dir = self.dir(owner).ok_or(FileBoxError::BadOwner)?;
        let entry = self.get(owner, id).ok_or(FileBoxError::NotFound)?;
        let file = open_in(&dir, &entry.name)?;
        Ok((entry, file))
    }

    /// Read a file by its name in the box, at most `max` bytes (the `my_files` tool).
    pub fn read_named(
        &self,
        owner: &Principal,
        name: &str,
        max: usize,
    ) -> Result<(BoxFile, Vec<u8>, bool), FileBoxError> {
        self.sync(owner);
        let dir = self.dir(owner).ok_or(FileBoxError::BadOwner)?;
        let entry = self
            .indexed(owner)
            .into_iter()
            .find(|f| f.name == name)
            .ok_or(FileBoxError::NotFound)?;
        let file = open_in(&dir, &entry.name)?;
        let mut bytes = Vec::new();
        file.take(max as u64 + 1).read_to_end(&mut bytes)?;
        let cut = bytes.len() > max;
        bytes.truncate(max);
        Ok((entry, bytes, cut))
    }

    /// Save `bytes` as `name` in `owner`'s box, replacing a file of that name (the agent's
    /// `my_files` tool). The name is cleaned like an upload's; the file counts as added now,
    /// by the agent.
    pub fn save_named(
        &self,
        owner: &Principal,
        name: &str,
        bytes: &[u8],
    ) -> Result<BoxFile, FileBoxError> {
        let key = Self::key(owner)?;
        let name = safe_name(name);
        let dir = self.ensure_dir(owner)?;
        let used: u64 = self
            .indexed(owner)
            .iter()
            .filter(|f| f.name != name)
            .map(|f| f.size)
            .sum();
        if used.saturating_add(bytes.len() as u64) > MAX_BOX_BYTES {
            return Err(FileBoxError::Invalid(AttachmentError::BoxFull.to_string()));
        }
        // Written beside it, then renamed over it (a symlink there is replaced, not followed).
        let tmp = dir.join(format!(".{}.saving", random_id()?));
        write_new(&tmp, bytes)?;
        if let Err(e) = fs::rename(&tmp, dir.join(&name)) {
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
        let now = self.now();
        let file = BoxFile {
            id: random_id()?,
            name: name.clone(),
            mime: sniff(bytes).0.to_string(),
            size: bytes.len() as u64,
            added: now,
            by: AddedBy::Agent,
            modified: modified_of(&dir.join(&name)),
        };
        let mut boxes = lock(&self.inner.boxes);
        let index = self.index(&mut boxes, &key);
        index.retain(|f| f.name != name);
        index.push(file.clone());
        let snapshot = index.clone();
        drop(boxes);
        self.save_index(&key, &snapshot);
        self.changed(owner);
        Ok(file)
    }

    /// Delete a file by id (the owner in the UI or API, or the agent's tool). Audited
    /// `files.deleted` with `actor`.
    pub fn delete(
        &self,
        owner: &Principal,
        id: &str,
        actor: &str,
    ) -> Result<BoxFile, FileBoxError> {
        let key = Self::key(owner)?;
        let dir = self.dir(owner).ok_or(FileBoxError::BadOwner)?;
        let entry = self.get(owner, id).ok_or(FileBoxError::NotFound)?;
        // `remove_file` on a symlink removes the link itself; the name is one component.
        match fs::remove_file(dir.join(&entry.name)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let mut boxes = lock(&self.inner.boxes);
        let index = self.index(&mut boxes, &key);
        index.retain(|f| f.id != entry.id);
        let snapshot = index.clone();
        drop(boxes);
        self.save_index(&key, &snapshot);
        self.audit(
            "files.deleted",
            actor,
            &format!("{key}'s box: {} ({} bytes)", entry.name, entry.size),
        );
        self.changed(owner);
        Ok(entry)
    }

    /// The owner's retention settings.
    pub fn retention(&self) -> Retention {
        lock(&self.inner.retention).clone()
    }

    /// How long `owner`'s box keeps files: its override, else the default.
    pub fn keep_for(&self, owner: &Principal) -> Keep {
        let r = lock(&self.inner.retention);
        box_key(owner)
            .and_then(|k| r.overrides.get(&k).copied())
            .unwrap_or(r.default)
    }

    /// `owner`'s own retention, if it overrides the default.
    pub fn override_for(&self, owner: &Principal) -> Option<Keep> {
        let r = lock(&self.inner.retention);
        box_key(owner).and_then(|k| r.overrides.get(&k).copied())
    }

    fn save_retention(&self, next: Retention) -> Result<(), FileBoxError> {
        let data = serde_json::to_vec_pretty(&next).map_err(|e| FileBoxError::Io(e.to_string()))?;
        crate::device::write_private(&self.inner.root.join("retention.json"), &data)?;
        *lock(&self.inner.retention) = next;
        Ok(())
    }

    /// Change the default retention (audited `files.retention`).
    pub fn set_default(&self, keep: Keep, actor: &str) -> Result<(), FileBoxError> {
        if let Keep::Days(n) = keep {
            Keep::days(n)?;
        }
        let mut next = self.retention();
        next.default = keep;
        self.save_retention(next)?;
        self.audit(
            "files.retention",
            actor,
            &format!("default: keep files {}", keep_phrase(keep)),
        );
        Ok(())
    }

    /// Set (or with None, clear) `owner`'s own retention (audited `files.retention`).
    pub fn set_override(
        &self,
        owner: &Principal,
        keep: Option<Keep>,
        actor: &str,
    ) -> Result<(), FileBoxError> {
        let key = Self::key(owner)?;
        if let Some(Keep::Days(n)) = keep {
            Keep::days(n)?;
        }
        let mut next = self.retention();
        match keep {
            Some(k) => next.overrides.insert(key.clone(), k),
            None => next.overrides.remove(&key),
        };
        self.save_retention(next)?;
        let what = match keep {
            Some(k) => format!("keep files {}", keep_phrase(k)),
            None => "use the default".into(),
        };
        self.audit("files.retention", actor, &format!("{key}'s box: {what}"));
        Ok(())
    }

    /// Every box that exists (on disk or in an index).
    pub fn owners(&self) -> Vec<Principal> {
        let mut keys: HashSet<String> = HashSet::new();
        for (sub, strip) in [("boxes", ""), ("index", ".json")] {
            if let Ok(entries) = fs::read_dir(self.inner.root.join(sub)) {
                for e in entries.flatten() {
                    if let Ok(name) = e.file_name().into_string() {
                        keys.insert(name.strip_suffix(strip).unwrap_or(&name).to_string());
                    }
                }
            }
        }
        let mut owners: Vec<Principal> = keys.iter().filter_map(|k| owner_of_key(k)).collect();
        owners.sort();
        owners
    }

    /// Remove every file older than its box keeps files (since it was added). Each box with
    /// removals is audited once (`files.pruned`). Returns what was removed.
    pub fn prune(&self) -> Vec<(Principal, BoxFile)> {
        let now = self.now();
        let mut removed = Vec::new();
        for owner in self.owners() {
            let keep = self.keep_for(&owner);
            if keep == Keep::Forever {
                continue;
            }
            self.sync(&owner);
            let expired: Vec<BoxFile> = self
                .indexed(&owner)
                .into_iter()
                .filter(|f| keep.expires(f.added).is_some_and(|t| t <= now))
                .collect();
            if expired.is_empty() {
                continue;
            }
            let (Ok(key), Some(dir)) = (Self::key(&owner), self.dir(&owner)) else {
                continue;
            };
            let mut gone = Vec::new();
            for f in expired {
                match fs::remove_file(dir.join(&f.name)) {
                    Ok(()) => gone.push(f),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => gone.push(f),
                    Err(e) => tracing::warn!(file = %f.name, "removing an expired file: {e}"),
                }
            }
            let ids: HashSet<&str> = gone.iter().map(|f| f.id.as_str()).collect();
            let mut boxes = lock(&self.inner.boxes);
            let index = self.index(&mut boxes, &key);
            index.retain(|f| !ids.contains(f.id.as_str()));
            let snapshot = index.clone();
            drop(boxes);
            self.save_index(&key, &snapshot);
            let names: Vec<&str> = gone.iter().map(|f| f.name.as_str()).collect();
            self.audit(
                "files.pruned",
                "files",
                &format!(
                    "{key}'s box: {} file(s) older than {}: {}",
                    gone.len(),
                    keep.words(),
                    names.join(", ")
                ),
            );
            self.changed(&owner);
            removed.extend(gone.into_iter().map(|f| (owner.clone(), f)));
        }
        removed
    }

    /// Move the files of the old shared attachment store (`old`, files named
    /// `<32 hex>.<ext>`) into the boxes, once: `whose` says, by stored name, which box and
    /// display name a file had (from the chats that sent it); the rest go to the lead
    /// agent's box. Files are copied (the originals stay) and keep their id, so earlier
    /// messages still find them; they count as added now. When every file is copied the old
    /// directory is renamed to `<old>.migrated`. Returns how many files were copied.
    pub fn migrate_attachments(
        &self,
        old: &Path,
        whose: &HashMap<String, (Principal, String)>,
    ) -> io::Result<usize> {
        let entries = match fs::read_dir(old) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        let now = self.now();
        let (mut copied, mut failed) = (0usize, 0usize);
        let mut touched: Vec<Principal> = Vec::new();
        for entry in entries.flatten() {
            let Ok(file) = entry.file_name().into_string() else {
                continue;
            };
            if !crate::attachments::valid_file_name(&file) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let id = attachment_id(&file).to_string();
            let (owner, shown) = whose.get(&file).cloned().unwrap_or_else(|| {
                let ext = file.rsplit_once('.').map_or("bin", |(_, e)| e);
                (Principal::Lead, format!("file.{ext}"))
            });
            let owner = if box_key(&owner).is_some() {
                owner
            } else {
                Principal::Lead
            };
            if self.get(&owner, &id).is_some() {
                continue;
            }
            let result = (|| -> io::Result<BoxFile> {
                let key = Self::key(&owner).map_err(io::Error::other)?;
                let dir = self.ensure_dir(&owner)?;
                let bytes = fs::read(entry.path())?;
                let mut boxes = lock(&self.inner.boxes);
                let index = self.index(&mut boxes, &key);
                let taken: HashSet<String> = index.iter().map(|f| f.name.clone()).collect();
                let name = unique_name(&dir, &safe_name(&shown), &taken);
                write_new(&dir.join(&name), &bytes)?;
                let f = BoxFile {
                    id: id.clone(),
                    name: name.clone(),
                    mime: sniff(&bytes).0.to_string(),
                    size: bytes.len() as u64,
                    added: now,
                    by: AddedBy::Owner,
                    modified: modified_of(&dir.join(&name)),
                };
                index.push(f.clone());
                let snapshot = index.clone();
                drop(boxes);
                self.save_index(&key, &snapshot);
                Ok(f)
            })();
            match result {
                Ok(_) => {
                    copied += 1;
                    if !touched.contains(&owner) {
                        touched.push(owner);
                    }
                }
                Err(e) => {
                    failed += 1;
                    tracing::warn!(file = %file, "moving an attachment into a file box: {e}");
                }
            }
        }
        if failed == 0 {
            let mut done = old.as_os_str().to_os_string();
            done.push(".migrated");
            fs::rename(old, PathBuf::from(done))?;
        }
        if copied > 0 {
            self.audit(
                "files.migrated",
                "files",
                &format!("{copied} earlier chat attachment(s) moved into file boxes"),
            );
        }
        for owner in touched {
            self.changed(&owner);
        }
        Ok(copied)
    }
}

fn keep_phrase(k: Keep) -> String {
    match k {
        Keep::Forever => "forever".into(),
        other => format!("for {}", other.words()),
    }
}

fn mtime(meta: &fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    meta.mtime()
}

fn modified_of(path: &Path) -> i64 {
    fs::symlink_metadata(path).map_or(0, |m| mtime(&m))
}

#[cfg(test)]
mod tests;
mod tool;

pub use tool::MyFilesTool;
