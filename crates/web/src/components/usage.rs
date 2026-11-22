//! Usage and resources: the providers' plan limits, cost and tokens (2026-11-22, owner design
//! "Usage" B1–B4 in the local Penpot, without the pace marker).
//!
//! Part of the fixed component library (`components.rs`); a separate file to keep it together.
//! The panel has three views (Limits, Cost, Tokens) behind a section switcher: links with
//! `aria-current` that swap the view region (`#usage-view`) with htmx and are plain links
//! (`/usage?view=…`) without script (COMPONENT-SPEC 2.6: navigation is never a tablist). The
//! providers' part of a view is its own region (`#usage-providers`): it loads after the page
//! when no fresh numbers are at hand, and "Check again" swaps it, updating "Checked …" out of
//! band so the button keeps its focus.

use maud::{html, Markup};

use super::{
    agent::{avatar, Avatar},
    csrf_field, glyph, provider_logo, Btn, Glyph,
};

/// The view region (the switcher swaps it).
pub const USAGE_VIEW_ID: &str = "usage-view";
/// The providers' part of a view (loading and Check again swap it).
pub const USAGE_PROVIDERS_ID: &str = "usage-providers";
/// "Checked 2 min ago" (updated out of band with the providers' part).
pub const USAGE_CHECKED_ID: &str = "usage-checked";

/// The views of Usage and resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UsageView {
    #[default]
    Limits,
    Cost,
    Tokens,
}

impl UsageView {
    pub const ALL: [Self; 3] = [Self::Limits, Self::Cost, Self::Tokens];

    /// The `?view=` value.
    pub fn key(self) -> &'static str {
        match self {
            Self::Limits => "limits",
            Self::Cost => "cost",
            Self::Tokens => "tokens",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Limits => "Limits",
            Self::Cost => "Cost",
            Self::Tokens => "Tokens",
        }
    }

    /// From `?view=`; anything else is `None`.
    pub fn parse(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.key() == key)
    }
}

/// A limit with this share left (percent) or less is low: the bar turns to the attention tone.
/// A third of a week is about two days of usual use, enough notice to plan around it.
pub const LOW_AT: u32 = 33;
/// A limit with this share left or less is nearly gone: the caution tone. A tenth is about one
/// long session.
pub const NEARLY_GONE_AT: u32 = 10;

/// How much of a limit is left, as a tone: the bar's colour (the words carry the number).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitLevel {
    /// Plenty left (ok).
    Plenty,
    /// Getting low (attention).
    Low,
    /// Nearly or all gone (caution).
    NearlyGone,
}

impl LimitLevel {
    /// The level of a limit with `left` percent left; the provider's own word wins when it
    /// says the limit is reached (`rejected`) or close (`allowed_warning`).
    pub fn of(left: u32, status: Option<&str>) -> Self {
        match status {
            Some("rejected") => Self::NearlyGone,
            _ if left <= NEARLY_GONE_AT => Self::NearlyGone,
            Some("allowed_warning") => Self::Low,
            _ if left <= LOW_AT => Self::Low,
            _ => Self::Plenty,
        }
    }

    fn tone(self) -> &'static str {
        match self {
            Self::Plenty => "ok",
            Self::Low => "attention",
            Self::NearlyGone => "caution",
        }
    }
}

/// One limit window of a provider's plan.
pub struct LimitCard<'a> {
    /// "Session", "Weekly", "Weekly · Opus".
    pub label: &'a str,
    /// Share left, 0 to 100 (the meter shows the rest, the share used).
    pub left: u32,
    /// The share left in words ("62%", "<1%").
    pub left_text: &'a str,
    pub level: LimitLevel,
    /// "Resets in 2 h 10 min", "Resets Mon 09:00".
    pub resets: Option<&'a str>,
}

/// What a provider's group shows under its name.
pub enum ProviderLimitsBody<'a> {
    /// One card per window.
    Cards(Vec<LimitCard<'a>>),
    /// No numbers right now: one card saying why ("Sign in again to read the limits.").
    Unavailable(&'a str),
    /// The provider reports no limits: one quiet line.
    NoLimits(&'a str),
}

/// One provider in the Limits view.
pub struct ProviderLimits<'a> {
    /// The provider id (picks its logo).
    pub id: &'a str,
    pub name: &'a str,
    /// The letter of its mark when Melissa ships no logo for it.
    pub letter: &'a str,
    pub body: ProviderLimitsBody<'a>,
}

/// One provider's or agent's figure (cost, context); `None`: not reported.
pub struct UsageLine<'a> {
    pub name: &'a str,
    pub mark: LineMark<'a>,
    pub value: Option<&'a str>,
}

/// What stands in front of a [`UsageLine`].
pub enum LineMark<'a> {
    /// A provider's logo or letter mark: (provider id, letter).
    Provider(&'a str, &'a str),
    /// An agent's icon.
    Agent(Avatar<'a>),
}

/// One model's tokens in the Tokens view (counts in words: "45k").
pub struct TokenRow<'a> {
    pub model: &'a str,
    pub input: &'a str,
    pub output: &'a str,
    pub cache_read: &'a str,
    pub cache_write: &'a str,
}

/// One provider's tokens per model.
pub struct ProviderTokens<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub letter: &'a str,
    pub rows: Vec<TokenRow<'a>>,
}

/// What the providers' part of a view shows.
pub enum ProvidersPart<'a> {
    /// No fresh numbers yet: the region loads them (`url`) once the page is in.
    Loading {
        url: &'a str,
    },
    /// No provider is set up.
    NoneSetUp {
        settings: &'a str,
    },
    Limits(Vec<ProviderLimits<'a>>),
    Cost {
        lines: Vec<UsageLine<'a>>,
        /// "Counted since 21 Nov." or nothing.
        note: &'a str,
    },
    Tokens {
        providers: Vec<ProviderTokens<'a>>,
        note: &'a str,
    },
}

/// The bar over every view: the switcher, when the providers were checked and Check again.
pub struct UsageBar<'a> {
    pub current: UsageView,
    /// "Checked 2 min ago"; `None` while not checked yet.
    pub checked: Option<&'a str>,
    /// Where Check again posts (with the view).
    pub check_url: &'a str,
}

/// The switcher: links drawn as a segmented control (the Apps filter's look).
fn usage_tabs(current: UsageView) -> Markup {
    html! {
        nav.show-filter.usage-tabs aria-label="Usage views" {
            ul.segmented {
                @for v in UsageView::ALL {
                    li {
                        a.seg href={ "/usage?view=" (v.key()) }
                            hx-get={ "/ui/usage/view?view=" (v.key()) }
                            hx-target={ "#" (USAGE_VIEW_ID) } hx-swap="outerHTML"
                            aria-current=[(v == current).then_some("page")] {
                            (v.label())
                        }
                    }
                }
            }
        }
    }
}

/// "Checked 2 min ago" (or, while the first check runs, "Checking…" with script). `oob`: as an
/// out-of-band update next to the providers' part.
pub fn usage_checked(checked: Option<&str>, oob: bool) -> Markup {
    html! {
        span.usage-checked id=(USAGE_CHECKED_ID) role="status"
            hx-swap-oob=[oob.then_some("true")] {
            @match checked {
                Some(text) => (text),
                None => span data-script-only { "Checking\u{2026}" },
            }
        }
    }
}

fn usage_bar(b: &UsageBar<'_>) -> Markup {
    html! {
        div.usage-bar {
            (usage_tabs(b.current))
            div.usage-check {
                (usage_checked(b.checked, false))
                form method="post" action=(b.check_url) hx-post=(b.check_url)
                    hx-target={ "#" (USAGE_PROVIDERS_ID) } hx-swap="outerHTML" hx-sync="this:drop" {
                    (csrf_field())
                    button.usage-check-button.(Btn::secondary()) type="submit" {
                        (glyph(Glyph::Refresh))
                        span.spin aria-hidden="true" {}
                        "Check again"
                    }
                }
            }
        }
    }
}

/// A limit card: the label, the share left (big), the bar and the reset time.
fn limit_card(c: &LimitCard<'_>, id: &str) -> Markup {
    let words = format!("{} left", c.left_text);
    html! {
        article.limit-card aria-labelledby=(id) {
            p.limit-label id=(id) { (c.label) }
            p.limit-left { span.limit-num { (c.left_text) } " left" }
            // The bar fills with the share used, like the devices' meters (owner board B1);
            // the words say what is left. Native ranges match the tones: from `low` used the
            // limit is low, above `high` nearly gone.
            meter.limit-meter min="0" max="100" low=(100 - LOW_AT)
                high=(99 - NEARLY_GONE_AT) optimum="0" value=(100 - c.left.min(100))
                data-tone=(c.level.tone()) aria-labelledby=(id) aria-valuetext=(words) {
                (words)
            }
            @if let Some(r) = c.resets { p.limit-reset { (r) } }
        }
    }
}

fn provider_head(id: &str, letter: &str, name: &str, heading_id: &str) -> Markup {
    html! {
        h3.usage-provider-head {
            (provider_logo(id, letter, false))
            span id=(heading_id) { (name) }
        }
    }
}

fn limits(groups: &[ProviderLimits<'_>]) -> Markup {
    html! {
        h2.visually-hidden { "Plan limits" }
        @for (n, p) in groups.iter().enumerate() {
            @let head = format!("limits-p{n}");
            section.usage-provider aria-labelledby=(head) {
                (provider_head(p.id, p.letter, p.name, &head))
                @match &p.body {
                    ProviderLimitsBody::Cards(cards) => div.limit-cards {
                        @for (i, c) in cards.iter().enumerate() {
                            (limit_card(c, &format!("limits-p{n}-w{i}")))
                        }
                    },
                    ProviderLimitsBody::Unavailable(reason) => div.limit-cards {
                        article.limit-card.limit-off aria-labelledby={ (head) "-off" } {
                            p.limit-label id={ (head) "-off" } { "Limits" }
                            p.limit-left { "Unavailable" }
                            p.limit-reset { (reason) }
                        }
                    },
                    ProviderLimitsBody::NoLimits(text) => p.limit-none { (text) },
                }
            }
        }
    }
}

fn line_mark(m: &LineMark<'_>) -> Markup {
    match m {
        LineMark::Provider(id, letter) => provider_logo(id, letter, false),
        LineMark::Agent(a) => html! { span.usage-line-icon { (avatar(a, "av-sm")) } },
    }
}

/// A titled list of figures (cost, context): mark, name, value or "Not reported".
pub fn usage_lines(title: &str, id: &str, lines: &[UsageLine<'_>]) -> Markup {
    html! {
        section.usage-lines aria-labelledby=(id) {
            h2.usage-lines-head id=(id) { (title) }
            ul.usage-line-list {
                @for l in lines {
                    li.usage-line {
                        (line_mark(&l.mark))
                        span.usage-line-name { (l.name) }
                        @match l.value {
                            Some(v) => span.usage-line-value { (v) },
                            None => span.usage-line-value.quiet { "Not reported" },
                        }
                    }
                }
            }
        }
    }
}

fn tokens(providers: &[ProviderTokens<'_>], note: &str) -> Markup {
    html! {
        h2.visually-hidden { "Tokens by provider" }
        @if providers.is_empty() {
            p.limit-none { "No provider has reported tokens yet." }
        }
        @for (n, p) in providers.iter().enumerate() {
            @let head = format!("tokens-p{n}");
            section.usage-provider aria-labelledby=(head) {
                (provider_head(p.id, p.letter, p.name, &head))
                div.table-wrap {
                    table.usage {
                        caption.visually-hidden { "Tokens per model, " (p.name) }
                        thead { tr {
                            th scope="col" { "Model" }
                            th.num scope="col" { "Input" }
                            th.num scope="col" { "Output" }
                            th.num scope="col" { "Cache read" }
                            th.num scope="col" { "Cache write" }
                        } }
                        tbody {
                            @for r in &p.rows {
                                tr {
                                    th scope="row" { (r.model) }
                                    td.num { (r.input) }
                                    td.num { (r.output) }
                                    td.num { (r.cache_read) }
                                    td.num { (r.cache_write) }
                                }
                            }
                        }
                    }
                }
            }
        }
        @if !note.is_empty() { p.quiet { (note) } }
    }
}

/// The providers' part of a view, as its own region.
pub fn usage_providers(part: &ProvidersPart<'_>) -> Markup {
    match part {
        ProvidersPart::Loading { url } => html! {
            div.usage-providers id=(USAGE_PROVIDERS_ID) hx-get=(url) hx-trigger="load"
                hx-swap="outerHTML" aria-busy="true" {
                p.limit-none {
                    span data-script-only { span.spin aria-hidden="true" {} "Checking providers\u{2026}" }
                    span data-no-script-only { "Not checked yet." }
                }
            }
        },
        ProvidersPart::NoneSetUp { settings } => html! {
            div.usage-providers id=(USAGE_PROVIDERS_ID) {
                p.limit-none {
                    "No provider is set up. "
                    a href=(settings) { "Set up a provider" }
                }
            }
        },
        ProvidersPart::Limits(groups) => html! {
            div.usage-providers id=(USAGE_PROVIDERS_ID) { (limits(groups)) }
        },
        ProvidersPart::Cost { lines, note } => html! {
            div.usage-providers id=(USAGE_PROVIDERS_ID) {
                (usage_lines("By provider", "cost-providers", lines))
                @if !note.is_empty() { p.quiet { (note) } }
            }
        },
        ProvidersPart::Tokens { providers, note } => html! {
            div.usage-providers id=(USAGE_PROVIDERS_ID) { (tokens(providers, note)) }
        },
    }
}

/// A view of Usage and resources (the owner's): the bar, the providers' part and what the view
/// adds below it (the agents and devices under Limits, cost by agent under Cost).
/// `providers` is [`usage_providers`]' region.
pub fn usage_view(bar: &UsageBar<'_>, providers: Markup, below: Markup) -> Markup {
    html! {
        div.usage-view id=(USAGE_VIEW_ID) {
            (usage_bar(bar))
            (providers)
            (below)
        }
    }
}
