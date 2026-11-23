//! The Settings pages, assembled only from `components` (no markup of their own beyond
//! layout wrappers). The view structs are filled by `providers.rs` and `updates.rs`.

use maud::{html, Markup};

use super::apply::Pending;
use crate::{
    components::{
        about_head, action_button, actions, away_card, badge, code, command_block, confirm_card,
        danger_confirm, detail_head, disclosure, external_button, facts, group, inline_form,
        inline_number, inline_select, inline_text_form, license_text, lines_field, link,
        link_button, live_region, log_view, markdown_document, new_tab_button, notice,
        one_field_form, pick_list, plain_action, progress_bar, provider_logo, region,
        select_field_at, setting_card, setting_row, setting_row_details, setting_row_full,
        setting_row_with, settings_form, settings_frame_at, settings_head, settings_tools_oob,
        split_card, text_field, toggle, var_list, Btn, Choice, ConfirmCard, DangerConfirm, Glyph,
        LogLine, Nav, PickRow, SettingsHead, SettingsSection, Toggle, Tone, VarRow,
    },
    components::{avatar, icon_picker, Avatar},
    pages::{rail_for, Frame},
};
use melissa_settings::providers::EnvVarView;

/// A Settings section: where it is, its name, the group it is listed under, its icon and the
/// titles of the settings in it (for the search field).
pub struct Section {
    pub href: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    pub icon: Glyph,
    pub keywords: &'static str,
}

/// The Settings sections, in order and grouped. Add new ones here (and a route in `mod.rs`).
pub const SECTIONS: [Section; 14] = [
    Section {
        href: "/settings/providers",
        label: "Providers",
        group: "Agents",
        icon: Glyph::Plug,
        keywords: "Claude Code Grok OpenCode Gemini Codex Muse install sign in API key add provider remove provider version",
    },
    Section {
        href: AGENTS_URL,
        label: "Agents",
        group: "Agents",
        icon: Glyph::Person,
        keywords: "Melissa helpers delete lead agent runs on",
    },
    Section {
        href: PERMISSIONS_URL,
        label: "Permissions",
        group: "Agents",
        icon: Glyph::Key,
        keywords: "Always allow rules allowed requests",
    },
    Section {
        href: SCHEDULES_URL,
        label: "Schedules",
        group: "Agents",
        icon: Glyph::Clock,
        keywords: "Scheduled jobs run now timetable When a run needs your OK wait minutes",
    },
    Section {
        href: VOICE_URL,
        label: "Voice",
        group: "Chat",
        icon: Glyph::Mic,
        keywords: "Voice input speech model microphone",
    },
    Section {
        href: FILES_URL,
        label: "Files",
        group: "Chat",
        icon: Glyph::File,
        keywords: "Keep files for file boxes expired",
    },
    Section {
        href: APPS_URL,
        label: "Apps",
        group: "System",
        icon: Glyph::Hexagon,
        keywords: "Apps open to This device Tailnet Home network",
    },
    Section {
        href: TAILSCALE_URL,
        label: "Tailscale",
        group: "System",
        icon: Glyph::Server,
        keywords: "Tailscale tailnet enable sign in disable connected",
    },
    Section {
        href: UPDATES_URL,
        label: "Updates",
        group: "System",
        icon: Glyph::Refresh,
        keywords: "Melissa version check for updates update roll back restart automatic updates This device Device name rename",
    },
    Section {
        href: DEVELOPER_URL,
        label: "Developer",
        group: "System",
        icon: Glyph::Code,
        keywords: "Developer mode No permission prompts Full access per agent agent tasks Show thinking and timings chat",
    },
    Section {
        href: RESET_URL,
        label: "Reset",
        group: "System",
        icon: Glyph::Trash,
        keywords: "Reset Melissa factory reset erase start over",
    },
    Section {
        href: "/settings/account",
        label: "Account",
        group: "People",
        icon: Glyph::Key,
        keywords: "Username password change password signed in sessions sign out everywhere",
    },
    Section {
        href: "/settings/people",
        label: "People",
        group: "People",
        icon: Glyph::Person,
        keywords: "Members invite link reset password remove owner",
    },
    // Last, for everyone signed in (2026-11-22).
    Section {
        href: ABOUT_URL,
        label: "About",
        group: "Melissa",
        icon: Glyph::Info,
        keywords: "About version revision build license licence MIT credits acknowledgments third-party notices sponsor donate source code GitHub report a problem issues",
    },
];

/// The About section: version, links, Sponsor, license and credits (everyone signed in).
pub const ABOUT_URL: &str = "/settings/about";
/// Plain text: the license report for the Rust crates, where the image ships one.
pub const RUST_LICENSES_URL: &str = "/settings/about/rust-crates";

/// The Agents section: Melissa and her helpers (Delete).
pub const AGENTS_URL: &str = "/settings/agents";
/// DOM id of the Agents region (Delete swaps it).
pub const AGENTS_ID: &str = "agents";

/// The Schedules section: scheduled jobs.
pub const SCHEDULES_URL: &str = "/settings/schedules";
/// DOM id of the Schedules section (every action swaps it).
pub const SCHEDULES_ID: &str = "schedules";

/// The Apps section: who may open the apps on this device.
pub const APPS_URL: &str = "/settings/apps";
/// DOM id of the Apps region.
pub const APPS_ID: &str = "apps-settings";

/// The Tailscale section: Melissa's own Tailscale (off by default).
pub const TAILSCALE_URL: &str = "/settings/tailscale";
/// DOM id of the Tailscale region.
pub const TAILSCALE_ID: &str = "tailscale-settings";

/// The Files section: how long file boxes keep files.
pub const FILES_URL: &str = "/settings/files";
/// DOM id of the Files region.
pub const FILES_ID: &str = "files-settings";

/// The Permissions section: the "Always allow" rules.
pub const PERMISSIONS_URL: &str = "/settings/permissions";
/// DOM id of the rule list (Remove swaps it).
pub const PERMISSIONS_ID: &str = "permissions";
/// DOM id of No permission prompts on the Developer page.
pub const BYPASS_ID: &str = "bypass";
/// DOM id of Full access per agent on the Developer page.
pub const FULL_ACCESS_ID: &str = "full-access";

/// The Reset section: erase Melissa's data and start again (owner only, container install).
pub const RESET_URL: &str = "/settings/reset";
/// DOM id of the Reset region.
pub const RESET_ID: &str = "reset-settings";
/// Polled while a reset runs (public: the session ends with the reset).
pub const RESET_STATUS_URL: &str = "/reset-status";

/// The Voice section: voice input for the chat composer.
pub const VOICE_URL: &str = "/settings/voice";
/// DOM id of the Voice region.
pub const VOICE_ID: &str = "voice-settings";

/// The Developer section: Agent tasks (`/tasks` redirects here when the lead agent is present).
pub const DEVELOPER_URL: &str = "/settings/developer";

/// The Updates section: Melissa's version and this device's name.
pub const UPDATES_URL: &str = "/settings/updates";
/// DOM id of the device name's region on the Updates page (its form swaps it).
pub const DEVICE_NAME_ID: &str = "device-name";

/// DOM id of the Providers region (forms swap it).
pub const PROVIDERS_ID: &str = "providers";
/// DOM id of the Updates region.
pub const UPDATES_ID: &str = "updates";
/// DOM id of the chat display preference on the Developer page.
pub const CHAT_PREFS_ID: &str = "chat-prefs";
const STEPS_URL: &str = "/ui/settings/developer/steps";
const ENTER_URL: &str = "/ui/settings/developer/enter";

/// The Developer page's chat preferences: show the agents' thinking and step timings, and
/// what Enter does while an agent works.
pub fn chat_prefs_region(prefs: melissa_settings::prefs::Prefs, error: Option<&str>) -> Markup {
    let label = "Show thinking and timings";
    let enter: [(String, String); 2] = [
        ("queue".into(), "Queues".into()),
        ("steer".into(), "Steers".into()),
    ];
    let selected = if prefs.enter_steers { "steer" } else { "queue" };
    region(
        CHAT_PREFS_ID,
        html! {
            (setting_row_with(
                html! { (label) },
                Some(html! { "Thoughts open in the chat; steps show their time." }),
                toggle(&Toggle {
                    action: STEPS_URL,
                    target: CHAT_PREFS_ID,
                    label,
                    on: prefs.show_steps,
                    locked: None,
                }),
                None,
            ))
            (setting_row_with(
                html! { "Enter while working" },
                Some(html! { "Double Enter does the other." }),
                inline_form(ENTER_URL, CHAT_PREFS_ID, inline_select("enter", "Enter while working", &enter, selected)),
                error.map(|e| notice(e, Tone::Caution)),
            ))
        },
    )
}

/// A section's page with its one-line lead and head bar tools.
fn frame(f: &Frame<'_>, current: &str, head: SettingsHead<'_>, content: Markup) -> Markup {
    frame_head(f, current, None, head, content)
}

/// A Settings page: `current` is the section, `page` a page below it (its breadcrumb).
pub(crate) fn frame_at(
    f: &Frame<'_>,
    current: &str,
    page: Option<&str>,
    content: Markup,
) -> Markup {
    frame_head(f, current, page, SettingsHead::default(), content)
}

/// [`frame_at`] with a lead line and head bar tools.
pub(crate) fn frame_head(
    f: &Frame<'_>,
    current: &str,
    page: Option<&str>,
    head: SettingsHead<'_>,
    content: Markup,
) -> Markup {
    // Members don't see the owner's sections (the gate refuses them anyway).
    let owner = crate::auth::signed_in().is_none_or(|p| p.is_owner());
    // `GET /settings`: the list level, with each row's value.
    let list = super::list::current();
    let sections: Vec<SettingsSection<'_>> = SECTIONS
        .iter()
        .filter(|s| owner || crate::auth::need(s.href) != crate::auth::Need::Owner)
        .map(|s| SettingsSection {
            href: s.href,
            label: s.label,
            current: s.label == current,
            group: s.group,
            icon: s.icon,
            keywords: s.keywords,
            value: list.as_ref().and_then(|l| l.value(s.href)),
            danger: s.href == RESET_URL,
        })
        .collect();
    let title = match page {
        _ if list.is_some() => "Settings".to_string(),
        Some(p) => format!("{p} · {current} · Settings"),
        None => format!("{current} · Settings"),
    };
    crate::pages::panel_shell(
        f,
        &title,
        rail_for(Nav::Settings, f, None),
        settings_frame_at(&sections, current, page, list.is_some(), head, content),
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
    /// The program is on this device (an enabled provider that isn't shows its switch off).
    pub installed: bool,
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
    /// The in-page question before Remove provider.
    pub remove_confirm: String,
    pub details: ProviderDetails,
}

impl ProviderCard {
    /// Installed and ready to use (whether or not it is switched on).
    pub fn ready(&self) -> bool {
        self.installed && self.tone == Tone::Ok
    }
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
    let head = SettingsHead {
        lead: None,
        tools: Some(providers_tools(v)),
    };
    frame(f, "Providers", head, providers_body(v))
}

/// The head bar's tools: when the list was checked, Check again, Add provider.
fn providers_tools(v: &ProvidersView) -> Markup {
    html! {
        (settings_head(&v.checked, "/ui/settings/providers/refresh", PROVIDERS_ID, "Check again", Btn::link()))
        (link_button(Btn::secondary().sm(), "/settings/providers/add", "Add provider"))
    }
}

/// The region for htmx answers: the section and, out of band, the head bar's tools.
pub fn providers_region(v: &ProvidersView) -> Markup {
    html! {
        (providers_body(v))
        (settings_tools_oob(providers_tools(v)))
    }
}

/// A provider's switch shows on only when it is enabled and its program is here:
/// an enabled built-in that isn't installed yet must not look in use.
fn switch_on(c: &ProviderCard) -> bool {
    c.enabled && c.installed
}

/// Why the switch can't be used now: in use, or waiting for its install (the detail shows how).
fn switch_lock(c: &ProviderCard) -> Option<&str> {
    if !c.enabled {
        None
    } else if let Some(why) = c.lock.as_deref() {
        Some(why)
    } else if !c.installed {
        Some("Install it first.")
    } else {
        None
    }
}

fn providers_body(v: &ProvidersView) -> Markup {
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
            mark: provider_logo(&c.id, &letter(&c.label), !switch_on(c)),
            name: &c.label,
            version: &versions[i],
            status: &c.status,
            tone: c.tone,
            current: i == v.selected,
            toggle: Some(toggle(&Toggle {
                action: &toggle_urls[i],
                target: PROVIDERS_ID,
                label: &toggle_labels[i],
                on: switch_on(c),
                locked: switch_lock(c),
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
            @if let Some(fl) = &v.flash { (notice(&fl.text, fl.tone)) }
            @if let Some(p) = v.pending { (pending_note(p, v.manual_restart.as_deref())) }
            (split_card("provider-card", "Providers", pick_list("Providers", &rows, None), detail))
        },
    )
}

/// First-run setup's Provider step (`settings::setup`): the providers as `tiles`, then the
/// chosen one's next action (the same forms as [`providers_region`], so the same handlers),
/// and the step's buttons. Same id, so those handlers' answers replace it.
pub fn setup_provider_region(
    v: &ProvidersView,
    tiles: Markup,
    selected: Option<&ProviderCard>,
    footer: Markup,
) -> Markup {
    live_region(
        PROVIDERS_ID,
        v.poll.as_deref().unwrap_or(""),
        2,
        v.poll.is_some(),
        html! {
            (tiles)
            @if let Some(fl) = &v.flash { (notice(&fl.text, fl.tone)) }
            @if let Some(p) = v.pending { (pending_note(p, v.manual_restart.as_deref())) }
            @if let Some(c) = selected {
                div.setup-provider {
                    @let url = provider_url(&c.id);
                    (detail_head(provider_logo(&c.id, &letter(&c.label), false), &c.label, "",
                        html! { (badge(&c.status, c.tone)) }))
                    (step_view(c, &url))
                    @if let Some(set) = c.key { (key_view(set, c.key_open, &url)) }
                    @if c.tone != Tone::Ok {
                        (actions(plain_action(
                            &format!("/ui/settings/providers/refresh?p={}", c.id),
                            PROVIDERS_ID, "Check again", false)))
                    }
                }
            }
            (footer)
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
        (detail_head(provider_logo(&c.id, &letter(&c.label), !switch_on(c)), &c.label, &sub,
            html! { (badge(&c.status, c.tone)) }))
        (step_view(c, &url))
        @if let Some(set) = c.key { (key_view(set, c.key_open, &url)) }
        (details_view(c, v, &url))
        div.remove-provider {
            (action_button(Btn::danger_quiet(), &format!("{url}/remove"), PROVIDERS_ID, "Remove provider", &c.remove_confirm))
        }
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
            @if let Some(f) = failed { (notice(f, Tone::Caution)) }
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
            @if let Some(a) = again { (notice(a, Tone::Caution)) }
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
            html! { span.badges { @for m in &d.models { (badge(m, Tone::Neutral)) " " } } },
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
            (settings_form(url, PROVIDERS_ID, "Save", None, html! {
                (text_field("display_name", "Display name", &d.display_name, melissa_settings::providers::default_label(&c.id), "text", None))
                (text_field("command", "Program path", &d.command, &d.default_command, "text",
                    Some("Empty: the installed one.")))
                (lines_field("args", "Extra launch arguments", &d.args.join("\n"),
                    Some("One per line.")))
            }))
            (group("Environment", Some("Secret values are never shown again."), html! {
                (var_list(&var_rows, PROVIDERS_ID, "None."))
                (settings_form(&format!("{url}/env"), PROVIDERS_ID, "Add", None, html! {
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
    /// This device's name (owner only, like the page).
    pub device: DeviceNameView,
}

/// The device name's field: the saved name (else the host's) and the last save's answer.
pub struct DeviceNameView {
    pub value: String,
    pub flash: Option<Flash>,
}

/// The device's name as a field and a Save button; its form swaps only this region.
pub fn device_name_region(v: &DeviceNameView) -> Markup {
    region(
        DEVICE_NAME_ID,
        setting_row_with(
            html! { "Device name" },
            Some(html! { "Shown in Melissa only." }),
            inline_text_form(
                super::device::SAVE_URL,
                DEVICE_NAME_ID,
                "name",
                "Device name",
                &v.value,
                64,
            ),
            v.flash.as_ref().map(|f| notice(&f.text, f.tone)),
        ),
    )
}

pub fn updates_page(f: &Frame<'_>, v: &UpdatesView) -> Markup {
    let head = SettingsHead {
        lead: None,
        tools: updates_tools(v),
    };
    frame(f, "Updates", head, updates_body(v))
}

/// The head bar's tools: when Melissa last looked, and Check for updates.
fn updates_tools(v: &UpdatesView) -> Option<Markup> {
    (!v.checked.is_empty()).then(|| {
        settings_head(
            &v.checked,
            "/ui/settings/updates/check",
            UPDATES_ID,
            if v.can_check {
                "Check for updates"
            } else {
                "Look again"
            },
            Btn::secondary().sm(),
        )
    })
}

/// The region for htmx answers: the section and, out of band, the head bar's tools.
pub fn updates_region(v: &UpdatesView) -> Markup {
    html! {
        (updates_body(v))
        (settings_tools_oob(updates_tools(v).unwrap_or_default()))
    }
}

fn updates_body(v: &UpdatesView) -> Markup {
    let melissa = html! { "Version " (v.version) };
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
                        cancel_href: UPDATES_URL,
                    }))
                } @else {
                    (confirm_card(&ConfirmCard {
                        title: "Go back to the previous version?",
                        text: "Melissa restarts on it. Apps and chats pause for a few minutes. Data is kept.",
                        action: "/ui/settings/updates/rollback",
                        target: UPDATES_ID,
                        confirm: "Roll back",
                        cancel_href: UPDATES_URL,
                    }))
                }
            }
            // One card (2026-11-23): Melissa with her details, then the device's name.
            (setting_card(None, html! {
                (setting_row_with(
                    html! { "Melissa" },
                    Some(melissa),
                    html! {
                        @if let Some((word, tone)) = v.status { (badge(word, tone)) }
                        @if v.can_rollback { (plain_action("/ui/settings/updates/rollback", UPDATES_ID, "Roll back", false)) }
                        @if v.can_apply { (plain_action("/ui/settings/updates/apply", UPDATES_ID, "Update", true)) }
                    },
                    Some(html! {
                        @if let Some(n) = v.note { p.quiet { (n) } }
                        @if let Some(l) = &v.last { (notice(&l.text, l.tone)) }
                        @if v.commands_open { (commands) }
                        (disclosure("Details", false, html! {
                            (facts(&detail_rows))
                            @if !v.commands_open { (commands) }
                        }))
                    }),
                ))
                (device_name_region(&v.device))
            }))
        }
    }
}

// --- Permissions ---------------------------------------------------------------------------------

/// One "Always allow" rule, in words.
pub struct RuleRow {
    pub id: u64,
    /// "Ada", "Melissa".
    pub who: String,
    /// "update Minecraft", "run commands".
    pub what: String,
    /// The tool, for the curious.
    pub tool: String,
    /// Unix seconds.
    pub created: u64,
}

pub fn permissions_page(f: &Frame<'_>, rules: &[RuleRow], flash: Option<&Flash>) -> Markup {
    let head = SettingsHead {
        lead: Some("What helpers may do without asking you."),
        tools: None,
    };
    frame(f, "Permissions", head, permissions_region(rules, flash))
}

/// The rule list (swapped whole by Remove): one row per rule.
pub fn permissions_region(rules: &[RuleRow], flash: Option<&Flash>) -> Markup {
    region(
        PERMISSIONS_ID,
        html! {
            @if let Some(f) = flash { (notice(&f.text, f.tone)) }
            (setting_card(None, html! {
                @if rules.is_empty() {
                    (setting_row("Nothing yet", Some("Every request asks you first. Add a rule with Always allow on a request."), html! {}))
                }
                @for r in rules {
                    (setting_row_with(
                        html! { (r.who) " may " (r.what) },
                        Some(html! { (code(&r.tool)) " · since " (crate::pages::short_date(r.created)) }),
                        // No second step: removing a rule only means Melissa asks again.
                        plain_action(&format!("/ui/settings/permissions/{}/remove", r.id), PERMISSIONS_ID, "Remove", false),
                        None,
                    ))
                }
            }))
        },
    )
}

/// No permission prompts on the Developer page: the switch with its warning, or the
/// confirmation before it goes on.
pub fn bypass_region(on: bool, confirming: bool, flash: Option<&Flash>) -> Markup {
    region(
        BYPASS_ID,
        setting_row_with(
            html! { "No permission prompts" },
            Some(html! { "Every request is allowed without asking you. Each one is logged." }),
            html! {
                @if !confirming {
                    (toggle(&Toggle {
                        action: "/ui/settings/developer/bypass",
                        target: BYPASS_ID,
                        label: "No permission prompts",
                        on,
                        locked: None,
                    }))
                }
            },
            // The warning only while it is on or about to be (2026-11-23).
            (on || confirming || flash.is_some()).then(|| html! {
                @if on || confirming {
                    (notice(
                        "Risky: helpers can change, stop and delete apps without your yes.",
                        Tone::Attention,
                    ))
                }
                @if let Some(f) = flash { (notice(&f.text, f.tone)) }
                @if confirming {
                    (confirm_card(&ConfirmCard {
                        title: "Turn off permission prompts?",
                        text: "Melissa and every helper act without asking you until you turn this off. A warning shows on every page.",
                        action: "/ui/settings/developer/bypass/on",
                        target: BYPASS_ID,
                        confirm: "Turn off prompts",
                        cancel_href: DEVELOPER_URL,
                    }))
                }
            }),
        ),
    )
}

/// Full access per agent on the Developer page (2026-11-20, owner request): the switch
/// that adds Access (Ask / Full access) to each chat's model picker. Off puts every agent
/// back on Ask.
pub fn full_access_region(on: bool, flash: Option<&Flash>) -> Markup {
    region(
        FULL_ACCESS_ID,
        setting_row_with(
            html! { "Full access per agent" },
            Some(html! { "Adds Access to each chat's model picker. Each use is logged." }),
            toggle(&Toggle {
                action: "/ui/settings/developer/full-access",
                target: FULL_ACCESS_ID,
                label: "Full access per agent",
                on,
                locked: None,
            }),
            flash.map(|f| notice(&f.text, f.tone)),
        ),
    )
}

// --- Schedules -----------------------------------------------------------------------------------

/// A schedule's last run, in words.
pub struct LastRunCell {
    /// "Done", "Skipped".
    pub words: String,
    pub tone: Tone,
    /// Local time, `2026-11-20 03:00`.
    pub at: String,
    /// Why, if there is more to say ("the agent was busy").
    pub detail: Option<String>,
}

/// One schedule in the list.
pub struct ScheduleRow {
    pub id: u64,
    pub name: String,
    /// "Melissa", "Ada".
    pub who: String,
    /// Their conversation.
    pub chat: String,
    /// "Every day at 03:00" (or the cron expression).
    pub when: String,
    /// Local time, or "Off".
    pub next: String,
    pub last: Option<LastRunCell>,
    pub enabled: bool,
}

/// The add/edit form's values.
pub struct ScheduleForm {
    /// Editing this schedule (None: adding one).
    pub id: Option<u64>,
    pub name: String,
    /// `lead` or an agent id.
    pub agent: String,
    /// (value, label): Melissa and the helpers.
    pub agents: Vec<(String, String)>,
    pub instruction: String,
    /// `hourly`, `daily`, `weekly` or `custom`.
    pub repeat: String,
    /// `0` (Sunday) … `6`.
    pub day: String,
    /// `HH:MM`.
    pub time: String,
    pub cron: String,
    pub max_minutes: String,
}

pub struct SchedulesView {
    pub rows: Vec<ScheduleRow>,
    pub form: ScheduleForm,
    pub flash: Option<Flash>,
    /// False: this device runs no scheduler (nothing to show but a note).
    pub available: bool,
    /// The box's time zone, e.g. `Europe/Athens`.
    pub zone: String,
    /// Minutes a scheduled run's request waits for the owner.
    pub wait: String,
    /// When some agents stop waiting sooner (their backend's own limit), saying so.
    pub wait_note: Option<String>,
    /// The add/edit form shows (Add schedule, Edit, or a save that was refused).
    pub open: bool,
}

pub fn schedules_page(f: &Frame<'_>, v: &SchedulesView) -> Markup {
    let head = SettingsHead {
        lead: None,
        tools: Some(schedules_tools(v)),
    };
    frame(f, "Schedules", head, schedules_body(v))
}

/// The head bar's tool: Add schedule, while the form is closed.
fn schedules_tools(v: &SchedulesView) -> Markup {
    html! {
        @if v.available && !v.open {
            (link_button(Btn::secondary().sm(), &format!("{SCHEDULES_URL}?add=1"), "Add schedule"))
        }
    }
}

/// The region for htmx answers: the section and, out of band, the head bar's tool.
pub fn schedules_region(v: &SchedulesView) -> Markup {
    html! {
        (schedules_body(v))
        (settings_tools_oob(schedules_tools(v)))
    }
}

/// The whole section (every action swaps it): the add/edit form when open (owner decision 3,
/// 2026-11-23: behind "Add schedule" or a row's Edit), the schedules as rows, the wait.
fn schedules_body(v: &SchedulesView) -> Markup {
    region(
        SCHEDULES_ID,
        html! {
            @if let Some(f) = &v.flash { (notice(&f.text, f.tone)) }
            @if v.available {
                @if v.open {
                    (setting_card(None, schedule_form(&v.form, &v.zone)))
                }
                (setting_card(None, html! {
                    @if v.rows.is_empty() {
                        (setting_row("Nothing scheduled yet", Some("Jobs your helpers do on their own, on a timetable."), html! {}))
                    }
                    @for r in &v.rows {
                        (schedule_row(r))
                    }
                }))
                (setting_card(None, setting_row_with(
                    html! { "When a run needs your OK" },
                    Some(html! { "Minutes to wait, then the run goes on without it." }),
                    inline_form("/ui/settings/schedules/wait", SCHEDULES_ID, inline_number(
                        "minutes",
                        "Wait for your answer (minutes)",
                        &v.wait,
                        1,
                        1440,
                    )),
                    v.wait_note.as_deref().map(|n| notice(n, Tone::Neutral)),
                )))
            } @else {
                (notice("Schedules don't run on this device.", Tone::Neutral))
            }
        },
    )
}

/// One schedule: its name, who and when (and how the last run went), its switch and Edit.
fn schedule_row(r: &ScheduleRow) -> Markup {
    let base = format!("/ui/settings/schedules/{}", r.id);
    setting_row_with(
        html! { (r.name) },
        Some(html! {
            (link(&r.chat, &r.who)) " · " (r.when) " · Next: " (r.next)
            @if let Some(l) = &r.last {
                " · Last: " (l.at) @if let Some(d) = &l.detail { " (" (d) ")" }
            }
        }),
        html! {
            @if let Some(l) = &r.last { (badge(&l.words, l.tone)) }
            (toggle(&Toggle {
                action: &format!("{base}/enabled"),
                target: SCHEDULES_ID,
                label: &format!("Run {} on its schedule", r.name),
                on: r.enabled,
                locked: None,
            }))
            (link_button(Btn::ghost().sm(), &format!("{SCHEDULES_URL}?edit={}", r.id), "Edit"))
        },
        None,
    )
}

fn schedule_form(f: &ScheduleForm, zone: &str) -> Markup {
    let agents: Vec<Choice<'_>> = f
        .agents
        .iter()
        .map(|(value, label)| Choice {
            value,
            label,
            enabled: true,
        })
        .collect();
    let repeats = [
        ("hourly", "Every hour"),
        ("daily", "Every day"),
        ("weekly", "Every week"),
        ("custom", "Custom (cron)"),
    ]
    .map(|(value, label)| Choice {
        value,
        label,
        enabled: true,
    });
    let days: Vec<(String, &str)> = melissa_agent::lead::schedules::DAYS
        .iter()
        .enumerate()
        .map(|(i, d)| (i.to_string(), *d))
        .collect();
    let days: Vec<Choice<'_>> = days
        .iter()
        .map(|(value, label)| Choice {
            value,
            label,
            enabled: true,
        })
        .collect();
    let (title, action, submit) = match f.id {
        Some(id) => (
            format!("Edit \u{201c}{}\u{201d}", f.name),
            format!("/ui/settings/schedules/{id}"),
            "Save schedule",
        ),
        None => (
            "New schedule".to_string(),
            "/ui/settings/schedules".to_string(),
            "Add schedule",
        ),
    };
    let cron_hint = format!("minute hour day month weekday, {zone} time. Used with Custom.");
    setting_row_full(
        Some(&title),
        html! {
            (settings_form(&action, SCHEDULES_ID, submit, Some(SCHEDULES_URL), html! {
                (text_field("name", "Name", &f.name, "Nightly updates", "text", None))
                (select_field_at("agent", "Who", &agents, Some(&f.agent)))
                (lines_field("instruction", "What to do", &f.instruction, Some("Sent as a message on every run.")))
                div.field-row {
                    (select_field_at("repeat", "Repeats", &repeats, Some(&f.repeat)))
                    (text_field("time", "Time", &f.time, "03:00", "time", None))
                }
                // Shown for "Every week" only (CSS).
                div.field-day { (select_field_at("day", "Day", &days, Some(&f.day))) }
                (disclosure("Advanced", f.repeat == "custom", html! {
                    (text_field("cron", "Cron expression", &f.cron, "0 3 * * *", "text", Some(&cron_hint)))
                    (text_field("max_minutes", "Time limit (minutes)", &f.max_minutes, "60", "number", Some("A run still going after this is stopped.")))
                }))
            }))
            @if let Some(id) = f.id {
                @let base = format!("/ui/settings/schedules/{id}");
                (actions(html! {
                    (plain_action(&format!("{base}/run"), SCHEDULES_ID, "Run now", false))
                    (action_button(
                        Btn::danger_quiet(),
                        &format!("{base}/remove"),
                        SCHEDULES_ID,
                        "Remove schedule",
                        &format!("Remove \u{201c}{}\u{201d}? It won't run again.", f.name),
                    ))
                }))
            }
        },
    )
}

/// One agent on the Agents page. `id`: None for Melissa (the lead agent: no Delete).
pub struct AgentRow {
    pub id: Option<String>,
    pub name: String,
    /// Its page in Settings → Agents (Melissa: her chat).
    pub href: String,
    /// "Claude · Opus 5.5 · High".
    pub runs_on: String,
    pub status: (&'static str, Tone),
}

pub fn agents_page(f: &Frame<'_>, rows: &[AgentRow], flash: Option<&Flash>) -> Markup {
    let head = SettingsHead {
        lead: Some("Melissa and her helpers. Change what one runs on under its chat."),
        tools: Some(link_button(
            Btn::secondary().sm(),
            NEW_AGENT_URL,
            "New helper",
        )),
    };
    frame(f, "Agents", head, agents_region(rows, flash))
}

/// The agent list (swapped whole by Delete).
pub fn agents_region(rows: &[AgentRow], flash: Option<&Flash>) -> Markup {
    region(
        AGENTS_ID,
        html! {
            @if let Some(f) = flash { (notice(&f.text, f.tone)) }
            (setting_card(None, html! {
                @for r in rows {
                    (setting_row_with(
                        link(&r.href, &r.name),
                        Some(html! { (r.runs_on) }),
                        html! {
                            (badge(r.status.0, r.status.1))
                            @match &r.id {
                                None => span.quiet { "Lead agent" },
                                Some(id) => (action_button(
                                    Btn::danger_quiet(),
                                    &format!("/ui/settings/agents/{id}/delete"),
                                    AGENTS_ID,
                                    "Delete",
                                    &format!(
                                        "Delete {}? Its chat, file box and schedules go with it. This can't be undone.",
                                        r.name
                                    ),
                                )),
                            }
                        },
                        None,
                    ))
                }
            }))
        },
    )
}

/// Where the new-helper form is (the sidebar's "+").
pub const NEW_AGENT_URL: &str = "/settings/agents/new";
/// DOM id of a helper's settings (its forms swap it).
pub const AGENT_ID: &str = "agent-settings";

/// A helper's page in Settings → Agents (its menu's Rename, Change icon, Agent settings and
/// Delete lead here).
pub struct AgentPage<'a> {
    /// The helper's id, or `lead` for the lead agent (its page has its name only).
    pub id: &'a str,
    pub lead: bool,
    pub name: &'a str,
    pub shape: &'a str,
    pub color: &'a str,
    pub shapes: &'a [&'a str],
    pub colors: &'a [&'a str],
    /// "Claude · Opus 5.5 · High".
    pub runs_on: &'a str,
    pub status: (&'static str, Tone),
    /// "By Melissa, 20 Nov".
    pub made: &'a str,
    pub pinned: bool,
    /// Why it can't be deleted now ("Looks after Minecraft. …"), if so.
    pub kept: Option<&'a str>,
    pub flash: Option<&'a Flash>,
    /// Show `flash` in the Icon section (its form's answer) instead of at the top.
    pub flash_at_icon: bool,
}

pub fn agent_settings_page(f: &Frame<'_>, a: &AgentPage<'_>) -> Markup {
    frame_at(f, "Agents", Some(a.name), agent_settings_region(a))
}

/// A helper's settings in one card (owner decision 4, 2026-11-23): Name, Icon, Pinned, Fresh
/// start, Delete helper, and when it was made below. Swapped whole by its forms; the lead
/// agent's has its name only.
pub fn agent_settings_region(a: &AgentPage<'_>) -> Markup {
    let base = format!("/ui/settings/agents/{}", a.id);
    let (chat, face) = if a.lead {
        (crate::pages::MELISSA_CHAT_URL.to_string(), Avatar::Melissa)
    } else {
        (
            crate::pages::agent_href(a.id),
            Avatar::Agent {
                shape: a.shape,
                color: a.color,
            },
        )
    };
    let delete_ask = format!(
        "Delete {}? Its chat, file box and schedules go with it. This can't be undone.",
        a.name
    );
    region(
        AGENT_ID,
        html! {
            @if let Some(f) = a.flash.filter(|_| !a.flash_at_icon) { (notice(&f.text, f.tone)) }
            (setting_card(None, html! {
                (region("agent-name", setting_row(
                    "Name",
                    None,
                    inline_text_form(&format!("{base}/rename"), AGENT_ID, "name", "Name", a.name, 40),
                )))
                @if !a.lead {
                    (setting_row_details(
                        "Icon",
                        None,
                        Some(avatar(&face, "av")),
                        "Change",
                        a.flash_at_icon,
                        html! {
                            // The icon form's answer stays by the form.
                            @if let Some(f) = a.flash.filter(|_| a.flash_at_icon) { (notice(&f.text, f.tone)) }
                            (settings_form(&format!("{base}/icon"), AGENT_ID, "Save icon", None,
                                icon_picker(a.shapes, a.colors, a.shape, a.color)))
                        },
                    ))
                    (setting_row(
                        "Pinned",
                        Some("Pinned helpers stay at the top of the sidebar."),
                        toggle(&Toggle {
                            action: &format!("{base}/pin"),
                            target: AGENT_ID,
                            label: "Pinned",
                            on: a.pinned,
                            locked: None,
                        }),
                    ))
                    (setting_row(
                        "Fresh start",
                        Some("Clears its memory. The chat stays."),
                        action_button(
                            Btn::secondary(),
                            &format!("{base}/fresh-start"),
                            AGENT_ID,
                            "Clear memory",
                            &format!("Clear {}'s memory? It can't be brought back.", a.name),
                        ),
                    ))
                    (region("agent-delete", match a.kept {
                        Some(why) => setting_row("Delete helper", Some(why), html! {}),
                        None => setting_row(
                            "Delete helper",
                            Some("Its chat, file box and schedules go too."),
                            action_button(Btn::danger_quiet(), &format!("{base}/delete"), AGENT_ID, "Delete helper", &delete_ask),
                        ),
                    }))
                }
            }))
            p.settings-foot {
                @if !a.lead { (a.made) " · " }
                (a.runs_on) " · " (a.status.0) " · " (link(&chat, "Open chat"))
            }
        },
    )
}

/// The new-helper form (the sidebar's "+"): a name and an icon. `name`: what was typed (kept
/// when it was refused).
pub fn new_agent_page(
    f: &Frame<'_>,
    shapes: &[&str],
    colors: &[&str],
    name: &str,
    icon: (&str, &str),
    flash: Option<&Flash>,
) -> Markup {
    let head = SettingsHead {
        lead: Some("A helper works on its own, in its own sandbox."),
        tools: None,
    };
    frame_head(
        f,
        "Agents",
        Some("New helper"),
        head,
        new_agent_region(shapes, colors, name, icon, flash),
    )
}

/// DOM id of the new-helper form.
pub const NEW_AGENT_ID: &str = "new-agent";

pub fn new_agent_region(
    shapes: &[&str],
    colors: &[&str],
    name: &str,
    icon: (&str, &str),
    flash: Option<&Flash>,
) -> Markup {
    region(
        NEW_AGENT_ID,
        html! {
            @if let Some(f) = flash { (notice(&f.text, f.tone)) }
            (setting_card(None, setting_row_full(None, html! {
                form.stack.settings-form method="post" action="/ui/settings/agents/new"
                    hx-post="/ui/settings/agents/new" hx-target={ "#" (NEW_AGENT_ID) } hx-swap="outerHTML" {
                    (crate::components::csrf_field())
                    label.field {
                        span { "Name" }
                        input.input type="text" name="name" value=(name) required maxlength="40"
                            autocomplete="off" spellcheck="false" autofocus;
                    }
                    (icon_picker(shapes, colors, icon.0, icon.1))
                    div { button.(Btn::primary()) type="submit" { "Create helper" } }
                }
            })))
        },
    )
}

/// What the Files page shows.
pub struct FilesView {
    /// (value, label) choices and the chosen one.
    pub choices: Vec<(String, String)>,
    pub selected: String,
    /// Boxes with their own setting: (who, their chat, how long).
    pub overrides: Vec<(String, String, String)>,
    pub flash: Option<Flash>,
}

pub fn files_page(f: &Frame<'_>, v: &FilesView) -> Markup {
    let head = SettingsHead {
        lead: Some("Each helper's file box: the files people share in its chat."),
        tools: None,
    };
    frame(f, "Files", head, files_region(v))
}

pub fn files_region(v: &FilesView) -> Markup {
    region(
        FILES_ID,
        html! {
            @if let Some(f) = &v.flash { (notice(&f.text, f.tone)) }
            (setting_card(None, setting_row(
                "Keep files for",
                Some("Older files leave the chat. Messages show them as expired."),
                inline_form("/ui/settings/files", FILES_ID, inline_select("keep", "Keep files for", &v.choices, &v.selected)),
            )))
            // Boxes with their own setting: a card only when there are some (2026-11-23).
            @if v.overrides.is_empty() {
                p.settings-foot { "No file box has its own setting." }
            } @else {
                (setting_card(Some("Boxes with their own setting"), html! {
                    @for (who, chat, keep) in &v.overrides {
                        (setting_row_with(link(chat, who), None, html! { (keep) }, None))
                    }
                }))
                p.settings-foot { "Change one in the Files panel next to its chat." }
            }
        },
    )
}

/// What the Apps page shows.
pub struct AppsView {
    /// (value, label) choices for "Apps open to".
    pub choices: Vec<(String, String)>,
    /// `None`: Melissa doesn't filter app ports on this install.
    pub state: Option<AppsState>,
    pub flash: Option<Flash>,
}

/// The setting and whether it is in force.
pub struct AppsState {
    pub selected: String,
    /// Who can open apps, as a sentence.
    pub sentence: &'static str,
    /// Why the setting isn't what is in force.
    pub problem: Option<String>,
}

pub fn apps_page(f: &Frame<'_>, v: &AppsView) -> Markup {
    let head = SettingsHead {
        lead: Some("Helpers ask you before they open apps to more devices."),
        tools: None,
    };
    frame(f, "Apps", head, apps_region(v))
}

pub fn apps_region(v: &AppsView) -> Markup {
    region(
        APPS_ID,
        html! {
            @if let Some(f) = &v.flash { (notice(&f.text, f.tone)) }
            @match &v.state {
                Some(s) => {
                    @if let (Some(p), None) = (&s.problem, &v.flash) { (notice(p, Tone::Caution)) }
                    (setting_card(None, setting_row(
                        "Apps open to",
                        Some(s.sentence),
                        inline_form("/ui/settings/apps", APPS_ID, inline_select("open_to", "Apps open to", &v.choices, &s.selected)),
                    )))
                }
                None => {
                    (setting_card(None, setting_row("Apps open to", Some("Set by this device's firewall."), html! {})))
                }
            }
        },
    )
}

/// What the Tailscale page shows.
pub struct TailscaleView {
    /// The state in words (an error is one line).
    pub sentence: String,
    pub enable: Option<&'static str>,
    pub sign_in: bool,
    pub disable: bool,
    /// The sign-in page to open (only in the answer to Sign in; never in the page's HTML).
    pub login_url: Option<String>,
    /// Refresh the region from this URL every few seconds (starting, waiting for a sign-in).
    pub poll: Option<&'static str>,
    pub flash: Option<Flash>,
    /// In first-run setup: a switch instead of Enable/Disable, no heading, no filled button.
    pub compact: bool,
}

pub fn tailscale_page(f: &Frame<'_>, v: &TailscaleView) -> Markup {
    let head = SettingsHead {
        lead: Some("Puts this device on your tailnet. Off until you enable it."),
        tools: None,
    };
    frame(f, "Tailscale", head, tailscale_region(v))
}

pub fn tailscale_region(v: &TailscaleView) -> Markup {
    const DISABLE_ASK: &str =
        "Disable Tailscale? This device leaves your tailnet, and tailnet devices can't open its apps.";
    let controls = html! {
        @if let Some(url) = &v.login_url {
            (external_button(url, "Open Tailscale sign-in"))
        } @else if v.sign_in {
            (plain_action("/ui/settings/tailscale/sign-in", TAILSCALE_ID, "Sign in", true))
        }
        @if let Some(label) = v.enable {
            (plain_action("/ui/settings/tailscale/enable", TAILSCALE_ID, label, true))
        }
        @if v.disable {
            (action_button(Btn::secondary(), "/ui/settings/tailscale/disable", TAILSCALE_ID, "Disable", DISABLE_ASK))
        }
    };
    if v.compact {
        return tailscale_switch(v);
    }
    live_region(
        TAILSCALE_ID,
        v.poll.unwrap_or(""),
        3,
        v.poll.is_some(),
        html! {
            @if let Some(f) = &v.flash { (notice(&f.text, f.tone)) }
            (setting_card(None, setting_row("Tailscale", Some(&v.sentence), actions(controls))))
        },
    )
}

/// What the Reset page shows.
pub struct ResetView {
    /// This install can reset (the container's root unit is there).
    pub available: bool,
    /// A reset runs: the page waits for Melissa to come back.
    pub resetting: bool,
    /// Why it can't run now (an update waits to install).
    pub blocked: Option<&'static str>,
    /// The word to type.
    pub word: String,
    /// The last try was refused.
    pub error: Option<String>,
}

pub fn reset_page(f: &Frame<'_>, v: &ResetView) -> Markup {
    frame(f, "Reset", SettingsHead::default(), reset_region(v))
}

pub fn reset_region(v: &ResetView) -> Markup {
    const LABEL: &str = "Reset Melissa";
    region(
        RESET_ID,
        html! {
            @if v.resetting {
                (away_card(
                    "Resetting Melissa…",
                    "This takes a few minutes. The setup opens here when she is back.",
                    RESET_STATUS_URL,
                    ("/claim", "Open the setup"),
                ))
            } @else if !v.available {
                (setting_card(None, setting_row(LABEL, Some("Not available on this install."), html! {})))
            } @else if let Some(b) = v.blocked {
                (setting_card(None, setting_row(LABEL, Some(b), html! {})))
            } @else {
                (setting_card(None, setting_row_with(
                    html! { (LABEL) },
                    Some(html! { "Erases her data on this device. This can't be undone." }),
                    html! {},
                    Some(danger_confirm(&DangerConfirm {
                        label: LABEL,
                        erased: melissa_settings::factory_reset::ERASED,
                        kept: melissa_settings::factory_reset::KEPT,
                        word: &v.word,
                        action: "/ui/settings/reset",
                        target: RESET_ID,
                        error: v.error.as_deref(),
                    })),
                )))
            }
        },
    )
}

/// Tailscale as one switch (first-run setup): on is Enable, off is Disable; a sign-in it
/// needs shows below the row. Not managed by Melissa: the sentence only.
fn tailscale_switch(v: &TailscaleView) -> Markup {
    let managed = v.enable.is_some() || v.disable || v.poll.is_some();
    let on = v.disable || (v.poll.is_some() && v.enable.is_none());
    let locked = (v.poll.is_some() && !v.disable).then_some("Starting…");
    let control = if managed {
        toggle(&Toggle {
            action: "/ui/settings/tailscale/switch",
            target: TAILSCALE_ID,
            label: "Tailscale",
            on,
            locked,
        })
    } else {
        html! {}
    };
    let extra = html! {
        @if let Some(url) = &v.login_url {
            (actions(external_button(url, "Open Tailscale sign-in")))
        } @else if v.sign_in {
            (actions(plain_action("/ui/settings/tailscale/sign-in", TAILSCALE_ID, "Sign in", false)))
        }
        @if let Some(f) = &v.flash { (notice(&f.text, f.tone)) }
    };
    live_region(
        TAILSCALE_ID,
        v.poll.unwrap_or(""),
        3,
        v.poll.is_some(),
        setting_card(
            None,
            setting_row_with(
                html! { "Tailscale" },
                Some(html! { (v.sentence) }),
                control,
                (!extra.0.is_empty()).then_some(extra),
            ),
        ),
    )
}

/// What the Voice page shows.
pub struct VoiceView {
    pub on: bool,
    /// whisper.cpp is installed and the model can be fetched (or is here).
    pub available: bool,
    pub model: VoiceModel,
    /// The model's size, e.g. "148 MB".
    pub size: String,
    pub error: Option<String>,
}

/// Where the speech model is.
pub enum VoiceModel {
    Absent,
    Downloading { done: u64, total: u64 },
    Ready,
    Failed(String),
}

impl VoiceModel {
    fn downloading(&self) -> bool {
        matches!(self, Self::Downloading { .. })
    }
}

pub fn voice_page(f: &Frame<'_>, v: &VoiceView) -> Markup {
    let head = SettingsHead {
        lead: Some("Speak instead of typing. Speech becomes text on this device."),
        tools: None,
    };
    frame(f, "Voice", head, voice_region(v))
}

pub fn voice_region(v: &VoiceView) -> Markup {
    let label = "Voice input";
    let locked = (!v.available).then_some("Not available on this install");
    // The state in one line under the title; progress, retries and errors below the row.
    let (desc, extra): (Option<String>, Markup) = if !v.available {
        (Some("This install has no speech engine.".into()), html! {})
    } else {
        match (&v.model, v.on) {
            (VoiceModel::Downloading { done, total }, _) => (
                None,
                progress_bar(
                    "Downloading the speech model",
                    &format!("{} of {}", mb(*done), mb(*total)),
                    *done,
                    *total,
                ),
            ),
            (VoiceModel::Ready, true) => (
                Some("Ready. Use the microphone next to Send.".into()),
                html! {},
            ),
            (VoiceModel::Ready, false) => (
                Some(format!("The speech model is still here ({}).", v.size)),
                actions(plain_action(
                    "/ui/settings/voice/delete",
                    VOICE_ID,
                    "Delete it",
                    false,
                )),
            ),
            (VoiceModel::Failed(why), true) => (
                None,
                html! {
                    (notice(why, Tone::Caution))
                    (actions(plain_action("/ui/settings/voice/retry", VOICE_ID, "Try again", true)))
                },
            ),
            (VoiceModel::Absent, true) => (
                None,
                actions(plain_action(
                    "/ui/settings/voice/retry",
                    VOICE_ID,
                    "Download the speech model",
                    true,
                )),
            ),
            (_, false) => (
                Some(format!(
                    "Turning it on downloads a {} speech model once.",
                    v.size
                )),
                html! {},
            ),
        }
    };
    let extra = html! {
        (extra)
        @if let Some(e) = &v.error { (notice(e, Tone::Caution)) }
    };
    let body = setting_card(
        None,
        setting_row_with(
            html! { (label) },
            desc.map(|d| html! { (d) }),
            toggle(&Toggle {
                action: "/ui/settings/voice",
                target: VOICE_ID,
                label,
                on: v.on && v.available,
                locked,
            }),
            (!extra.0.is_empty()).then_some(extra),
        ),
    );
    live_region(
        VOICE_ID,
        "/ui/settings/voice/poll",
        1,
        v.model.downloading(),
        body,
    )
}

/// Bytes as whole megabytes (10^6, like download sizes elsewhere).
pub fn mb(bytes: u64) -> String {
    format!("{} MB", bytes.div_ceil(1_000_000))
}

// --- About ---------------------------------------------------------------------------------------

/// DOM id of the About region.
pub const ABOUT_ID: &str = "about";
/// Melissa's public pages (fixed; they open in a new tab).
const SOURCE_URL: &str = "https://github.com/svin24/melissa";
const ISSUES_URL: &str = "https://github.com/svin24/melissa/issues";
const SPONSOR_URL: &str = "https://github.com/sponsors/svin24";

/// Something built into Melissa's pages: what it is, its license, and the license's text where
/// one ships with it.
pub struct Notice {
    pub name: &'static str,
    pub license: &'static str,
    pub text: Option<&'static str>,
}

/// What the About page shows. The texts are built into the binary.
pub struct AboutView {
    /// The same values as Settings → Updates.
    pub version: String,
    pub revision: Option<String>,
    /// Melissa's LICENSE.
    pub license: &'static str,
    /// ACKNOWLEDGMENTS.md, whole.
    pub credits: &'static str,
    pub notices: &'static [Notice],
    /// The image ships a license report for the Rust crates ([`RUST_LICENSES_URL`]).
    pub rust_report: bool,
}

pub fn about_page(f: &Frame<'_>, v: &AboutView) -> Markup {
    frame(f, "About", SettingsHead::default(), about_region(v))
}

pub fn about_region(v: &AboutView) -> Markup {
    let detail = match &v.revision {
        Some(r) => format!("Version {} · Revision {r}", v.version),
        None => format!("Version {}", v.version),
    };
    let notices = html! {
        @for n in v.notices {
            p { strong { (n.name) } " · " (n.license) }
            @if let Some(t) = n.text { (license_text(&format!("{} license", n.name), t)) }
        }
    };
    region(
        ABOUT_ID,
        html! {
            // One card for Melissa and her links, one for the licenses (2026-11-23). The
            // pre-alpha warning is the banner's.
            (setting_card(None, html! {
                (about_head("Melissa", &detail))
                (setting_row(
                    "Links",
                    Some("Open in a new tab."),
                    html! {
                        (new_tab_button(Btn::secondary().sm(), SOURCE_URL, "Source code", None))
                        (new_tab_button(Btn::secondary().sm(), ISSUES_URL, "Report a problem", None))
                        (new_tab_button(Btn::secondary().sm(), SPONSOR_URL, "Sponsor", Some(Glyph::Heart)))
                    },
                ))
            }))
            (setting_card(Some("Licenses"), html! {
                (setting_row_details(
                    "Melissa\u{2019}s license",
                    Some("MIT License"),
                    None,
                    "Show",
                    false,
                    license_text("Melissa's license", v.license),
                ))
                (setting_row_details(
                    "Fonts, scripts and logos",
                    Some("Built into Melissa\u{2019}s pages."),
                    None,
                    "Show",
                    false,
                    notices,
                ))
                (setting_row_details(
                    "Credits",
                    Some("Projects Melissa learned from or includes."),
                    None,
                    "Show",
                    false,
                    html! {
                        p.visually-hidden { "Links open in a new tab." }
                        (markdown_document(v.credits))
                    },
                ))
                @if v.rust_report {
                    (setting_row_with(
                        html! { "Rust crates" },
                        Some(html! { "Their licenses, as shipped in this image." }),
                        new_tab_button(Btn::secondary().sm(), RUST_LICENSES_URL, "Open list", None),
                        None,
                    ))
                }
            }))
        },
    )
}
