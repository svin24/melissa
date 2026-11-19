//! The Settings pages, assembled only from `components` (no markup of their own beyond
//! layout wrappers). The view structs are filled by `providers.rs` and `updates.rs`.

use maud::{html, Markup};

use crate::{
    components::{
        actions, app_page, badge, card, code, command_block, confirm_card, detail_head, facts,
        group, lines_field, link, live_region, log_view, notice, pick_list, plain_action,
        provider_mark, settings_form, settings_frame, settings_head, split_card, text_field,
        toggle, var_list, ConfirmCard, LogLine, Nav, PickRow, SettingsSection, Toggle, Tone,
        VarRow,
    },
    pages::{rail_for, Frame},
};
use melissa_settings::{install::InstallState, providers::EnvVarView};

/// The Settings sections, in order. Add new ones here (and a route in `mod.rs`).
pub const SECTIONS: [(&str, &str); 2] = [
    ("/settings/providers", "Providers"),
    ("/settings/updates", "Updates"),
];

/// DOM id of the Providers region (forms swap it).
pub const PROVIDERS_ID: &str = "providers";
/// DOM id of the Updates region.
pub const UPDATES_ID: &str = "updates";

fn frame(f: &Frame<'_>, current: &str, content: Markup) -> Markup {
    let sections: Vec<SettingsSection<'_>> = SECTIONS
        .iter()
        .map(|(href, label)| SettingsSection {
            href,
            label,
            current: *label == current,
        })
        .collect();
    app_page(
        &format!("{current} · Settings"),
        rail_for(Nav::Settings, f, None),
        settings_frame(&sections, current, content),
    )
}

/// A message after an action.
pub struct Flash {
    pub text: String,
    pub tone: Tone,
}

/// Whether saved settings wait for a restart, and how to do it.
pub struct RestartHint {
    /// Why a restart is needed.
    pub text: &'static str,
    /// The UI can restart Melissa (root helper installed).
    pub button: bool,
    /// Else: what to type.
    pub command: String,
}

// --- Providers ----------------------------------------------------------------------------------

/// One way Melissa runs a provider (a configured backend).
pub struct BackendLine {
    pub name: String,
    /// "chat session" or "one-shot task".
    pub kind: &'static str,
    /// "ready", "not logged in", "left out (disabled)", ...
    pub state: String,
}

/// One provider as the Settings page shows it. All CLI-sourced text is already cleaned;
/// components escape it.
pub struct ProviderCard {
    pub id: String,
    pub label: String,
    pub version: Option<String>,
    /// The saved setting.
    pub enabled: bool,
    pub status: String,
    pub tone: Tone,
    /// Where the program is ("not found" if it isn't).
    pub program: Option<String>,
    pub used_by: Vec<String>,
    /// Why it can't be disabled now.
    pub lock: Option<String>,
    pub backends: Vec<BackendLine>,
    pub models: Vec<String>,
    /// Saved overrides (empty: none).
    pub display_name: String,
    pub command: String,
    pub default_command: String,
    pub args: Vec<String>,
    pub env: Vec<EnvVarView>,
    /// How to install it, while it isn't installed.
    pub install: Option<InstallView>,
    /// How to sign in, once it is installed and not signed in.
    pub sign_in: Option<SignIn>,
    /// Example variable name for the Environment form.
    pub var_example: &'static str,
}

/// The Install section of a provider that isn't installed.
pub enum InstallView {
    /// The Install button and the install's progress (the install unit is there).
    Button {
        vendor: &'static str,
        state: InstallState,
        detail: Option<String>,
        log: Vec<String>,
    },
    /// A Nix install: the agent comes from the Nix configuration.
    Nix,
    /// No install mechanism here: a note and maybe a command to run on the host.
    Manual {
        text: String,
        command: Option<String>,
    },
}

/// How to sign in to an installed agent: guidance, and a command for the device if there is one.
pub struct SignIn {
    pub text: &'static str,
    pub command: Option<String>,
    /// A device-code sign-in from the page (Grok, where the login unit is there).
    pub device: Option<DeviceLogin>,
}

/// A device-code sign-in: its state and, once printed, the vendor's URL and code.
pub struct DeviceLogin {
    pub state: InstallState,
    pub url: Option<String>,
    pub code: Option<String>,
    pub detail: Option<String>,
    pub log: Vec<String>,
}

pub struct ProvidersView {
    pub cards: Vec<ProviderCard>,
    pub selected: usize,
    /// "Checked just now".
    pub checked: String,
    pub restart: Option<RestartHint>,
    pub flash: Option<Flash>,
    /// The agents' home directory (logins, config), e.g. `/var/lib/melissa/home`.
    pub home: Option<String>,
    /// The settings file.
    pub file: Option<String>,
    /// An install is running: the region refreshes itself from this URL.
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
    let footer = html! {
        p.quiet {
            "Add a provider with a " (code("[[backend]]")) " entry in "
            (code("queen.toml")) ", then restart Melissa."
        }
    };
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
                "The agent programs Melissa can run for her and her helpers.",
                &v.checked,
                "/ui/settings/providers/refresh",
                PROVIDERS_ID,
                "Check again",
            ))
            @if let Some(fl) = &v.flash { (notice(&fl.text, fl.tone)) }
            @if let Some(r) = &v.restart { (restart_note(r, PROVIDERS_ID)) }
            (split_card("provider-card", "Providers", pick_list("Providers", &rows, Some(footer)), detail))
        },
    )
}

fn restart_note(r: &RestartHint, target: &str) -> Markup {
    html! {
        (notice(r.text, Tone::Info))
        @if r.button {
            (actions(plain_action("/ui/settings/restart", target, "Restart Melissa now", false)))
        } @else {
            (command_block(&r.command))
        }
    }
}

fn provider_detail(c: &ProviderCard, v: &ProvidersView) -> Markup {
    let url = provider_url(&c.id);
    let badges = html! {
        @if c.enabled { (badge("Enabled", Tone::Ok)) } @else { (badge("Disabled", Tone::Muted)) }
    };
    let sub = match &c.version {
        Some(ver) => format!("{} · version {ver}", c.id),
        None => c.id.clone(),
    };
    let used = if c.used_by.is_empty() {
        html! { span.quiet { "Nothing right now" } }
    } else {
        html! { (c.used_by.join(", ")) }
    };
    let ways = html! {
        ul.plain-list {
            @for b in &c.backends {
                li { (code(&b.name)) " · " (b.kind) " · " span.quiet { (b.state) } }
            }
        }
    };
    let models = if c.models.is_empty() {
        html! { span.quiet { "Not known yet: Melissa learns them when a chat first opens on it." } }
    } else {
        html! { span.badges { @for m in &c.models { (badge(m, Tone::Muted)) " " } } }
    };
    let mut rows = vec![
        (
            "Status",
            html! { span class=(tone_class(c.tone)) { (c.status) } },
        ),
        ("Used by", used),
        ("Runs as", ways),
        ("Models", models),
    ];
    if let Some(lock) = &c.lock {
        rows.push(("Can't disable", html! { (lock) }));
    }
    let remove = format!("{url}/env/remove");
    let var_rows: Vec<VarRow<'_>> = c
        .env
        .iter()
        .map(|e| VarRow {
            name: &e.name,
            value: e.value.as_deref(),
            remove_action: &remove,
        })
        .collect();
    let home = v
        .home
        .as_deref()
        .unwrap_or("the home folder of Melissa's user");
    html! {
        (detail_head(provider_mark(&letter(&c.label), !c.enabled), &c.label, &sub, badges))
        (facts(&rows))
        @if let Some(i) = &c.install { (install_section(c, i, &url)) }
        @if let Some(si) = &c.sign_in {
            (group("Sign in", Some(si.text), html! {
                @if let Some(d) = &si.device { (device_login(d, &url)) }
                @if let Some(cmd) = &si.command {
                    p.quiet { "Or sign in with your account on the device (run on the host):" }
                    (command_block(cmd))
                }
            }))
        }
        (settings_form(&url, PROVIDERS_ID, "Save", html! {
            (group("Name", None, text_field("display_name", "Display name", &c.display_name, melissa_settings::providers::default_label(&c.id), "text", None)))
            (group("Runtime", Some("Used after Melissa restarts."), html! {
                (text_field("command", "Program path", &c.command, &c.default_command, "text",
                    Some("Leave empty for the installed one. The file must be named like the provider.")))
                (facts(&[("Program found", html! { (c.program.as_deref().unwrap_or("not found")) }),
                         ("Home and config folder", html! { (code(home)) })]))
                (lines_field("args", "Extra launch arguments", &c.args.join("\n"),
                    Some("One per line. They come before Melissa's own; options that change permissions or tools are refused.")))
            }))
        }))
        (group("Environment", Some("Variables set for this agent inside its sandbox, such as an API key or a base URL. Values of names with KEY, TOKEN, SECRET or PASSWORD are never shown again."), html! {
            (var_list(&var_rows, PROVIDERS_ID, "No variables set."))
            (settings_form(&format!("{url}/env"), PROVIDERS_ID, "Add variable", html! {
                div.field-row {
                    (text_field("name", "Name", "", c.var_example, "text", None))
                    (text_field("value", "Value", "", "", "password", None))
                }
            }))
        }))
        @if let Some(file) = &v.file {
            p.quiet.small { "Saved in " (code(file)) ", over " (code("queen.toml")) "." }
        }
    }
}

fn device_login(d: &DeviceLogin, url: &str) -> Markup {
    let action = format!("{url}/login");
    let lines = |log: &[String]| {
        log_view(log.iter().map(|l| LogLine {
            text: l,
            is_error: l.starts_with("melissa-setup:"),
        }))
    };
    match d.state {
        InstallState::Idle => actions(plain_action(
            &action,
            PROVIDERS_ID,
            "Sign in with a code",
            true,
        )),
        InstallState::Running => match (&d.url, &d.code) {
            (Some(u), Some(c)) => html! {
                (facts(&[("Open", link(u, u)), ("Code", code(c))]))
                (notice("Waiting for you to confirm the code. This page updates by itself.", Tone::Info))
            },
            _ => html! {
                (notice("Starting the sign-in…", Tone::Info))
                @if !d.log.is_empty() { (lines(&d.log)) }
            },
        },
        InstallState::Done => notice("Signed in.", Tone::Ok),
        InstallState::Failed => html! {
            (notice(&match &d.detail {
                Some(x) => format!("The sign-in didn't finish ({x})."),
                None => "The sign-in didn't finish.".to_string(),
            }, Tone::Error))
            // Not the log: it holds the expired code.
            (actions(plain_action(&action, PROVIDERS_ID, "Try again", true)))
        },
    }
}

fn install_section(c: &ProviderCard, i: &InstallView, url: &str) -> Markup {
    let action = format!("{url}/install");
    let lines = |log: &[String]| {
        log_view(log.iter().map(|l| LogLine {
            text: l,
            is_error: l.starts_with("melissa-setup:"),
        }))
    };
    match i {
        InstallView::Button {
            vendor,
            state,
            detail,
            log,
        } => match state {
            InstallState::Running => group(
                "Install",
                Some(&format!(
                    "Installing {}… This usually takes a minute.",
                    c.label
                )),
                lines(log),
            ),
            InstallState::Failed | InstallState::Done => {
                let text = match (state, detail) {
                    (InstallState::Failed, Some(d)) => format!("The install failed ({d})."),
                    (InstallState::Failed, None) => "The install failed.".to_string(),
                    _ => "The installer finished, but Melissa can't find the program.".to_string(),
                };
                group(
                    "Install",
                    None,
                    html! {
                        (notice(&text, Tone::Error))
                        @if !log.is_empty() { (lines(log)) }
                        (actions(plain_action(&action, PROVIDERS_ID, "Try again", true)))
                    },
                )
            }
            InstallState::Idle => group(
                "Install",
                Some(&format!(
                    "Melissa downloads {vendor}'s official installer and installs {} on this \
                     device. By installing it you accept {vendor}'s terms.",
                    c.label
                )),
                actions(plain_action(
                    &action,
                    PROVIDERS_ID,
                    &format!("Install {}", c.label),
                    true,
                )),
            ),
        },
        InstallView::Nix => group(
            "Install",
            Some(&format!(
                "This requires you add {} via your Nix configuration.",
                c.label
            )),
            html! {},
        ),
        InstallView::Manual { text, command } => group(
            "Install",
            Some(text),
            html! { @if let Some(cmd) = command { (command_block(cmd)) } },
        ),
    }
}

fn tone_class(t: Tone) -> &'static str {
    match t {
        Tone::Ok => "tone-ok",
        Tone::Info => "tone-info",
        Tone::Warn => "tone-warn",
        Tone::Error => "tone-error",
        Tone::Muted => "tone-muted",
    }
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
    /// The source revision of the image, if recorded.
    pub revision: Option<String>,
    /// "container", "from source".
    pub install: String,
    /// One-line explanation of how this host updates.
    pub how: String,
    /// The page's subtitle (what is updated here).
    pub about: &'static str,
    pub images: Vec<ImageLine>,
    /// "Checked 5 min ago", or why not.
    pub checked: String,
    pub last: Option<Flash>,
    pub flash: Option<Flash>,
    /// Buttons the page may show (the helper is there).
    pub can_check: bool,
    pub can_apply: bool,
    pub can_rollback: bool,
    /// A pending confirmation for `apply` or `rollback`.
    pub confirm: Option<&'static str>,
    /// Commands to type instead (label, command).
    pub commands: Vec<(String, String)>,
    /// Whether and how the image updates by itself.
    pub automatic: String,
    pub agents: Vec<(String, String)>,
    pub restart: Option<RestartHint>,
}

pub fn updates_page(f: &Frame<'_>, v: &UpdatesView) -> Markup {
    frame(f, "Updates", updates_region(v))
}

pub fn updates_region(v: &UpdatesView) -> Markup {
    let mut rows = vec![
        ("Melissa", html! { (v.version) }),
        ("Installed as", html! { (v.install) }),
    ];
    if let Some(revision) = &v.revision {
        rows.push(("Revision", html! { (revision) }));
    }
    let images: Vec<(&str, Markup)> = v
        .images
        .iter()
        .map(|i| {
            (
                i.label,
                html! { (code(&i.image)) br; span.quiet { (i.detail) } },
            )
        })
        .collect();
    let agent_rows: Vec<(&str, Markup)> = v
        .agents
        .iter()
        .map(|(name, ver)| (name.as_str(), html! { (ver) }))
        .collect();
    html! {
        div id=(UPDATES_ID) {
            (settings_head(
                "Updates",
                v.about,
                &v.checked,
                "/ui/settings/updates/check",
                UPDATES_ID,
                if v.can_check { "Check for updates" } else { "Look again" },
            ))
            @if let Some(fl) = &v.flash { (notice(&fl.text, fl.tone)) }
            @if let Some(r) = &v.restart { (restart_note(r, UPDATES_ID)) }
            @if let Some(action) = v.confirm {
                @if action == "apply" {
                    (confirm_card(&ConfirmCard {
                        title: "Install the update and restart Melissa?",
                        text: "Melissa downloads the new image, then restarts on it. Apps and chats stop for a few minutes; their data is kept. If the new image doesn't start, Melissa goes back to the current one, and you can roll back later too.",
                        action: "/ui/settings/updates/apply",
                        target: UPDATES_ID,
                        confirm: "Install and restart",
                        cancel_href: "/settings/updates",
                    }))
                } @else {
                    (confirm_card(&ConfirmCard {
                        title: "Go back to the previous image and restart Melissa?",
                        text: "Melissa restarts on the image it ran before the last update. Apps and chats stop for a few minutes. Data in apps is kept.",
                        action: "/ui/settings/updates/rollback",
                        target: UPDATES_ID,
                        confirm: "Roll back and restart",
                        cancel_href: "/settings/updates",
                    }))
                }
            }
            (card("This device", html! {
                (facts(&rows))
                p { (v.how) }
                @if let Some(l) = &v.last { (notice(&l.text, l.tone)) }
                @if !images.is_empty() { (facts(&images)) }
                @if v.can_apply || v.can_rollback {
                    (actions(html! {
                        @if v.can_apply { (plain_action("/ui/settings/updates/apply", UPDATES_ID, "Apply update (restarts)", true)) }
                        @if v.can_rollback { (plain_action("/ui/settings/updates/rollback", UPDATES_ID, "Roll back (restarts)", false)) }
                    }))
                }
                @for (label, cmd) in &v.commands {
                    p.quiet { (label) }
                    (command_block(cmd))
                }
                p.quiet { (v.automatic) }
            }))
            (card("Agent programs", html! {
                @if agent_rows.is_empty() { p.quiet { "None configured." } } @else { (facts(&agent_rows)) }
                p { (link("/settings/providers", "Manage providers")) }
            }))
        }
    }
}
