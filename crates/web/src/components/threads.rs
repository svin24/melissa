//! Agent threads (2026-11-24, owner design "Agent threads"): what agents said to each other,
//! for the owner.
//!
//! - [`event_line`]: "Messaged Ada" / "Message from Melissa" in a chat, the other agent's
//!   icon and a link to the pair's thread. It never shows the message text.
//! - [`thread_panel`]: the thread itself, read-only, in the details panel's slot (a column at
//!   1200px and up, a sheet below): "Melissa ⇄ Ada", then each message with its sender, time
//!   and, for a message Melissa passed on from another helper, the [`relayed_mark`].
//! - [`detail_conversations`]: the details panel's "Conversations" section, one row per agent
//!   with the last message and its time.
//!
//! Part of the fixed component library (`components.rs`). Agent text is untrusted: messages go
//! through [`markdown`], the last-message line is escaped plain text.

use maud::{html, Markup};

use super::{
    activity_icon,
    agent::{avatar, message_head, Avatar, MessageHead},
    markdown,
    mobile::sheet_handle,
    panel::{glyph, Glyph},
    ActivityState, Btn,
};

/// Where the thread panel goes (inside the details panel).
pub const THREAD_SLOT_ID: &str = "thread-slot";
/// The Conversations section of the details panel (replaced whole by live updates).
pub const DETAILS_CONVERSATIONS_ID: &str = "details-conversations";

/// A link that opens a thread: a page without script (`href`), the panel fragment with it
/// (`fetch`, swapped into [`THREAD_SLOT_ID`]; melissa.js shows the panel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadLink<'a> {
    pub href: &'a str,
    pub fetch: &'a str,
}

/// An event line in a chat.
pub struct EventLine<'a> {
    /// "Messaged Ada".
    pub label: &'a str,
    /// The other agent's icon.
    pub avatar: Avatar<'a>,
    pub state: ActivityState,
    /// The thread (the owner's); None: plain text.
    pub link: Option<ThreadLink<'a>>,
}

/// "Messaged Ada" under the agent's name on the turn's first item: the other agent's icon (a
/// fail mark when it could not be sent) and the words, a link to the thread.
pub fn event_line(head: Option<&MessageHead<'_>>, e: &EventLine<'_>) -> Markup {
    let state = match e.state {
        ActivityState::Working => "in progress",
        ActivityState::Done => "done",
        ActivityState::Failed => "failed",
        ActivityState::Stopped => "stopped",
    };
    html! {
        div.msg.msg-event {
            @if let Some(h) = head { (message_head(h)) }
            div.activity.event-line {
                @if e.state == ActivityState::Failed {
                    (activity_icon(e.state))
                } @else {
                    (avatar(&e.avatar, "event-icon"))
                }
                span.label {
                    @match e.link {
                        Some(l) => a href=(l.href) hx-get=(l.fetch) hx-target={ "#" (THREAD_SLOT_ID) }
                            hx-swap="innerHTML" data-thread-open { (e.label) },
                        None => (e.label),
                    }
                    span.visually-hidden { " (" (state) ")" }
                }
            }
        }
    }
}

/// "↳ from Ivy": a message Melissa passed on from another helper. Muted, in the message's
/// head.
pub fn relayed_mark(from: &str) -> Markup {
    html! {
        span.relayed-mark {
            svg.glyph viewBox="0 0 16 16" aria-hidden="true" focusable="false" {
                path d="M4 3v5a2 2 0 0 0 2 2h6.5M10 7.5l2.5 2.5-2.5 2.5" {}
            }
            "from " (from)
        }
    }
}

/// One message of a thread.
pub struct ThreadMessage<'a> {
    pub avatar: Avatar<'a>,
    pub name: &'a str,
    /// "10:42".
    pub time: &'a str,
    /// The day above it ("Today"), on the first message of each day.
    pub day: Option<&'a str>,
    /// Melissa passed it on from this helper.
    pub relayed_from: Option<&'a str>,
    /// Untrusted agent text.
    pub text: &'a str,
}

/// A thread between Melissa and a helper.
pub struct ThreadPanel<'a> {
    pub lead: &'a str,
    pub helper: &'a str,
    pub helper_avatar: Avatar<'a>,
    pub messages: &'a [ThreadMessage<'a>],
    /// Back to the agent details (opened from Conversations); the chat page without script.
    pub back: Option<&'a str>,
    /// Close: the chat page (melissa.js closes it in place).
    pub close: &'a str,
}

fn swap_glyph() -> Markup {
    html! {
        svg.glyph.thread-swap viewBox="0 0 16 16" aria-hidden="true" focusable="false" {
            path d="M3 5.5h9.5M10 3l2.5 2.5L10 8M13 10.5H3.5M6 8l-2.5 2.5L6 13" {}
        }
    }
}

fn back_glyph() -> Markup {
    html! {
        svg.glyph viewBox="0 0 16 16" aria-hidden="true" focusable="false" {
            path d="M10 3 5 8l5 5" {}
        }
    }
}

fn forward_glyph() -> Markup {
    html! {
        svg.glyph.row-forward viewBox="0 0 16 16" aria-hidden="true" focusable="false" {
            path d="m6 3 5 5-5 5" {}
        }
    }
}

/// The thread panel: its head ("Melissa ⇄ Ada", back when opened from Conversations, close)
/// and the messages, oldest first. Read-only: no composer.
pub fn thread_panel(t: &ThreadPanel<'_>) -> Markup {
    html! {
        div.thread-view {
            div.details-head.thread-head {
                (sheet_handle())
                @if let Some(back) = t.back {
                    a.thread-back.(Btn::ghost().icon()) href=(back) data-thread-back
                        title="Back to agent details" {
                        (back_glyph())
                        span.visually-hidden { "Back to agent details" }
                    }
                }
                h2.thread-title #thread-h {
                    span.thread-party { (avatar(&Avatar::Melissa, "av-sm")) span { (t.lead) } }
                    (swap_glyph())
                    span.visually-hidden { " and " }
                    span.thread-party { (avatar(&t.helper_avatar, "av-sm")) span { (t.helper) } }
                }
                a.thread-close.(Btn::ghost().icon()) href=(t.close) data-thread-close
                    title="Close conversation" {
                    (glyph(Glyph::Close))
                    span.visually-hidden { "Close conversation" }
                }
            }
            div.thread-body {
                @if t.messages.is_empty() {
                    p.quiet { "No messages yet." }
                } @else {
                    ol.thread-list aria-labelledby="thread-h" {
                        @for m in t.messages {
                            @if let Some(day) = m.day { li.thread-day { (day) } }
                            li.thread-msg {
                                div.msg-head {
                                    (avatar(&m.avatar, "av-sm"))
                                    h3.msg-name { (m.name) }
                                    span.msg-time { (m.time) }
                                    @if let Some(from) = m.relayed_from { (relayed_mark(from)) }
                                }
                                div.body { (markdown(m.text)) }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One row of the Conversations section.
pub struct ConversationRow<'a> {
    pub avatar: Avatar<'a>,
    pub name: &'a str,
    /// The last message, one line (plain text).
    pub last: &'a str,
    /// "11:21" today, "18 Nov" before.
    pub when: &'a str,
    pub link: ThreadLink<'a>,
}

/// The details panel's "Conversations" section: one row per agent this one talked with, the
/// last message under the name, the time on the right. Nothing (a hidden section) when there
/// are none. `oob`: as a live update (replaces the section).
pub fn detail_conversations(rows: &[ConversationRow<'_>], oob: bool) -> Markup {
    html! {
        section.details-section id=(DETAILS_CONVERSATIONS_ID) aria-labelledby="details-conversations-h"
            hidden[rows.is_empty()] hx-swap-oob=[oob.then_some("true")] {
            h3.details-label #details-conversations-h {
                "Conversations"
                span.count { (rows.len()) }
            }
            ul.conversation-list {
                @for r in rows {
                    li {
                        a.conversation-row href=(r.link.href) hx-get=(r.link.fetch)
                            hx-target={ "#" (THREAD_SLOT_ID) } hx-swap="innerHTML" data-thread-open {
                            (avatar(&r.avatar, "av-sm"))
                            span.conversation-text {
                                span.conversation-name { (r.name) }
                                span.conversation-last { (r.last) }
                            }
                            span.conversation-when { (r.when) }
                            (forward_glyph())
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINK: ThreadLink<'static> = ThreadLink {
        href: "/chat?thread=d-0a1b2c3d.a1",
        fetch: "/ui/threads/d-0a1b2c3d.a1?in=lead",
    };

    const ADA: Avatar<'static> = Avatar::Agent {
        shape: "cube",
        color: "teal",
    };

    #[test]
    fn an_event_line_has_the_other_agents_icon_and_links_to_the_thread() {
        let head = MessageHead {
            avatar: Avatar::Melissa,
            name: "Melissa",
            time: "10:42",
        };
        let html = event_line(
            Some(&head),
            &EventLine {
                label: "Messaged <Ada>",
                avatar: ADA,
                state: ActivityState::Done,
                link: Some(LINK),
            },
        )
        .into_string();
        assert!(html.contains("msg-head"), "{html}");
        assert!(html.contains("avatar ic-teal event-icon"), "{html}");
        // Not the check mark: the other agent's icon (owner, 2026-11-24).
        assert!(!html.contains("icon ok"), "{html}");
        assert!(
            html.contains(r#"href="/chat?thread=d-0a1b2c3d.a1""#),
            "{html}"
        );
        assert!(html.contains(r##"hx-target="#thread-slot""##), "{html}");
        assert!(html.contains("Messaged &lt;Ada&gt;"), "{html}");
        // Without a link (anyone but the owner): the words only.
        let plain = event_line(
            None,
            &EventLine {
                label: "Message from Ada",
                avatar: ADA,
                state: ActivityState::Failed,
                link: None,
            },
        )
        .into_string();
        assert!(!plain.contains("href"), "{plain}");
        assert!(!plain.contains("msg-head"), "{plain}");
        assert!(plain.contains("icon fail"), "{plain}");
    }

    #[test]
    fn a_thread_panel_shows_each_message_and_the_relayed_mark() {
        let messages = [
            ThreadMessage {
                avatar: Avatar::Melissa,
                name: "Melissa",
                time: "10:42",
                day: Some("Today"),
                relayed_from: None,
                text: "Is last night's backup done?",
            },
            ThreadMessage {
                avatar: Avatar::Melissa,
                name: "Melissa",
                time: "11:20",
                day: None,
                relayed_from: Some("Ivy"),
                text: "Which port? <script>alert(1)</script>",
            },
        ];
        let t = ThreadPanel {
            lead: "Melissa",
            helper: "Ada",
            helper_avatar: ADA,
            messages: &messages,
            back: Some("/chat"),
            close: "/chat",
        };
        let html = thread_panel(&t).into_string();
        assert!(html.contains(r#"id="thread-h""#), "{html}");
        assert!(html.contains("data-thread-back"), "{html}");
        assert!(html.contains("data-thread-close"), "{html}");
        assert_eq!(html.matches("thread-day").count(), 1, "{html}");
        assert_eq!(html.matches("relayed-mark").count(), 1, "{html}");
        assert!(html.contains("from Ivy"), "{html}");
        assert!(!html.contains("<script>"), "{html}");
        // Read-only: no composer, no form, no footer line.
        assert!(!html.contains("<form"), "{html}");
        assert!(!html.contains("Read only"), "{html}");
        // Without back: no back button; an empty thread says so.
        let empty = thread_panel(&ThreadPanel {
            back: None,
            messages: &[],
            ..t
        })
        .into_string();
        assert!(!empty.contains("data-thread-back"), "{empty}");
        assert!(empty.contains("No messages yet."), "{empty}");
    }

    #[test]
    fn conversations_rows_and_their_empty_section() {
        let rows = [ConversationRow {
            avatar: Avatar::Melissa,
            name: "Melissa",
            last: "2283, HTTP only.",
            when: "11:21",
            link: LINK,
        }];
        let html = detail_conversations(&rows, false).into_string();
        assert!(html.contains("Conversations"), "{html}");
        assert!(html.contains(r#"<span class="count">1</span>"#), "{html}");
        assert!(html.contains("2283, HTTP only."), "{html}");
        assert!(html.contains("11:21"), "{html}");
        assert!(html.contains("data-thread-open"), "{html}");
        assert!(!html.contains("conversations-h\" hidden"), "{html}");
        let none = detail_conversations(&[], true).into_string();
        assert!(none.contains("conversations-h\" hidden"), "{none}");
        assert!(none.contains(r#"hx-swap-oob="true""#), "{none}");
    }
}
