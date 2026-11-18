//! Live checks against a real ACP agent (`grok agent stdio`, sandboxed). Ignored by default:
//! they need Grok installed and logged in and spend a few model calls. Run with
//! `cargo nextest run -p melissa-agent --run-ignored only -E 'binary(acp_live)'`.

// Test-only file: panicking on setup failure is the intended behaviour.
#![allow(clippy::unwrap_used)]

use std::{sync::Arc, time::Duration};

use melissa_agent::{
    session::{
        OpenSession, PermissionDecision, PermissionHandler, PermissionRequest, SessionBackend,
        SessionEvent, StopReason,
    },
    AcpBackend, AcpSpec, BoxFuture,
};

/// Refuses everything: these checks need no tools.
struct RejectAll;
impl PermissionHandler for RejectAll {
    fn decide<'a>(&'a self, _: &'a PermissionRequest) -> BoxFuture<'a, PermissionDecision> {
        Box::pin(async { PermissionDecision::RejectOnce })
    }
}

fn grok() -> AcpBackend {
    let spec = AcpSpec::new(
        "grok-acp",
        "grok",
        &[
            "--permission-mode",
            "default",
            "agent",
            "--no-leader",
            "stdio",
        ],
    );
    let workdir = std::env::temp_dir().join(format!("melissa-acp-live-{}", std::process::id()));
    AcpBackend::new(spec, workdir)
}

#[tokio::test]
#[ignore = "needs a logged-in grok CLI and network"]
async fn grok_cancel_then_second_prompt_in_one_session() {
    let backend = grok();
    assert!(SessionBackend::info(&backend).available, "grok not found");
    let cwd = std::env::temp_dir().join(format!("melissa-acp-live-{}", std::process::id()));
    let opened = backend
        .open(OpenSession::new(
            cwd.clone(),
            Vec::new(),
            Arc::new(RejectAll),
        ))
        .await
        .unwrap();
    let session: Arc<dyn melissa_agent::session::AgentSession> = Arc::from(opened.session);
    let mut events = opened.events;
    eprintln!("capabilities: {:?}", session.capabilities());

    // Turn 1: a long answer, cancelled as soon as text streams.
    let turn = {
        let s = session.clone();
        tokio::spawn(async move {
            s.prompt("Write the numbers from 1 to 400 in words, one per line. No tools.")
                .await
        })
    };
    let first_text = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            match events.recv().await {
                Some(SessionEvent::Text(t)) => break t,
                Some(_) => {}
                None => panic!("session ended"),
            }
        }
    })
    .await
    .unwrap();
    eprintln!("first text: {first_text:?}; cancelling");
    let t0 = std::time::Instant::now();
    session.cancel().await.unwrap();
    let stop = tokio::time::timeout(Duration::from_secs(30), turn)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    eprintln!("turn 1 stop: {stop:?} after {:?}", t0.elapsed());
    assert_eq!(stop, StopReason::Cancelled);

    // Turn 2 in the same session (the Queen keeps one session across prompts).
    while events.try_recv().is_ok() {}
    let stop = tokio::time::timeout(
        Duration::from_secs(90),
        session.prompt("Reply with exactly one word: pong"),
    )
    .await
    .unwrap()
    .unwrap();
    let mut reply = String::new();
    while let Ok(e) = events.try_recv() {
        if let SessionEvent::Text(t) = e {
            reply.push_str(&t);
        }
    }
    eprintln!("turn 2 stop: {stop:?}, reply: {reply:?}");
    assert_eq!(stop, StopReason::EndTurn);
    assert!(reply.to_lowercase().contains("pong"), "{reply}");

    session.close().await.unwrap();
    assert!(session.prompt("again").await.is_err(), "closed session");
    let _ = std::fs::remove_dir_all(cwd);
}
