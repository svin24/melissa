//! First-run setup (2026-11-21, owner decision "wizard plus checklist"): the setup page with
//! its steps, the step's buttons with a quiet "Later", the checklist card in Melissa's empty
//! chat, the fixed suggestion chips there, and a notice with a link (the chat without a
//! usable provider). Part of the fixed component library (`components.rs`); a separate file
//! only to keep the diff apart from other work there.
//!
//! The chips' words are fixed strings in the code (`crate::settings::setup`), never written
//! by an agent.

use maud::{html, Markup};

use super::{csrf_field, page, stepper, Btn, Tone};

/// The setup page: one panel with the steps, the step's heading and its content. A page of
/// its own (no rail: the owner hasn't finished setting up).
pub fn setup_page(steps: &[&str], current: usize, heading: &str, content: Markup) -> Markup {
    page(
        "Set up Melissa",
        html! {
            div.setup-wrap {
                section.add-panel.setup-panel aria-labelledby="setup-h" {
                    (stepper(steps, current))
                    h1 #setup-h { (heading) }
                    div.add-panel-body { (content) }
                }
            }
        },
    )
}

/// A step's buttons: its one primary action and, if the step can wait, a quiet "Later"
/// (a POST to `later`; its own form, so it sits next to a submit button of another form).
pub fn setup_actions(primary: Markup, later: Option<&str>) -> Markup {
    html! {
        div.action-row.setup-actions {
            (primary)
            @if let Some(action) = later {
                form.inline method="post" action=(action) {
                    (csrf_field())
                    button.(Btn::link()) type="submit" { "Later" }
                }
            }
        }
    }
}

/// The submit button of the step's form `form` (placed outside it, in [`setup_actions`]).
pub fn setup_submit(form: &str, label: &str) -> Markup {
    html! { button.(Btn::primary()) type="submit" form=(form) { (label) } }
}

/// One line of the setup checklist.
pub struct ChecklistItem<'a> {
    pub label: &'a str,
    pub href: &'a str,
    /// "Later" (put off) or "Not done".
    pub state: &'a str,
}

pub const CHECKLIST_ID: &str = "setup-checklist";

/// The compact card in Melissa's empty chat: the setup steps put off or not done, each a link
/// back to its step, and "Hide" (`dismiss`, a POST; htmx removes the card).
pub fn setup_checklist(items: &[ChecklistItem<'_>], dismiss: &str) -> Markup {
    html! {
        section.setup-checklist id=(CHECKLIST_ID) aria-labelledby="setup-checklist-h" {
            div.setup-checklist-head {
                h2 #setup-checklist-h { "Finish setup" }
                form.inline method="post" action=(dismiss) hx-post=(dismiss)
                    hx-target={ "#" (CHECKLIST_ID) } hx-swap="outerHTML" {
                    (csrf_field())
                    button.(Btn::link()) type="submit" { "Hide" }
                }
            }
            ul.setup-checklist-items {
                @for i in items {
                    li {
                        a href=(i.href) { (i.label) }
                        span.quiet { (i.state) }
                    }
                }
            }
        }
    }
}

/// One suggestion in Melissa's empty chat: its words and the chat link that puts them in
/// the message box (`?draft=`; never sent by itself).
pub struct Suggestion<'a> {
    pub text: &'a str,
    pub href: String,
}

/// The suggestion chips under Melissa's empty chat.
pub fn suggestion_chips(items: &[Suggestion<'_>]) -> Markup {
    html! {
        ul.suggestions aria-label="Suggestions" {
            @for s in items {
                li { a.suggestion href=(s.href) { (s.text) } }
            }
        }
    }
}

/// A one-line message with a link to where it gets fixed (`link`: None for someone who
/// can't fix it).
pub fn notice_link(text: &str, link: Option<(&str, &str)>, tone: Tone) -> Markup {
    html! {
        p class={ "notice " (tone.class()) } role="status" {
            (text)
            @if let Some((href, label)) = link { " " a href=(href) { (label) } }
        }
    }
}
