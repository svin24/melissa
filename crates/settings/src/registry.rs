//! The public ACP agent registry (Settings → Providers → Add provider): parsing, search, a
//! local cache, and installing an agent's release binary into the agents' home.
//!
//! The registry is a JSON file published by the Agent Client Protocol project
//! ([`REGISTRY_URL`]), the same list other ACP clients use. Melissa fetches it only when the
//! owner asks (the panel's "Load the list" / "Refresh"), never in the background; the web
//! crate does the download and hands the text to [`parse`] and [`Cache::store`]. This module
//! never touches the network.
//!
//! Format (version 1.0.0, checked 2026-11-20): `{"version", "agents": [{"id", "name",
//! "version", "description", "repository"?, "website"?, "license"?, "icon"?, "distribution":
//! {"binary": {"linux-x86_64": {"archive", "cmd", "args"?, "sha256"?}, ...}, "npx": {...},
//! "uvx": {...}}}]}`. Melissa installs only `binary` entries for this machine that carry a
//! SHA-256; `npx`/`uvx` need runtimes the image doesn't have, so those are "Enter manually".
//! Icons are never loaded (remote images); the UI uses Melissa's own marks.

use std::{
    io::Read,
    path::{Component, Path, PathBuf},
    process::Command,
};

use serde::Deserialize;
use serde_json::Value;

use crate::{
    clean_text,
    providers::{provider_id, valid_id, BUILT_IN},
    write_private, SettingsError,
};

/// Where the registry lives (documented in docs/PLAN.md §6).
pub const REGISTRY_URL: &str =
    "https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json";
/// The largest registry file Melissa reads (it is about 80 KB today).
pub const MAX_BYTES: usize = 2 << 20;
/// The largest release archive Melissa downloads.
pub const MAX_ARCHIVE_BYTES: u64 = 512 << 20;
/// The cache's file name in Melissa's state directory.
pub const CACHE_FILE: &str = "acp-registry.json";
/// Where registry agents are installed, relative to the agents' home.
pub const AGENTS_DIR: &str = ".local/share/melissa-agents";
/// Most agents kept from one registry file.
const MAX_AGENTS: usize = 500;

/// The registry, as far as Melissa uses it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Registry {
    pub agents: Vec<RegistryAgent>,
}

/// One agent in the registry. Text is cleaned (no control characters, cut to length).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryAgent {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    /// Its website or repository (https only).
    pub link: Option<String>,
    pub license: Option<String>,
    pub install: Install,
}

/// How Melissa can install a registry agent on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    /// A release binary with a checksum: Melissa downloads, checks and unpacks it.
    Binary(BinaryInstall),
    /// Not by Melissa; the owner can still enter a command by hand. The reason is short UI
    /// text ("Needs Node.js").
    Manual(&'static str),
}

/// A release archive for this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryInstall {
    pub url: String,
    /// Lowercase hex SHA-256 of the archive.
    pub sha256: String,
    /// The program inside the archive (relative, no `..`).
    pub cmd: String,
    /// The arguments that start its ACP server.
    pub args: Vec<String>,
    pub archive: ArchiveKind,
    /// The provider id it becomes (the program's file name).
    pub provider: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    TarGz,
    TarBz2,
    TarXz,
    Zip,
    /// The download is the program itself.
    Raw,
}

impl ArchiveKind {
    fn of(url: &str) -> Self {
        let path = url
            .split(['?', '#'])
            .next()
            .unwrap_or(url)
            .to_ascii_lowercase();
        if path.ends_with(".tar.gz") || path.ends_with(".tgz") {
            Self::TarGz
        } else if path.ends_with(".tar.bz2") || path.ends_with(".tbz2") {
            Self::TarBz2
        } else if path.ends_with(".tar.xz") || path.ends_with(".txz") {
            Self::TarXz
        } else if path.ends_with(".zip") {
            Self::Zip
        } else {
            Self::Raw
        }
    }
}

/// This machine's key in the registry's `binary` table (`linux-x86_64`, `linux-aarch64`).
pub fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

#[derive(Deserialize)]
struct RawAgent {
    id: String,
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    repository: Option<String>,
    #[serde(default)]
    website: Option<String>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    distribution: RawDistribution,
}

#[derive(Deserialize, Default)]
struct RawDistribution {
    #[serde(default)]
    binary: Option<serde_json::Map<String, Value>>,
    #[serde(default)]
    npx: Option<Value>,
    #[serde(default)]
    uvx: Option<Value>,
}

#[derive(Deserialize)]
struct RawBinary {
    archive: String,
    cmd: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    sha256: Option<String>,
}

fn https(url: &str) -> Option<String> {
    let ok = url.starts_with("https://")
        && url.len() <= 2048
        && !url.chars().any(|c| c.is_control() || c.is_whitespace());
    ok.then(|| url.to_string())
}

/// A relative path inside an archive: no `..`, not absolute.
fn safe_relative(p: &str) -> bool {
    let path = Path::new(p);
    !p.is_empty()
        && p.len() <= 512
        && !path.is_absolute()
        && path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        && path.file_name().is_some()
}

fn binary_install(raw: RawBinary) -> Install {
    let Some(url) = https(&raw.archive) else {
        return Install::Manual("Not a secure download");
    };
    let Some(sha) = raw
        .sha256
        .map(|s| s.to_ascii_lowercase())
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
    else {
        return Install::Manual("No checksum published");
    };
    if !safe_relative(&raw.cmd) {
        return Install::Manual("Unusual package");
    }
    let provider = provider_id(&raw.cmd);
    let file = Path::new(&raw.cmd).file_name().and_then(|n| n.to_str());
    if BUILT_IN.contains(&provider.as_str()) {
        return Install::Manual("Built into Melissa");
    }
    if !valid_id(&provider) || file != Some(provider.as_str()) {
        return Install::Manual("Unusual program name");
    }
    if raw.args.len() > 16
        || raw
            .args
            .iter()
            .any(|a| a.len() > 256 || a.chars().any(char::is_control))
    {
        return Install::Manual("Unusual package");
    }
    Install::Binary(BinaryInstall {
        archive: ArchiveKind::of(&url),
        url,
        sha256: sha,
        cmd: raw.cmd,
        args: raw.args,
        provider,
    })
}

fn agent(raw: RawAgent, platform: &str) -> Option<RegistryAgent> {
    if !valid_id(&raw.id) {
        return None;
    }
    let name = clean_text(&raw.name, 60);
    if name.trim().is_empty() {
        return None;
    }
    let d = raw.distribution;
    let install = match d.binary.as_ref().and_then(|b| b.get(platform)) {
        Some(v) => match serde_json::from_value::<RawBinary>(v.clone()) {
            Ok(b) => binary_install(b),
            Err(_) => Install::Manual("Unusual package"),
        },
        None if d.npx.is_some() => Install::Manual("Needs Node.js"),
        None if d.uvx.is_some() => Install::Manual("Needs Python (uv)"),
        None => Install::Manual("Not for this computer"),
    };
    Some(RegistryAgent {
        id: raw.id,
        name,
        version: clean_text(&raw.version, 40),
        description: clean_text(&raw.description, 200),
        link: raw
            .website
            .as_deref()
            .and_then(https)
            .or_else(|| raw.repository.as_deref().and_then(https)),
        license: raw
            .license
            .map(|l| clean_text(&l, 40))
            .filter(|l| !l.is_empty()),
        install,
    })
}

/// Read the registry for `platform` (see [`platform`]). Entries Melissa can't read are
/// skipped; a file that isn't the registry at all is an error.
pub fn parse(text: &str, platform: &str) -> Result<Registry, SettingsError> {
    if text.len() > MAX_BYTES {
        return Err(SettingsError::Invalid("the registry is too large".into()));
    }
    let root: Value = serde_json::from_str(text)
        .map_err(|_| SettingsError::Invalid("the registry is not valid JSON".into()))?;
    let Some(list) = root.get("agents").and_then(Value::as_array) else {
        return Err(SettingsError::Invalid(
            "the registry lists no agents".into(),
        ));
    };
    let mut agents: Vec<RegistryAgent> = list
        .iter()
        .take(MAX_AGENTS)
        .filter_map(|v| serde_json::from_value::<RawAgent>(v.clone()).ok())
        .filter_map(|a| agent(a, platform))
        .collect();
    agents.sort_by_key(|a| a.name.to_lowercase());
    agents.dedup_by(|a, b| a.id == b.id);
    Ok(Registry { agents })
}

impl Registry {
    /// Agents whose name, id or description contains every word of `query` (any case).
    pub fn search(&self, query: &str) -> Vec<&RegistryAgent> {
        let words: Vec<String> = query
            .split_whitespace()
            .take(8)
            .map(str::to_lowercase)
            .collect();
        self.agents
            .iter()
            .filter(|a| {
                let hay = format!("{} {} {}", a.name, a.id, a.description).to_lowercase();
                words.iter().all(|w| hay.contains(w))
            })
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<&RegistryAgent> {
        self.agents.iter().find(|a| a.id == id)
    }
}

/// The registry as last fetched, in Melissa's state directory.
#[derive(Debug, Clone)]
pub struct Cache {
    path: PathBuf,
}

impl Cache {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The cached registry and when it was fetched (seconds since the epoch); None when there
    /// is no usable copy.
    pub fn load(&self, platform: &str) -> Option<(Registry, u64)> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        let registry = parse(&text, platform).ok()?;
        let at = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        Some((registry, at))
    }

    /// Keep a freshly fetched registry (checked first: a bad download never replaces a good
    /// copy).
    pub fn store(&self, text: &str, platform: &str) -> Result<Registry, SettingsError> {
        let registry = parse(text, platform)?;
        write_private(&self.path, text.as_bytes()).map_err(|source| SettingsError::Write {
            path: self.path.display().to_string(),
            source,
        })?;
        Ok(registry)
    }
}

/// Where a registry agent's version is installed: `<home>/.local/share/melissa-agents/<id>/<version>`.
pub fn install_dir(home: &Path, id: &str, version: &str) -> Option<PathBuf> {
    let ok = valid_id(id)
        && !version.is_empty()
        && version.len() <= 64
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
        && !version.starts_with('.');
    ok.then(|| home.join(AGENTS_DIR).join(id).join(version))
}

/// SHA-256 of a file, lowercase hex.
pub fn file_sha256(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn failed(msg: impl Into<String>) -> SettingsError {
    SettingsError::Failed(msg.into())
}

/// Unpack a checked download into `dest` (empty, made here) and return the program's path.
/// Archives go through the system's `tar`/`unzip`, which refuse members that climb out with
/// `..`; the program must then resolve inside `dest` and be a regular file.
pub fn unpack(download: &Path, b: &BinaryInstall, dest: &Path) -> Result<PathBuf, SettingsError> {
    std::fs::create_dir_all(dest).map_err(|e| failed(format!("can't make the folder: {e}")))?;
    let run = |cmd: &mut Command| -> Result<(), SettingsError> {
        let out = cmd
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LC_ALL", "C")
            .output()
            .map_err(|e| failed(format!("can't unpack: {e}")))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(failed("the download couldn't be unpacked"))
        }
    };
    let program = dest.join(&b.cmd);
    match b.archive {
        ArchiveKind::Raw => {
            std::fs::copy(download, &program).map_err(|e| failed(format!("can't copy: {e}")))?;
        }
        ArchiveKind::Zip => run(Command::new("unzip")
            .arg("-q")
            .arg("-o")
            .arg(download)
            .arg("-d")
            .arg(dest))?,
        kind => {
            let flag = match kind {
                ArchiveKind::TarBz2 => "-j",
                ArchiveKind::TarXz => "-J",
                _ => "-z",
            };
            run(Command::new("tar")
                .arg("-x")
                .arg(flag)
                .arg("--no-same-owner")
                .arg("-f")
                .arg(download)
                .arg("-C")
                .arg(dest))?;
        }
    }
    let real =
        std::fs::canonicalize(&program).map_err(|_| failed("the program isn't in the download"))?;
    let root = std::fs::canonicalize(dest).map_err(|e| failed(e.to_string()))?;
    let meta = std::fs::metadata(&real).map_err(|e| failed(e.to_string()))?;
    if !real.starts_with(&root) || !meta.is_file() {
        return Err(failed("the program isn't in the download"));
    }
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = meta.permissions();
        perm.set_mode(perm.mode() | 0o755);
        std::fs::set_permissions(&real, perm).map_err(|e| failed(e.to_string()))?;
    }
    Ok(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) const FIXTURE: &str = include_str!("../tests/fixtures/acp-registry.json");

    #[test]
    fn parses_the_registry_fixture() {
        let r = parse(FIXTURE, "linux-x86_64").unwrap();
        let ids: Vec<&str> = r.agents.iter().map(|a| a.id.as_str()).collect();
        // Sorted by name; the entry with a bad id and the one without a name are skipped.
        assert_eq!(
            ids,
            ["codex-acp", "fake-agent", "gemini", "goose", "nochecksum"]
        );
        let fake = r.get("fake-agent").unwrap();
        let Install::Binary(b) = &fake.install else {
            panic!("{fake:?}")
        };
        assert_eq!(b.provider, "fake-agent");
        assert_eq!(b.archive, ArchiveKind::TarGz);
        assert_eq!(b.args, ["acp"]);
        assert_eq!(fake.link.as_deref(), Some("https://example.com/fake"));
        // Control characters are gone from text.
        assert!(!fake.description.contains('\u{1b}'));
        assert_eq!(
            r.get("codex-acp").unwrap().install,
            Install::Manual("Needs Node.js")
        );
        assert_eq!(
            r.get("nochecksum").unwrap().install,
            Install::Manual("No checksum published")
        );
        // goose: tar.bz2 on x86_64, nothing on another platform; its http link is dropped.
        let goose = r.get("goose").unwrap();
        assert!(matches!(&goose.install, Install::Binary(b) if b.archive == ArchiveKind::TarBz2));
        assert_eq!(goose.link, None);
        let other = parse(FIXTURE, "linux-riscv64").unwrap();
        assert_eq!(
            other.get("goose").unwrap().install,
            Install::Manual("Not for this computer")
        );
    }

    #[test]
    fn rejects_what_is_not_a_registry() {
        assert!(parse("{}", "linux-x86_64").is_err());
        assert!(parse("not json", "linux-x86_64").is_err());
        assert!(parse(&" ".repeat(MAX_BYTES + 1), "linux-x86_64").is_err());
    }

    #[test]
    fn unsafe_entries_are_manual_only() {
        let raw = |archive: &str, cmd: &str| RawBinary {
            archive: archive.into(),
            cmd: cmd.into(),
            args: vec![],
            sha256: Some("a".repeat(64)),
        };
        for (archive, cmd) in [
            ("http://example.com/a.tar.gz", "./agent"),
            ("https://example.com/a.tar.gz", "../agent"),
            ("https://example.com/a.tar.gz", "/usr/bin/agent"),
            ("https://example.com/a.tar.gz", "./Agent Tool"),
            ("https://example.com/a.tar.gz", "./claude"),
        ] {
            assert!(
                matches!(binary_install(raw(archive, cmd)), Install::Manual(_)),
                "{archive} {cmd}"
            );
        }
        assert!(matches!(
            binary_install(raw("https://example.com/a.tar.gz", "./bin/agent")),
            Install::Binary(_)
        ));
    }

    #[test]
    fn search_matches_every_word() {
        let r = parse(FIXTURE, "linux-x86_64").unwrap();
        let names = |q: &str| -> Vec<String> { r.search(q).iter().map(|a| a.id.clone()).collect() };
        assert_eq!(names("GOOSE"), ["goose"]);
        assert_eq!(names("fake test"), ["fake-agent"]);
        assert_eq!(names("").len(), r.agents.len());
        assert!(names("nothing-like-this").is_empty());
    }

    #[test]
    fn cache_keeps_only_good_copies() {
        let dir = crate::test_support::scratch("registry");
        let cache = Cache::new(dir.join(CACHE_FILE));
        assert!(cache.load("linux-x86_64").is_none());
        assert!(cache.store("{}", "linux-x86_64").is_err());
        assert!(cache.load("linux-x86_64").is_none());
        cache.store(FIXTURE, "linux-x86_64").unwrap();
        let (r, at) = cache.load("linux-x86_64").unwrap();
        assert_eq!(r.agents.len(), 5);
        assert!(at > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_dirs_stay_under_the_agents_folder() {
        let home = Path::new("/home/m");
        assert_eq!(
            install_dir(home, "kimi", "1.2.0").unwrap(),
            Path::new("/home/m/.local/share/melissa-agents/kimi/1.2.0")
        );
        for (id, v) in [
            ("kimi", "../x"),
            ("kimi", ""),
            ("../k", "1"),
            ("kimi", ".."),
            ("kimi", "a/b"),
        ] {
            assert!(install_dir(home, id, v).is_none(), "{id} {v}");
        }
    }

    #[test]
    fn unpacks_a_tarball_and_finds_the_program() {
        let dir = crate::test_support::scratch("unpack");
        let src = dir.join("src/pkg");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("agent"), "#!/bin/sh\n").unwrap();
        let tgz = dir.join("a.tar.gz");
        let ok = Command::new("tar")
            .arg("-czf")
            .arg(&tgz)
            .arg("-C")
            .arg(dir.join("src"))
            .arg("pkg")
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let b = BinaryInstall {
            url: "https://example.com/a.tar.gz".into(),
            sha256: file_sha256(&tgz).unwrap(),
            cmd: "./pkg/agent".into(),
            args: vec![],
            archive: ArchiveKind::TarGz,
            provider: "agent".into(),
        };
        let p = unpack(&tgz, &b, &dir.join("out")).unwrap();
        assert!(p.ends_with("pkg/agent"));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o111,
            0o111
        );
        // A program that isn't in the archive.
        let missing = BinaryInstall {
            cmd: "./pkg/other".into(),
            ..b.clone()
        };
        assert!(unpack(&tgz, &missing, &dir.join("out2")).is_err());
        // A raw download is the program itself.
        let raw = BinaryInstall {
            cmd: "agent".into(),
            archive: ArchiveKind::Raw,
            ..b
        };
        assert!(unpack(&src.join("agent"), &raw, &dir.join("out3"))
            .unwrap()
            .is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
