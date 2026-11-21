//! `melissa-server factory-reset run [--unit | --confirm <word>]`: the root side of the factory
//! reset (`melissa_settings::factory_reset`, docs/DECISIONS.md "Factory reset").
//!
//! - `--unit`: what `melissa-factory-reset.service` runs (via `/usr/libexec/melissa/factory-reset`).
//!   Does (or finishes) the reset the marker asks for; nothing without one. A failure waits a
//!   minute before the unit fails, and the path unit starts it again while the marker exists.
//! - By hand, as root in the container (`podman exec melissa /usr/libexec/melissa/factory-reset
//!   --confirm melissa`), e.g. when the server doesn't start: writes the marker (the server is
//!   not asked to tidy up), then starts the unit and waits for it, so there is only ever one
//!   run, the unit's.

use std::path::Path;

use anyhow::{bail, Context};
use melissa_settings::factory_reset::{self as fr, Marker, ResetError, SystemHost};

const USAGE: &str = "usage: factory-reset run --confirm melissa   (root, inside the container)";

pub fn command(args: &[String]) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .init();
    match args {
        [u] if u == "--unit" => unit(),
        [] => by_hand(None),
        [c, word] if c == "--confirm" => by_hand(Some(word)),
        _ => bail!(USAGE),
    }
}

fn unit() -> anyhow::Result<()> {
    let host = SystemHost::from_env();
    let mut say = |line: &str| println!("{line}");
    match fr::run(Path::new("/"), &host, &mut say) {
        Ok(fr::Outcome::NothingToDo) => {
            println!("factory reset: nothing asked for");
            Ok(())
        }
        Ok(fr::Outcome::Done) => Ok(()),
        Err(ResetError::Busy) => {
            println!("factory reset: already running");
            Ok(())
        }
        Err(e) => {
            eprintln!(
                "factory reset: {e}; trying again in {} s",
                fr::RETRY_AFTER.as_secs()
            );
            std::thread::sleep(fr::RETRY_AFTER);
            std::process::exit(1)
        }
    }
}

fn is_root() -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0)
}

fn by_hand(confirm: Option<&str>) -> anyhow::Result<()> {
    if !is_root() {
        bail!(
            "run this as root inside the container: \
             podman exec melissa /usr/libexec/melissa/factory-reset --confirm melissa"
        );
    }
    let dir = Path::new(fr::MARKER_DIR);
    if !dir.is_dir() {
        bail!(
            "factory reset is for the container install only ({} is missing)",
            dir.display()
        );
    }
    let marker = dir.join(fr::MARKER_NAME);
    if fr::read_marker(&marker)?.is_none() {
        match confirm {
            Some(w) if fr::confirm_matches(w, fr::CONFIRM_WORD) => {}
            _ => {
                println!("Reset Melissa: erase her data on this device and start again unclaimed.");
                println!("  Erased: {}", fr::ERASED);
                println!("  Kept:   {}", fr::KEPT);
                bail!(
                    "to go ahead: factory-reset run --confirm {}",
                    fr::CONFIRM_WORD
                );
            }
        }
        fr::write_marker(&marker, &Marker::new("host", false))?;
    } else {
        println!("A factory reset was already asked for; finishing it.");
    }
    println!("Resetting Melissa. This takes a few minutes.");
    // The path unit may have started it already; `start` then waits for that run.
    let status = std::process::Command::new("systemctl")
        .args(["start", fr::UNIT])
        .status()
        .context("running systemctl")?;
    if !marker.exists() {
        println!("Done. Melissa starts again unclaimed. The setup code: melissa-server setup-code");
        return Ok(());
    }
    bail!(
        "not finished ({status}); it tries again every minute. Details: journalctl -u {}",
        fr::UNIT
    )
}
