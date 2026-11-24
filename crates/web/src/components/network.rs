//! An app's network permissions (2026-11-24, owner design "Network permissions" N1–N6): the
//! Network card in an app's details, its link rows and inline add-link form, and the network
//! box inside an approval card.
//!
//! Part of the fixed component library (`components.rs`). The card is one region
//! (`#app-network`) its actions swap whole. Without script: the switches have their own Save
//! (the [`toggle`] component), "Add link" and Cancel are links, the forms post and redirect.

use maud::{html, Markup};

use super::{glyph, status_badge, toggle, Btn, Glyph, Mark, Toggle, Tone};

/// The Network card's region id (its actions swap it).
pub const APP_NETWORK_ID: &str = "app-network";

/// "Agent asked": an agent's change to this row waits for the owner; it links to the request's
/// approval card in the chat (`href`).
fn asked_badge(href: &str) -> Markup {
    html! {
        a.net-asked href=(href) title="Open the request" {
            (status_badge("Agent asked", Tone::Attention, Mark::Dot))
        }
    }
}

/// Internet or Local network.
pub struct NetworkSwitch<'a> {
    pub label: &'a str,
    pub on: bool,
    /// Where the switch posts (`enabled=on` or nothing). None: read-only (a member).
    pub action: Option<&'a str>,
    /// An agent asked to change it: where its approval card is.
    pub asked: Option<&'a str>,
}

/// One link to another app's port: "→ postgres  db · 5432  ✕".
pub struct NetworkLink<'a> {
    /// The other app's display name.
    pub name: &'a str,
    /// "db · 5432", or "all ports".
    pub port: &'a str,
    /// It doesn't resolve now (the app or port is gone).
    pub inactive: bool,
    /// Remove: (POST url, app, port name). None: read-only.
    pub remove: Option<(&'a str, &'a str, Option<&'a str>)>,
}

/// The inline add-link form: the other app, then one of its named ports.
pub struct AddLinkForm<'a> {
    /// Where it posts (`app`, `port` = `<app>/<port>`).
    pub action: &'a str,
    /// Cancel: (the details page, the card without the form).
    pub cancel: (&'a str, &'a str),
    /// The card with the form for another app (`?add=1&to=<app>`, the app select's change).
    pub refresh: &'a str,
    /// (value, label) of each app that has a port to link to.
    pub apps: &'a [(&'a str, &'a str)],
    /// The app chosen.
    pub app: &'a str,
    /// (value `<app>/<port>`, label "api · 11434") of the chosen app's ports.
    pub ports: &'a [(String, String)],
}

/// The Network card (owner design N1/N2/N4).
pub struct NetworkCard<'a> {
    pub internet: NetworkSwitch<'a>,
    pub lan: NetworkSwitch<'a>,
    /// An agent asked to change the links: where its approval card is.
    pub links_asked: Option<&'a str>,
    /// "Add link": (the plain URL, the htmx URL). None: read-only, or the form is open.
    pub add: Option<(&'a str, &'a str)>,
    pub form: Option<AddLinkForm<'a>>,
    pub links: &'a [NetworkLink<'a>],
    /// The muted footer ("DNS and Immich's web ports always work.").
    pub footer: &'a str,
    pub message: Option<(&'a str, Tone)>,
}

fn switch_row(s: &NetworkSwitch<'_>) -> Markup {
    html! {
        li.net-row {
            span.net-label { (s.label) @if let Some(h) = s.asked { (asked_badge(h)) } }
            @match s.action {
                Some(action) => (toggle(&Toggle {
                    action,
                    target: APP_NETWORK_ID,
                    label: s.label,
                    on: s.on,
                    locked: None,
                })),
                None => span.net-value { @if s.on { "On" } @else { "Off" } },
            }
        }
    }
}

/// One link row: the arrow, the app, its port in muted text, the remove button.
pub fn link_row(l: &NetworkLink<'_>) -> Markup {
    html! {
        li.net-row.net-link {
            span.net-label {
                span.net-arrow aria-hidden="true" { "→" }
                span.visually-hidden { "Link to " }
                (l.name)
                span.net-port { (l.port) }
                @if l.inactive { span.net-port { "· not reachable now" } }
            }
            @if let Some((action, app, port)) = l.remove {
                form method="post" action=(action) hx-post=(action)
                    hx-target={ "#" (APP_NETWORK_ID) } hx-swap="outerHTML" {
                    (super::csrf_field())
                    input type="hidden" name="app" value=(app);
                    @if let Some(p) = port { input type="hidden" name="port" value=(p); }
                    button.net-remove.(Btn::ghost().sm().icon()) type="submit"
                        aria-label={ "Remove link to " (l.name) } title="Remove link" {
                        (glyph(Glyph::Close))
                    }
                }
            }
        }
    }
}

/// The inline add-link form row (N2): app, port, "Add link", Cancel. Without script a
/// changed app posts with a port of another app; the server answers with that app's ports.
pub fn add_link_form(f: &AddLinkForm<'_>) -> Markup {
    let target = format!("#{APP_NETWORK_ID}");
    html! {
        @if f.apps.is_empty() {
            div.net-add {
                p.net-none { "No other app has a port to link to." }
                a.(Btn::ghost()) href=(f.cancel.0) hx-get=(f.cancel.1) hx-target=(target)
                    hx-swap="outerHTML" { "Cancel" }
            }
        } @else {
            form.net-add method="post" action=(f.action) hx-post=(f.action) hx-target=(target)
                hx-swap="outerHTML" {
                (super::csrf_field())
                span.select-wrap {
                    select.input name="app" aria-label="App" hx-get=(f.refresh) hx-trigger="change"
                        hx-target=(target) hx-swap="outerHTML" hx-include="this" {
                        @for (value, label) in f.apps {
                            option value=(value) selected[*value == f.app] { (label) }
                        }
                    }
                }
                span.select-wrap {
                    select.input name="port" aria-label="Port" {
                        @for (value, label) in f.ports { option value=(value) { (label) } }
                    }
                }
                button.(Btn::primary()) type="submit" { "Add link" }
                a.(Btn::ghost()) href=(f.cancel.0) hx-get=(f.cancel.1) hx-target=(target)
                    hx-swap="outerHTML" { "Cancel" }
            }
        }
    }
}

/// The Network card of an app's details.
pub fn network_card(c: &NetworkCard<'_>) -> Markup {
    html! {
        section.app-card.net-card id=(APP_NETWORK_ID) aria-labelledby="app-network-h" {
            h3 #app-network-h { "Network" }
            @if let Some((text, tone)) = c.message {
                p class={ "notice " (tone.class()) } role="status" { (super::notice_glyph(tone)) (text) }
            }
            ul.net-rows {
                (switch_row(&c.internet))
                (switch_row(&c.lan))
                li.net-row.net-links-head {
                    span.net-label { "Links to apps" @if let Some(h) = c.links_asked { (asked_badge(h)) } }
                    @if let Some((href, get)) = c.add {
                        a.(Btn::secondary().sm()) href=(href) hx-get=(get)
                            hx-target={ "#" (APP_NETWORK_ID) } hx-swap="outerHTML" { "Add link" }
                    }
                    @if let Some(f) = &c.form { (add_link_form(f)) }
                }
                @for l in c.links { (link_row(l)) }
            }
            p.net-foot { (c.footer) }
        }
    }
}

/// The network box inside an approval card (N3): label and value rows (Internet, Local
/// network, Links). A single row is the short, muted form ("Network: None", an install that
/// may reach nothing).
pub fn network_box(rows: &[(&str, &str)]) -> Markup {
    html! {
        dl.net-box.net-box-quiet[rows.len() == 1] {
            @for (label, value) in rows {
                div { dt { (label) } dd { (value) } }
            }
        }
    }
}
