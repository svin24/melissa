//! Settings → Updates (owner design "Updates: schedule and channels", 2026-11-28): Melissa's
//! version and one status at a time (in order: updating, checking, failed, rolled back, update
//! available, up to date), the automatic update's switch and time, the channel, and every
//! technical fact under Details. While a check, an update or a rollback runs nothing on the page can be pressed,
//! the server refuses those requests (and other restart-causing ones), and the region polls
//! itself; the sections bar's Updates link shows the spinner meanwhile.

use axum::{
    extract::{Form, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Json,
};
use jiff::{tz::TimeZone, Timestamp, Zoned};
use melissa_settings::updates::{
    ago, now_secs, Busy, HeldReason, HostKind, ImageStatus, ImageView, RunningAction, Schedule,
    UpdateAction, UpdateDay, UpdateEnv, UpdateSetting, UpdateState,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::Settings;
use super::{
    apply,
    pages::{self, mb, AutoView, BusyView, Flash, NoticeAction, UpdateNotice, UpdatesView},
    providers::{agent_versions, manual_restart},
};
use crate::{
    components::{update_tab_state, Tone, UpdateHint},
    is_htmx,
    time_format::Hours,
    ui::Ui,
};

/// What a from-source install does.
const SOURCE_UPDATE: &str = "git pull && just demo";
/// The `melissa-host` profile on a non-NixOS host with Nix (packaging/nix/host).
const NIX_PROFILE: &str = "/nix/var/nix/profiles/melissa";
/// How long "Updated from … to …" stays with its Roll back (owner, 2026-11-28).
const JUST_UPDATED_SECS: u64 = 7 * 86_400;
/// Where the helper's journal is, for Details.
const LOG_COMMAND: &str = "journalctl -u melissa-update -u melissa-auto-update";

// --- Words -----------------------------------------------------------------------------------------

/// A version for people: `0.0.4`, with the revision when the version alone doesn't tell it apart
/// from what runs (edge builds share a version number).
fn version_words(version: Option<&str>, revision: Option<&str>, running: Option<&str>) -> String {
    match (version, revision) {
        (Some(v), Some(r)) if Some(v) == running => format!("{v} ({r})"),
        (Some(v), _) => v.to_string(),
        (None, Some(r)) => format!("revision {r}"),
        (None, None) => "a newer version".to_string(),
    }
}

fn image_version(i: &ImageView, running: Option<&str>) -> String {
    version_words(i.version.as_deref(), i.revision.as_deref(), running)
}

/// Letters, digits, dots and dashes only (a revision or version in a URL).
fn url_safe(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

/// "What's new" between two builds: the commits between their revisions, else the release
/// page of the newer version. Opens in a new tab; nothing is fetched by the device.
fn whats_new(
    from: (Option<&str>, Option<&str>),
    to: (Option<&str>, Option<&str>),
) -> Option<String> {
    let src = pages::SOURCE_URL;
    match (from.1, to.1) {
        (Some(a), Some(b)) if a != b && url_safe(a) && url_safe(b) => {
            Some(format!("{src}/compare/{a}...{b}"))
        }
        _ => match to.0 {
            Some(v) if Some(v) != from.0 && url_safe(v) => Some(format!("{src}/releases/tag/v{v}")),
            _ => None,
        },
    }
}

fn install_words(env: &UpdateEnv) -> &'static str {
    match env.kind {
        HostKind::Container => "Container",
        HostKind::Nix if env.nixos() => "Nix (part of this computer's NixOS configuration)",
        HostKind::Nix => "Nix (the melissa-host profile on this computer)",
        HostKind::Other => "From source on this computer",
    }
}

/// One short sentence where updating from here isn't the way (None: the buttons are).
fn note(env: &UpdateEnv) -> Option<&'static str> {
    match env.kind {
        HostKind::Container if env.helper_installed() => None,
        HostKind::Container => Some("Updating from here isn't set up on this device."),
        HostKind::Nix => Some("Melissa updates with your Nix configuration."),
        HostKind::Other => Some("Running from source: update it in the source folder."),
    }
}

/// The channel from the image's tag (`:edge`, `:stable`); None for anything else.
fn channel(status: &ImageStatus) -> Option<&'static str> {
    let image = status.image.as_deref()?;
    let tag = image.rsplit_once(':').map(|(_, t)| t)?;
    match tag {
        "edge" => Some("edge"),
        "stable" => Some("stable"),
        _ => None,
    }
}

fn channel_label(c: &str) -> &'static str {
    match c {
        "edge" => "Edge",
        "beta" => "Beta",
        _ => "Stable",
    }
}

// --- Time ------------------------------------------------------------------------------------------

/// Now in the host's time zone (the container follows the host's, Quadlet `Timezone=local`).
pub(crate) fn local_now() -> Zoned {
    Timestamp::now().to_zoned(TimeZone::system())
}

fn zoned(secs: u64, tz: &TimeZone) -> Option<Zoned> {
    let t = Timestamp::from_second(i64::try_from(secs).ok()?).ok()?;
    Some(t.to_zoned(tz.clone()))
}

/// The automatic update's next start after `now` (without its up-to-15-minute delay).
pub(crate) fn next_run(s: Schedule, now: &Zoned) -> Option<Zoned> {
    let mut date = now.date();
    for _ in 0..8 {
        let at = date
            .at(i8::try_from(s.hour).ok()?, 0, 0, 0)
            .to_zoned(now.time_zone().clone())
            .ok()?;
        let day_ok = s
            .day
            .weekday()
            .is_none_or(|w| date.weekday().to_monday_one_offset() == w);
        if day_ok && at > *now {
            return Some(at);
        }
        date = date.tomorrow().ok()?;
    }
    None
}

/// "tonight around 03:00", "today around 15:00", "tomorrow around 03:00", "Sunday around 03:00".
fn when_words(run: &Zoned, now: &Zoned, hours: Hours) -> String {
    let time = hours.hour(u8::try_from(run.hour()).unwrap_or(0));
    let away = run.timestamp().as_second() - now.timestamp().as_second();
    let night = run.hour() >= 18 || run.hour() < 6;
    let days = now
        .date()
        .until(run.date())
        .map(|s| s.get_days())
        .unwrap_or(0);
    if night && away <= 12 * 3600 {
        format!("tonight around {time}")
    } else if days == 0 {
        format!("today around {time}")
    } else if days == 1 {
        format!("tomorrow around {time}")
    } else {
        format!("{} around {time}", weekday_name(run))
    }
}

fn weekday_name(z: &Zoned) -> &'static str {
    match z.date().weekday().to_monday_one_offset() {
        1 => "Monday",
        2 => "Tuesday",
        3 => "Wednesday",
        4 => "Thursday",
        5 => "Friday",
        6 => "Saturday",
        _ => "Sunday",
    }
}

/// A moment for people: "today at 03:07", "on 26 Nov at 03:07".
fn moment_words(secs: u64, now: &Zoned, hours: Hours) -> String {
    let Some(z) = zoned(secs, now.time_zone()) else {
        return String::new();
    };
    let time = hours.time(
        u8::try_from(z.hour()).unwrap_or(0),
        u8::try_from(z.minute()).unwrap_or(0),
    );
    if z.date() == now.date() {
        format!("today at {time}")
    } else if z.date().tomorrow().ok() == Some(now.date()) {
        format!("yesterday at {time}")
    } else {
        format!("on {} at {time}", crate::pages::short_date(secs))
    }
}

/// "09:14" today, else "26 Nov".
fn since_words(secs: u64, now: &Zoned, hours: Hours) -> String {
    match zoned(secs, now.time_zone()) {
        Some(z) if z.date() == now.date() => hours.time(
            u8::try_from(z.hour()).unwrap_or(0),
            u8::try_from(z.minute()).unwrap_or(0),
        ),
        _ => crate::pages::short_date(secs),
    }
}

/// "UTC+01:00".
fn utc_offset(now: &Zoned) -> String {
    let secs = now.offset().seconds();
    let sign = if secs < 0 { '-' } else { '+' };
    let secs = secs.unsigned_abs();
    format!("UTC{sign}{:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

// --- The status, one at a time ---------------------------------------------------------------------

/// The container with the helper: the page's buttons and switch work.
fn managed(env: &UpdateEnv) -> bool {
    env.kind == HostKind::Container && env.helper_installed()
}

/// Which status the page shows.
enum Shown<'a> {
    Busy(Busy),
    /// The new version didn't start; Melissa went back.
    NoStart,
    /// The last update, rollback or check didn't work (no held version).
    Failed(&'a str, &'a str),
    RolledBack,
    Available,
    UpToDate,
}

fn shown<'a>(state: &'a UpdateState, s: &'a ImageStatus) -> Shown<'a> {
    if let Some(b) = state.busy() {
        return Shown::Busy(b);
    }
    if s.held
        .as_ref()
        .is_some_and(|h| h.reason == HeldReason::Failed)
    {
        return Shown::NoStart;
    }
    if let Some(l) = state.last.as_ref().filter(|l| !l.ok) {
        return Shown::Failed(l.action.as_str(), l.kind.as_deref().unwrap_or(""));
    }
    if s.held
        .as_ref()
        .is_some_and(|h| h.reason == HeldReason::RolledBack)
    {
        return Shown::RolledBack;
    }
    if s.staged.is_some() || s.available.is_some() {
        return Shown::Available;
    }
    Shown::UpToDate
}

/// The badge: its word, tone and whether it spins.
fn badge_for(shown: &Shown<'_>) -> (&'static str, Tone, bool) {
    match shown {
        Shown::Busy(Busy::Checking) => ("Checking", Tone::Neutral, true),
        Shown::Busy(Busy::Updating) => ("Updating", Tone::Attention, true),
        Shown::Busy(Busy::RollingBack) => ("Rolling back", Tone::Attention, true),
        Shown::NoStart => ("Update failed", Tone::Caution, false),
        Shown::Failed("check", _) => ("Check failed", Tone::Caution, false),
        Shown::Failed("rollback", _) => ("Roll back failed", Tone::Caution, false),
        Shown::Failed(..) => ("Update failed", Tone::Caution, false),
        Shown::RolledBack => ("Previous version", Tone::Neutral, false),
        Shown::Available => ("Update available", Tone::Info, false),
        Shown::UpToDate => ("Up to date", Tone::Ok, false),
    }
}

/// The rail's Updates link: the spinner while something runs, a dot when an update is ready
/// (container with the helper only; a held version doesn't count).
pub(super) fn hint(s: &Settings) -> UpdateHint {
    let env = &s.parts().updates;
    if !managed(env) {
        return UpdateHint::NONE;
    }
    hint_for(&env.state())
}

fn hint_for(state: &UpdateState) -> UpdateHint {
    if let Some(b) = state.busy() {
        return UpdateHint {
            busy: Some(match b {
                Busy::Checking => "Checking for updates",
                Busy::Updating => "Updating Melissa",
                Busy::RollingBack => "Going back to the previous version",
            }),
            ..UpdateHint::NONE
        };
    }
    match &state.status {
        Some(st) if st.staged.is_some() || st.available.is_some() => UpdateHint {
            label: "Melissa update available",
            available: true,
            ..UpdateHint::NONE
        },
        _ => UpdateHint::NONE,
    }
}

/// The update status in words ("Up to date"), where Melissa knows it.
fn status_words(env: &UpdateEnv, state: &UpdateState) -> Option<(&'static str, Tone)> {
    if !managed(env) {
        return None;
    }
    let s = state.status.as_ref()?;
    let (w, t, _) = badge_for(&shown(state, s));
    Some((w, t))
}

/// The Settings list's value for Updates: the status, else how Melissa is installed.
pub(super) fn list_word(ui: &Ui) -> &'static str {
    let env = &ui.settings.parts().updates;
    status_words(env, &env.state())
        .map(|(w, _)| w)
        .unwrap_or(match env.kind {
            HostKind::Container => "Not set up",
            HostKind::Nix => "Nix",
            HostKind::Other => "From source",
        })
}

/// When the automatic update runs next: the host timer's own answer (`next_run` in status.json),
/// unless a change still waits for the helper or the answer is already past (then computed from
/// the schedule). None while on means the timer isn't scheduled on the host.
fn planned_run(state: &UpdateState, now: &Zoned) -> Option<Zoned> {
    let st = state.status.as_ref()?;
    let computed = || next_run(state.schedule(), now);
    if state.pending.schedule.is_some() || state.pending.auto.is_some() {
        return computed();
    }
    match st.next_run.and_then(|s| zoned(s, now.time_zone())) {
        Some(at) if at > *now => Some(at),
        Some(_) => computed(),
        None => None,
    }
}

fn auto_words(env: &UpdateEnv, state: &UpdateState, run: Option<&Zoned>, hours: Hours) -> String {
    if env.kind == HostKind::Nix {
        return if env.nixos() {
            "Through the NixOS configuration (system.autoUpgrade), if set".into()
        } else {
            "Off (the Nix profile is updated by hand)".into()
        };
    }
    let Some(s) = state.status.as_ref() else {
        return "Not set up".into();
    };
    if s.local_image() {
        return if state.auto_on() {
            "On for images loaded on the host".into()
        } else {
            "Off".into()
        };
    }
    if !state.auto_on() {
        return "Off (a daily check still looks)".into();
    }
    let sched = state.schedule();
    let at = hours.hour(sched.hour);
    let mut out = match sched.day {
        UpdateDay::Daily => format!("Every day at {at}"),
        d => format!("{} at {at}", d.label()),
    };
    match run {
        Some(run) => {
            let end = hours.time(sched.hour, 15);
            out.push_str(&format!(
                " · next run {} between {at} and {end}",
                run.strftime("%Y-%m-%d")
            ));
        }
        None => out.push_str(" · not scheduled on the host (melissa-auto-update.timer)"),
    }
    out
}

/// Commands for the page: Nix and runs from source only.
fn manual_commands(env: &UpdateEnv) -> Vec<(String, String)> {
    match env.kind {
        // No host commands in the container's UI or API: the buttons are the way.
        HostKind::Container => Vec::new(),
        HostKind::Nix if env.nixos() => vec![
            (
                "In the folder of this computer's system flake, update its inputs:".into(),
                "nix flake update".into(),
            ),
            (
                "Rebuild (Melissa restarts if it changed):".into(),
                "sudo nixos-rebuild switch".into(),
            ),
            (
                "Roll back to the previous system:".into(),
                "sudo nixos-rebuild switch --rollback".into(),
            ),
        ],
        HostKind::Nix => vec![
            (
                "Update (Melissa restarts):".into(),
                format!(
                    "sudo nix profile upgrade --profile {NIX_PROFILE} --all && sudo {NIX_PROFILE}/bin/melissa-host install"
                ),
            ),
            (
                "Roll back:".into(),
                format!(
                    "sudo nix profile rollback --profile {NIX_PROFILE} && sudo {NIX_PROFILE}/bin/melissa-host install"
                ),
            ),
        ],
        HostKind::Other => vec![("In the Melissa source folder:".into(), SOURCE_UPDATE.into())],
    }
}

/// Whole megabytes without the unit ("59"), for "59 of 141 MB".
fn mb_number(bytes: u64) -> u64 {
    bytes / 1_000_000
}

/// The bar and its words while an update or a rollback runs.
fn busy_view(b: Busy, r: Option<&RunningAction>, s: &ImageStatus) -> Option<BusyView> {
    let running = s.running.as_ref().and_then(|i| i.version.as_deref());
    let target = r
        .and_then(|r| r.version.clone())
        .or_else(|| match b {
            Busy::RollingBack => s.previous.as_ref().map(|p| image_version(p, None)),
            _ => s
                .staged
                .as_ref()
                .or(s.available.as_ref())
                .map(|a| image_version(a, running)),
        })
        .unwrap_or_else(|| "the new version".into());
    let reconnects = "This page reconnects by itself.";
    let step = r.map_or("check", |r| r.step.as_str());
    Some(match (b, step) {
        (Busy::Checking, _) => return None,
        (Busy::RollingBack, _) => BusyView {
            label: format!("Going back to {target}\u{2026} Melissa restarts in a moment."),
            text: String::new(),
            done: 0,
            total: 0,
            below: format!("Apps and chats pause for a few minutes. Data is kept. {reconnects}"),
        },
        (Busy::Updating, "download") => {
            let (done, total) = r.map_or((0, 0), |r| (r.bytes_done, r.bytes_total));
            if let Some(pct) = (done.min(total) * 100).checked_div(total) {
                // Never 100 % before the download is done.
                let pct = pct.min(99);
                BusyView {
                    label: format!("Downloading {target}\u{2026} {pct}%"),
                    text: format!("{} of {}", mb_number(done), mb(total)),
                    done,
                    total,
                    below: format!(
                        "Next: Melissa restarts on {target}. Apps and chats pause for a few minutes. {reconnects}"
                    ),
                }
            } else {
                BusyView {
                    label: format!("Downloading {target}\u{2026}"),
                    text: if done > 0 { mb(done) } else { String::new() },
                    done: 0,
                    total: 0,
                    below: format!(
                        "Next: Melissa restarts on {target}. Apps and chats pause for a few minutes. {reconnects}"
                    ),
                }
            }
        }
        (Busy::Updating, "restart") => BusyView {
            label: "Restarting\u{2026} Melissa is back in a few minutes.".into(),
            text: String::new(),
            done: 0,
            total: 0,
            below: "Step 2 of 3. Then: Checking that Melissa works.".into(),
        },
        (Busy::Updating, "health") => BusyView {
            label: "Checking that Melissa works\u{2026}".into(),
            text: String::new(),
            done: 0,
            total: 0,
            below: format!("Step 3 of 3. {reconnects}"),
        },
        (Busy::Updating, _) => BusyView {
            label: "Getting the update ready\u{2026}".into(),
            text: String::new(),
            done: 0,
            total: 0,
            below:
                "Next: Melissa downloads it, then restarts. Apps and chats pause for a few minutes."
                    .into(),
        },
    })
}

async fn view(ui: &Ui, headers: &HeaderMap, flash: Option<Flash>) -> UpdatesView {
    let s = &ui.settings;
    let env = &s.parts().updates;
    let state = env.state();
    let release = &s.parts().release;
    let hours = Hours::pick(s.prefs().time_format, headers);
    let now = local_now();
    let now_s = now_secs();
    let managed = managed(env);
    let mut flash = flash;
    if flash.is_none() {
        if let Some(e) = &state.error {
            tracing::warn!("update status: {e}");
            flash = Some(Flash {
                text: "Couldn't read the update status. Details has the reason.".into(),
                tone: Tone::Caution,
            });
        }
    }
    let mut v = UpdatesView {
        version: format!("Version {}", release.version),
        note: note(env),
        commands: manual_commands(env),
        commands_open: env.kind == HostKind::Other,
        pending: apply::pending(ui).await,
        manual_restart: manual_restart(s),
        flash,
        ..UpdatesView::default()
    };
    let mut details: Vec<(&'static str, String, bool)> =
        vec![("Installed as", install_words(env).to_string(), false)];

    let status = state.status.as_ref().filter(|_| managed);
    let Some(st) = status else {
        // No helper (or not a container): what is known, and the commands.
        if let Some(r) = &release.revision {
            details.push(("Revision", r.clone(), false));
        }
        details.push((
            "Automatic updates",
            auto_words(env, &state, None, hours),
            false,
        ));
        if let Some(i) = env.container_image() {
            details.push(("Image", i.image.clone(), true));
        }
        if let Some(e) = &state.error {
            details.push(("Reason", e.clone(), false));
        }
        if managed {
            v.checked = "Not checked yet".into();
            v.can_check = true;
        }
        v.details = details;
        return v;
    };

    let running_v = st.running.as_ref().and_then(|i| i.version.as_deref());
    let running_rev = st.running.as_ref().and_then(|i| i.revision.as_deref());
    let here = running_v.unwrap_or(&release.version).to_string();
    let local = st.local_image();
    let auto_on = state.auto_on();
    let sched = state.schedule();
    let run = planned_run(&state, &now);
    let next = (!local && auto_on)
        .then_some(run.as_ref())
        .flatten()
        .map(|r| when_words(r, &now, hours));
    // The version that runs (after a rollback: the one Melissa went back to), as the notices say.
    v.version = format!("Version {here}");
    let shown = shown(&state, st);
    let busy = state.busy();
    v.badge = Some(badge_for(&shown));
    v.live = busy.is_some();
    v.tab = hint_for(&state);
    let held_v = st
        .held
        .as_ref()
        .map(|h| version_words(h.version.as_deref(), h.revision.as_deref(), Some(&here)))
        .unwrap_or_else(|| "the new version".into());
    let next_line = |prefix: &str| -> Option<String> {
        if local {
            None
        } else if let Some(n) = &next {
            Some(format!("{prefix}{n}."))
        } else if !auto_on {
            Some("Automatic updates are off.".into())
        } else {
            Some("Automatic updates are on, but the host has no time planned for them. Details has more.".into())
        }
    };
    match &shown {
        Shown::Busy(Busy::Checking) => {
            v.line = Some("Looking for a newer version. This takes about a minute.".into());
        }
        Shown::Busy(b) => v.busy = busy_view(*b, state.running.as_ref(), st),
        Shown::NoStart => {
            let skip = if auto_on && !local {
                format!(" Automatic updates skip {held_v} and wait for a newer version.")
            } else {
                String::new()
            };
            v.notice = Some(UpdateNotice {
                text: format!(
                    "Version {held_v} didn't start, so Melissa went back to {here}. Your data is kept.{skip}"
                ),
                tone: Tone::Caution,
                action: NoticeAction::Apply("Try again".into()),
                whats_new: None,
            });
        }
        Shown::Failed(action, kind) => {
            let found = state
                .last
                .as_ref()
                .and_then(|l| l.version.clone())
                .unwrap_or_else(|| "the new version".into());
            let again = if auto_on && !local {
                " Melissa tries again at the next automatic update."
            } else {
                ""
            };
            let (text, act) = match (*action, *kind) {
                ("check", _) => (
                    "Melissa couldn't reach the update server. It tries again tomorrow, or use Check now."
                        .to_string(),
                    NoticeAction::None,
                ),
                ("rollback", _) => (
                    "Melissa couldn't go back to the previous version. Details has the reason."
                        .to_string(),
                    NoticeAction::None,
                ),
                (_, "download") => (
                    format!("Melissa couldn't download {found}. Nothing changed.{again}"),
                    NoticeAction::Apply("Try again".into()),
                ),
                _ => (
                    format!("The update didn't finish. Melissa still runs {here}.{again}"),
                    NoticeAction::Apply("Try again".into()),
                ),
            };
            v.notice = Some(UpdateNotice {
                text,
                tone: Tone::Caution,
                action: act,
                whats_new: None,
            });
        }
        Shown::RolledBack => {
            let since = st
                .held
                .as_ref()
                .map(|h| since_words(h.at, &now, hours))
                .unwrap_or_default();
            let skip = if auto_on && !local {
                format!(" Automatic updates skip {held_v} and wait for a newer version.")
            } else {
                String::new()
            };
            v.notice = Some(UpdateNotice {
                text: format!("Back on {here} since {since}.{skip}"),
                tone: Tone::Neutral,
                action: NoticeAction::Apply(format!("Install {held_v} anyway")),
                whats_new: None,
            });
            v.line = next_line("Next automatic update: ");
        }
        Shown::Available => {
            let new = st.staged.as_ref().or(st.available.as_ref());
            let new_v =
                new.map_or_else(|| "A newer version".into(), |n| image_version(n, running_v));
            v.line = Some(if local {
                "A newer image is loaded on this computer. Update Melissa to use it.".into()
            } else if !auto_on {
                format!("Version {new_v} is ready. Automatic updates are off, so it waits for you.")
            } else if let Some(n) = &next {
                format!("Version {new_v} is ready. Melissa installs it {n}, or you can update now.")
            } else {
                format!("Version {new_v} is ready.")
            });
            v.primary = Some("Update Melissa");
            v.whats_new = new.and_then(|n| {
                whats_new(
                    (running_v, running_rev),
                    (n.version.as_deref(), n.revision.as_deref()),
                )
            });
        }
        Shown::UpToDate => {
            if let Some(u) = st.just_updated.as_ref().filter(|u| {
                now_s.saturating_sub(u.at) < JUST_UPDATED_SECS && u.to.as_deref() == running_v
            }) {
                let from = u.from.as_deref().unwrap_or("the previous version");
                let to = u.to.as_deref().unwrap_or(&here);
                let (from, to) = if from == to {
                    (
                        version_words(Some(from), u.from_revision.as_deref(), Some(from)),
                        version_words(Some(to), u.to_revision.as_deref(), Some(to)),
                    )
                } else {
                    (from.to_string(), to.to_string())
                };
                v.notice = Some(UpdateNotice {
                    text: format!(
                        "Updated from {from} to {to} {}.",
                        moment_words(u.at, &now, hours)
                    ),
                    tone: Tone::Ok,
                    action: if env.can(UpdateAction::Rollback) && st.previous.is_some() {
                        NoticeAction::Rollback(format!(
                            "Go back to {from}? Melissa restarts on it, apps and chats pause for a few minutes. Data is kept."
                        ))
                    } else {
                        NoticeAction::None
                    },
                    whats_new: whats_new(
                        (u.from.as_deref(), u.from_revision.as_deref()),
                        (u.to.as_deref(), u.to_revision.as_deref()),
                    ),
                });
            }
            v.line = next_line("Next automatic update: ");
        }
    }

    // The head bar: when Melissa last looked, and Check now (busy or disabled meanwhile).
    let last_look = state
        .last
        .as_ref()
        .filter(|l| matches!(l.action.as_str(), "check" | "auto" | "apply"))
        .map(|l| l.at)
        .or(state.status_at);
    v.checked = last_look.map_or_else(
        || "Not checked yet".into(),
        |at| format!("Checked {}", ago(at, now_s)),
    );
    v.can_check = env.can(UpdateAction::Check);
    v.head_busy = busy;

    // Details: every technical fact, and Roll back.
    if let Some(c) = channel(st).filter(|_| !local) {
        details.push(("Channel", channel_label(c).to_string(), false));
    }
    if let Some(r) = &st.running {
        let mut parts = vec![version_words(r.version.as_deref(), None, None)];
        if let Some(rev) = &r.revision {
            parts.push(format!("revision {rev}"));
        }
        if let Some(b) = r.built() {
            parts.push(format!("built {b}"));
        }
        details.push(("Running", parts.join(" · "), false));
    }
    if let Some(img) = &st.image {
        let digest = st.running.as_ref().and_then(ImageView::short_digest);
        let text = match digest {
            Some(d) => format!("{img} · {d}"),
            None => img.clone(),
        };
        details.push(("Image", text, true));
    }
    if let Some(sg) = &st.staged {
        details.push((
            "Downloaded",
            format!("{} · used at the next start", image_version(sg, running_v)),
            false,
        ));
    }
    if let Some(l) = state
        .last
        .as_ref()
        .filter(|l| matches!(l.action.as_str(), "check" | "auto"))
    {
        let found = match (l.ok, l.kind.as_deref()) {
            (false, _) => "couldn't reach the update server".to_string(),
            (true, Some("available" | "held")) => format!(
                "found {}",
                l.version.as_deref().unwrap_or("a newer version")
            ),
            (true, Some("updated")) => "updated".to_string(),
            _ => "no newer version".to_string(),
        };
        details.push((
            "Last check",
            format!("{} · {found}", ago(l.at, now_s)),
            false,
        ));
    }
    details.push((
        "Automatic updates",
        auto_words(env, &state, run.as_ref(), hours),
        false,
    ));
    if let Some(h) = &st.held {
        let what = version_words(h.version.as_deref(), h.revision.as_deref(), Some(&here));
        match h.reason {
            HeldReason::Failed => {
                details.push(("Didn't start", what, false));
                if let Some(m) = &h.message {
                    details.push(("Reason", m.clone(), false));
                }
            }
            HeldReason::RolledBack => details.push(("Held back", what, false)),
        }
    }
    if let Some(l) = state.last.as_ref().filter(|l| !l.ok) {
        if let Some(m) = &l.message {
            details.push(("Last error", m.clone(), false));
        }
    }
    if st.held.is_some() || state.last.as_ref().is_some_and(|l| !l.ok) {
        details.push(("Log", LOG_COMMAND.to_string(), true));
    }
    if let Some(p) = &st.previous {
        let mut text = image_version(p, running_v);
        if let Some(b) = p.built() {
            text.push_str(&format!(" · built {b}"));
        }
        v.previous = Some(text);
        v.rollback_question = format!(
            "Go back to {}? Melissa restarts on it, apps and chats pause for a few minutes. Data is kept.",
            image_version(p, running_v)
        );
        v.can_rollback = env.can(UpdateAction::Rollback) && busy.is_none();
    }
    v.details = details;

    // Automatic updates: the switch, its time and the channel (registry images only).
    if !local {
        let locked = match busy {
            Some(Busy::Updating) => Some("You can change these when the update is done."),
            Some(Busy::RollingBack) => Some("You can change these when Melissa is back."),
            _ => None,
        };
        v.auto = Some(AutoView {
            on: auto_on,
            locked,
            day: sched.day.as_str(),
            hour: format!("{:02}", sched.hour),
            hours: (0..24u8)
                .map(|h| (format!("{h:02}"), hours.hour(h)))
                .collect(),
            device_time: utc_offset(&now),
            channel: channel(st),
        });
    }
    if v.notice.is_some() && matches!(shown, Shown::NoStart | Shown::Failed(..)) {
        // A failure says it in the notice; the next-run line would only repeat the schedule.
        v.line = None;
    }
    v
}

fn page_markup(f: &crate::pages::Frame<'_>, v: &UpdatesView) -> maud::Markup {
    pages::updates_page(f, v)
}

pub(super) async fn page(State(ui): State<Ui>, headers: HeaderMap) -> Response {
    let v = view(&ui, &headers, None).await;
    ui.framed(|f| page_markup(f, &v)).await.into_response()
}

/// `GET /ui/settings/updates`: the region alone (its poll while something runs).
pub(super) async fn region(State(ui): State<Ui>, headers: HeaderMap) -> Response {
    let mut v = view(&ui, &headers, None).await;
    // The poll that finds the work done says so to screen readers (once: it stops polling).
    if !v.live {
        v.announce = Some(announcement(&v));
    }
    pages::updates_region(&v).into_response()
}

/// "Up to date. Updated from 0.0.3 to 0.0.4 today at 03:07." (the badge and the notice or line).
fn announcement(v: &UpdatesView) -> String {
    let word = v.badge.map_or("", |(w, ..)| w);
    let more = v
        .notice
        .as_ref()
        .map(|n| n.text.as_str())
        .or(v.line.as_deref())
        .unwrap_or("");
    match (word.is_empty(), more.is_empty()) {
        (false, false) => format!("{word}. {more}"),
        (false, true) => word.to_string(),
        _ => more.to_string(),
    }
}

/// `GET /ui/update-hint`: the sections bar's Updates link (its poll while something runs).
pub(crate) async fn hint_link(State(ui): State<Ui>) -> Response {
    update_tab_state(&hint(&ui.settings), false).into_response()
}

/// What an action did, or why not.
enum Outcome {
    Started,
    NotHere(Flash),
    /// Something else runs (one at a time).
    Busy(Flash),
    Failed(Flash),
    /// Needs `confirm` (the API's apply and rollback).
    Confirm,
}

/// How long the lead agent waits after answering before it asks systemd to restart it.
const RESTART_DELAY: std::time::Duration = std::time::Duration::from_millis(750);

/// Why a request that restarts Melissa (or a check) can't start now, if it can't.
pub(super) fn refused_while_busy(s: &Settings, restarts: bool) -> Option<Flash> {
    let env = &s.parts().updates;
    if !managed(env) {
        return None;
    }
    let text = match env.state().busy()? {
        Busy::Checking if !restarts => return None,
        Busy::Checking => "Melissa is checking for updates. Try again in a minute.",
        Busy::Updating => "Melissa is updating. Try again when it's done.",
        Busy::RollingBack => {
            "Melissa is going back to the previous version. Try again when she's back."
        }
    };
    Some(Flash {
        text: text.into(),
        tone: Tone::Attention,
    })
}

async fn run(ui: &Ui, action: UpdateAction, confirmed: bool, actor: &str) -> Outcome {
    let s = &ui.settings;
    let env = &s.parts().updates;
    if !env.can(action) {
        let text = match env.kind {
            HostKind::Container => "Melissa can't do that from here.".into(),
            HostKind::Nix => "Melissa updates with your Nix configuration.".into(),
            HostKind::Other => "There is no container image to update here.".into(),
        };
        return Outcome::NotHere(Flash {
            text,
            tone: Tone::Attention,
        });
    }
    // Nothing starts while a check, an update or a rollback runs (a check refuses a second
    // check too).
    if let Some(f) = refused_while_busy(s, true) {
        return Outcome::Busy(f);
    }
    if action.restarts_melissa() && !confirmed {
        return Outcome::Confirm;
    }
    s.audit(
        "settings.update",
        actor,
        &format!("{} requested", action.as_str()),
        None,
    );
    if action == UpdateAction::Restart {
        // Restarting stops this process at once (no graceful shutdown): asked inline, the
        // reply was lost and the browser/CLI saw a dropped connection. Answer first.
        let settings = s.clone();
        tokio::spawn(async move {
            tokio::time::sleep(RESTART_DELAY).await;
            if let Err(e) = settings.parts().updates.request(action).await {
                tracing::warn!("restart request failed: {e}");
                settings.audit("settings.update", "lead", "restart failed", None);
            }
        });
        return Outcome::Started;
    }
    match env.request(action).await {
        Ok(()) => Outcome::Started,
        Err(e) => {
            tracing::warn!("update request {}: {e}", action.as_str());
            Outcome::Failed(Flash {
                text: "Couldn't ask the update helper. Try again.".into(),
                tone: Tone::Caution,
            })
        }
    }
}

async fn ui_reply(ui: &Ui, headers: &HeaderMap, flash: Option<Flash>) -> Response {
    let v = view(ui, headers, flash).await;
    if is_htmx(headers) {
        return pages::updates_region(&v).into_response();
    }
    ui.framed(|f| page_markup(f, &v)).await.into_response()
}

fn outcome_flash(o: Outcome) -> Option<Flash> {
    match o {
        // The page itself shows what runs now (no separate message).
        Outcome::Started | Outcome::Confirm => None,
        Outcome::NotHere(f) | Outcome::Busy(f) | Outcome::Failed(f) => Some(f),
    }
}

/// `POST /ui/settings/updates/{check|apply|rollback}`. No confirmation card: Update Melissa can
/// be rolled back, and Roll back asks in the page first (its `action_button`).
pub(super) async fn action(
    State(ui): State<Ui>,
    Path(action): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(action) = UpdateAction::parse(&action).filter(|a| *a != UpdateAction::Restart) else {
        return Redirect::to(pages::UPDATES_URL).into_response();
    };
    let outcome = run(&ui, action, true, "ui").await;
    ui_reply(&ui, &headers, outcome_flash(outcome)).await
}

pub(super) async fn restart(State(ui): State<Ui>, headers: HeaderMap) -> Response {
    let outcome = run(&ui, UpdateAction::Restart, true, "ui").await;
    let flash = match outcome {
        Outcome::Started => Some(Flash {
            text: "Restarting\u{2026}".into(),
            tone: Tone::Info,
        }),
        o => outcome_flash(o),
    };
    ui_reply(&ui, &headers, flash).await
}

/// Place a setting request for the helper (the switch, the time).
fn request_setting(ui: &Ui, setting: UpdateSetting) -> Option<Flash> {
    let s = &ui.settings;
    let env = &s.parts().updates;
    if !managed(env) {
        return Some(Flash {
            text: "Melissa can't do that from here.".into(),
            tone: Tone::Attention,
        });
    }
    if let Some(f) =
        refused_while_busy(s, true).filter(|_| env.state().busy() != Some(Busy::Checking))
    {
        return Some(f);
    }
    match env.request_setting(setting) {
        Ok(()) => {
            s.audit(
                "settings.update",
                "ui",
                &format!("{} requested", setting.request_name()),
                None,
            );
            None
        }
        Err(e) => {
            tracing::warn!("update setting {}: {e}", setting.request_name());
            Some(Flash {
                text: "Couldn't save it. Try again.".into(),
                tone: Tone::Caution,
            })
        }
    }
}

#[derive(Deserialize, Default)]
pub(super) struct AutoForm {
    #[serde(default)]
    enabled: String,
}

/// `POST /ui/settings/updates/auto` (`enabled=on` or nothing): automatic updates on or off.
/// Easy to undo, so no confirmation; the warning shows while it is off.
pub(super) async fn auto(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Form(form): Form<AutoForm>,
) -> Response {
    let flash = request_setting(&ui, UpdateSetting::Auto(form.enabled == "on"));
    ui_reply(&ui, &headers, flash).await
}

#[derive(Deserialize, Default)]
pub(super) struct WhenForm {
    #[serde(default)]
    day: String,
    #[serde(default)]
    hour: String,
}

/// `POST /ui/settings/updates/when` (`day` = daily|mon..sun, `hour` = 00..23): the time of the
/// automatic update, checked against the fixed lists before it becomes a request name.
pub(super) async fn when(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Form(form): Form<WhenForm>,
) -> Response {
    let parsed = Schedule::parse(&format!("{}-{}", form.day.trim(), form.hour.trim()));
    let flash = match parsed {
        Some(s) => request_setting(&ui, UpdateSetting::Schedule(s)),
        None => Some(Flash {
            text: "Pick a day and an hour from the lists.".into(),
            tone: Tone::Caution,
        }),
    };
    ui_reply(&ui, &headers, flash).await
}

// --- API ------------------------------------------------------------------------------------

pub(super) async fn api_status(State(ui): State<Ui>) -> Json<Value> {
    let s = &ui.settings;
    let env = &s.parts().updates;
    let actions: Vec<&str> = [
        UpdateAction::Check,
        UpdateAction::Apply,
        UpdateAction::Rollback,
        UpdateAction::Restart,
    ]
    .into_iter()
    .filter(|a| env.can(*a))
    .map(UpdateAction::as_str)
    .collect();
    let agents: Vec<Value> = agent_versions(&ui)
        .await
        .into_iter()
        .map(|(name, version)| json!({"name": name, "version": version}))
        .collect();
    let state = env.state();
    Json(json!({
        "release": s.parts().release,
        "host": env.kind,
        "helper": env.helper_installed(),
        "busy": state.busy(),
        "automatic": state.status.as_ref().map(|_| json!({
            "on": state.auto_on(),
            "schedule": state.schedule().as_string(),
        })),
        "state": state,
        "actions": actions,
        "commands": manual_commands(env)
            .into_iter()
            .map(|(label, command)| json!({"label": label, "command": command}))
            .collect::<Vec<_>>(),
        "agents": agents,
        "restart_needed": s.parts().store.restart_needed(),
    }))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct ActionBody {
    #[serde(default)]
    confirm: bool,
}

pub(super) async fn api_action(
    State(ui): State<Ui>,
    Path(action): Path<String>,
    body: Option<Json<ActionBody>>,
) -> Response {
    let Some(action) = UpdateAction::parse(&action) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "unknown action (check, apply, rollback, restart)"})),
        )
            .into_response();
    };
    let confirmed = body.map(|Json(b)| b.confirm).unwrap_or(false);
    match run(&ui, action, confirmed, "api").await {
        Outcome::Started => (
            StatusCode::ACCEPTED,
            Json(json!({"started": action.as_str()})),
        )
            .into_response(),
        Outcome::Confirm => (
            StatusCode::CONFLICT,
            Json(json!({"error": "this restarts Melissa; send {\"confirm\": true}"})),
        )
            .into_response(),
        Outcome::Busy(f) => (
            StatusCode::CONFLICT,
            Json(json!({"error": f.text, "busy": true})),
        )
            .into_response(),
        Outcome::NotHere(f) => {
            (StatusCode::NOT_IMPLEMENTED, Json(json!({"error": f.text}))).into_response()
        }
        Outcome::Failed(f) => {
            (StatusCode::BAD_GATEWAY, Json(json!({"error": f.text}))).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;
    use melissa_settings::updates::{PendingRequests, RunningAction};

    fn at(y: i16, m: i8, d: i8, h: i8, min: i8) -> Zoned {
        date(y, m, d)
            .at(h, min, 0, 0)
            .to_zoned(TimeZone::fixed(jiff::tz::offset(1)))
            .unwrap()
    }

    #[test]
    fn the_next_run_in_words() {
        let daily = Schedule::default();
        // 2026-11-28 is a Saturday.
        let afternoon = at(2026, 11, 28, 14, 0);
        let run = next_run(daily, &afternoon).unwrap();
        assert_eq!((run.day(), run.hour()), (29, 3));
        assert_eq!(
            when_words(&run, &afternoon, Hours::H24),
            "tomorrow around 03:00"
        );
        let evening = at(2026, 11, 28, 21, 30);
        let run = next_run(daily, &evening).unwrap();
        assert_eq!(
            when_words(&run, &evening, Hours::H24),
            "tonight around 03:00"
        );
        assert_eq!(
            when_words(&run, &evening, Hours::H12),
            "tonight around 3:00\u{a0}AM"
        );
        let early = at(2026, 11, 28, 1, 0);
        let run = next_run(daily, &early).unwrap();
        assert_eq!(run.day(), 28);
        assert_eq!(when_words(&run, &early, Hours::H24), "tonight around 03:00");
        let sunday_15 = Schedule::parse("sun-15").unwrap();
        let run = next_run(sunday_15, &afternoon).unwrap();
        assert_eq!(
            when_words(&run, &afternoon, Hours::H24),
            "tomorrow around 15:00"
        );
        let wed = Schedule::parse("wed-03").unwrap();
        let run = next_run(wed, &afternoon).unwrap();
        assert_eq!((run.day(), run.hour()), (2, 3));
        assert_eq!(
            when_words(&run, &afternoon, Hours::H24),
            "Wednesday around 03:00"
        );
        // Exactly at the hour: the next one.
        let three = at(2026, 11, 28, 3, 0);
        assert_eq!(next_run(daily, &three).unwrap().day(), 29);
        assert_eq!(utc_offset(&three), "UTC+01:00");
    }

    #[test]
    fn whats_new_links_are_ours_and_safe() {
        assert_eq!(
            whats_new(
                (Some("0.0.3"), Some("7c41d0e")),
                (Some("0.0.4"), Some("9e2b5f1"))
            )
            .as_deref(),
            Some("https://github.com/svin24/melissa/compare/7c41d0e...9e2b5f1")
        );
        assert_eq!(
            whats_new((Some("0.0.3"), None), (Some("0.0.4"), None)).as_deref(),
            Some("https://github.com/svin24/melissa/releases/tag/v0.0.4")
        );
        assert_eq!(
            whats_new((Some("0.0.3"), None), (Some("0.0.3"), None)),
            None
        );
        assert_eq!(
            whats_new((None, Some("a\"b")), (None, Some("c/../d"))),
            None
        );
        assert_eq!(
            version_words(Some("0.0.2"), Some("aee030d"), Some("0.0.2")),
            "0.0.2 (aee030d)"
        );
        assert_eq!(
            version_words(Some("0.0.4"), Some("aee030d"), Some("0.0.2")),
            "0.0.4"
        );
    }

    #[test]
    fn the_host_timer_says_when_unless_a_change_waits() {
        let now = at(2026, 11, 28, 14, 0);
        let secs = |z: &Zoned| u64::try_from(z.timestamp().as_second()).unwrap();
        let timer = at(2026, 11, 29, 3, 7);
        let mut state = UpdateState {
            status: Some(ImageStatus {
                next_run: Some(secs(&timer)),
                ..ImageStatus::default()
            }),
            ..UpdateState::default()
        };
        assert_eq!(planned_run(&state, &now).unwrap(), timer);
        // Past (a status written before the run): computed from the schedule instead.
        state.status.as_mut().unwrap().next_run = Some(secs(&now) - 60);
        assert_eq!(planned_run(&state, &now).unwrap().hour(), 3);
        // Not scheduled on the host: None (the page says so).
        state.status.as_mut().unwrap().next_run = None;
        assert!(planned_run(&state, &now).is_none());
        // A new time the helper hasn't written yet: computed from it.
        state.pending.schedule = Schedule::parse("wed-23");
        let run = planned_run(&state, &now).unwrap();
        assert_eq!((run.day(), run.hour()), (2, 23));
    }

    #[test]
    fn one_status_at_a_time() {
        let mut state = UpdateState {
            status: Some(ImageStatus {
                available: Some(ImageView::default()),
                ..ImageStatus::default()
            }),
            ..UpdateState::default()
        };
        let st = state.status.clone().unwrap();
        assert!(matches!(shown(&state, &st), Shown::Available));
        assert!(hint_for(&state).available);
        state.pending = PendingRequests {
            action: Some(UpdateAction::Check),
            ..PendingRequests::default()
        };
        assert!(matches!(shown(&state, &st), Shown::Busy(Busy::Checking)));
        let h = hint_for(&state);
        assert!(!h.available && h.busy == Some("Checking for updates"));
        state.pending = PendingRequests::default();
        state.running = Some(RunningAction {
            action: "auto".into(),
            step: "download".into(),
            version: Some("0.0.4".into()),
            bytes_done: 59_000_000,
            bytes_total: 141_000_000,
            started: 1,
            updated: 1,
        });
        assert!(matches!(shown(&state, &st), Shown::Busy(Busy::Updating)));
        let b = busy_view(Busy::Updating, state.running.as_ref(), &st).unwrap();
        assert_eq!(b.label, "Downloading 0.0.4\u{2026} 41%");
        assert_eq!(b.text, "59 of 141 MB");
        // Never 100 % before it is done.
        state.running.as_mut().unwrap().bytes_done = 141_000_000;
        let b = busy_view(Busy::Updating, state.running.as_ref(), &st).unwrap();
        assert!(b.label.ends_with("99%"), "{}", b.label);
        state.running.as_mut().unwrap().step = "restart".into();
        let b = busy_view(Busy::Updating, state.running.as_ref(), &st).unwrap();
        assert_eq!(b.total, 0);
        assert!(b.below.starts_with("Step 2 of 3"));
    }
}
