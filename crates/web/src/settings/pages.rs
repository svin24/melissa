//! The Settings pages, assembled only from `components` (no markup of their own beyond
//! layout wrappers). The view structs are filled by `providers.rs` and `updates.rs`.

use maud::{html, Markup};

use super::apply::Pending;
use crate::{
    components::{
        actions, app_page, badge, card, code, command_block, confirm_card, detail_head, disclosure,
        external_button, facts, group, lines_field, live_region, log_view, notice, one_field_form,
        pick_list, plain_action, provider_mark, region, settings_form, settings_frame_at,
        settings_head, split_card, text_field, toggle, var_list, ConfirmCard, LogLine, Nav,
        PickRow, SettingsSection, Toggle, Tone, VarRow,
    },
    pages::{rail_for, Frame},
};
use melissa_settings::providers::EnvVarView;

/// The Settings sections, in order. Add new ones here (and a route in `mod.rs`).
pub const SECTIONS: [(&str, &str); 3] = [
    ("/settings/providers", "Providers"),
    ("/settings/updates", "Updates"),
    (DEVELOPER_URL, "Developer"),
];

/// The Developer section: Agent tasks (`/tasks` redirects here when the lead agent is present).
pub const DEVELOPER_URL: &str = "/settings/developer";

/// DOM id of the Providers region (forms swap it).
pub const PROVIDERS_ID: &str = "providers";
/// DOM id of the Updates region.
pub const UPDATES_ID: &str = "updates";
/// DOM id of the chat display preference on the Developer page.
pub const CHAT_PREFS_ID: &str = "chat-prefs";
const STEPS_URL: &str = "/ui/settings/developer/steps";

/// The Developer page's chat preference: show the agents' thinking and step timings.
pub fn chat_prefs_region(on: bool, error: Option<&str>) -> Markup {
    let label = "Show thinking and timings";
    region(
        CHAT_PREFS_ID,
        html! {
            (facts(&[(label, toggle(&Toggle {
                action: STEPS_URL,
                target: CHAT_PREFS_ID,
                label,
                on,
                locked: None,
            }))]))
            @if let Some(e) = error { (notice(e, Tone::Error)) }
        },
    )
}

fn frame(f: &Frame<'_>, current: &str, content: Markup) -> Markup {
    frame_at(f, current, None, content)
}

/// A Settings page: `current` is the section, `page` a page below it (its breadcrumb).
pub(crate) fn frame_at(
    f: &Frame<'_>,
    current: &str,
    page: Option<&str>,
    content: Markup,
) -> Markup {
    let sections: Vec<SettingsSection<'_>> = SECTIONS
        .iter()
        .map(|(href, label)| SettingsSection {
            href,
            label,
            current: *label == current,
        })
        .collect();
    let title = match page {
        Some(p) => format!("{p} · {current} · Settings"),
        None => format!("{current} · Settings"),
    };
    app_page(
        &title,
        rail_for(Nav::Settings, f, None),
        settings_frame_at(&sections, current, page, content),
    )
}

/// A message after an action.
pub struct Flash {
    pub text: String,
    pub tone: Tone,
}

// --- Providers ----------------------------------------------------------------------------------

/// One way Melissa runs a provider (a configured backend), for Details.
pub struct BackendLine {
    pub name: String,
    /// "ready", "not signed in", "off", ...
    pub state: String,
}

/// The one thing to do for a provider now (at most one primary action).
pub enum Step {
    /// Nothing to do (ready, disabled, or a change waits to apply).
    Nothing,
    /// One short line, e.g. the Nix note.
    Note(String),
    /// A command to type: runs from source only, never in the container.
    Command {
        text: String,
        command: String,
    },
    /// The Install button, after a failure with the reason and the installer's log.
    Install {
        vendor: &'static str,
        failed: Option<String>,
        log: Vec<String>,
    },
    Installing,
    /// The Sign in button, after a failed try with a short reason.
    SignIn {
        again: Option<&'static str>,
    },
    /// A sign-in started; the vendor's link isn't there yet.
    Starting,
    /// Claude: open the link, sign in, paste the code here.
    PasteCode {
        url: String,
    },
    /// Claude: the code was sent; waiting for the CLI.
    Checking,
    /// Grok: open the link and confirm the code there.
    DeviceCode {
        url: String,
        code: String,
    },
}

/// The technical side of a provider, behind "Details".
pub struct ProviderDetails {
    pub used_by: Vec<String>,
    pub backends: Vec<BackendLine>,
    pub models: Vec<String>,
    /// Where the program is (None: not found).
    pub program: Option<String>,
    /// Saved overrides (empty: none).
    pub display_name: String,
    pub command: String,
    pub default_command: String,
    pub args: Vec<String>,
    pub env: Vec<EnvVarView>,
}

/// One provider as the Settings page shows it. All CLI-sourced text is already cleaned;
/// components escape it.
pub struct ProviderCard {
    pub id: String,
    pub label: String,
    pub version: Option<String>,
    /// The saved setting.
    pub enabled: bool,
    /// One word or short phrase: Ready, Sign in needed, Not installed, Installing…, ...
    pub status: String,
    pub tone: Tone,
    /// Why it can't be disabled now.
    pub lock: Option<String>,
    pub step: Step,
    /// "Use an API key instead": Some(already set) where the CLI takes one.
    pub key: Option<bool>,
    /// Open the key field at once (there is no sign-in from the page here).
    pub key_open: bool,
    pub details: ProviderDetails,
}

pub struct ProvidersView {
    pub cards: Vec<ProviderCard>,
    pub selected: usize,
    /// "Checked just now".
    pub checked: String,
    /// Saved changes wait to apply.
    pub pending: Option<Pending>,
    /// Where Melissa can't restart herself and it is a run from source: how to.
    pub manual_restart: Option<String>,
    pub flash: Option<Flash>,
    /// The agents' home directory (logins, config), e.g. `/var/lib/melissa/home`.
    pub home: Option<String>,
    /// The settings file.
    pub file: Option<String>,
    /// Something moves (install, sign-in, apply): the region refreshes itself from this URL.
    pub poll: Option<String>,
}

fn letter(label: &str) -> String {
    label
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "?".into())
}

fn provider_url(id: &str) -> String {
    format!("/ui/settings/providers/{id}")
}

pub fn providers_page(f: &Frame<'_>, v: &ProvidersView) -> Markup {
    frame(f, "Providers", providers_region(v))
}

pub fn providers_region(v: &ProvidersView) -> Markup {
    let hrefs: Vec<String> = v
        .cards
        .iter()
        .map(|c| format!("/settings/providers?p={}", c.id))
        .collect();
    let versions: Vec<String> = v
        .cards
        .iter()
        .map(|c| {
            c.version
                .as_deref()
                .map(|v| format!("v{v}"))
                .unwrap_or_default()
        })
        .collect();
    let toggle_urls: Vec<String> = v
        .cards
        .iter()
        .map(|c| format!("{}/enabled", provider_url(&c.id)))
        .collect();
    let toggle_labels: Vec<String> = v.cards.iter().map(|c| format!("Use {}", c.label)).collect();
    let rows: Vec<PickRow<'_>> = v
        .cards
        .iter()
        .enumerate()
        .map(|(i, c)| PickRow {
            href: &hrefs[i],
            mark: provider_mark(&letter(&c.label), !c.enabled),
            name: &c.label,
            version: &versions[i],
            status: &c.status,
            tone: c.tone,
            current: i == v.selected,
            toggle: Some(toggle(&Toggle {
                action: &toggle_urls[i],
                target: PROVIDERS_ID,
                label: &toggle_labels[i],
                on: c.enabled,
                locked: if c.enabled { c.lock.as_deref() } else { None },
            })),
        })
        .collect();
    let detail = match v.cards.get(v.selected) {
        Some(c) => provider_detail(c, v),
        None => html! { p.quiet { "No agents are configured." } },
    };
    live_region(
        PROVIDERS_ID,
        v.poll.as_deref().unwrap_or(""),
        2,
        v.poll.is_some(),
        html! {
            (settings_head(
                "Providers",
                "The agents Melissa can use.",
                &v.checked,
                "/ui/settings/providers/refresh",
                PROVIDERS_ID,
                "Check again",
            ))
            @if let Some(fl) = &v.flash { (notice(&fl.text, fl.tone)) }
            @if let Some(p) = v.pending { (pending_note(p, v.manual_restart.as_deref())) }
            (split_card("provider-card", "Providers", pick_list("Providers", &rows, None), detail))
        },
    )
}

/// Saved changes that wait: one line (and, from source only, how to restart).
pub(super) fn pending_note(p: Pending, manual: Option<&str>) -> Markup {
    let text = match p {
        Pending::AfterTask => "Applying after the current task.",
        Pending::Now => "Applying… Melissa is back in a few seconds.",
        Pending::Manual => "Restart Melissa to apply.",
    };
    html! {
        (notice(text, Tone::Info))
        @if p == Pending::Manual { @if let Some(cmd) = manual { (command_block(cmd)) } }
    }
}

fn provider_detail(c: &ProviderCard, v: &ProvidersView) -> Markup {
    let url = provider_url(&c.id);
    let sub = c
        .version
        .as_deref()
        .map(|v| format!("Version {v}"))
        .unwrap_or_default();
    html! {
        (detail_head(provider_mark(&letter(&c.label), !c.enabled), &c.label, &sub,
            html! { (badge(&c.status, c.tone)) }))
        (step_view(c, &url))
        @if let Some(set) = c.key { (key_view(set, c.key_open, &url)) }
        (details_view(c, v, &url))
    }
}

fn step_view(c: &ProviderCard, url: &str) -> Markup {
    let login = format!("{url}/login");
    match &c.step {
        Step::Nothing => html! {},
        Step::Note(text) => html! { p.quiet { (text) } },
        Step::Command { text, command } => html! {
            p.quiet { (text) }
            (command_block(command))
        },
        Step::Install {
            vendor,
            failed,
            log,
        } => html! {
            @if let Some(f) = failed { (notice(f, Tone::Error)) }
            (actions(plain_action(&format!("{url}/install"), PROVIDERS_ID,
                if failed.is_some() { "Try again" } else { "Install" }, true)))
            @if failed.is_none() {
                p.quiet.small { "Uses " (vendor) "'s official installer; you accept their terms." }
            }
            @if !log.is_empty() {
                (disclosure("Log", false, log_view(log.iter().map(|l| LogLine {
                    text: l,
                    is_error: l.starts_with("melissa-setup:"),
                }))))
            }
        },
        Step::Installing => notice("Installing… This takes about a minute.", Tone::Info),
        Step::SignIn { again } => html! {
            @if let Some(a) = again { (notice(a, Tone::Error)) }
            (actions(plain_action(&login, PROVIDERS_ID, "Sign in", true)))
        },
        Step::Starting => notice("Starting the sign-in…", Tone::Info),
        Step::PasteCode { url: link } => html! {
            ol.steps {
                li { (external_button(link, "Open the sign-in page")) }
                li {
                    (one_field_form(&format!("{url}/login/code"), PROVIDERS_ID, "Continue",
                        text_field("code", "Paste the code", "", "", "text", None)))
                }
            }
            (actions(plain_action(&format!("{url}/login/cancel"), PROVIDERS_ID, "Cancel", false)))
        },
        Step::Checking => notice("Signing in…", Tone::Info),
        Step::DeviceCode {
            url: link,
            code: dc,
        } => html! {
            (facts(&[("Code", code(dc))]))
            (actions(external_button(link, "Open the sign-in page")))
            p.quiet.small { "Confirm the code there. This page updates by itself." }
        },
    }
}

/// "Use an API key instead": one write-only field; the variable name stays out of sight.
fn key_view(set: bool, open: bool, url: &str) -> Markup {
    let action = format!("{url}/key");
    if set {
        return html! {
            div.key-set {
                span.quiet { "API key set" }
                (plain_action(&action, PROVIDERS_ID, "Remove", false))
            }
        };
    }
    disclosure(
        "Use an API key instead",
        open,
        one_field_form(
            &action,
            PROVIDERS_ID,
            "Save",
            text_field("value", "API key", "", "", "password", None),
        ),
    )
}

/// Everything technical, behind one "Details".
fn details_view(c: &ProviderCard, v: &ProvidersView, url: &str) -> Markup {
    let d = &c.details;
    let used = if d.used_by.is_empty() {
        html! { span.quiet { "Nothing" } }
    } else {
        html! { (d.used_by.join(", ")) }
    };
    let ways = html! {
        ul.plain-list {
            @for b in &d.backends {
                li { (code(&b.name)) " " span.quiet { (b.state) } }
            }
        }
    };
    let mut rows = vec![("Used by", used), ("Backends", ways)];
    if !d.models.is_empty() {
        rows.push((
            "Models",
            html! { span.badges { @for m in &d.models { (badge(m, Tone::Muted)) " " } } },
        ));
    }
    rows.push((
        "Program",
        html! { (d.program.as_deref().unwrap_or("not found")) },
    ));
    if let Some(h) = &v.home {
        rows.push(("Home folder", code(h)));
    }
    if let Some(lock) = &c.lock {
        rows.push(("Can't turn off", html! { (lock) }));
    }
    let remove = format!("{url}/env/remove");
    let var_rows: Vec<VarRow<'_>> = d
        .env
        .iter()
        .map(|e| VarRow {
            name: &e.name,
            value: e.value.as_deref(),
            remove_action: &remove,
        })
        .collect();
    disclosure(
        "Details",
        false,
        html! {
            (facts(&rows))
            (settings_form(url, PROVIDERS_ID, "Save", html! {
                (text_field("display_name", "Display name", &d.display_name, melissa_settings::providers::default_label(&c.id), "text", None))
                (text_field("command", "Program path", &d.command, &d.default_command, "text",
                    Some("Empty: the installed one.")))
                (lines_field("args", "Extra launch arguments", &d.args.join("\n"),
                    Some("One per line.")))
            }))
            (group("Environment", Some("Secret values are never shown again."), html! {
                (var_list(&var_rows, PROVIDERS_ID, "None."))
                (settings_form(&format!("{url}/env"), PROVIDERS_ID, "Add", html! {
                    div.field-row {
                        (text_field("name", "Name", "", "", "text", None))
                        (text_field("value", "Value", "", "", "password", None))
                    }
                }))
            }))
            @if let Some(file) = &v.file {
                p.quiet.small { "Saved in " (code(file)) "." }
            }
        },
    )
}

// --- Updates ------------------------------------------------------------------------------------

/// One container image line.
pub struct ImageLine {
    pub label: &'static str,
    pub image: String,
    pub detail: String,
}

/// What the Updates page shows.
pub struct UpdatesView {
    pub version: String,
    /// The image state in a word or two ("Up to date"), where the buttons work.
    pub status: Option<(&'static str, Tone)>,
    /// One short sentence where updating from here isn't the way.
    pub note: Option<&'static str>,
    /// Behind Details: installed as, revision, automatic updates.
    pub details: Vec<(&'static str, String)>,
    pub images: Vec<ImageLine>,
    /// "Checked 5 min ago" (empty: nothing to check here).
    pub checked: String,
    pub last: Option<Flash>,
    pub flash: Option<Flash>,
    /// Buttons the page may show (the helper is there).
    pub can_check: bool,
    pub can_apply: bool,
    pub can_rollback: bool,
    /// A pending confirmation for `apply` or `rollback`.
    pub confirm: Option<&'static str>,
    /// Commands to type (Nix and runs from source only): (label, command).
    pub commands: Vec<(String, String)>,
    /// Show the commands at once (runs from source) instead of behind Details.
    pub commands_open: bool,
    pub pending: Option<Pending>,
    pub manual_restart: Option<String>,
}

pub fn updates_page(f: &Frame<'_>, v: &UpdatesView) -> Markup {
    frame(f, "Updates", updates_region(v))
}

pub fn updates_region(v: &UpdatesView) -> Markup {
    let melissa = html! {
        (v.version)
        @if let Some((word, tone)) = v.status { " " (badge(word, tone)) }
    };
    let mut detail_rows: Vec<(&str, Markup)> = v
        .details
        .iter()
        .map(|(k, val)| (*k, html! { (val) }))
        .collect();
    for i in &v.images {
        detail_rows.push((
            i.label,
            html! { (code(&i.image)) br; span.quiet { (i.detail) } },
        ));
    }
    let commands = html! {
        @for (label, cmd) in &v.commands {
            p.quiet { (label) }
            (command_block(cmd))
        }
    };
    html! {
        div id=(UPDATES_ID) {
            @if v.checked.is_empty() {
                header.settings-head { div { h1 { "Updates" } p.lead { "Melissa on this device." } } }
            } @else {
                (settings_head(
                    "Updates",
                    "Melissa on this device.",
                    &v.checked,
                    "/ui/settings/updates/check",
                    UPDATES_ID,
                    if v.can_check { "Check for updates" } else { "Look again" },
                ))
            }
            @if let Some(fl) = &v.flash { (notice(&fl.text, fl.tone)) }
            @if let Some(p) = v.pending { (pending_note(p, v.manual_restart.as_deref())) }
            @if let Some(action) = v.confirm {
                @if action == "apply" {
                    (confirm_card(&ConfirmCard {
                        title: "Update and restart Melissa?",
                        text: "Apps and chats pause for a few minutes. Data is kept, and you can roll back.",
                        action: "/ui/settings/updates/apply",
                        target: UPDATES_ID,
                        confirm: "Update",
                        cancel_href: "/settings/updates",
                    }))
                } @else {
                    (confirm_card(&ConfirmCard {
                        title: "Go back to the previous version?",
                        text: "Melissa restarts on it. Apps and chats pause for a few minutes. Data is kept.",
                        action: "/ui/settings/updates/rollback",
                        target: UPDATES_ID,
                        confirm: "Roll back",
                        cancel_href: "/settings/updates",
                    }))
                }
            }
            (card("This device", html! {
                (facts(&[("Melissa", melissa)]))
                @if let Some(n) = v.note { p.quiet { (n) } }
                @if let Some(l) = &v.last { (notice(&l.text, l.tone)) }
                @if v.can_apply || v.can_rollback {
                    (actions(html! {
                        @if v.can_apply { (plain_action("/ui/settings/updates/apply", UPDATES_ID, "Update", true)) }
                        @if v.can_rollback { (plain_action("/ui/settings/updates/rollback", UPDATES_ID, "Roll back", false)) }
                    }))
                }
                @if v.commands_open { (commands) }
                (disclosure("Details", false, html! {
                    (facts(&detail_rows))
                    @if !v.commands_open { (commands) }
                }))
            }))
        }
    }
}
