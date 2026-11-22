//! Usage and resources (`/usage`, the rail's chart icon; 2026-11-20, owner-requested): per agent
//! (Melissa and each helper) the turns, the time spent in them, the approvals they asked for,
//! and tokens in context and cost only where the provider reports them (counted in the agent
//! layer, `melissa_agent::lead::usage`); then the devices with their resources and the number
//! of running apps. Opens as a modal like Settings (see `crate::panel`).

use std::collections::BTreeMap;

use axum::extract::State;
use maud::Markup;
use melissa_agent::lead::usage::Usage;

use crate::{components::UsageRow, pages, ui::Ui};

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

/// `GET /usage`: the page, or the panel alone for the modal.
pub(crate) async fn page(State(ui): State<Ui>) -> Markup {
    let lead = ui.lead.usage();
    let mut all = vec![(
        words(
            ui.lead.agents().lead_name(),
            "/".into(),
            "Lead agent",
            &lead,
        ),
        lead.since,
    )];
    for a in ui.lead.agents().list() {
        let u = ui.lead.agents().usage_of(&a.id);
        let since = u.since;
        all.push((
            words(a.name.clone(), pages::agent_href(&a.id), "Helper", &u),
            since,
        ));
    }
    let since = all
        .iter()
        .map(|(_, s)| *s)
        .filter(|s| *s > 0)
        .min()
        .map(pages::short_date);
    let all: Vec<Words> = all.into_iter().map(|(w, _)| w).collect();
    let note = note(since, &all);
    let rows: Vec<UsageRow<'_>> = all
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
    ui.framed(|f| pages::usage(f, &rows, &note)).await
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
}
