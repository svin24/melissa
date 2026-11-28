//! An app's backup hook: the optional `backup` field of the spec (docs/design/BACKUPS-2026-11-28.md).
//!
//! How Melissa makes a consistent copy of the app's data for the root backup service
//! (`melissa_backup`):
//! - `dump`: run `dump.command` in `dump.container` and keep its standard output as `dump.file`
//!   (a database's own dump tool: `pg_dumpall`, `mariadb-dump`). The volumes are copied as they
//!   are afterwards, without the paths in `exclude` (usually the database's own files, which the
//!   dump covers). On a restore the volumes come back first, then `restore.command` gets the dump
//!   on its standard input. Without `restore` the dump is only a step before the copy (Redis
//!   writing its `dump.rdb`).
//! - `stop`: scale the app to 0, copy its volumes, start it again. The default for an app whose
//!   spec has no `backup` field: always consistent, a short break.
//! - `live`: copy the volumes while the app runs (apps whose files are always consistent).
//! - `skip`: no volume data (caches, apps with nothing worth keeping); the spec and the app's
//!   generated secrets are still backed up.
//!
//! Commands run like `app_exec`: an argv in the app's own container, no shell unless the argv
//! names one, with the app's own permissions. [`known_dumps`] and [`suggest`] are a small
//! catalogue for well-known database images that the deploy path offers to agents.

use serde::{Deserialize, Serialize};

use crate::{validate::dns_label, AppSpec, SpecError, MAX_ARGS, MAX_ARG_BYTES};

/// Most `exclude` entries.
pub const MAX_BACKUP_EXCLUDES: usize = 16;

/// How the app's data is copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BackupMode {
    Dump,
    Stop,
    Live,
    Skip,
}

impl BackupMode {
    pub const ALL: [BackupMode; 4] = [Self::Dump, Self::Stop, Self::Live, Self::Skip];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dump => "dump",
            Self::Stop => "stop",
            Self::Live => "live",
            Self::Skip => "skip",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == name)
    }
}

/// The spec's `backup` field.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Backup {
    pub mode: BackupMode,
    /// Required with mode `dump`, refused otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dump: Option<DumpHook>,
    /// Only with `dump`: how the dump goes back in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore: Option<RestoreHook>,
    /// Not copied from the volumes: `<volume>` (all of it) or `<volume>/<path inside it>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

/// A command whose standard output is the dump.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DumpHook {
    pub container: String,
    pub command: Vec<String>,
    /// The dump's file name in the backup (`db.sql`).
    pub file: String,
}

/// A command that reads the dump on its standard input.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreHook {
    pub container: String,
    pub command: Vec<String>,
    /// The dump file it reads (must be `dump.file`).
    pub stdin: String,
}

/// One `exclude` entry, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exclude<'a> {
    pub volume: &'a str,
    /// Relative path inside the volume; `None`: the whole volume.
    pub path: Option<&'a str>,
}

impl Backup {
    /// The parsed `exclude` entries (call on a validated spec; bad entries are skipped).
    pub fn excludes(&self) -> Vec<Exclude<'_>> {
        self.exclude
            .iter()
            .filter_map(|e| parse_exclude(e).ok())
            .collect()
    }

    /// One line for the confirmation.
    pub fn words(&self) -> String {
        let base = match (self.mode, &self.dump) {
            (BackupMode::Dump, Some(d)) => format!(
                "dump with `{}` in {}, then copy the files",
                d.command.join(" "),
                d.container
            ),
            (BackupMode::Stop, _) | (BackupMode::Dump, None) => {
                "stop the app briefly and copy its files".into()
            }
            (BackupMode::Live, _) => "copy its files while it runs".into(),
            (BackupMode::Skip, _) => "settings and secrets only, no files".into(),
        };
        if self.exclude.is_empty() {
            base
        } else {
            format!("{base} (except {})", self.exclude.join(", "))
        }
    }

    /// Check the hook against the spec it belongs to.
    pub(crate) fn validate(&self, spec: &AppSpec) -> Result<(), SpecError> {
        let err = |path: &str, m: String| SpecError::invalid(format!("backup.{path}"), m);
        let container = |path: &str, name: &str| {
            if spec.containers.iter().any(|c| c.name == name) {
                Ok(())
            } else {
                Err(err(path, format!("no container `{name}`")))
            }
        };
        let command = |path: &str, argv: &[String]| {
            if argv.is_empty()
                || argv.len() > MAX_ARGS
                || argv
                    .iter()
                    .any(|a| a.is_empty() || a.len() > MAX_ARG_BYTES || a.contains('\0'))
            {
                Err(err(
                    path,
                    format!("1 to {MAX_ARGS} non-empty items of at most {MAX_ARG_BYTES} bytes"),
                ))
            } else {
                Ok(())
            }
        };
        match (self.mode, &self.dump) {
            (BackupMode::Dump, None) => {
                return Err(err("dump", "mode `dump` needs a `dump` command".into()))
            }
            (BackupMode::Dump, Some(d)) => {
                container("dump.container", &d.container)?;
                command("dump.command", &d.command)?;
                if !file_name(&d.file) {
                    return Err(err(
                        "dump.file",
                        "a file name: 1-64 letters, digits, `.`, `-` or `_`".into(),
                    ));
                }
            }
            (_, Some(_)) => {
                return Err(err("dump", "only with mode `dump`".into()));
            }
            (_, None) => {}
        }
        if let Some(r) = &self.restore {
            let Some(d) = &self.dump else {
                return Err(err("restore", "only with mode `dump`".into()));
            };
            container("restore.container", &r.container)?;
            command("restore.command", &r.command)?;
            if r.stdin != d.file {
                return Err(err(
                    "restore.stdin",
                    format!("must be the dump's file `{}`", d.file),
                ));
            }
        }
        if self.mode == BackupMode::Skip && !self.exclude.is_empty() {
            return Err(err("exclude", "mode `skip` copies no files anyway".into()));
        }
        if self.exclude.len() > MAX_BACKUP_EXCLUDES {
            return Err(err(
                "exclude",
                format!("at most {MAX_BACKUP_EXCLUDES} entries"),
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for (i, e) in self.exclude.iter().enumerate() {
            let path = format!("exclude[{i}]");
            let parsed = parse_exclude(e).map_err(|m| err(&path, m))?;
            if !spec.volumes.iter().any(|v| v.name == parsed.volume) {
                return Err(err(&path, format!("no volume `{}`", parsed.volume)));
            }
            if !seen.insert(e.as_str()) {
                return Err(err(&path, format!("`{e}` is listed twice")));
            }
            // With a dump, a volume left out whole is only in the dump: without a restore
            // command its data could never come back. (In the other modes leaving out a whole
            // volume means "not worth keeping", e.g. a cache.)
            if self.mode == BackupMode::Dump && parsed.path.is_none() && self.restore.is_none() {
                return Err(err(
                    &path,
                    format!(
                        "volume `{}` is left out whole, so its data is only in the dump: add a \
                         `restore` command, or exclude paths inside it instead",
                        parsed.volume
                    ),
                ));
            }
        }
        Ok(())
    }
}

/// `volume` or `volume/rel/path`: a volume name, then plain path parts (letters, digits, `.`,
/// `-`, `_`; no `.`/`..`, no glob characters).
pub fn parse_exclude(entry: &str) -> Result<Exclude<'_>, String> {
    let (volume, path) = match entry.split_once('/') {
        Some((v, p)) => (v, Some(p)),
        None => (entry, None),
    };
    dns_label(volume, 40)?;
    if let Some(p) = path {
        let ok = !p.is_empty()
            && p.len() <= 256
            && p.split('/').all(|part| {
                !part.is_empty()
                    && part != "."
                    && part != ".."
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            });
        if !ok {
            return Err(format!(
                "`{entry}`: `<volume>` or `<volume>/<path>` with plain path parts \
                 (letters, digits, `.`, `-`, `_`)"
            ));
        }
    }
    Ok(Exclude { volume, path })
}

/// A dump file name: 1-64 of `[A-Za-z0-9._-]`, starting with a letter or digit.
pub fn file_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.starts_with(|c: char| c.is_ascii_alphanumeric())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// A known way to back up a well-known image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownDump {
    /// `postgres`.
    pub name: &'static str,
    /// The last part of image repositories it fits (`postgres` matches
    /// `docker.io/library/postgres` and `docker.io/postgis/postgis`... only by this name).
    /// Empty: never suggested (a template the agent fills in).
    pub images: &'static [&'static str],
    pub mode: BackupMode,
    pub dump: &'static [&'static str],
    pub file: &'static str,
    /// Empty: no restore hook (the file copy is enough).
    pub restore: &'static [&'static str],
    /// What the agent should check.
    pub note: &'static str,
}

const POSTGRES: KnownDump = KnownDump {
    name: "postgres",
    images: &["postgres", "postgis", "pgvector", "timescaledb"],
    mode: BackupMode::Dump,
    dump: &[
        "sh",
        "-c",
        "pg_dumpall --clean --if-exists -U \"${POSTGRES_USER:-postgres}\"",
    ],
    file: "postgres.sql",
    restore: &[
        "sh",
        "-c",
        "psql -q -U \"${POSTGRES_USER:-postgres}\" -d postgres",
    ],
    note: "the official image trusts local socket connections; with another image check how \
           pg_dumpall and psql log in",
};

const MARIADB: KnownDump = KnownDump {
    name: "mariadb",
    images: &["mariadb"],
    mode: BackupMode::Dump,
    dump: &[
        "sh",
        "-c",
        "mariadb-dump --all-databases --single-transaction --routines --events -uroot \
         -p\"$MARIADB_ROOT_PASSWORD\"",
    ],
    file: "mariadb.sql",
    restore: &["sh", "-c", "mariadb -uroot -p\"$MARIADB_ROOT_PASSWORD\""],
    note: "needs MARIADB_ROOT_PASSWORD in the container's environment (a generated secret)",
};

const MYSQL: KnownDump = KnownDump {
    name: "mysql",
    images: &["mysql"],
    mode: BackupMode::Dump,
    dump: &[
        "sh",
        "-c",
        "mysqldump --all-databases --single-transaction --routines --events -uroot \
         -p\"$MYSQL_ROOT_PASSWORD\"",
    ],
    file: "mysql.sql",
    restore: &["sh", "-c", "mysql -uroot -p\"$MYSQL_ROOT_PASSWORD\""],
    note: "needs MYSQL_ROOT_PASSWORD in the container's environment (a generated secret)",
};

const REDIS: KnownDump = KnownDump {
    name: "redis",
    images: &["redis", "valkey"],
    mode: BackupMode::Dump,
    // BGSAVE, wait for it, print the file: the copy of the volume that follows then holds the
    // fresh dump.rdb (Redis renames it into place, so a copy is never half written), and a
    // restore needs no hook (Redis loads it at start). Valkey ships redis-cli as a link.
    dump: &[
        "sh",
        "-c",
        "t=$(redis-cli LASTSAVE); redis-cli BGSAVE >/dev/null; i=0; \
         while [ \"$(redis-cli LASTSAVE)\" = \"$t\" ] && [ $i -lt 600 ]; do sleep 1; i=$((i+1)); done; \
         cat \"$(redis-cli CONFIG GET dir | tail -n 1)/$(redis-cli CONFIG GET dbfilename | tail -n 1)\"",
    ],
    file: "dump.rdb",
    restore: &[],
    note: "with a password set, give redis-cli REDISCLI_AUTH; with appendonly yes, Redis loads \
           the AOF instead of dump.rdb at start",
};

const SQLITE: KnownDump = KnownDump {
    name: "sqlite",
    images: &[],
    mode: BackupMode::Dump,
    dump: &[
        "sh",
        "-c",
        "sqlite3 /data/app.db '.backup /tmp/melissa-backup.db' && cat /tmp/melissa-backup.db \
         && rm -f /tmp/melissa-backup.db",
    ],
    file: "app.db",
    restore: &[
        "sh",
        "-c",
        "cat > /tmp/melissa-restore.db && sqlite3 /data/app.db '.restore /tmp/melissa-restore.db' \
         && rm -f /tmp/melissa-restore.db",
    ],
    note: "a template: replace /data/app.db with the app's database file; the image needs the \
           sqlite3 command",
};

/// The catalogue: well-known database images, and the SQLite template.
pub fn known_dumps() -> &'static [KnownDump] {
    &[POSTGRES, MARIADB, MYSQL, REDIS, SQLITE]
}

/// A suggested `backup` field for a spec without one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// `postgres in container db`.
    pub why: String,
    pub backup: Backup,
    pub note: &'static str,
}

impl KnownDump {
    /// This entry as a `backup` field for `container`, excluding `exclude` (volume names).
    pub fn backup_for(&self, container: &str, exclude: Vec<String>) -> Backup {
        let argv = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        Backup {
            mode: self.mode,
            dump: Some(DumpHook {
                container: container.into(),
                command: argv(self.dump),
                file: self.file.into(),
            }),
            restore: (!self.restore.is_empty()).then(|| RestoreHook {
                container: container.into(),
                command: argv(self.restore),
                stdin: self.file.into(),
            }),
            exclude,
        }
    }
}

/// The last part of an image's repository (`docker.io/library/postgres:17` → `postgres`).
fn image_base(image: &str) -> &str {
    let no_digest = image.split('@').next().unwrap_or(image);
    let last = no_digest.rsplit('/').next().unwrap_or(no_digest);
    last.split(':').next().unwrap_or(last)
}

/// A backup hook from the catalogue for the first container whose image it knows. Volumes
/// mounted only by that container are excluded when the dump restores them (the dump holds the
/// database; a copy of its live files would not be consistent anyway).
pub fn suggest(spec: &AppSpec) -> Option<Suggestion> {
    for c in &spec.containers {
        let base = image_base(&c.image);
        let Some(known) = known_dumps().iter().find(|k| k.images.contains(&base)) else {
            continue;
        };
        let exclude: Vec<String> = if known.restore.is_empty() {
            Vec::new()
        } else {
            c.mounts
                .iter()
                .map(|m| m.volume.clone())
                .filter(|v| {
                    !spec
                        .containers
                        .iter()
                        .any(|o| o.name != c.name && o.mounts.iter().any(|m| &m.volume == v))
                })
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect()
        };
        return Some(Suggestion {
            why: format!("{} in container {}", known.name, c.name),
            backup: known.backup_for(&c.name, exclude),
            note: known.note,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(backup: serde_json::Value) -> Result<AppSpec, SpecError> {
        AppSpec::from_json(&json!({
            "description": "Paperless",
            "containers": [
                {"name": "web", "image": "ghcr.io/example/app:1.0",
                 "mounts": [{"volume": "data", "path": "/data"}]},
                {"name": "db", "image": "postgres:17",
                 "mounts": [{"volume": "pg", "path": "/var/lib/postgresql/data"}]}
            ],
            "volumes": [{"name": "data", "size": "1Gi"}, {"name": "pg", "size": "1Gi"}],
            "backup": backup,
        }))
    }

    #[test]
    fn a_dump_hook_parses_and_round_trips() {
        let s = spec(json!({
            "mode": "dump",
            "dump": {"container": "db", "command": ["pg_dumpall", "-U", "postgres"], "file": "db.sql"},
            "restore": {"container": "db", "command": ["psql", "-U", "postgres"], "stdin": "db.sql"},
            "exclude": ["pg", "data/cache"]
        }))
        .unwrap();
        let b = s.backup.as_ref().unwrap();
        assert_eq!(b.mode, BackupMode::Dump);
        assert_eq!(
            b.excludes(),
            vec![
                Exclude {
                    volume: "pg",
                    path: None
                },
                Exclude {
                    volume: "data",
                    path: Some("cache")
                }
            ]
        );
        let again = AppSpec::from_text(&s.to_text()).unwrap();
        assert_eq!(again, s);
        assert!(s
            .summary()
            .iter()
            .any(|l| l.starts_with("backup: dump with")));
    }

    #[test]
    fn simple_modes_need_nothing_else() {
        for mode in ["stop", "live", "skip"] {
            assert!(spec(json!({"mode": mode})).is_ok(), "{mode}");
        }
        assert_eq!(
            spec(json!({"mode": "live", "exclude": ["data/tmp"]}))
                .unwrap()
                .backup_mode(),
            BackupMode::Live
        );
    }

    #[test]
    fn no_backup_field_means_stop() {
        let s = AppSpec::from_json(&json!({
            "description": "x",
            "containers": [{"name": "a", "image": "nginx:1.27"}],
        }))
        .unwrap();
        assert_eq!(s.backup_mode(), BackupMode::Stop);
        assert!(!s.summary().iter().any(|l| l.starts_with("backup:")));
    }

    #[test]
    fn bad_hooks_are_refused_with_the_field() {
        let cases = [
            (json!({"mode": "dump"}), "backup.dump"),
            (json!({"mode": "nap"}), ""),
            (json!({"mode": "stop", "extra": 1}), ""),
            (
                json!({"mode": "stop", "dump": {"container": "db", "command": ["x"], "file": "a"}}),
                "backup.dump",
            ),
            (
                json!({"mode": "dump", "dump": {"container": "nope", "command": ["x"], "file": "a"}}),
                "backup.dump.container",
            ),
            (
                json!({"mode": "dump", "dump": {"container": "db", "command": [], "file": "a"}}),
                "backup.dump.command",
            ),
            (
                json!({"mode": "dump", "dump": {"container": "db", "command": ["x"], "file": "../a"}}),
                "backup.dump.file",
            ),
            (
                json!({"mode": "dump", "dump": {"container": "db", "command": ["x"], "file": "a"},
                       "restore": {"container": "db", "command": ["y"], "stdin": "b"}}),
                "backup.restore.stdin",
            ),
            (
                json!({"mode": "live", "restore": {"container": "db", "command": ["y"], "stdin": "b"}}),
                "backup.restore",
            ),
            (
                json!({"mode": "live", "exclude": ["nope"]}),
                "backup.exclude[0]",
            ),
            (
                json!({"mode": "live", "exclude": ["data/../etc"]}),
                "backup.exclude[0]",
            ),
            (
                json!({"mode": "live", "exclude": ["data/*.log"]}),
                "backup.exclude[0]",
            ),
            (
                json!({"mode": "live", "exclude": ["data/"]}),
                "backup.exclude[0]",
            ),
            (
                json!({"mode": "live", "exclude": ["data", "data"]}),
                "backup.exclude[1]",
            ),
            (
                json!({"mode": "skip", "exclude": ["data"]}),
                "backup.exclude",
            ),
            // A whole volume only in a dump that can't be loaded again.
            (
                json!({"mode": "dump", "dump": {"container": "db", "command": ["x"], "file": "a"},
                       "exclude": ["data/cache", "pg"]}),
                "backup.exclude[1]",
            ),
        ];
        for (backup, field) in cases {
            match spec(backup.clone()) {
                Err(SpecError::Invalid { path, .. }) => assert_eq!(path, field, "{backup}"),
                Err(SpecError::Parse(_)) => assert_eq!(field, "", "{backup}"),
                Ok(_) => panic!("accepted {backup}"),
            }
        }
    }

    #[test]
    fn the_catalogue_suggests_a_dump_for_known_databases() {
        let s = spec(serde_json::Value::Null).unwrap();
        let sug = suggest(&s).unwrap();
        assert_eq!(sug.why, "postgres in container db");
        assert_eq!(sug.backup.mode, BackupMode::Dump);
        assert_eq!(sug.backup.exclude, vec!["pg".to_string()]);
        // The suggestion is a valid field for this spec.
        let mut with = s.clone();
        with.backup = Some(sug.backup);
        with.validate().unwrap();

        let plain = AppSpec::from_json(&json!({
            "description": "x",
            "containers": [{"name": "a", "image": "nginx:1.27"}],
        }))
        .unwrap();
        assert_eq!(suggest(&plain), None);
    }

    #[test]
    fn every_catalogue_entry_is_a_valid_hook() {
        for k in known_dumps() {
            let s = AppSpec::from_json(&json!({
                "description": "x",
                "containers": [{"name": "db", "image": "nginx:1.27",
                                "mounts": [{"volume": "v", "path": "/data"}]}],
                "volumes": [{"name": "v", "size": "1Gi"}],
            }))
            .unwrap();
            let mut with = s.clone();
            with.backup = Some(k.backup_for("db", vec![]));
            with.validate()
                .unwrap_or_else(|e| panic!("{}: {e}", k.name));
        }
        for (image, name) in [
            ("docker.io/library/redis:7.4", "redis"),
            ("docker.io/valkey/valkey:8", "redis"),
            ("docker.io/library/mariadb:11@sha256:aaaa", "mariadb"),
            ("docker.io/postgis/postgis:17-3.5", "postgres"),
        ] {
            let base = image_base(image);
            let k = known_dumps()
                .iter()
                .find(|k| k.images.contains(&base))
                .unwrap();
            assert_eq!(k.name, name, "{image}");
        }
    }
}
