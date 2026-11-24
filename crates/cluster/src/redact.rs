//! Keeping an app's generated secrets out of what agents read (docs/design/
//! SECRET-EXPOSURE-RESEARCH-2026-11-24.md, step 1).
//!
//! `app_exec` and `app_logs` return text the app wrote, and an app may print its own
//! passwords (`env`, a config file, a startup banner). Before such text reaches an agent, the
//! server replaces every copy of a secret it generated for that app with `[hidden: <field>]`.
//!
//! The server never keeps the values: when it generates one ([`crate::generate_secret`]) it
//! records keyed fingerprints of every 8-character piece of it ([`SecretPrints`], a file
//! per app in the server's state directory, mode 0600) and forgets the value. Redaction
//! hashes the pieces of the output and compares. So no identity gets new read access to
//! Secrets, and a dismissed setup password stays unreadable by Melissa.
//!
//! What is matched, in the output (after `%XX` escapes are decoded):
//! - any run of [`PIECE`] or more consecutive characters of a value, also when spaces, tabs or
//!   line breaks sit between them (wrapped or split copies);
//! - the same inside lower or upper case hex (both nibble alignments);
//! - the same inside standard or URL-safe base64, with or without padding, at all four
//!   character phases, so a value anywhere in a larger base64 blob is found (its three byte
//!   alignments).
//!
//! Limits (stated plainly; masking catches accidents, not a determined script):
//! - pieces shorter than [`PIECE`] characters stay visible (e.g. a value cut by the output
//!   limit after 5 characters; [`drop_cut_tail`] trims the cut end for that case);
//! - any other transformation is not caught: reversed, one character per line, `od`, other
//!   escapes (`\uXXXX`, `\x41`), a line prefix between the parts of a wrapped copy, other
//!   encodings (gzip, base32), or text the app derives from it;
//! - values generated before this existed have no fingerprints and are not hidden until the
//!   app's secrets are generated again;
//! - anyone who can read the fingerprint file can test guesses of 8-character pieces offline
//!   (62^8 per piece); that is the server's own state, whose identities can already exec into
//!   the app.
//!
//! Generated values are 16-128 characters from a 62-letter alphabet, so a chance match of an
//! 8-character piece is about 1 in 2×10^14 per position, and URL or JSON escaping leaves them
//! unchanged. Cost: one SHA-256 per 8-character alphanumeric window of the output (plus the
//! few windows of decoded hex and base64 that are all letters and digits).

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io::{Read, Write},
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
};

use melissa_appspec::{AppSpec, EnvSource};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Shortest piece of a value that is hidden.
pub const PIECE: usize = 8;
/// Per-app random key of the fingerprints, in bytes.
const SALT_BYTES: usize = 16;
/// Most trailing characters [`drop_cut_tail`] removes (7 raw characters are 10 in base64 and
/// 14 in hex).
const CUT_TAIL: usize = 16;

type Print = [u8; 16];

/// The fingerprints of one app's generated secrets, ready to scan text.
pub(crate) struct Redactor {
    salt: Vec<u8>,
    pieces: HashMap<Print, usize>,
    fields: Vec<String>,
}

/// Text after redaction, and the fields that were hidden in it.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Redacted {
    pub text: String,
    pub hidden: BTreeSet<String>,
}

/// One byte of the scanned text after decoding, and the bytes of the original it came from.
#[derive(Clone, Copy)]
struct Unit {
    byte: u8,
    start: usize,
    end: usize,
}

impl Redactor {
    /// From the values themselves (tests, and right after generating).
    #[cfg(test)]
    pub(crate) fn of_values(salt: &[u8], values: &[(&str, &str)]) -> Self {
        let mut r = Self {
            salt: salt.to_vec(),
            pieces: HashMap::new(),
            fields: Vec::new(),
        };
        for (field, value) in values {
            for p in pieces_of(salt, value) {
                r.pieces.insert(p, r.fields.len());
            }
            r.fields.push((*field).to_string());
        }
        r
    }

    fn print(&self, window: &[u8]) -> Print {
        print_of(&self.salt, window)
    }

    /// Replace every copy of a value (see the module docs for what counts) with
    /// `[hidden: <field>]`.
    pub(crate) fn redact(&self, text: &str) -> Redacted {
        if self.pieces.is_empty() {
            return Redacted {
                text: text.to_string(),
                hidden: BTreeSet::new(),
            };
        }
        let units = percent_decoded(text);
        let mut spans = Vec::new();
        for seg in segments(&units, |b| b.is_ascii_alphanumeric()) {
            self.scan(&seg, &mut spans);
        }
        for seg in segments(&units, |b| b.is_ascii_hexdigit()) {
            for phase in 0..2 {
                self.scan(&hex_decoded(&seg, phase), &mut spans);
            }
        }
        for seg in segments(&units, |b| b64_value(b).is_some()) {
            for phase in 0..4 {
                self.scan(&b64_decoded(&seg, phase), &mut spans);
            }
        }
        spans.sort_unstable();
        let mut out = String::with_capacity(text.len());
        let mut hidden = BTreeSet::new();
        let mut at = 0;
        let mut i = 0;
        while i < spans.len() {
            let (start, mut end, field) = spans[i];
            hidden.insert(self.fields[field].clone());
            i += 1;
            while i < spans.len() && spans[i].0 <= end {
                end = end.max(spans[i].1);
                hidden.insert(self.fields[spans[i].2].clone());
                i += 1;
            }
            // Spans start and end at ASCII bytes, so these are char boundaries.
            out.push_str(&text[at..start.max(at)]);
            out.push_str(&format!("[hidden: {}]", self.fields[field]));
            at = end.max(at);
        }
        out.push_str(&text[at..]);
        Redacted { text: out, hidden }
    }

    /// Record the original span of every window of `seq` that is a piece of a value. Windows
    /// with anything but letters and digits are skipped without hashing (values have none).
    fn scan(&self, seq: &[Unit], spans: &mut Vec<(usize, usize, usize)>) {
        let mut run = 0;
        let mut window = [0u8; PIECE];
        for (i, u) in seq.iter().enumerate() {
            run = if u.byte.is_ascii_alphanumeric() {
                run + 1
            } else {
                0
            };
            if run >= PIECE {
                let first = i + 1 - PIECE;
                for (w, s) in window.iter_mut().zip(&seq[first..=i]) {
                    *w = s.byte;
                }
                if let Some(&field) = self.pieces.get(&self.print(&window)) {
                    spans.push((seq[first].start, u.end, field));
                }
            }
        }
    }
}

fn print_of(salt: &[u8], window: &[u8]) -> Print {
    let digest = Sha256::new()
        .chain_update(salt)
        .chain_update(window)
        .finalize();
    let mut p = [0u8; 16];
    p.copy_from_slice(&digest[..16]);
    p
}

/// The fingerprints of every [`PIECE`]-character window of `value` (the whole value if it
/// is shorter, which generated values never are).
fn pieces_of(salt: &[u8], value: &str) -> BTreeSet<Print> {
    let bytes = value.as_bytes();
    if bytes.len() < PIECE {
        return [print_of(salt, bytes)].into();
    }
    bytes.windows(PIECE).map(|w| print_of(salt, w)).collect()
}

/// The text as units, `%XX` escapes decoded to one unit spanning three bytes.
fn percent_decoded(text: &str) -> Vec<Unit> {
    let b = text.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex_value(b[i + 1]), hex_value(b[i + 2])) {
                out.push(Unit {
                    byte: h << 4 | l,
                    start: i,
                    end: i + 3,
                });
                i += 3;
                continue;
            }
        }
        out.push(Unit {
            byte: b[i],
            start: i,
            end: i + 1,
        });
        i += 1;
    }
    out
}

/// Runs of units in `alphabet`, joined across spaces, tabs and line breaks (wrapped copies).
fn segments(units: &[Unit], alphabet: impl Fn(u8) -> bool) -> Vec<Vec<Unit>> {
    let mut out = Vec::new();
    let mut cur: Vec<Unit> = Vec::new();
    for u in units {
        if alphabet(u.byte) {
            cur.push(*u);
        } else if !matches!(u.byte, b' ' | b'\t' | b'\n' | b'\r') && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out.retain(|s| s.len() >= PIECE);
    out
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Standard and URL-safe base64 at once (`=` padding ends a segment).
fn b64_value(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    }
}

fn hex_decoded(seg: &[Unit], phase: usize) -> Vec<Unit> {
    seg.get(phase..)
        .unwrap_or_default()
        .as_chunks::<2>()
        .0
        .iter()
        .filter_map(|[hi, lo]| {
            Some(Unit {
                byte: hex_value(hi.byte)? << 4 | hex_value(lo.byte)?,
                start: hi.start,
                end: lo.end,
            })
        })
        .collect()
}

/// Decode from character `phase` on; each byte spans its whole 4-character group, so a hit
/// hides every character that carries bits of the value.
fn b64_decoded(seg: &[Unit], phase: usize) -> Vec<Unit> {
    let mut out = Vec::new();
    for group in seg.get(phase..).unwrap_or_default().chunks(4) {
        let v: Vec<u8> = group.iter().filter_map(|u| b64_value(u.byte)).collect();
        let (start, end) = (group[0].start, group[group.len() - 1].end);
        let bytes = [
            v.first().zip(v.get(1)).map(|(a, b)| a << 2 | b >> 4),
            v.get(1)
                .zip(v.get(2))
                .map(|(a, b)| (a & 0x0f) << 4 | b >> 2),
            v.get(2).zip(v.get(3)).map(|(a, b)| (a & 0x03) << 6 | b),
        ];
        out.extend(
            bytes
                .into_iter()
                .flatten()
                .map(|byte| Unit { byte, start, end }),
        );
    }
    out
}

/// Output cut at a size limit may end in the first few characters of a value, too short to
/// match: drop the last characters (at most [`CUT_TAIL`]) that could belong to one, and mark
/// the cut. Returns whether anything was dropped.
pub(crate) fn drop_cut_tail(text: &mut String) -> bool {
    let keep = text
        .bytes()
        .rev()
        .take(CUT_TAIL)
        .take_while(|&b| b64_value(b).is_some() || b == b'%')
        .count();
    if keep == 0 {
        return false;
    }
    text.truncate(text.len() - keep);
    text.push_str("[cut]");
    true
}

// ---- fingerprint store ------------------------------------------------------------------

/// Where fingerprints are kept: `<dir>/<app>.json` (mode 0600), or in memory (tests, and
/// installs without a state directory).
#[derive(Clone)]
pub struct SecretPrints {
    dir: Option<PathBuf>,
    mem: Arc<Mutex<BTreeMap<String, Value>>>,
    /// One writer at a time (two installs of one app can't race on the record).
    write: Arc<Mutex<()>>,
}

impl SecretPrints {
    pub fn in_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: Some(dir.into()),
            mem: Arc::default(),
            write: Arc::default(),
        }
    }

    pub fn in_memory() -> Self {
        Self {
            dir: None,
            mem: Arc::default(),
            write: Arc::default(),
        }
    }

    fn path(&self, app: &str) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join(format!("{app}.json")))
    }

    fn load(&self, app: &str) -> std::io::Result<Option<Value>> {
        match self.path(app) {
            None => Ok(self
                .mem
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(app)
                .cloned()),
            Some(path) => match std::fs::read(&path) {
                Ok(bytes) => serde_json::from_slice(&bytes)
                    .map(Some)
                    .map_err(std::io::Error::other),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            },
        }
    }

    fn store(&self, app: &str, doc: Value) -> std::io::Result<()> {
        let Some(path) = self.path(app) else {
            self.mem
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(app.to_string(), doc);
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(doc.to_string().as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &path)
    }

    /// Remember the fingerprints of a newly generated `value` of `field` (replacing an older
    /// value's). The value itself is not kept.
    pub(crate) fn record(&self, app: &str, field: &str, value: &str) -> std::io::Result<()> {
        let _write = self.write.lock().unwrap_or_else(PoisonError::into_inner);
        let mut doc = self.load(app)?.unwrap_or_else(|| json!({}));
        let salt = match doc["salt"].as_str().and_then(from_hex) {
            Some(s) if s.len() == SALT_BYTES => s,
            _ => {
                let mut s = vec![0u8; SALT_BYTES];
                std::fs::File::open("/dev/urandom")?.read_exact(&mut s)?;
                doc = json!({"salt": to_hex(&s), "fields": {}});
                s
            }
        };
        let pieces: Vec<String> = pieces_of(&salt, value).iter().map(|p| to_hex(p)).collect();
        doc["fields"][field] = json!(pieces);
        self.store(app, doc)
    }

    /// The app's redactor; `None` when nothing was recorded (or the record can't be read,
    /// which is logged: the output is then shown unredacted, as before this existed).
    pub(crate) fn redactor(&self, app: &str) -> Option<Redactor> {
        let doc = match self.load(app) {
            Ok(doc) => doc?,
            Err(e) => {
                tracing::warn!(app, "secret fingerprints unreadable: {e}");
                return None;
            }
        };
        let salt = doc["salt"].as_str().and_then(from_hex)?;
        let mut r = Redactor {
            salt,
            pieces: HashMap::new(),
            fields: Vec::new(),
        };
        for (field, pieces) in doc["fields"].as_object()? {
            for p in pieces.as_array().into_iter().flatten() {
                if let Some(p) = p.as_str().and_then(from_hex) {
                    if let Ok(p) = Print::try_from(p.as_slice()) {
                        r.pieces.insert(p, r.fields.len());
                    }
                }
            }
            r.fields.push(field.clone());
        }
        (!r.pieces.is_empty()).then_some(r)
    }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    let (pairs, rest) = s.as_bytes().as_chunks::<2>();
    if !rest.is_empty() {
        return None;
    }
    pairs
        .iter()
        .map(|&[hi, lo]| Some(hex_value(hi)? << 4 | hex_value(lo)?))
        .collect()
}

// ---- approval card hints ----------------------------------------------------------------

/// The approval card's line for an `app_exec` that can print the environment or a config or
/// secret file.
pub(crate) const EXEC_SECRETS_NOTE: &str =
    "can show the app's passwords (Melissa hides the ones it generated from the agent, but not reliably)";

/// Programs that print the environment.
const ENV_PROGRAMS: &[&str] = &[
    "env", "printenv", "set", "export", "declare", "typeset", "compgen", "ps",
];
/// Programs that run the next argument.
const WRAPPERS: &[&str] = &[
    "busybox", "sudo", "doas", "nice", "nohup", "timeout", "exec", "command", "su", "su-exec",
    "gosu", "chroot",
];
const SHELLS: &[&str] = &["sh", "bash", "ash", "dash", "zsh", "ksh", "mksh"];
/// Parts of paths and names of files that commonly hold config or secrets.
const SENSITIVE: &[&str] = &[
    "environ",
    ".env",
    "secret",
    "passw",
    "credential",
    "token",
    "/etc/",
    "config",
    ".conf",
    ".cfg",
    ".ini",
    ".yml",
    ".yaml",
    ".toml",
    ".json",
    ".xml",
    ".properties",
    ".php",
    ".db",
    ".sqlite",
];

fn base_name(arg: &str) -> &str {
    arg.rsplit('/').next().unwrap_or(arg)
}

fn sensitive(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    SENSITIVE.iter().any(|s| lower.contains(s))
}

/// Whether an `app_exec` argv may print the environment or a config or secret file: `env`,
/// `printenv`, `set`, `export`, `ps`, anything naming `/proc/*/environ` or a config-like
/// path, and shell scripts (`sh -c`) that expand a variable (`$`) or do any of these. A
/// hint for the person approving, not a gate: commands can always find other ways.
pub(crate) fn exec_may_show_secrets(argv: &[String]) -> bool {
    let mut program = argv.first().map_or("", |a| base_name(a));
    let mut rest = argv.get(1..).unwrap_or_default();
    while WRAPPERS.contains(&program) {
        // Skip the wrapper's own options (`timeout 5 env`, `nice -n 5 env`).
        let Some(i) = rest
            .iter()
            .position(|a| !a.starts_with('-') && !a.chars().all(|c| c.is_ascii_digit()))
        else {
            break;
        };
        program = base_name(&rest[i]);
        rest = &rest[i + 1..];
    }
    if ENV_PROGRAMS.contains(&program) {
        return true;
    }
    if SHELLS.contains(&program) {
        let script = rest
            .iter()
            .position(|a| a.starts_with('-') && a.contains('c'))
            .and_then(|i| rest.get(i + 1));
        if let Some(script) = script {
            let words = script
                .split(|c: char| !(c.is_ascii_alphanumeric() || "_./-".contains(c)))
                .filter(|w| !w.is_empty());
            return script.contains('$')
                || words
                    .map(|w| (w, base_name(w)))
                    .any(|(w, b)| ENV_PROGRAMS.contains(&b) || sensitive(w));
        }
    }
    rest.iter().any(|a| sensitive(a))
}

/// The start of the approval card line for a deploy or update that passes a secret's
/// variable to the command, arguments, files or other env values (the web UI keeps this
/// part of the reason on the card).
pub const SECRET_USE_NOTE: &str = "uses generated secret";

/// The card line for a spec that puts a secret-backed env variable into a command, its
/// arguments, a file or another env value (where it can end up printed), e.g.
/// `sh -c 'echo $ADMIN_PASSWORD'` or an env value `$(ADMIN_PASSWORD)`. `None`: no such use.
pub(crate) fn spec_secret_use(spec: &AppSpec) -> Option<String> {
    let mut used = BTreeSet::new();
    for c in &spec.containers {
        let mut secret_vars = Vec::new();
        let mut texts: Vec<&str> = c
            .command
            .iter()
            .chain(&c.args)
            .map(String::as_str)
            .collect();
        texts.extend(c.files.iter().map(|f| f.content.as_str()));
        let plain: Vec<String> = c
            .env
            .iter()
            .filter_map(|(name, v)| match v.source()? {
                EnvSource::Secret(_) => {
                    secret_vars.push(name.as_str());
                    None
                }
                EnvSource::Value(v) => Some(v.into_owned()),
                EnvSource::Builtin(_) => None,
            })
            .collect();
        texts.extend(plain.iter().map(String::as_str));
        for var in secret_vars {
            if texts.iter().any(|t| mentions_var(t, var)) {
                used.insert(var);
            }
        }
    }
    if used.is_empty() {
        return None;
    }
    let names: Vec<&str> = used.into_iter().collect();
    let plural = if names.len() == 1 { "" } else { "s" };
    Some(format!(
        "{SECRET_USE_NOTE}{plural} {} in its command, files or settings: check it isn't printed",
        names.join(", ")
    ))
}

/// `$VAR`, `${VAR...}` or `$(VAR)` in `text`.
fn mentions_var(text: &str, var: &str) -> bool {
    ["$", "${", "$("].iter().any(|open| {
        let needle = format!("{open}{var}");
        text.match_indices(&needle).any(|(i, _)| {
            text[i + needle.len()..]
                .chars()
                .next()
                .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'))
        })
    })
}

#[cfg(test)]
mod tests;
