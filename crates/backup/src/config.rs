//! Root's backup settings (`config.json` in [`crate::CONFIG_DIR`], 0600, versioned) and the
//! settings change the server may ask for ([`ConfigRequest`], checked here).

use std::path::{Component, Path, PathBuf};

use jiff::{civil, Timestamp, Zoned};
use serde::{Deserialize, Serialize};

use crate::{fsutil, BackupError};

/// The format of `config.json`. A newer one is refused (never half understood); older ones
/// are migrated when read.
pub const CONFIG_VERSION: u32 = 1;
pub const CONFIG_FILE: &str = "config.json";
/// Largest config or request file read.
pub const MAX_CONFIG_BYTES: u64 = 16 * 1024;
/// Most snapshots of each kind kept per period.
pub const MAX_KEEP: u32 = 400;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    /// The directory backups go to (a drive's mountpoint). `None`: not set up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<PathBuf>,
    /// Encrypt with a recovery key (recommended; the default).
    #[serde(default = "yes")]
    pub encrypted: bool,
    /// The repository this device made or adopted at `target` (restic's id, also in the
    /// drive's marker). `None`: not initialised yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_id: Option<String>,
    #[serde(default)]
    pub schedule: Schedule,
    #[serde(default)]
    pub retention: Retention,
}

fn yes() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            target: None,
            encrypted: true,
            repo_id: None,
            schedule: Schedule::default(),
            retention: Retention::default(),
        }
    }
}

/// A day of the week, as in config and requests (`mon` .. `sun`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Day {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Day {
    fn civil(self) -> civil::Weekday {
        match self {
            Self::Mon => civil::Weekday::Monday,
            Self::Tue => civil::Weekday::Tuesday,
            Self::Wed => civil::Weekday::Wednesday,
            Self::Thu => civil::Weekday::Thursday,
            Self::Fri => civil::Weekday::Friday,
            Self::Sat => civil::Weekday::Saturday,
            Self::Sun => civil::Weekday::Sunday,
        }
    }
}

/// When scheduled backups run, in the device's time zone. Weekly on Sunday at 03:00 by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "every", rename_all = "lowercase", deny_unknown_fields)]
pub enum Schedule {
    Weekly {
        day: Day,
        hour: u8,
    },
    Daily {
        hour: u8,
    },
    /// Only when asked.
    Off,
}

impl Default for Schedule {
    fn default() -> Self {
        Self::Weekly {
            day: Day::Sun,
            hour: 3,
        }
    }
}

/// How long a failed scheduled run waits before the next try (a drive plugged in later).
pub const RETRY_AFTER_SECS: i64 = 6 * 3600;

impl Schedule {
    pub fn validate(&self) -> Result<(), BackupError> {
        match self {
            Self::Weekly { hour, .. } | Self::Daily { hour } if *hour > 23 => {
                Err(BackupError::Invalid("the hour must be 0-23".into()))
            }
            _ => Ok(()),
        }
    }

    /// The latest scheduled time at or before `now`; `None` when off.
    pub fn latest_slot(&self, now: &Zoned) -> Option<Timestamp> {
        let (hour, day) = match *self {
            Self::Off => return None,
            Self::Daily { hour } => (hour, None),
            Self::Weekly { day, hour } => (hour, Some(day.civil())),
        };
        let mut date = now.date();
        // At most a week back (plus one day for a slot later today).
        for _ in 0..9 {
            if day.is_none_or(|d| date.weekday() == d) {
                let slot = date
                    .at(i8::try_from(hour).ok()?, 0, 0, 0)
                    .to_zoned(now.time_zone().clone())
                    .ok()?
                    .timestamp();
                if slot <= now.timestamp() {
                    return Some(slot);
                }
            }
            date = date.yesterday().ok()?;
        }
        None
    }

    /// The next scheduled time after `now`.
    pub fn next_slot(&self, now: &Zoned) -> Option<Timestamp> {
        let (hour, day) = match *self {
            Self::Off => return None,
            Self::Daily { hour } => (hour, None),
            Self::Weekly { day, hour } => (hour, Some(day.civil())),
        };
        let mut date = now.date();
        for _ in 0..9 {
            if day.is_none_or(|d| date.weekday() == d) {
                let slot = date
                    .at(i8::try_from(hour).ok()?, 0, 0, 0)
                    .to_zoned(now.time_zone().clone())
                    .ok()?
                    .timestamp();
                if slot > now.timestamp() {
                    return Some(slot);
                }
            }
            date = date.tomorrow().ok()?;
        }
        None
    }

    /// Is a scheduled run due? Yes when nothing was tried since the latest slot, or the last
    /// try failed, nothing has succeeded since the slot, and [`RETRY_AFTER_SECS`] have passed.
    pub fn is_due(&self, now: &Zoned, last_attempt: Option<i64>, last_ok: Option<i64>) -> bool {
        let Some(slot) = self.latest_slot(now).map(|t| t.as_second()) else {
            return false;
        };
        if last_ok.is_some_and(|ok| ok >= slot) {
            return false;
        }
        match last_attempt {
            None => true,
            Some(a) if a < slot => true,
            Some(a) => now.timestamp().as_second() - a >= RETRY_AFTER_SECS,
        }
    }
}

/// How many runs are kept (restic `forget --keep-*`, then `prune`), per item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retention {
    #[serde(default)]
    pub daily: u32,
    #[serde(default)]
    pub weekly: u32,
    #[serde(default)]
    pub monthly: u32,
}

impl Default for Retention {
    /// Four weekly and six monthly.
    fn default() -> Self {
        Self {
            daily: 0,
            weekly: 4,
            monthly: 6,
        }
    }
}

impl Retention {
    pub fn validate(&self) -> Result<(), BackupError> {
        if [self.daily, self.weekly, self.monthly]
            .iter()
            .any(|n| *n > MAX_KEEP)
        {
            return Err(BackupError::Invalid(format!(
                "keep at most {MAX_KEEP} of each"
            )));
        }
        if self.daily + self.weekly + self.monthly == 0 {
            return Err(BackupError::Invalid("keep at least one backup".into()));
        }
        Ok(())
    }

    /// restic's `forget` options for this policy (zero counts left out).
    pub fn forget_args(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (flag, n) in [
            ("--keep-daily", self.daily),
            ("--keep-weekly", self.weekly),
            ("--keep-monthly", self.monthly),
        ] {
            if n > 0 {
                out.push(flag.to_string());
                out.push(n.to_string());
            }
        }
        out
    }
}

impl Config {
    pub fn path(dir: &Path) -> PathBuf {
        dir.join(CONFIG_FILE)
    }

    /// Read the config in `dir` (the default when there is none).
    pub fn load(dir: &Path) -> Result<Self, BackupError> {
        let path = Self::path(dir);
        let Some(text) = fsutil::read_small(&path, MAX_CONFIG_BYTES)? else {
            return Ok(Self::default());
        };
        Self::parse(&text).map_err(|m| BackupError::Invalid(format!("{}: {m}", path.display())))
    }

    /// Parse (and migrate) config text.
    pub fn parse(text: &str) -> Result<Self, String> {
        let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let version = value
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .ok_or("no version")?;
        if version > u64::from(CONFIG_VERSION) {
            return Err(format!(
                "written by a newer Melissa (format {version}, this one reads up to {CONFIG_VERSION})"
            ));
        }
        // Version 1 is the first: nothing to migrate yet. A later format migrates here.
        let mut config: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        config.version = CONFIG_VERSION;
        config.schedule.validate().map_err(|e| e.to_string())?;
        config.retention.validate().map_err(|e| e.to_string())?;
        Ok(config)
    }

    /// Save atomically (0600).
    pub fn save(&self, dir: &Path) -> Result<(), BackupError> {
        let text =
            serde_json::to_vec_pretty(self).map_err(|e| BackupError::Failed(e.to_string()))?;
        fsutil::ensure_dir(dir, 0o700)?;
        fsutil::write_atomic(&Self::path(dir), &text, 0o600, false)
    }

    /// Settings, repository and target are there: backups can run.
    pub fn ready(&self) -> Result<(&Path, &str), BackupError> {
        let target = self.target.as_deref().ok_or(BackupError::NotConfigured)?;
        let id = self.repo_id.as_deref().ok_or(BackupError::NoRepository)?;
        Ok((target, id))
    }
}

/// A settings change the server asks for (`config.json` in the request directory). Absent
/// fields stay as they are.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<Retention>,
}

/// A plain absolute path: no `.`/`..`, no empty parts, no trailing `/`.
pub fn plain_absolute(p: &Path) -> bool {
    let s = p.to_string_lossy();
    p.is_absolute()
        && !s.ends_with('/')
        && !s.contains("//")
        && p.components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
}

/// May a backup target be at `target`? Below one of `roots`, at least one level down.
pub fn target_allowed(target: &Path, roots: &[&Path]) -> bool {
    plain_absolute(target) && roots.iter().any(|r| target.starts_with(r) && target != *r)
}

impl ConfigRequest {
    /// Apply to `config`. A new target forgets the repository (it is set up again or adopted);
    /// encryption can only change together with a new, empty target.
    pub fn apply(&self, config: &mut Config, roots: &[&Path]) -> Result<(), BackupError> {
        let mut next = config.clone();
        if let Some(t) = &self.target {
            if !target_allowed(t, roots) {
                let list: Vec<String> = roots.iter().map(|r| r.display().to_string()).collect();
                return Err(BackupError::Invalid(format!(
                    "a backup drive must be below {}",
                    list.join(" or ")
                )));
            }
            if next.target.as_deref() != Some(t.as_path()) {
                next.target = Some(t.clone());
                next.repo_id = None;
            }
        }
        if let Some(e) = self.encrypted {
            if e != next.encrypted && next.repo_id.is_some() {
                return Err(BackupError::Invalid(
                    "encryption is set when a drive is set up; to change it, set up a new drive \
                     or an empty folder"
                        .into(),
                ));
            }
            next.encrypted = e;
        }
        if let Some(s) = self.schedule {
            s.validate()?;
            next.schedule = s;
        }
        if let Some(r) = self.retention {
            r.validate()?;
            next.retention = r;
        }
        *config = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::scratch;
    use jiff::tz::TimeZone;

    fn at(s: &str) -> Zoned {
        s.parse::<civil::DateTime>()
            .unwrap()
            .to_zoned(TimeZone::UTC)
            .unwrap()
    }

    #[test]
    fn defaults_are_weekly_encrypted_four_weekly_six_monthly() {
        let c = Config::default();
        assert!(c.encrypted);
        assert_eq!(
            c.schedule,
            Schedule::Weekly {
                day: Day::Sun,
                hour: 3
            }
        );
        assert_eq!(
            c.retention.forget_args(),
            ["--keep-weekly", "4", "--keep-monthly", "6"]
        );
        assert!(matches!(c.ready(), Err(BackupError::NotConfigured)));
    }

    #[test]
    fn config_round_trips_and_refuses_newer_or_unknown() {
        let d = scratch("config");
        assert_eq!(Config::load(&d).unwrap(), Config::default());
        let c = Config {
            target: Some("/run/melissa-backup/usb1".into()),
            repo_id: Some("abc".into()),
            schedule: Schedule::Daily { hour: 22 },
            ..Config::default()
        };
        c.save(&d).unwrap();
        assert_eq!(Config::load(&d).unwrap(), c);
        let text = std::fs::read_to_string(Config::path(&d)).unwrap();
        assert!(text.contains("\"every\": \"daily\""), "{text}");

        assert!(Config::parse(r#"{"version": 2}"#)
            .unwrap_err()
            .contains("newer"));
        assert!(Config::parse(r#"{"version": 1, "surprise": true}"#).is_err());
        assert!(Config::parse(r#"{"encrypted": true}"#).is_err());
        assert!(
            Config::parse(r#"{"version": 1, "schedule": {"every": "daily", "hour": 24}}"#).is_err()
        );
        // Old minimal files get the defaults.
        assert_eq!(
            Config::parse(r#"{"version": 1}"#).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn retention_is_bounded() {
        assert!(Retention {
            daily: 0,
            weekly: 0,
            monthly: 0
        }
        .validate()
        .is_err());
        assert!(Retention {
            daily: 401,
            weekly: 0,
            monthly: 0
        }
        .validate()
        .is_err());
        let r = Retention {
            daily: 7,
            weekly: 0,
            monthly: 12,
        };
        r.validate().unwrap();
        assert_eq!(
            r.forget_args(),
            ["--keep-daily", "7", "--keep-monthly", "12"]
        );
    }

    #[test]
    fn weekly_and_daily_slots() {
        let weekly = Schedule::default();
        // 2026-11-28 is a Saturday: the latest Sunday 03:00 is 2026-11-22.
        let sat = at("2026-11-28T12:00:00");
        assert_eq!(
            weekly.latest_slot(&sat).unwrap(),
            at("2026-11-22T03:00:00").timestamp()
        );
        assert_eq!(
            weekly.next_slot(&sat).unwrap(),
            at("2026-11-29T03:00:00").timestamp()
        );
        let sun_early = at("2026-11-29T02:59:00");
        assert_eq!(
            weekly.latest_slot(&sun_early).unwrap(),
            at("2026-11-22T03:00:00").timestamp()
        );
        let sun = at("2026-11-29T03:00:00");
        assert_eq!(weekly.latest_slot(&sun).unwrap(), sun.timestamp());

        let daily = Schedule::Daily { hour: 22 };
        assert_eq!(
            daily.latest_slot(&sat).unwrap(),
            at("2026-11-27T22:00:00").timestamp()
        );
        assert_eq!(Schedule::Off.latest_slot(&sat), None);
        assert!(!Schedule::Off.is_due(&sat, None, None));
    }

    #[test]
    fn due_once_per_slot_with_slow_retries() {
        let s = Schedule::default();
        let slot = at("2026-11-29T03:00:00").timestamp().as_second();
        let now = at("2026-11-29T04:00:00");
        assert!(s.is_due(&now, None, None));
        assert!(s.is_due(&now, Some(slot - 10), Some(slot - 10)));
        // Tried after the slot and it worked: not again.
        assert!(!s.is_due(&now, Some(slot + 5), Some(slot + 60)));
        // Tried and failed (drive missing): not right away, but after the retry wait.
        assert!(!s.is_due(&now, Some(slot + 5), Some(slot - 7 * 86400)));
        let later = at("2026-11-29T10:00:00");
        assert!(s.is_due(&later, Some(slot + 5), Some(slot - 7 * 86400)));
    }

    #[test]
    fn requests_are_checked_against_the_drive_root() {
        let roots = [Path::new("/run/melissa-backup")];
        let mut c = Config::default();
        for bad in [
            "/etc",
            "/run/melissa-backup",
            "/run/melissa-backup/../../etc",
            "/run/melissa-backup/a/",
            "relative",
        ] {
            let r = ConfigRequest {
                target: Some(bad.into()),
                ..Default::default()
            };
            assert!(r.apply(&mut c, &roots).is_err(), "{bad}");
        }
        let r = ConfigRequest {
            target: Some("/run/melissa-backup/usb1".into()),
            encrypted: Some(false),
            schedule: Some(Schedule::Daily { hour: 1 }),
            retention: None,
        };
        r.apply(&mut c, &roots).unwrap();
        assert!(!c.encrypted);
        c.repo_id = Some("r1".into());
        // Encryption can't change under an existing repository ...
        let enc = ConfigRequest {
            encrypted: Some(true),
            ..Default::default()
        };
        assert!(enc.apply(&mut c, &roots).is_err());
        assert_eq!(c.repo_id.as_deref(), Some("r1"));
        // ... but with a new target it can (the repository is forgotten).
        let both = ConfigRequest {
            target: Some("/run/melissa-backup/usb2".into()),
            encrypted: Some(true),
            ..Default::default()
        };
        both.apply(&mut c, &roots).unwrap();
        assert!(c.encrypted);
        assert_eq!(c.repo_id, None);
        // The same target keeps the repository.
        c.repo_id = Some("r2".into());
        let same = ConfigRequest {
            target: Some("/run/melissa-backup/usb2".into()),
            ..Default::default()
        };
        same.apply(&mut c, &roots).unwrap();
        assert_eq!(c.repo_id.as_deref(), Some("r2"));
        assert!(serde_json::from_str::<ConfigRequest>(r#"{"target": "/x", "key": "y"}"#).is_err());
    }
}
