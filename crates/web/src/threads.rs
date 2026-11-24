//! Agent threads in the web UI (owner design "Agent threads", 2026-11-24): what Melissa and
//! each helper said to each other, read-only, for the owner.
//!
//! Helpers only talk through Melissa, so the UI shows one thread per helper: Melissa ⇄ helper.
//! It holds the pair's own thread (`melissa_agent::lead::threads`: `ask_lead`, `message_agent`,
//! replies) and every message Melissa passed on to the helper from another helper (stored in
//! the two helpers' thread as `relayed`), shown as Melissa's with "from <helper>".
//!
//! - `GET /ui/threads/{helper}?in=<chat>&back=1`: the thread panel (htmx swaps it into the
//!   details panel's slot); `in` is the chat it opens from (`lead` or the helper's id), `back`
//!   adds the button back to the agent details. Owner only (`auth::need`).
//! - Without script a chat page takes `?thread=<helper>` (and `&back=1`) and renders the
//!   thread in the details panel's place; for anyone but the owner the parameter does nothing.
//! - The details panel's "Conversations" section: [`Ui::conversations_of`].

use std::collections::BTreeMap;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use maud::Markup;
use melissa_agent::{
    lead::threads::{Threads, LEAD},
    tools::permissions::Principal,
};
use serde::Deserialize;

use crate::{
    components::{thread_panel, Avatar, ThreadMessage, ThreadPanel},
    feed::Clock,
    pages::Who,
    ui::Ui,
};

/// One message in Melissa's thread with a helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Msg {
    pub ts: u64,
    /// Melissa wrote it (else the helper).
    pub from_lead: bool,
    /// Melissa passed it on from this helper (an agent id).
    pub relayed_from: Option<String>,
    /// Untrusted agent text.
    pub text: String,
}

/// Every helper's thread with Melissa, oldest message first, by helper id.
pub(crate) fn by_helper(threads: &Threads) -> BTreeMap<String, Vec<Msg>> {
    let mut out: BTreeMap<String, Vec<Msg>> = BTreeMap::new();
    for pair in threads.list() {
        let Some(entries) = threads.read(&pair.a, &pair.b) else {
            continue;
        };
        if pair.a == LEAD || pair.b == LEAD {
            let helper = if pair.a == LEAD { &pair.b } else { &pair.a };
            out.entry(helper.clone())
                .or_default()
                .extend(entries.into_iter().map(|e| Msg {
                    ts: e.ts,
                    from_lead: e.from == LEAD,
                    relayed_from: None,
                    text: e.text,
                }));
        } else {
            // Two helpers: only what Melissa relayed, in the receiving helper's thread.
            for e in entries.into_iter().filter(|e| e.delivery == "relayed") {
                out.entry(e.to.clone()).or_default().push(Msg {
                    ts: e.ts,
                    from_lead: true,
                    relayed_from: Some(e.from),
                    text: e.text,
                });
            }
        }
    }
    for msgs in out.values_mut() {
        msgs.sort_by_key(|m| m.ts);
    }
    out
}

/// A helper's name and icon (shape, colour) for a thread; a deleted helper's id gets words
/// and a plain circle.
fn helper_look(ui: &Ui, id: &str) -> (String, (String, String)) {
    match ui.agent(id) {
        Some(a) => (a.name, (a.icon.shape, a.icon.color)),
        None => ("Deleted helper".into(), (String::new(), String::new())),
    }
}

/// One row of the details' Conversations section, owned (the stream compares them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConvData {
    pub name: String,
    /// (shape, colour); None: Melissa.
    pub icon: Option<(String, String)>,
    pub last: String,
    pub when: String,
    pub href: String,
    pub fetch: String,
    /// Messages in the thread: a new one changes the row even when its words and minute
    /// don't (the open thread reloads when the rows change).
    pub count: usize,
}

/// Characters of the last message a Conversations row shows (CSS cuts it to one line).
const LAST_CHARS: usize = 120;

fn one_line(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= LAST_CHARS {
        return line;
    }
    let mut cut: String = line.chars().take(LAST_CHARS - 1).collect();
    cut.push('…');
    cut
}

impl Ui {
    /// The Conversations rows of `owner`'s details panel (newest first): Melissa's lists every
    /// helper she talked with, a helper's lists Melissa. Empty unless `may` (the owner).
    pub(crate) fn conversations_of(&self, owner: &Principal, may: bool) -> Vec<ConvData> {
        if !may {
            return Vec::new();
        }
        let all = by_helper(self.lead.agents().threads());
        let clock = Clock::default();
        let row = |who: Who<'_>, helper: &str, name: String, icon, msgs: &[Msg]| {
            let last = msgs.last()?;
            let (href, fetch) = who.thread_urls(helper, true);
            Some(ConvData {
                name,
                icon,
                last: one_line(&last.text),
                when: clock.short_when(last.ts),
                href,
                fetch,
                count: msgs.len(),
            })
        };
        match owner {
            Principal::Lead => {
                let mut rows: Vec<(u64, ConvData)> = all
                    .iter()
                    .filter_map(|(helper, msgs)| {
                        let ts = msgs.last()?.ts;
                        let (name, icon) = helper_look(self, helper);
                        Some((ts, row(Who::Lead, helper, name, Some(icon), msgs)?))
                    })
                    .collect();
                rows.sort_by_key(|r| std::cmp::Reverse(r.0));
                rows.into_iter().map(|(_, r)| r).collect()
            }
            Principal::Agent(id) => all
                .get(id)
                .and_then(|msgs| {
                    let lead = self.lead.agents().lead_name();
                    row(Who::Agent(id), id, lead, None, msgs)
                })
                .into_iter()
                .collect(),
        }
    }

    /// The thread between Melissa and `helper` as opened from `who`'s chat (`back`: from
    /// Conversations). None if `helper` is not a helper id that may have a thread there.
    pub(crate) fn thread_data(&self, who: Who<'_>, helper: &str, back: bool) -> Option<ThreadData> {
        if !melissa_agent::device::valid_agent_id(helper) {
            return None;
        }
        if let Who::Agent(id) = who {
            if id != helper {
                return None;
            }
        }
        let msgs = by_helper(self.lead.agents().threads())
            .remove(helper)
            .unwrap_or_default();
        let (name, icon) = helper_look(self, helper);
        let clock = Clock::default();
        let mut last_day: Option<u64> = None;
        let lines = msgs
            .into_iter()
            .map(|m| {
                let day = match last_day {
                    Some(d) if clock.same_date(d, m.ts) => None,
                    _ => Some(clock.day(m.ts)),
                };
                last_day = Some(m.ts);
                ThreadLine {
                    from_lead: m.from_lead,
                    time: clock.time(m.ts),
                    day,
                    relayed_from: m.relayed_from.map(|f| helper_look(self, &f).0),
                    text: m.text,
                }
            })
            .collect();
        let chat = who.chat_page();
        Some(ThreadData {
            lead: self.lead.agents().lead_name(),
            helper: name,
            icon,
            lines,
            back: back.then(|| chat.clone()),
            close: chat,
            refresh: who.thread_urls(helper, back).1,
        })
    }
}

/// One message of [`ThreadData`], worded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ThreadLine {
    from_lead: bool,
    time: String,
    day: Option<String>,
    relayed_from: Option<String>,
    text: String,
}

/// A thread ready to render (owned; borrow with [`ThreadData::messages`] and
/// [`ThreadData::panel`]).
pub(crate) struct ThreadData {
    lead: String,
    helper: String,
    icon: (String, String),
    lines: Vec<ThreadLine>,
    back: Option<String>,
    close: String,
    /// Where it loads from (the live update reloads it).
    refresh: String,
}

impl ThreadData {
    fn helper_avatar(&self) -> Avatar<'_> {
        Avatar::Agent {
            shape: &self.icon.0,
            color: &self.icon.1,
        }
    }

    pub(crate) fn messages(&self) -> Vec<ThreadMessage<'_>> {
        self.lines
            .iter()
            .map(|l| ThreadMessage {
                avatar: if l.from_lead {
                    Avatar::Melissa
                } else {
                    self.helper_avatar()
                },
                name: if l.from_lead {
                    &self.lead
                } else {
                    &self.helper
                },
                time: &l.time,
                day: l.day.as_deref(),
                relayed_from: l.relayed_from.as_deref(),
                text: &l.text,
            })
            .collect()
    }

    pub(crate) fn panel<'a>(&'a self, messages: &'a [ThreadMessage<'a>]) -> ThreadPanel<'a> {
        ThreadPanel {
            lead: &self.lead,
            helper: &self.helper,
            helper_avatar: self.helper_avatar(),
            messages,
            back: self.back.as_deref(),
            close: &self.close,
            refresh: Some(&self.refresh),
        }
    }

    pub(crate) fn render(&self) -> Markup {
        let messages = self.messages();
        thread_panel(&self.panel(&messages))
    }
}

#[derive(Deserialize)]
pub(crate) struct ThreadQuery {
    /// The chat it opens from: `lead` or the helper's id.
    #[serde(default, rename = "in")]
    chat: Option<String>,
    #[serde(default)]
    back: Option<String>,
}

/// `GET /ui/threads/{helper}`: the thread panel.
pub(crate) async fn thread(
    State(ui): State<Ui>,
    Path(helper): Path<String>,
    Query(q): Query<ThreadQuery>,
) -> Response {
    // The sign-in gate lets only the owner here; routers without it (tests) are the owner's.
    if !crate::app_list::Looker::current().owner {
        return StatusCode::FORBIDDEN.into_response();
    }
    let who = match q.chat.as_deref() {
        None | Some("lead") => Who::Lead,
        Some(id) => Who::Agent(id),
    };
    match ui.thread_data(who, &helper, q.back.is_some()) {
        Some(t) => t.render().into_response(),
        None => (StatusCode::NOT_FOUND, crate::pages::not_found()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use melissa_agent::lead::threads::ThreadEntry;

    use super::*;

    #[test]
    fn a_helpers_thread_holds_what_melissa_relayed_to_it() {
        let dir = std::env::temp_dir().join(format!("melissa-web-threads-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let t = Threads::new(&dir);
        let (ada, ivy) = ("d-0a1b2c3d.a1", "d-0a1b2c3d.a2");
        t.record(&ThreadEntry::new(
            LEAD,
            ada,
            "message",
            "data",
            "backup done?",
        ));
        t.record(&ThreadEntry::new(ada, LEAD, "reply", "data", "yes"));
        t.record(&ThreadEntry::new(
            ivy,
            ada,
            "message",
            "relayed",
            "which port?",
        ));
        t.record(&ThreadEntry::new(
            ivy,
            LEAD,
            "ask",
            "data",
            "ask Ada for the port",
        ));
        let all = by_helper(&t);
        let ada_thread = &all[ada];
        assert_eq!(ada_thread.len(), 3, "{ada_thread:?}");
        let relayed = ada_thread
            .iter()
            .find(|m| m.relayed_from.is_some())
            .unwrap();
        assert!(relayed.from_lead);
        assert_eq!(relayed.relayed_from.as_deref(), Some(ivy));
        assert_eq!(relayed.text, "which port?");
        // Ivy's thread has only its own request: the relayed message is Ada's.
        assert_eq!(all[ivy].len(), 1);
        assert!(!all[ivy][0].from_lead);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rows_show_one_line() {
        assert_eq!(one_line("a\n\n b  c"), "a b c");
        let long = one_line(&"x".repeat(500));
        assert_eq!(long.chars().count(), LAST_CHARS);
        assert!(long.ends_with('…'));
    }
}
