#![forbid(unsafe_code)]
//! Melissa's persisted files: a format version on every file, forward migrations, and writes
//! that an update, a rollback or a power cut cannot half-do (docs/design/STATE-INVENTORY-2026-11-28.md,
//! DECISIONS 2026-11-28 "Chats and state survive upgrades").
//!
//! **Whole files** (JSON or TOML snapshots: the agent registry, schedules, accounts, settings, ...)
//! carry a top-level `format` number. A file without one is format 1: every file Melissa wrote
//! before formats existed. Reading ([`read_json`], [`read_toml`]):
//!
//! - format == current: read as it is;
//! - format < current: the [`Format::migrations`] bring it up, one step at a time, in memory; the
//!   next save writes the current format;
//! - format > current (an older Melissa after a rollback): the file is **never changed**. It is
//!   read as well as this version can (unknown fields ignored) and marked read-only
//!   ([`Loaded::read_only`]); if even that fails, [`FormatError::Newer`]. Every save
//!   ([`save_json`], [`save_toml`]) checks the file on disk first and refuses to replace a newer
//!   one, so a store that forgot to look at `read_only` still cannot destroy newer data.
//!
//! **Bump the format** whenever an older Melissa would misread a file this one writes: a field
//! removed, renamed or changed in meaning or type, or any new field in a struct with
//! `deny_unknown_fields`. A new optional field elsewhere needs no bump. A bump adds a migration
//! from the previous format and a fixture test that the previous format still loads.
//!
//! **Formats from 2 on are JSON objects or TOML tables**: a file whose format 1 is a JSON array
//! or map keyed by data (`models.json`, file box indexes) becomes an object with a `format` key
//! when it first changes. For such files, and for files read by structs with
//! `deny_unknown_fields`, format 1 is written without the key ([`Format::v1_marker`] false), so
//! the images from before formats existed still read them after a rollback.
//!
//! **Logs** (JSON Lines: transcripts, threads, the audit log) carry the format on each line
//! ([`encode_line`], [`decode_line`]): logs are only appended to, so an older Melissa keeps
//! appending its own lines after a rollback and skips (never removes) lines from a newer one.
//! [`append_line`] first ends a line a crash cut short, so the next record is not glued to it.
//!
//! **Writes** go to a temporary file in the same directory, which is flushed to disk, renamed
//! over the old file and the directory flushed ([`write_atomic`]): a crash leaves the old file or
//! the new one, never a mix.

use std::{
    fs,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{de::DeserializeOwned, Serialize};

/// The key holding the format number, in files and on log lines.
pub const KEY: &str = "format";

/// One step: turns a document of format `n` into format `n + 1`.
pub type Migration<D> = fn(&mut D) -> Result<(), String>;

/// A persisted format: its name for messages, the version this build writes and how older
/// versions become it.
pub struct Format<D: 'static> {
    /// What the file is, for log lines and errors ("agent registry").
    pub name: &'static str,
    /// The format this build reads and writes (1 or more).
    pub current: u32,
    /// `migrations[i]` turns format `i + 1` into `i + 2`; exactly `current - 1` of them.
    pub migrations: &'static [Migration<D>],
    /// Write `format = 1` too. False for files the images from before formats existed would
    /// reject with an extra key (a JSON array or data-keyed map, `deny_unknown_fields`).
    pub v1_marker: bool,
}

/// A JSON file's format.
pub type JsonFormat = Format<serde_json::Value>;
/// A TOML file's format.
pub type TomlFormat = Format<toml::Table>;

impl<D> Format<D> {
    /// The format number written into a file, if any.
    fn marker(&self) -> Option<u32> {
        (self.current > 1 || self.v1_marker).then_some(self.current)
    }
}

/// A file read with [`read_json`] or [`read_toml`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded<T> {
    pub value: T,
    /// The format the file had on disk.
    pub found: u32,
    /// Written by a newer Melissa: read as well as this version can, never to be written.
    pub read_only: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    #[error(
        "{path} was written by a newer version of Melissa ({name} format {found}; this version \
         reads up to {supported}). It is left as it is; changes to it are not saved until Melissa \
         is updated again"
    )]
    Newer {
        path: String,
        name: &'static str,
        found: u32,
        supported: u32,
    },
    #[error("{path} is not a valid {name}: {message}")]
    Invalid {
        path: String,
        name: &'static str,
        message: String,
    },
    #[error("{path}: cannot bring the {name} from format {from} to {to}: {message}")]
    Migration {
        path: String,
        name: &'static str,
        from: u32,
        to: u32,
        message: String,
    },
    #[error("cannot read {path}: {source}")]
    Read { path: String, source: io::Error },
    #[error("cannot save {path}: {source}")]
    Write { path: String, source: io::Error },
}

impl FormatError {
    /// The file was written by a newer Melissa.
    pub fn is_newer(&self) -> bool {
        matches!(self, Self::Newer { .. })
    }
}

/// A parsed document that can carry a format number.
pub trait Doc: Sized {
    fn parse(bytes: &[u8]) -> Result<Self, String>;
    /// The format number in the document (absent: 1).
    fn version(&self) -> Result<u32, String>;
    /// Remove the format key (the structs that read the document do not know it).
    fn strip_version(&mut self);
    fn into_value<T: DeserializeOwned>(self) -> Result<T, String>;
}

fn check_version(n: Option<i64>) -> Result<u32, String> {
    match n {
        Some(n) if n >= 1 => u32::try_from(n).map_err(|_| format!("format {n} is out of range")),
        Some(n) => Err(format!("format {n} is not a format number")),
        None => Err(format!("`{KEY}` is not a whole number")),
    }
}

impl Doc for serde_json::Value {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(bytes).map_err(|e| e.to_string())
    }

    fn version(&self) -> Result<u32, String> {
        match self.as_object().and_then(|o| o.get(KEY)) {
            None => Ok(1),
            Some(v) => check_version(v.as_i64()),
        }
    }

    fn strip_version(&mut self) {
        if let Some(o) = self.as_object_mut() {
            o.remove(KEY);
        }
    }

    fn into_value<T: DeserializeOwned>(self) -> Result<T, String> {
        serde_json::from_value(self).map_err(|e| e.to_string())
    }
}

impl Doc for toml::Table {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
        text.parse::<toml::Table>()
            .map_err(|e| e.message().to_string())
    }

    fn version(&self) -> Result<u32, String> {
        match self.get(KEY) {
            None => Ok(1),
            Some(v) => check_version(v.as_integer()),
        }
    }

    fn strip_version(&mut self) {
        self.remove(KEY);
    }

    fn into_value<T: DeserializeOwned>(self) -> Result<T, String> {
        toml::Value::Table(self)
            .try_into()
            .map_err(|e: toml::de::Error| e.message().to_string())
    }
}

fn shown(path: &Path) -> String {
    path.display().to_string()
}

/// Bring `doc` (read from `path`) to `fmt.current` and deserialize it.
pub fn decode<D, T>(fmt: &Format<D>, path: &Path, mut doc: D) -> Result<Loaded<T>, FormatError>
where
    D: Doc,
    T: DeserializeOwned,
{
    let invalid = |message: String| FormatError::Invalid {
        path: shown(path),
        name: fmt.name,
        message,
    };
    let found = doc.version().map_err(invalid)?;
    if found > fmt.current {
        doc.strip_version();
        return match doc.into_value::<T>() {
            Ok(value) => {
                tracing::warn!(
                    path = %path.display(),
                    "{}",
                    FormatError::Newer {
                        path: shown(path),
                        name: fmt.name,
                        found,
                        supported: fmt.current,
                    }
                );
                Ok(Loaded {
                    value,
                    found,
                    read_only: true,
                })
            }
            Err(_) => Err(FormatError::Newer {
                path: shown(path),
                name: fmt.name,
                found,
                supported: fmt.current,
            }),
        };
    }
    for from in found..fmt.current {
        let step = usize::try_from(from - 1)
            .ok()
            .and_then(|i| fmt.migrations.get(i))
            .ok_or_else(|| FormatError::Migration {
                path: shown(path),
                name: fmt.name,
                from,
                to: from + 1,
                message: "no migration (a bug in this build)".into(),
            })?;
        step(&mut doc).map_err(|message| FormatError::Migration {
            path: shown(path),
            name: fmt.name,
            from,
            to: from + 1,
            message,
        })?;
    }
    if found < fmt.current {
        tracing::info!(
            path = %path.display(),
            "{}: migrated from format {found} to {} (written in the new format at the next change)",
            fmt.name,
            fmt.current
        );
    }
    doc.strip_version();
    let value = doc.into_value::<T>().map_err(invalid)?;
    Ok(Loaded {
        value,
        found,
        read_only: false,
    })
}

/// Read a JSON file of format `fmt` (None: it does not exist).
pub fn read_json<T: DeserializeOwned>(
    path: &Path,
    fmt: &JsonFormat,
) -> Result<Option<Loaded<T>>, FormatError> {
    match read_bytes(path)? {
        None => Ok(None),
        Some(bytes) => parse_json(path, fmt, &bytes).map(Some),
    }
}

/// Parse JSON `bytes` (from `path`, for messages) of format `fmt`.
pub fn parse_json<T: DeserializeOwned>(
    path: &Path,
    fmt: &JsonFormat,
    bytes: &[u8],
) -> Result<Loaded<T>, FormatError> {
    let doc = serde_json::Value::parse(bytes).map_err(|message| FormatError::Invalid {
        path: shown(path),
        name: fmt.name,
        message,
    })?;
    decode(fmt, path, doc)
}

/// Read a TOML file of format `fmt` (None: it does not exist).
pub fn read_toml<T: DeserializeOwned>(
    path: &Path,
    fmt: &TomlFormat,
) -> Result<Option<Loaded<T>>, FormatError> {
    match read_bytes(path)? {
        None => Ok(None),
        Some(bytes) => parse_toml(path, fmt, &bytes).map(Some),
    }
}

/// Parse TOML `bytes` (from `path`, for messages) of format `fmt`.
pub fn parse_toml<T: DeserializeOwned>(
    path: &Path,
    fmt: &TomlFormat,
    bytes: &[u8],
) -> Result<Loaded<T>, FormatError> {
    let doc = toml::Table::parse(bytes).map_err(|message| FormatError::Invalid {
        path: shown(path),
        name: fmt.name,
        message,
    })?;
    decode(fmt, path, doc)
}

fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>, FormatError> {
    match fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(FormatError::Read {
            path: shown(path),
            source,
        }),
    }
}

/// The format of the file at `path` if it is readable and newer than `fmt.current`.
/// A missing or unparseable file is not newer (an unparseable one is the store's to handle).
pub fn newer_on_disk<D: Doc>(path: &Path, fmt: &Format<D>) -> Option<u32> {
    let bytes = fs::read(path).ok()?;
    let found = D::parse(&bytes).ok()?.version().ok()?;
    (found > fmt.current).then_some(found)
}

/// Refuse to replace `path` if a newer Melissa wrote it.
pub fn guard<D: Doc>(path: &Path, fmt: &Format<D>) -> Result<(), FormatError> {
    match newer_on_disk(path, fmt) {
        Some(found) => Err(FormatError::Newer {
            path: shown(path),
            name: fmt.name,
            found,
            supported: fmt.current,
        }),
        None => Ok(()),
    }
}

/// `value` as JSON in format `fmt` (the `format` key first).
pub fn to_json<T: Serialize>(
    fmt: &JsonFormat,
    value: &T,
    pretty: bool,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut doc = serde_json::to_value(value)?;
    if let Some(n) = fmt.marker() {
        match &mut doc {
            serde_json::Value::Object(o) => {
                let mut out = serde_json::Map::new();
                out.insert(KEY.into(), n.into());
                out.extend(std::mem::take(o));
                *o = out;
            }
            // Only format 1 may be something else (see the module docs).
            _ if n > 1 => {
                return Err(serde::ser::Error::custom(format!(
                    "{} format {n} must be a JSON object",
                    fmt.name
                )))
            }
            _ => {}
        }
    }
    if pretty {
        serde_json::to_vec_pretty(&doc)
    } else {
        serde_json::to_vec(&doc)
    }
}

/// `value` as TOML in format `fmt`, after `header` (comment lines).
pub fn to_toml<T: Serialize>(
    fmt: &TomlFormat,
    header: &str,
    value: &T,
) -> Result<String, toml::ser::Error> {
    let mut text = String::from(header);
    if let Some(n) = fmt.marker() {
        // A top-level key before everything else: valid whatever the body starts with.
        text.push_str(&format!("{KEY} = {n}\n\n"));
    }
    text.push_str(&toml::to_string(value)?);
    Ok(text)
}

/// Save `value` to `path` (mode 0600) in format `fmt`, unless a newer Melissa wrote it.
pub fn save_json<T: Serialize>(
    path: &Path,
    fmt: &JsonFormat,
    value: &T,
    pretty: bool,
) -> Result<(), FormatError> {
    guard(path, fmt)?;
    let data = to_json(fmt, value, pretty).map_err(|e| FormatError::Write {
        path: shown(path),
        source: io::Error::other(e),
    })?;
    write_private(path, &data).map_err(|source| FormatError::Write {
        path: shown(path),
        source,
    })
}

/// Save `value` to `path` (mode 0600) in format `fmt` after `header`, unless a newer Melissa
/// wrote it.
pub fn save_toml<T: Serialize>(
    path: &Path,
    fmt: &TomlFormat,
    header: &str,
    value: &T,
) -> Result<(), FormatError> {
    guard(path, fmt)?;
    let text = to_toml(fmt, header, value).map_err(|e| FormatError::Write {
        path: shown(path),
        source: io::Error::other(e),
    })?;
    write_private(path, text.as_bytes()).map_err(|source| FormatError::Write {
        path: shown(path),
        source,
    })
}

/// Write `data` to `path` atomically with mode 0600 ([`write_atomic`]).
pub fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    write_atomic(path, data, 0o600)
}

/// Write `data` to `path` atomically: a new temporary file (mode `mode`) in the same directory,
/// flushed to disk, renamed over `path`, then the directory flushed. Creates the parent
/// directory. A failure or crash leaves the old file as it was.
pub fn write_atomic(path: &Path, data: &[u8], mode: u32) -> io::Result<()> {
    write_atomic_with(path, mode, |f| f.write_all(data))
}

/// [`write_atomic`] with the content written by `fill`.
pub fn write_atomic_with(
    path: &Path,
    mode: u32,
    fill: impl FnOnce(&mut fs::File) -> io::Result<()>,
) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    fs::create_dir_all(&dir)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    // Unique per process and call: two writers (threads, or processes sharing a state dir in
    // tests) never rename each other's half-written file.
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(
        ".{}-{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = dir.join(tmp_name);
    let result = (|| {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)?;
        fill(&mut f)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    // The rename is durable once the directory is: best effort (some filesystems refuse).
    if let Ok(d) = fs::File::open(&dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// `value` as one log line (no newline) in format `fmt`: a JSON object with the `format` key
/// first. `value` must serialize to an object.
pub fn encode_line<T: Serialize>(fmt: &JsonFormat, value: &T) -> Result<String, serde_json::Error> {
    let bytes = to_json(fmt, value, false)?;
    // serde_json writes UTF-8.
    String::from_utf8(bytes).map_err(serde::ser::Error::custom)
}

/// One log line read with [`decode_line`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line<T> {
    Record(T),
    /// Written by a newer Melissa (its format): skipped, never removed.
    Newer(u32),
    /// Not a record of this log (cut short by a crash, or garbage).
    Invalid,
}

/// Read one log line of format `fmt` (older formats migrated).
pub fn decode_line<T: DeserializeOwned>(fmt: &JsonFormat, line: &str) -> Line<T> {
    let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(line) else {
        return Line::Invalid;
    };
    let Ok(found) = doc.version() else {
        return Line::Invalid;
    };
    if found > fmt.current {
        return Line::Newer(found);
    }
    for from in found..fmt.current {
        let Some(step) = usize::try_from(from - 1)
            .ok()
            .and_then(|i| fmt.migrations.get(i))
        else {
            return Line::Invalid;
        };
        if step(&mut doc).is_err() {
            return Line::Invalid;
        }
    }
    doc.strip_version();
    match serde_json::from_value(doc) {
        Ok(v) => Line::Record(v),
        Err(_) => Line::Invalid,
    }
}

/// The records of `text` (a log of format `fmt`), skipping lines of newer formats and lines
/// that are not records. `log` names the file for the one warning about skipped newer lines.
pub fn decode_lines<T: DeserializeOwned>(fmt: &JsonFormat, text: &str, log: &Path) -> Vec<T> {
    let mut newer = 0usize;
    let out = text
        .lines()
        .filter_map(|l| match decode_line(fmt, l) {
            Line::Record(r) => Some(r),
            Line::Newer(_) => {
                newer += 1;
                None
            }
            Line::Invalid => None,
        })
        .collect();
    if newer > 0 {
        tracing::warn!(
            path = %log.display(),
            "{newer} {} line(s) were written by a newer version of Melissa; they are kept but not shown",
            fmt.name
        );
    }
    out
}

/// Append `line` and a newline to `path` (created with mode `mode`). A last line a crash cut
/// short gets its newline first, so it stays one bad line instead of spoiling this one too.
pub fn append_line(path: &Path, line: &str, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let mut file = fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(mode)
        .open(path)?;
    let mut out = String::with_capacity(line.len() + 2);
    if file.metadata()?.len() > 0 {
        let mut last = [0u8; 1];
        file.seek(SeekFrom::End(-1))?;
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            out.push('\n');
        }
    }
    out.push_str(line);
    out.push('\n');
    // One write in append mode: a reader never sees half of it next to another writer's.
    file.write_all(out.as_bytes())
}

#[cfg(test)]
mod tests;
