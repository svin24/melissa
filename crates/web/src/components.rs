//! The fixed component library.
//!
//! This module is the ONLY place that defines markup structure and CSS classes.
//! Pages (see `pages.rs`) are assembled from these functions; they never write their own
//! elements. AI agents never generate markup: they pick components and fill in data.
//! Adding or changing a component is a human-reviewed code change.
//!
//! All text passed in is escaped by maud. Nothing here emits raw, unescaped HTML.

use maud::{html, Markup, DOCTYPE};

/// Visual tone used by badges and notices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Ok,
    Info,
    Warn,
    Error,
    Muted,
}

impl Tone {
    fn class(self) -> &'static str {
        match self {
            Self::Ok => "tone-ok",
            Self::Info => "tone-info",
            Self::Warn => "tone-warn",
            Self::Error => "tone-error",
            Self::Muted => "tone-muted",
        }
    }
}

/// Full HTML page: header, navigation and content area.
pub fn page(title: &str, content: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                // Keep htmx from injecting inline styles, so the CSP can forbid them.
                meta name="htmx-config" content=r#"{"includeIndicatorStyles":false,"allowEval":false}"#;
                title { (title) " · Melissa" }
                link rel="stylesheet" href="/assets/melissa.css";
                script src="/assets/htmx.min.js" {}
            }
            body {
                header.site {
                    a.brand href="/" { "Melissa" }
                    span.subtitle { "queen bee" }
                }
                main { (content) }
            }
        }
    }
}

/// A titled section.
pub fn card(title: &str, content: Markup) -> Markup {
    html! {
        section.card {
            h2 { (title) }
            (content)
        }
    }
}

/// A data table. Shows `empty` when there are no rows.
pub fn table(headers: &[&str], rows: Vec<Vec<Markup>>, empty: &str) -> Markup {
    html! {
        @if rows.is_empty() {
            p.empty { (empty) }
        } @else {
            table {
                thead { tr { @for h in headers { th { (h) } } } }
                tbody {
                    @for row in rows {
                        tr { @for cell in row { td { (cell) } } }
                    }
                }
            }
        }
    }
}

/// A short coloured status label.
pub fn badge(label: &str, tone: Tone) -> Markup {
    html! { span class={ "badge " (tone.class()) } { (label) } }
}

/// A one-line message box.
pub fn notice(text: &str, tone: Tone) -> Markup {
    html! { p class={ "notice " (tone.class()) } role="status" { (text) } }
}

/// Inline monospaced text (commands, identifiers).
pub fn code(text: &str) -> Markup {
    html! { code { (text) } }
}

/// A plain link.
pub fn link(href: &str, text: &str) -> Markup {
    html! { a href=(href) { (text) } }
}

/// Text that is cut to `max` characters, with the full text as a tooltip.
pub fn truncated(text: &str, max: usize) -> Markup {
    let short: String = text.chars().take(max).collect();
    let cut = short.len() < text.len();
    html! { span title=(text) { (short) @if cut { "…" } } }
}

/// A region that refreshes itself from `url` every `seconds` while `live` is true.
/// The server returns the same region again, so polling stops once `live` turns false.
pub fn live_region(id: &str, url: &str, seconds: u32, live: bool, content: Markup) -> Markup {
    html! {
        @if live {
            div id=(id) hx-get=(url) hx-trigger={ "every " (seconds) "s" } hx-swap="outerHTML" {
                (content)
            }
        } @else {
            div id=(id) { (content) }
        }
    }
}

/// One line of a log view.
pub struct LogLine<'a> {
    pub text: &'a str,
    pub is_error: bool,
}

/// Scrollable monospaced output.
pub fn log_view<'a>(lines: impl IntoIterator<Item = LogLine<'a>>) -> Markup {
    html! {
        pre.log {
            @for line in lines {
                @if line.is_error { span.log-err { (line.text) } } @else { span { (line.text) } }
                "\n"
            }
        }
    }
}

/// An option in a [`select_field`].
pub struct Choice<'a> {
    pub value: &'a str,
    pub label: &'a str,
    pub enabled: bool,
}

/// A labelled drop-down.
pub fn select_field(name: &str, label: &str, choices: &[Choice<'_>]) -> Markup {
    html! {
        label {
            (label)
            select name=(name) required {
                @for c in choices {
                    option value=(c.value) disabled[!c.enabled] { (c.label) }
                }
            }
        }
    }
}

/// A labelled multi-line text input.
pub fn textarea_field(name: &str, label: &str, placeholder: &str, max_len: usize) -> Markup {
    html! {
        label {
            (label)
            textarea name=(name) rows="4" placeholder=(placeholder) maxlength=(max_len) required {}
        }
    }
}

/// A form that posts to `action`. With htmx it swaps the response into `#target`;
/// without JavaScript it falls back to a normal POST.
pub fn post_form(action: &str, target: &str, submit_label: &str, fields: Markup) -> Markup {
    html! {
        form method="post" action=(action) hx-post=(action) hx-target={ "#" (target) } hx-swap="outerHTML" {
            (fields)
            button type="submit" { (submit_label) }
        }
    }
}

/// A single-button form, e.g. "Cancel". Asks for confirmation with htmx.
pub fn action_button(action: &str, target: &str, label: &str, confirm: &str) -> Markup {
    html! {
        form.inline method="post" action=(action) hx-post=(action) hx-target={ "#" (target) }
            hx-swap="outerHTML" hx-confirm=(confirm) {
            button.secondary type="submit" { (label) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_escaped() {
        let out = card("<b>x</b>", notice("<script>alert(1)</script>", Tone::Error)).into_string();
        assert!(!out.contains("<script>"));
        assert!(!out.contains("<b>"));
        assert!(out.contains("&lt;script&gt;"));
    }

    #[test]
    fn live_region_stops_polling_when_not_live() {
        let live = live_region("t", "/x", 2, true, html! {}).into_string();
        let done = live_region("t", "/x", 2, false, html! {}).into_string();
        assert!(live.contains("hx-trigger=\"every 2s\""));
        assert!(!done.contains("hx-get"));
    }

    #[test]
    fn truncation_keeps_full_text_in_title() {
        let out = truncated("abcdef", 3).into_string();
        assert!(out.contains("title=\"abcdef\""));
        assert!(out.contains("abc…"));
    }
}
