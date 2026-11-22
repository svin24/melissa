//! Usage and resources (`/usage`, the rail's chart icon; 2026-11-20, owner-requested; the
//! providers' limits, cost and tokens 2026-11-22, owner design "Usage" B1–B4).
//!
//! For the owner, three views behind a switcher (`?view=limits|cost|tokens`):
//! - **Limits**: each provider that is set up, with a card per plan window (the share left, a
//!   bar, when it resets), then the agents table and the devices.
//! - **Cost**: what each provider and each agent cost, as reported.
//! - **Tokens**: tokens per model for each provider, and each agent's context.
//!
//! The providers' numbers come from `melissa_agent::provider_usage` and are the owner's only
//! (they are read with the agents' sign-ins; `/api/usage/providers` is owner-only too). A
//! member sees the page as before: the agents table and the devices.
//!
//! Pages never wait for the providers: with a report younger than the collector's cache the
//! numbers are in the page, else the providers' part loads itself after the page
//! (`/ui/usage/providers`). "Check again" posts to `/ui/usage/check`, which asks for a refresh;
//! the collector reuses a round younger than 15 s and runs one round at a time, so pressing it
//! repeatedly makes at most one round of calls per 15 s. Without script it is a form whose
//! answer goes back to the page.
//!
//! Agent rows: per agent (Melissa and each helper) the turns, the time spent in them, the
//! approvals they asked for, and tokens in context and cost only where the provider reports
//! them (counted in the agent layer, `melissa_agent::lead::usage`); then the devices with their
//! resources and the number of running apps. Opens as a modal like Settings (see `crate::panel`).

use std::collections::BTreeMap;

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};
use maud::{html, Markup};
use melissa_agent::{
    lead::usage::Usage,
    provider_usage::{Collector, ProviderUsage, SourceState, UsageReport, Window, WindowKind},
};
use serde::Deserialize;

use crate::{
    app_list::Looker,
    components::{
        usage_checked, usage_providers, usage_view, LimitCard, LimitLevel, LineMark,
        ProviderLimits, ProviderLimitsBody, ProviderTokens, ProvidersPart, TokenRow, UsageBar,
        UsageLine, UsageRow, UsageView,
    },
    pages,
    ui::Ui,
};

/// What one agent's row says, in words.
struct Words {
    name: String,
    href: String,
    role: &'static str,
    turns: String,
    time: String,
    approvals: String,
    /// `None` where the provider did not report it.
    context: Option<String>,
    /// `None` where the provider did not report it.
    cost: Option<String>,
}

fn words(name: String, href: String, role: &'static str, u: &Usage) -> Words {
    Words {
        name,
        href,
        role,
        turns: u.turns.to_string(),
        time: duration(u.busy_ms),
        approvals: u.approvals.to_string(),
        context: u
            .context
            .map(|c| format!("{} of {}", tokens(c.used), tokens(c.size))),
        cost: cost(&u.cost()),
    }
}

/// Time spent: "0 min", "12 min", "3 h 5 min" (a non-breaking space keeps number and unit).
fn duration(ms: u64) -> String {
    let min = ms / 60_000;
    let (h, m) = (min / 60, min % 60);
    if h == 0 {
        if ms > 0 && min == 0 {
            "<1\u{a0}min".into()
        } else {
            format!("{m}\u{a0}min")
        }
    } else {
        format!("{h}\u{a0}h {m}\u{a0}min")
    }
}

/// Tokens, shortened: "950", "45k", "1.2M".
fn tokens(n: u64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{}k", (n + 500) / 1_000)
    } else {
        let tenths = (n + 50_000) / 100_000;
        format!("{}.{}M", tenths / 10, tenths % 10)
    }
}

/// Reported cost per unit: "US$0.42", "3.5 credits"; `None` when nothing was reported.
fn cost(by_unit: &BTreeMap<String, u64>) -> Option<String> {
    if by_unit.is_empty() {
        return None;
    }
    let text = by_unit
        .iter()
        .map(|(unit, micros)| {
            if unit == "USD" {
                if *micros > 0 && *micros < 10_000 {
                    "<US$0.01".to_string()
                } else {
                    let cents = (micros + 5_000) / 10_000;
                    format!("US${}.{:02}", cents / 100, cents % 100)
                }
            } else {
                let hundredths = (micros + 5_000) / 10_000;
                let amount = format!("{}.{:02}", hundredths / 100, hundredths % 100);
                let amount = amount.trim_end_matches('0').trim_end_matches('.');
                format!("{amount}\u{a0}{unit}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    Some(text)
}

/// The note under the table: "Counted since 21 Nov." plus one sentence when Context or Cost is
/// missing for some or all agents (the table hides a column nobody reported).
fn note(since: Option<String>, rows: &[Words]) -> String {
    let context = rows.iter().any(|w| w.context.is_none());
    let cost = rows.iter().any(|w| w.cost.is_none());
    let missing = match (context, cost) {
        (true, true) => Some("Context and cost show only where the provider reports them."),
        (true, false) => Some("Context shows only where the provider reports it."),
        (false, true) => Some("Cost shows only where the provider reports it."),
        (false, false) => None,
    };
    let since = since.map(|d| format!("Counted since {d}."));
    [since.as_deref(), missing]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `?view=` of the Usage pages and regions.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct ViewQuery {
    view: Option<String>,
}

impl ViewQuery {
    fn view(&self) -> UsageView {
        self.view
            .as_deref()
            .and_then(UsageView::parse)
            .unwrap_or_default()
    }
}

/// Where the providers' numbers come from: the lead agent's collector, or the screenshot
/// harness's fake one.
fn collector(ui: &Ui) -> Collector {
    #[cfg(feature = "fake-usage")]
    if let Some(c) = crate::fake_usage::collector() {
        return c;
    }
    ui.lead.provider_usage().clone()
}

/// Unix seconds now.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The agents' rows in words, and the date counting started ("21 Nov").
struct AgentRows {
    rows: Vec<Words>,
    since: Option<String>,
    /// Each row's icon: `None` for Melissa, else (shape, colour).
    icons: Vec<Option<(String, String)>>,
}

fn agent_rows(ui: &Ui) -> AgentRows {
    let lead = ui.lead.usage();
    let mut all = vec![(
        words(
            ui.lead.agents().lead_name(),
            "/".into(),
            "Lead agent",
            &lead,
        ),
        lead.since,
        None,
    )];
    for a in ui.lead.agents().list() {
        let u = ui.lead.agents().usage_of(&a.id);
        let since = u.since;
        all.push((
            words(a.name.clone(), pages::agent_href(&a.id), "Helper", &u),
            since,
            Some((a.icon.shape.clone(), a.icon.color.clone())),
        ));
    }
    let since = all
        .iter()
        .map(|(_, s, _)| *s)
        .filter(|s| *s > 0)
        .min()
        .map(pages::short_date);
    let mut rows = Vec::new();
    let mut icons = Vec::new();
    for (w, _, icon) in all {
        rows.push(w);
        icons.push(icon);
    }
    AgentRows { rows, since, icons }
}

impl AgentRows {
    /// The agents table with its note.
    fn table(&self) -> Markup {
        let note = note(self.since.clone(), &self.rows);
        let rows: Vec<UsageRow<'_>> = self
            .rows
            .iter()
            .map(|w| UsageRow {
                name: &w.name,
                href: &w.href,
                role: w.role,
                turns: &w.turns,
                time: &w.time,
                approvals: &w.approvals,
                context: w.context.as_deref(),
                cost: w.cost.as_deref(),
            })
            .collect();
        pages::usage_agents(&rows, &note)
    }

    /// One line per agent: its icon, name and `value` of it.
    fn lines(&self, value: impl Fn(&Words) -> Option<&str>) -> Vec<UsageLine<'_>> {
        self.rows
            .iter()
            .zip(&self.icons)
            .map(|(w, icon)| UsageLine {
                name: &w.name,
                mark: LineMark::Agent(match icon {
                    None => crate::components::Avatar::Melissa,
                    Some((shape, color)) => crate::components::Avatar::Agent { shape, color },
                }),
                value: value(w),
            })
            .collect()
    }
}

/// `GET /usage`: the page, or the panel alone for the modal. The owner gets the views; a
/// member the agents and devices.
pub(crate) async fn page(State(ui): State<Ui>, Query(q): Query<ViewQuery>) -> Markup {
    let agents = agent_rows(&ui);
    if !Looker::current().owner {
        return ui
            .framed(|f| pages::usage(f, html! { (agents.table()) (pages::usage_devices(f)) }))
            .await;
    }
    let report = collector(&ui).cached();
    let view = q.view();
    ui.framed(|f| pages::usage(f, view_region(f, view, report.as_ref(), &agents)))
        .await
}

/// `GET /ui/usage/view` (owner): one view's region, for the switcher.
pub(crate) async fn view(State(ui): State<Ui>, Query(q): Query<ViewQuery>) -> Markup {
    let agents = agent_rows(&ui);
    let report = collector(&ui).cached();
    ui.framed(|f| view_region(f, q.view(), report.as_ref(), &agents))
        .await
}

/// `GET /ui/usage/providers` (owner): the providers' part, asking them if the last round is
/// older than the collector's cache; "Checked …" comes along out of band.
pub(crate) async fn providers(State(ui): State<Ui>, Query(q): Query<ViewQuery>) -> Markup {
    let report = collector(&ui).report(false).await;
    providers_answer(&ui, q.view(), &report)
}

/// `POST /ui/usage/check` (owner): Check again. With htmx the providers' part; without
/// script back to the page, which then has the fresh numbers.
pub(crate) async fn check(
    State(ui): State<Ui>,
    headers: HeaderMap,
    Query(q): Query<ViewQuery>,
) -> Response {
    let report = collector(&ui).report(true).await;
    let view = q.view();
    if crate::is_htmx(&headers) {
        providers_answer(&ui, view, &report).into_response()
    } else {
        Redirect::to(&format!("/usage?view={}", view.key())).into_response()
    }
}

fn providers_answer(_ui: &Ui, view: UsageView, report: &UsageReport) -> Markup {
    let now = now_secs();
    let checked = checked_words(report.checked_at, now);
    let part = providers_part(view, report, now);
    html! {
        (part.render())
        (usage_checked(checked.as_deref(), true))
    }
}

/// The view region: the bar, the providers' part and what the view shows below it.
fn view_region(
    f: &pages::Frame<'_>,
    view: UsageView,
    report: Option<&UsageReport>,
    agents: &AgentRows,
) -> Markup {
    let now = now_secs();
    let checked = report.and_then(|r| checked_words(r.checked_at, now));
    let check_url = format!("/ui/usage/check?view={}", view.key());
    let part = match report {
        Some(r) => providers_part(view, r, now),
        None => Part::Loading(format!("/ui/usage/providers?view={}", view.key())),
    };
    let below = match view {
        UsageView::Limits => html! { (agents.table()) (pages::usage_devices(f)) },
        UsageView::Cost => html! {
            (crate::components::usage_lines("By agent", "cost-agents", &agents.lines(|w| w.cost.as_deref())))
            p.quiet { "Only what providers report. Melissa never estimates cost." }
        },
        UsageView::Tokens => html! {
            (crate::components::usage_lines("Context, by agent", "context-agents", &agents.lines(|w| w.context.as_deref())))
        },
    };
    usage_view(
        &UsageBar {
            current: view,
            checked: checked.as_deref(),
            check_url: &check_url,
        },
        part.render(),
        below,
    )
}

/// The providers' part of a view, in words (owned; [`Part::render`] lends it to the
/// components).
#[derive(Debug, Clone, PartialEq)]
enum Part {
    Loading(String),
    NoneSetUp,
    Limits(Vec<Group>),
    Cost {
        lines: Vec<(Mark, Option<String>)>,
        note: String,
    },
    Tokens {
        providers: Vec<(Mark, Vec<[String; 5]>)>,
        note: String,
    },
}

/// A provider's id, name and mark letter.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Mark {
    id: String,
    name: String,
    letter: String,
}

impl Mark {
    fn of(p: &ProviderUsage) -> Self {
        Self {
            id: p.provider.clone(),
            name: p.label.clone(),
            letter: crate::agents::letter(&p.label),
        }
    }
}

/// One provider in the Limits view.
#[derive(Debug, Clone, PartialEq)]
struct Group {
    mark: Mark,
    body: Body,
}

#[derive(Debug, Clone, PartialEq)]
enum Body {
    Cards(Vec<Card>),
    Unavailable(String),
    NoLimits(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Card {
    label: String,
    left: u32,
    left_text: String,
    level: LimitLevel,
    resets: Option<String>,
}

impl Part {
    fn render(&self) -> Markup {
        let part = match self {
            Part::Loading(url) => ProvidersPart::Loading { url },
            Part::NoneSetUp => ProvidersPart::NoneSetUp {
                settings: "/settings/providers",
            },
            Part::Limits(groups) => ProvidersPart::Limits(
                groups
                    .iter()
                    .map(|g| ProviderLimits {
                        id: &g.mark.id,
                        name: &g.mark.name,
                        letter: &g.mark.letter,
                        body: match &g.body {
                            Body::Cards(cards) => ProviderLimitsBody::Cards(
                                cards
                                    .iter()
                                    .map(|c| LimitCard {
                                        label: &c.label,
                                        left: c.left,
                                        left_text: &c.left_text,
                                        level: c.level,
                                        resets: c.resets.as_deref(),
                                    })
                                    .collect(),
                            ),
                            Body::Unavailable(r) => ProviderLimitsBody::Unavailable(r),
                            Body::NoLimits(t) => ProviderLimitsBody::NoLimits(t),
                        },
                    })
                    .collect(),
            ),
            Part::Cost { lines, note } => ProvidersPart::Cost {
                lines: lines
                    .iter()
                    .map(|(m, v)| UsageLine {
                        name: &m.name,
                        mark: LineMark::Provider(&m.id, &m.letter),
                        value: v.as_deref(),
                    })
                    .collect(),
                note,
            },
            Part::Tokens { providers, note } => ProvidersPart::Tokens {
                providers: providers
                    .iter()
                    .map(|(m, rows)| ProviderTokens {
                        id: &m.id,
                        name: &m.name,
                        letter: &m.letter,
                        rows: rows
                            .iter()
                            .map(|[model, input, output, read, write]| TokenRow {
                                model,
                                input,
                                output,
                                cache_read: read,
                                cache_write: write,
                            })
                            .collect(),
                    })
                    .collect(),
                note,
            },
        };
        usage_providers(&part)
    }
}

/// The providers' part of `view` from `report` at `now`. Providers that aren't set up (not
/// installed, not signed in) are left out.
fn providers_part(view: UsageView, report: &UsageReport, now: u64) -> Part {
    let ready: Vec<&ProviderUsage> = report.providers.iter().filter(|p| p.ready).collect();
    if ready.is_empty() {
        return Part::NoneSetUp;
    }
    match view {
        UsageView::Limits => Part::Limits(ready.iter().map(|p| limits_group(p, now)).collect()),
        UsageView::Cost => {
            let since = ready
                .iter()
                .flat_map(|p| p.cost.iter().map(|c| c.since))
                .min();
            Part::Cost {
                lines: ready
                    .iter()
                    .map(|p| {
                        let mut by_unit: BTreeMap<String, u64> = BTreeMap::new();
                        for c in &p.cost {
                            let t = by_unit.entry(c.currency.clone()).or_default();
                            *t = t.saturating_add(c.micros);
                        }
                        (Mark::of(p), cost(&by_unit))
                    })
                    .collect(),
                note: counted_since(since),
            }
        }
        UsageView::Tokens => {
            let with: Vec<_> = ready
                .iter()
                .filter_map(|p| Some((p, p.tokens.as_ref().filter(|t| !t.models.is_empty())?)))
                .collect();
            let since = with.iter().map(|(_, t)| t.since).min();
            Part::Tokens {
                providers: with
                    .into_iter()
                    .map(|(p, t)| {
                        let rows = t
                            .models
                            .iter()
                            .map(|m| {
                                [
                                    m.model.clone(),
                                    tokens(m.tokens.input),
                                    tokens(m.tokens.output),
                                    tokens(m.tokens.cache_read),
                                    tokens(m.tokens.cache_write),
                                ]
                            })
                            .collect();
                        (Mark::of(p), rows)
                    })
                    .collect(),
                note: counted_since(since),
            }
        }
    }
}

/// "Counted since 21 Nov." (empty without a date).
fn counted_since(since: Option<u64>) -> String {
    since
        .filter(|s| *s > 0)
        .map(|s| format!("Counted since {}.", pages::short_date(s)))
        .unwrap_or_default()
}

/// A provider in the Limits view: its windows, else why there are none.
fn limits_group(p: &ProviderUsage, now: u64) -> Group {
    let body = if !p.windows.is_empty() {
        Body::Cards(p.windows.iter().map(|w| card(w, now)).collect())
    } else {
        let failed = p
            .sources
            .iter()
            .filter(|s| s.state == SourceState::Unavailable)
            .find_map(|s| s.reason.as_deref());
        match failed.map(reason_words) {
            Some(Some(words)) => Body::Unavailable(words.into()),
            _ => Body::NoLimits(format!("{} doesn't report limits.", p.label)),
        }
    };
    Group {
        mark: Mark::of(p),
        body,
    }
}

/// A window as a card at `now`.
fn card(w: &Window, now: u64) -> Card {
    let left_exact = (100.0 - w.used_pct).clamp(0.0, 100.0);
    // Never NaN (the data layer drops such values); in 0..=100 after the clamp.
    let left = left_exact.round() as u32;
    let left_text = if left == 0 && left_exact > 0.0 {
        "<1%".to_string()
    } else {
        format!("{left}%")
    };
    let label = if w.kind == WindowKind::Session && w.model_scope.is_none() {
        "Session".to_string()
    } else {
        w.label.clone()
    };
    Card {
        label,
        left,
        left_text,
        level: LimitLevel::of(left, w.status.as_deref()),
        resets: w.resets_at.map(|at| resets_words(at, now)),
    }
}

/// When a window starts over, in words: "Resets in 2 h 10 min" within a day, "Resets Mon
/// 09:00" within a week, else "Resets 3 Dec" (this device's time zone).
fn resets_words(at: u64, now: u64) -> String {
    const DAY: u64 = 24 * 3600;
    if at <= now {
        return "Resets now".into();
    }
    let left = at - now;
    if left < DAY {
        return format!("Resets in {}", duration(left * 1000));
    }
    let zoned = i64::try_from(at)
        .ok()
        .and_then(|s| jiff::Timestamp::from_second(s).ok())
        .map(|t| t.to_zoned(jiff::tz::TimeZone::system()));
    match zoned {
        Some(z) if left < 7 * DAY => format!("Resets {}", z.strftime("%a %H:%M")),
        Some(z) => format!("Resets {}", z.strftime("%-d %b")),
        None => format!("Resets {}", pages::short_date(at)),
    }
}

/// "Checked just now", "Checked 4 min ago", "Checked 2 h ago"; `None` before the first check.
fn checked_words(at: Option<u64>, now: u64) -> Option<String> {
    let ago = now.saturating_sub(at?);
    Some(match ago {
        0..60 => "Checked just now".into(),
        60..3600 => format!("Checked {} min ago", ago / 60),
        _ => format!("Checked {} h ago", ago / 3600),
    })
}

/// Why a provider has no numbers, in the UI's words; `None` when it means the provider has no
/// limits to report (an API key instead of a plan, a source that gives none). The data layer's
/// reasons are for logs and the JSON endpoint; anything not recognised here gets a plain
/// "Couldn't read the limits." instead of raw text.
fn reason_words(reason: &str) -> Option<&'static str> {
    let r = reason.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| r.contains(w));
    if has(&[
        "no plan limits",
        "no usage figures",
        "own protocol",
        "gives no usage numbers",
        "no go subscription",
        "reported no quota",
        "reported no go usage",
    ]) {
        None
    } else if has(&["renews it"]) {
        Some("The sign-in expired. It renews with the next chat.")
    } else if has(&[
        "sign-in",
        "sign in",
        "signed in",
        "refused the go key",
        "expired",
    ]) {
        Some("Sign in again to read the limits.")
    } else if has(&[
        "next chat",
        "first chat",
        "no usage numbers yet",
        "nothing reported yet",
    ]) {
        Some("Numbers come with the next chat.")
    } else if has(&["rate limiting", "no answer within"]) {
        Some("No answer from the provider. Check again later.")
    } else {
        Some("Couldn't read the limits.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use melissa_agent::lead::usage::Context;

    #[test]
    fn durations_tokens_and_costs_in_words() {
        assert_eq!(duration(0), "0\u{a0}min");
        assert_eq!(duration(20_000), "<1\u{a0}min");
        assert_eq!(duration(12 * 60_000), "12\u{a0}min");
        assert_eq!(duration(185 * 60_000), "3\u{a0}h 5\u{a0}min");
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(45_200), "45k");
        assert_eq!(tokens(1_234_567), "1.2M");
        let mut c = BTreeMap::new();
        assert_eq!(cost(&c), None);
        c.insert("USD".to_string(), 420_000);
        assert_eq!(cost(&c).as_deref(), Some("US$0.42"));
        c.insert("credits".to_string(), 3_500_000);
        assert_eq!(cost(&c).as_deref(), Some("US$0.42, 3.5\u{a0}credits"));
        c.clear();
        c.insert("USD".to_string(), 3_000);
        assert_eq!(cost(&c).as_deref(), Some("<US$0.01"));
    }

    #[test]
    fn a_row_has_no_context_or_cost_where_the_provider_said_nothing() {
        let mut u = Usage::starting(1);
        u.count_turn(90_000, 2);
        let w = words("Ada".into(), "/agents/a1".into(), "Helper", &u);
        assert_eq!(
            (w.turns.as_str(), w.time.as_str(), w.approvals.as_str()),
            ("1", "1\u{a0}min", "2")
        );
        assert_eq!(w.context, None);
        assert_eq!(w.cost, None);
        u.context = Some(Context {
            used: 45_000,
            size: 200_000,
        });
        u.report_cost("s", "USD", 1_250_000);
        let w = words("Ada".into(), "/agents/a1".into(), "Helper", &u);
        assert_eq!(w.context.as_deref(), Some("45k of 200k"));
        assert_eq!(w.cost.as_deref(), Some("US$1.25"));
    }

    #[test]
    fn the_note_mentions_context_and_cost_only_when_something_is_missing() {
        let mut full = Usage::starting(1);
        full.context = Some(Context {
            used: 1_000,
            size: 2_000,
        });
        full.report_cost("s", "USD", 10_000);
        let bare = Usage::starting(1);
        let w = |u: &Usage| words("Ada".into(), "/".into(), "Helper", u);
        let since = || Some("21 Nov".to_string());
        assert_eq!(note(since(), &[w(&full)]), "Counted since 21 Nov.");
        assert_eq!(
            note(since(), &[w(&full), w(&bare)]),
            "Counted since 21 Nov. Context and cost show only where the provider reports them."
        );
        let mut context_only = Usage::starting(1);
        context_only.context = full.context;
        assert_eq!(
            note(since(), &[w(&full), w(&context_only)]),
            "Counted since 21 Nov. Cost shows only where the provider reports it."
        );
        assert_eq!(
            note(None, &[w(&bare)]),
            "Context and cost show only where the provider reports them."
        );
    }

    // --- The providers' part ------------------------------------------------------------------

    use melissa_agent::provider_usage::SourceStatus;

    const NOW: u64 = 1_700_000_000;

    fn window(id: &str, label: &str, kind: WindowKind, used: f64) -> Window {
        Window {
            id: id.into(),
            label: label.into(),
            kind,
            used_pct: used,
            resets_at: Some(NOW + 2 * 3600 + 10 * 60),
            window_secs: None,
            model_scope: None,
            status: None,
            source: "test".into(),
            observed_at: NOW,
            pace: None,
        }
    }

    fn provider(id: &str, label: &str, windows: Vec<Window>) -> ProviderUsage {
        ProviderUsage {
            provider: id.into(),
            label: label.into(),
            ready: true,
            not_ready: None,
            plan: None,
            windows,
            credits: Vec::new(),
            cost: Vec::new(),
            tokens: None,
            sources: vec![source(SourceState::Ok, None)],
        }
    }

    fn source(state: SourceState, reason: Option<&str>) -> SourceStatus {
        SourceStatus {
            id: "x.test".into(),
            state,
            reason: reason.map(str::to_string),
            host: None,
            fetched_at: None,
        }
    }

    fn report(providers: Vec<ProviderUsage>) -> UsageReport {
        UsageReport {
            generated_at: NOW,
            checked_at: Some(NOW - 120),
            providers,
        }
    }

    /// Every card state: plenty left, low, nearly gone (by share and by the provider's word),
    /// unavailable, no limits; and a provider that isn't set up is left out.
    #[test]
    fn limit_cards_in_every_state() {
        let mut opus = window("seven_day_opus", "Weekly · Opus", WindowKind::Weekly, 91.0);
        opus.model_scope = Some("Opus".into());
        let mut rejected = window("seven_day", "Weekly", WindowKind::Weekly, 40.0);
        rejected.status = Some("rejected".into());
        let claude = provider(
            "claude",
            "Claude",
            vec![
                window("five_hour", "5-hour", WindowKind::Session, 38.0),
                window("seven_day", "Weekly", WindowKind::Weekly, 70.0),
                opus,
            ],
        );
        let codex = provider("codex-acp", "Codex", vec![rejected]);
        let mut gemini = provider("gemini", "Gemini", Vec::new());
        gemini.sources = vec![source(
            SourceState::Unavailable,
            Some("Google refused the sign-in; Gemini renews it on its next run"),
        )];
        let mut opencode = provider("opencode", "OpenCode", Vec::new());
        opencode.sources = vec![source(
            SourceState::Unavailable,
            Some("error sending request for url (https://opencode.ai/zen/go/v1/usage)"),
        )];
        let grok = provider("grok", "Grok", Vec::new());
        let mut muse = provider("muse", "Muse", Vec::new());
        muse.ready = false;
        muse.not_ready = Some("not installed".into());
        let r = report(vec![claude, codex, gemini, opencode, grok, muse]);

        let part = providers_part(UsageView::Limits, &r, NOW);
        let Part::Limits(groups) = &part else {
            panic!("{part:?}")
        };
        assert_eq!(groups.len(), 5, "Muse isn't set up");
        let Body::Cards(cards) = &groups[0].body else {
            panic!("{:?}", groups[0])
        };
        let seen: Vec<_> = cards
            .iter()
            .map(|c| (c.label.as_str(), c.left_text.as_str(), c.level))
            .collect();
        assert_eq!(
            seen,
            [
                ("Session", "62%", LimitLevel::Plenty),
                ("Weekly", "30%", LimitLevel::Low),
                ("Weekly · Opus", "9%", LimitLevel::NearlyGone),
            ]
        );
        assert_eq!(
            cards[0].resets.as_deref(),
            Some("Resets in 2\u{a0}h 10\u{a0}min")
        );
        let Body::Cards(codex) = &groups[1].body else {
            panic!()
        };
        assert_eq!(
            codex[0].level,
            LimitLevel::NearlyGone,
            "the provider says no more"
        );
        assert_eq!(
            groups[2].body,
            Body::Unavailable("The sign-in expired. It renews with the next chat.".into())
        );
        // Raw error text never reaches the page.
        assert_eq!(
            groups[3].body,
            Body::Unavailable("Couldn't read the limits.".into())
        );
        assert_eq!(
            groups[4].body,
            Body::NoLimits("Grok doesn't report limits.".into())
        );

        let html = part.render().into_string();
        for want in [
            "<span class=\"limit-num\">62%</span> left",
            "data-tone=\"ok\"",
            "data-tone=\"attention\"",
            "data-tone=\"caution\"",
            // A real meter with its value in words.
            "<meter class=\"limit-meter\" min=\"0\" max=\"100\" low=\"67\" high=\"89\" optimum=\"0\" value=\"38\"",
            "aria-valuetext=\"62% left\"",
            "<p class=\"limit-left\">Unavailable</p>",
            "<p class=\"limit-none\">Grok doesn't report limits.</p>",
            "Plan limits",
        ] {
            assert!(html.contains(want), "missing {want} in {html}");
        }
        for absent in ["Muse", "opencode.ai", "pace"] {
            assert!(!html.contains(absent), "unexpected {absent}");
        }
    }

    #[test]
    fn the_thresholds_are_a_third_and_a_tenth_left() {
        use crate::components::{LOW_AT, NEARLY_GONE_AT};
        assert_eq!((LOW_AT, NEARLY_GONE_AT), (33, 10));
        let level = |left| LimitLevel::of(left, None);
        assert_eq!(level(100), LimitLevel::Plenty);
        assert_eq!(level(34), LimitLevel::Plenty);
        assert_eq!(level(33), LimitLevel::Low);
        assert_eq!(level(11), LimitLevel::Low);
        assert_eq!(level(10), LimitLevel::NearlyGone);
        assert_eq!(level(0), LimitLevel::NearlyGone);
        assert_eq!(LimitLevel::of(80, Some("allowed_warning")), LimitLevel::Low);
        assert_eq!(
            LimitLevel::of(5, Some("allowed_warning")),
            LimitLevel::NearlyGone
        );
        assert_eq!(LimitLevel::of(80, Some("rejected")), LimitLevel::NearlyGone);
        assert_eq!(LimitLevel::of(80, Some("allowed")), LimitLevel::Plenty);
    }

    /// The meter's native ranges (it shows the share used) give the same tone as the server.
    #[test]
    fn the_meters_native_ranges_agree_with_the_tones() {
        use crate::components::{LOW_AT, NEARLY_GONE_AT};
        let (low, high) = (100 - LOW_AT, 99 - NEARLY_GONE_AT);
        for left in 0..=100 {
            let used = 100 - left;
            let native = if used < low {
                LimitLevel::Plenty
            } else if used <= high {
                LimitLevel::Low
            } else {
                LimitLevel::NearlyGone
            };
            assert_eq!(native, LimitLevel::of(left, None), "{left}% left");
        }
    }

    #[test]
    fn shares_left_round_and_never_leave_the_scale() {
        let c = |used| card(&window("w", "W", WindowKind::Weekly, used), NOW);
        assert_eq!((c(0.0).left, c(0.0).left_text.as_str()), (100, "100%"));
        assert_eq!((c(99.6).left, c(99.6).left_text.as_str()), (0, "<1%"));
        assert_eq!((c(130.0).left, c(130.0).left_text.as_str()), (0, "0%"));
        assert_eq!(c(37.6).left_text, "62%");
        // A model-wide session window is "Session"; a model's own keeps its label.
        let mut scoped = window("m", "Session · Opus", WindowKind::Session, 1.0);
        scoped.model_scope = Some("Opus".into());
        assert_eq!(card(&scoped, NOW).label, "Session · Opus");
    }

    #[test]
    fn resets_and_checks_in_words() {
        assert_eq!(resets_words(NOW + 30, NOW), "Resets in <1\u{a0}min");
        assert_eq!(
            resets_words(NOW + 3 * 3600 + 45 * 60, NOW),
            "Resets in 3\u{a0}h 45\u{a0}min"
        );
        assert_eq!(resets_words(NOW - 1, NOW), "Resets now");
        let in_days = resets_words(NOW + 3 * 24 * 3600, NOW);
        assert!(
            in_days.starts_with("Resets ") && in_days.contains(':'),
            "{in_days}"
        );
        let later = resets_words(NOW + 20 * 24 * 3600, NOW);
        assert!(
            !later.contains(':') && later.len() > "Resets ".len(),
            "{later}"
        );
        assert_eq!(checked_words(None, NOW), None);
        assert_eq!(
            checked_words(Some(NOW - 5), NOW).as_deref(),
            Some("Checked just now")
        );
        assert_eq!(
            checked_words(Some(NOW - 150), NOW).as_deref(),
            Some("Checked 2 min ago")
        );
        assert_eq!(
            checked_words(Some(NOW - 7300), NOW).as_deref(),
            Some("Checked 2 h ago")
        );
    }

    #[test]
    fn reasons_in_the_uis_words() {
        assert_eq!(
            reason_words("Claude reports no plan limits for this sign-in"),
            None
        );
        assert_eq!(
            reason_words("Grok's billing answer has no usage figures"),
            None
        );
        assert_eq!(reason_words("this key has no Go subscription"), None);
        assert_eq!(
            reason_words("OpenCode refused the Go key"),
            Some("Sign in again to read the limits.")
        );
        assert_eq!(
            reason_words("no Codex sessions yet: numbers appear after the first chat"),
            Some("Numbers come with the next chat.")
        );
        assert_eq!(
            reason_words("no answer within 45 s"),
            Some("No answer from the provider. Check again later.")
        );
        assert_eq!(
            reason_words("OpenCode answered 502"),
            Some("Couldn't read the limits.")
        );
    }

    #[test]
    fn nothing_set_up_says_so_and_cost_and_tokens_show_what_was_reported() {
        let mut off = provider("claude", "Claude", Vec::new());
        off.ready = false;
        assert_eq!(
            providers_part(UsageView::Limits, &report(vec![off]), NOW),
            Part::NoneSetUp
        );
        let html = Part::NoneSetUp.render().into_string();
        assert!(html.contains("href=\"/settings/providers\""), "{html}");

        let mut claude = provider("claude", "Claude", Vec::new());
        claude.cost = vec![melissa_agent::provider_usage::Cost {
            micros: 2_460_000,
            currency: "USD".into(),
            period: melissa_agent::provider_usage::CostPeriod::SinceCounting,
            since: NOW - 86_400,
            source: "test".into(),
        }];
        claude.tokens = Some(melissa_agent::provider_usage::TokenTotals {
            since: NOW - 86_400,
            models: vec![melissa_agent::provider_usage::ModelTokens {
                model: "opus".into(),
                tokens: melissa_agent::provider_usage::TokenCount {
                    input: 182_000,
                    output: 41_000,
                    cache_read: 2_400_000,
                    cache_write: 0,
                    reasoning: 0,
                },
                cost_micros: None,
                currency: None,
            }],
            source: "test".into(),
        });
        let grok = provider("grok", "Grok", Vec::new());
        let r = report(vec![claude, grok]);
        let cost = providers_part(UsageView::Cost, &r, NOW)
            .render()
            .into_string();
        assert!(cost.contains("US$2.46"), "{cost}");
        assert!(cost.contains("<span class=\"usage-line-value quiet\">Not reported</span>"));
        assert!(cost.contains("Counted since"));
        let tokens = providers_part(UsageView::Tokens, &r, NOW)
            .render()
            .into_string();
        for want in ["opus", "182k", "41k", "2.4M"] {
            assert!(tokens.contains(want), "missing {want} in {tokens}");
        }
        assert!(!tokens.contains("Grok"), "Grok reported no tokens");
    }

    #[test]
    fn loading_works_with_script_and_says_so_without() {
        let html = Part::Loading("/ui/usage/providers?view=cost".into())
            .render()
            .into_string();
        assert!(html.contains("hx-get=\"/ui/usage/providers?view=cost\" hx-trigger=\"load\""));
        assert!(html.contains("aria-busy=\"true\""));
        assert!(
            html.contains("<span data-no-script-only>Not checked yet.</span>"),
            "{html}"
        );
    }
}
