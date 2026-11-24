//! The owner's own chat messages as proof (owner decision, 2026-11-24).
//!
//! Every message the owner types into a chat (web UI, API or the host's CLI, signed in as the
//! owner) gets a short, stable id when its turn starts (`owner_msg` on the transcript's
//! `user_message` event). Agents see the id in front of the message and may cite it, e.g. as
//! `on_behalf_of` of `ask_lead`, to say "the owner asked for this". Melissa checks the id
//! against its own records ([`OwnerMessages::verify`]): only messages the owner typed count,
//! never agent text, a brokered message, a helper's report, a scheduled job's instruction or a
//! tool result, so an agent cannot make one up by writing an id into its own output.
//!
//! Proof never replaces approval: a cited message is shown to whoever decides ("You asked:
//! …", [`you_asked`]), and the owner still answers the confirmation.

use serde::Serialize;

/// A message the owner typed, found by its id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OwnerMessage {
    pub id: String,
    /// Whose chat it is in: `lead`, or the agent's id.
    pub conversation: String,
    /// When it was sent (Unix seconds).
    pub ts: u64,
    /// What the owner wrote (untrusted only in the sense that it is free text; it is the
    /// owner's own).
    pub text: String,
}

/// Where the owner's messages are looked up (the lead agent's and every helper's chat).
pub trait OwnerMessages: Send + Sync {
    /// The owner's message with this id, if there is one. None for anything else (an agent's
    /// text, a brokered message, an unknown id).
    fn verify(&self, id: &str) -> Option<OwnerMessage>;
}

/// Characters of the owner's message quoted on an approval card.
pub const QUOTE_CHARS: usize = 160;

/// The quote for an approval card: `You asked: “…”` (one line, shortened).
pub fn you_asked(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let quoted = if line.chars().count() > QUOTE_CHARS {
        let cut: String = line.chars().take(QUOTE_CHARS - 1).collect();
        format!("{}…", cut.trim_end())
    } else {
        line
    };
    format!("You asked: “{quoted}”")
}

/// Is `id` shaped like an owner message id (`m-` and 12 lowercase hex digits)?
pub fn valid_owner_msg_id(id: &str) -> bool {
    id.strip_prefix("m-").is_some_and(|h| {
        h.len() == 12
            && h.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// A new owner message id (`m-` and 48 random bits as hex). None if the OS has no randomness
/// to give (the message is then sent without an id).
pub fn new_owner_msg_id() -> Option<String> {
    match crate::device::random_hex(6) {
        Ok(h) => Some(format!("m-{h}")),
        Err(e) => {
            tracing::warn!("no id for the owner's message: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use serde_json::json;

    use super::*;
    use crate::tools::{Effect, TaskContext, ToolError, ToolRegistry, Verdict};

    /// Knows one owner message.
    struct One;
    impl OwnerMessages for One {
        fn verify(&self, id: &str) -> Option<OwnerMessage> {
            (id == "m-0123456789ab").then(|| OwnerMessage {
                id: id.into(),
                conversation: "lead".into(),
                ts: 1,
                text: "Let my monitoring app reach every app.".into(),
            })
        }
    }

    #[tokio::test]
    async fn a_cited_owner_message_is_checked_and_quoted_but_never_approves() {
        // A call that always asks and may cite the owner's message.
        let grant = crate::test_support::TestTool::new("grant")
            .effect(Effect::Destructive)
            .citing("on_behalf_of")
            .reply(|_| json!("granted"));
        let reg =
            ToolRegistry::new(vec![grant.arc()], None).with_confirm_timeout(Duration::from_secs(5));
        let mut ctx = TaskContext::default();
        // Nothing to verify against yet: refused.
        let r = reg
            .call(
                1,
                &mut ctx,
                "grant",
                json!({"on_behalf_of": "m-0123456789ab"}),
            )
            .await;
        assert!(matches!(r, Err(ToolError::Refused(_))), "{r:?}");
        reg.set_owner_messages(Arc::new(One));
        // Not an owner message (made up, or agent text): refused before anyone is asked.
        for bad in ["m-ffffffffffff", "[owner message m-0123456789ab]"] {
            let r = reg
                .call(1, &mut ctx, "grant", json!({"on_behalf_of": bad}))
                .await;
            assert!(matches!(r, Err(ToolError::Refused(_))), "{bad}: {r:?}");
        }
        assert!(reg.confirmations().pending().is_empty());
        // The owner's message: quoted on the confirmation, which still waits for a human.
        let call = {
            let reg = reg.clone();
            tokio::spawn(async move {
                let mut ctx = TaskContext::default();
                reg.call(
                    1,
                    &mut ctx,
                    "grant",
                    json!({"on_behalf_of": "m-0123456789ab"}),
                )
                .await
            })
        };
        let c = loop {
            if let Some(c) = reg.confirmations().pending().first().cloned() {
                break c;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert_eq!(
            c.owner_quote.as_deref(),
            Some("You asked: “Let my monitoring app reach every app.”")
        );
        reg.confirmations().resolve(c.id, Verdict::Denied);
        assert_eq!(call.await.unwrap(), Err(ToolError::Denied));
        // Without a citation: asked as before, no quote.
        let call = {
            let reg = reg.clone();
            tokio::spawn(async move {
                let mut ctx = TaskContext::default();
                reg.call(1, &mut ctx, "grant", json!({})).await
            })
        };
        let c = loop {
            if let Some(c) = reg.confirmations().pending().first().cloned() {
                break c;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert_eq!(c.owner_quote, None);
        reg.confirmations().resolve(c.id, Verdict::Approved);
        assert_eq!(call.await.unwrap(), Ok(json!("granted")));
    }

    #[test]
    fn quotes_are_one_shortened_line() {
        assert_eq!(
            you_asked("let my\nmonitoring  app in"),
            "You asked: “let my monitoring app in”"
        );
        let long = "x".repeat(400);
        let q = you_asked(&long);
        assert!(q.ends_with("…”"), "{q}");
        assert!(q.chars().count() < 200);
    }

    #[test]
    fn ids_have_one_shape() {
        let id = new_owner_msg_id().unwrap();
        assert!(valid_owner_msg_id(&id), "{id}");
        assert!(!valid_owner_msg_id("m-123"));
        assert!(!valid_owner_msg_id("m-ABCDEF123456"));
        assert!(!valid_owner_msg_id("x-0123456789ab"));
    }
}
