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
    context: String,
    cost: String,
}

const NOT_REPORTED: &str = "Not reported";

fn words(name: String, href: String, role: &'static str, u: &Usage) -> Words {
    Words {
        name,
        href,
        role,
        turns: u.turns.to_string(),
        time: duration(u.busy_ms),
        approvals: u.approvals.to_string(),
        context: u.context.map_or_else(
            || NOT_REPORTED.to_string(),
            |c| format!("{} of {}", tokens(c.used), tokens(c.size)),
        ),
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

/// Reported cost per unit: "US$0.42", "3.5 credits"; nothing reported: "Not reported".
fn cost(by_unit: &BTreeMap<String, u64>) -> String {
    if by_unit.is_empty() {
        return NOT_REPORTED.into();
    }
    by_unit
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
        .join(", ")
}

/// `GET /usage`: the page, or the panel alone for the modal.
pub(crate) async fn page(State(ui): State<Ui>) -> Markup {
    let lead = ui.lead.usage();
    let mut all = vec![(
        words("Melissa".into(), "/".into(), "Lead agent", &lead),
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
        .map_or_else(
            || "Context and cost show only where the provider reports them.".to_string(),
            |s| {
                format!(
                    "Counted since {}. Context and cost show only where the provider reports them.",
                    pages::short_date(s)
                )
            },
        );
    let rows: Vec<UsageRow<'_>> = all
        .iter()
        .map(|(w, _)| UsageRow {
            name: &w.name,
            href: &w.href,
            role: w.role,
            turns: &w.turns,
            time: &w.time,
            approvals: &w.approvals,
            context: &w.context,
            cost: &w.cost,
        })
        .collect();
    ui.framed(|f| pages::usage(f, &rows, &since)).await
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
        assert_eq!(cost(&c), "Not reported");
        c.insert("USD".to_string(), 420_000);
        assert_eq!(cost(&c), "US$0.42");
        c.insert("credits".to_string(), 3_500_000);
        assert_eq!(cost(&c), "US$0.42, 3.5\u{a0}credits");
        c.clear();
        c.insert("USD".to_string(), 3_000);
        assert_eq!(cost(&c), "<US$0.01");
    }

    #[test]
    fn a_row_says_not_reported_where_the_provider_said_nothing() {
        let mut u = Usage::starting(1);
        u.count_turn(90_000, 2);
        let w = words("Ada".into(), "/agents/a1".into(), "Helper", &u);
        assert_eq!(
            (w.turns.as_str(), w.time.as_str(), w.approvals.as_str()),
            ("1", "1\u{a0}min", "2")
        );
        assert_eq!(w.context, "Not reported");
        assert_eq!(w.cost, "Not reported");
        u.context = Some(Context {
            used: 45_000,
            size: 200_000,
        });
        u.report_cost("s", "USD", 1_250_000);
        let w = words("Ada".into(), "/agents/a1".into(), "Helper", &u);
        assert_eq!(w.context, "45k of 200k");
        assert_eq!(w.cost, "US$1.25");
    }
}
