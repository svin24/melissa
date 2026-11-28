//! `melissa-server backup ...`: the root side of backups (`melissa_backup`,
//! docs/design/BACKUPS-2026-11-28.md). Root only; the server itself never runs any of this
//! (it leaves requests and reads the status).
//!
//! - `unit`: what `melissa-backup.service` runs (started by its path unit when a request
//!   arrives, and hourly by its timer): handle the requests, then the scheduled backup if due.
//! - `status`: print the status JSON.
//! - `now`: back up now and print the report.
//! - `setup <dir> [--no-encryption]`: by hand, set up a drive at any directory (root may pick
//!   any; the server only below `/run/melissa-backup`). Prints the recovery key once.
//! - `adopt <dir>`: use the backups already on a drive (the key is read from stdin when they
//!   are encrypted).
//! - `snapshots`: list what the drive holds.
//! - `restore-app <app> [<unix time>]`: restore one app (latest, or that run).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use melissa_backup::{
    apps::Kubectl,
    config::Config,
    engine::Restic,
    k3s,
    key::RecoveryKey,
    run::Device,
    status::{self, Status},
};

const USAGE: &str = "usage: melissa-server backup unit | status | now | setup <dir> \
                     [--no-encryption] | adopt <dir> | snapshots | restore-app <app> [<time>]";

fn is_root() -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0)
}

pub fn command(args: &[String]) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .init();
    if args.first().is_some_and(|a| a == "status") {
        // The status directory is readable by the melissa group too.
        let s = Status::load(Path::new(melissa_backup::STATUS_DIR))?;
        println!("{}", serde_json::to_string_pretty(&s)?);
        return Ok(());
    }
    if !is_root() {
        bail!("backups run as root inside the container: podman exec melissa melissa-server backup ...");
    }
    let engine = Restic::system();
    let cluster = Kubectl::system();
    let mut device = Device::new("/", &engine);
    if Path::new(melissa_backup::ADMIN_KUBECONFIG).is_file() {
        device.cluster = Some(&cluster);
    }
    device.k3s_version = std::fs::read_to_string(k3s::K3S_LOCK)
        .ok()
        .and_then(|t| k3s::lock_version(&t));
    match args {
        [c] if c == "unit" => {
            device.unit(&jiff::Zoned::now())?;
            Ok(())
        }
        [c] if c == "now" => {
            let _lock = device.lock()?;
            let report = device.backup("host");
            println!("{}", serde_json::to_string_pretty(&report)?);
            let ok = report.ok;
            device.record_backup(report).ok();
            if !ok {
                bail!("the backup did not finish cleanly");
            }
            Ok(())
        }
        [c, dir, rest @ ..] if c == "setup" => {
            let encrypted = match rest {
                [] => true,
                [f] if f == "--no-encryption" => false,
                _ => bail!(USAGE),
            };
            let _lock = device.lock()?;
            let mut config = device.config()?;
            config.target = Some(target(dir)?);
            config.encrypted = encrypted;
            config.repo_id = None;
            config.save(&device.config_dir())?;
            match device.init(&mut config)? {
                Some(key) => {
                    // Shown here; the server's copy is withdrawn (root saw it on the console).
                    status::withdraw_key(&device.status_dir()).ok();
                    println!(
                        "Backups set up at {dir}. Your recovery key (write it down; \
                              without it the backups can't be read on another device):\n\n  {}\n",
                        key.expose()
                    );
                }
                None => println!("Backups set up at {dir}, without encryption."),
            }
            device.update_status(|s| s.problem = None)?;
            Ok(())
        }
        [c, dir] if c == "adopt" => {
            let _lock = device.lock()?;
            let mut config = Config {
                target: Some(target(dir)?),
                ..device.config()?
            };
            let marker =
                melissa_backup::marker::read(&target(dir)?)?.context("no Melissa backups there")?;
            let key = if marker.encrypted {
                eprintln!("Recovery key:");
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                Some(RecoveryKey::parse(&line)?)
            } else {
                None
            };
            device.adopt(&mut config, key)?;
            device.update_status(|s| s.problem = None)?;
            println!("Using the backups at {dir}.");
            Ok(())
        }
        [c] if c == "snapshots" => {
            for s in device.snapshots()? {
                println!(
                    "{}  {}  {}",
                    s.time,
                    &s.id[..s.id.len().min(8)],
                    s.tags.join(",")
                );
            }
            Ok(())
        }
        [c, app, rest @ ..] if c == "restore-app" => {
            let at = match rest {
                [] => None,
                [t] => Some(
                    t.parse::<i64>()
                        .context("the time is a Unix time (see snapshots)")?,
                ),
                _ => bail!(USAGE),
            };
            let _lock = device.lock()?;
            let report = device.restore_app(app, at, "host");
            println!("{}", serde_json::to_string_pretty(&report)?);
            let ok = report.ok;
            device
                .update_status(|s| {
                    s.running = None;
                    s.last_restore = Some(report);
                })
                .ok();
            if !ok {
                bail!("the restore did not finish cleanly");
            }
            Ok(())
        }
        _ => bail!(USAGE),
    }
}

fn target(dir: &str) -> anyhow::Result<PathBuf> {
    let p = PathBuf::from(dir);
    if !melissa_backup::config::plain_absolute(&p) {
        bail!("{dir}: an absolute path without `..`");
    }
    Ok(p)
}
