//! The action that was waiting for the password (owner, 2026-11-24): a sensitive change
//! (a Network switch, app access, an approval only the owner may give) that met "enter your
//! password again" is applied once the password is in, so nobody has to flip the switch twice.
//!
//! Safety:
//! - Kept on the server only, in memory, under the session's public id and its person: the
//!   browser never holds it, so nothing in a page or URL can bring back someone else's action.
//! - One per session (the latest), gone after [`PENDING_SECS`] and taken out when used: it runs
//!   at most once.
//! - Only what the browser itself sent here with the session's CSRF token, through the
//!   same-origin check, and only from a page (htmx or a form; never the JSON API).
//! - Replayed inside the server, through the whole app with the session's cookie and CSRF
//!   token (every check again); its path is never a place the browser is sent (the page goes
//!   back to `next`, which [`super::safe_next`] keeps on this site).

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard, OnceLock, Weak},
};

use axum::{
    body::{Body, Bytes},
    extract::Request,
    http::{header, HeaderMap, HeaderValue, Method},
    Router,
};
use tower::ServiceExt;

use super::{COOKIE_PLAIN, CSRF_HEADER};

/// How long an action waits for the password (seconds).
pub(crate) const PENDING_SECS: u64 = 3 * 60;
/// Sessions with an action waiting, at most (the oldest goes first).
const MAX_PENDING: usize = 64;

/// One request, as the browser sent it (after the gate took the CSRF field out). No `Debug`:
/// its body may hold a secret (a provider key) and must never reach a log.
#[derive(Clone)]
pub(crate) struct Action {
    method: Method,
    /// Path and query on this site.
    uri: String,
    content_type: Option<HeaderValue>,
    htmx: bool,
    current_url: Option<HeaderValue>,
    body: Bytes,
}

impl Action {
    /// The parts of `req` worth keeping, with its (already read) `body`; None for what is
    /// never replayed (the API, a path that isn't this site's own).
    pub(crate) fn of(req: &Request, body: Bytes) -> Option<Self> {
        let uri = req.uri().path_and_query()?.as_str().to_string();
        if !uri.starts_with('/') || uri.starts_with("//") || super::is_api(req.uri().path()) {
            return None;
        }
        let h = req.headers();
        Some(Self {
            method: req.method().clone(),
            uri,
            content_type: h.get(header::CONTENT_TYPE).cloned(),
            htmx: crate::is_htmx(h),
            current_url: h.get("hx-current-url").cloned(),
            body,
        })
    }

    /// The request again as `token`'s session (CSRF token `csrf`), with the site headers of
    /// `via` (the password form's post, which passed the same-origin check).
    fn into_request(self, via: &HeaderMap, token: &str, csrf: &str) -> Option<Request> {
        let mut b = Request::builder().method(self.method).uri(self.uri);
        for name in ["host", "origin", "sec-fetch-site", "x-forwarded-proto"] {
            if let Some(v) = via.get(name) {
                b = b.header(name, v);
            }
        }
        b = b
            .header(header::COOKIE, format!("{COOKIE_PLAIN}={token}"))
            .header(CSRF_HEADER, csrf);
        if let Some(ct) = self.content_type {
            b = b.header(header::CONTENT_TYPE, ct);
        }
        if self.htmx {
            b = b.header("hx-request", "true");
        }
        if let Some(u) = self.current_url {
            b = b.header("hx-current-url", u);
        }
        b.body(Body::from(self.body)).ok()
    }
}

struct Held {
    person: String,
    at: u64,
    action: Action,
}

/// Set on a handler's answer when it wants the password again (see
/// [`super::require_owner_reauth`]): the gate then holds the request.
#[derive(Clone, Copy)]
pub(crate) struct ReauthAsked;

/// The actions waiting for the password, by session id, and the app to replay them on.
#[derive(Clone, Default)]
pub(crate) struct PendingActions {
    held: Arc<Mutex<HashMap<String, Held>>>,
    /// Weak: the app holds the gate that holds this (see [`with_replay`]).
    app: Arc<OnceLock<Weak<Router>>>,
}

/// `app`, able to replay held actions through itself: the returned router owns `app`, and
/// `pending` (inside `app`'s gate) only a weak handle to it, so nothing keeps itself alive.
pub(crate) fn with_replay(app: Router, pending: &PendingActions) -> Router {
    let app = Arc::new(app);
    let _ = pending.app.set(Arc::downgrade(&app));
    Router::new().fallback_service(tower::service_fn(move |req: Request| {
        let app = Router::clone(&app);
        async move { app.oneshot(req).await }
    }))
}

impl PendingActions {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Held>> {
        // A panic while holding the lock leaves plain data behind; keep serving.
        self.held.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Keep `action` for `session` (of `person`) at `now`, in place of any earlier one.
    pub(crate) fn hold(&self, session: &str, person: &str, now: u64, action: Action) {
        let mut held = self.lock();
        held.retain(|_, h| now.saturating_sub(h.at) < PENDING_SECS);
        if held.len() >= MAX_PENDING && !held.contains_key(session) {
            if let Some(oldest) = held
                .iter()
                .min_by_key(|(_, h)| h.at)
                .map(|(k, _)| k.clone())
            {
                held.remove(&oldest);
            }
        }
        held.insert(
            session.to_string(),
            Held {
                person: person.to_string(),
                at: now,
                action,
            },
        );
    }

    /// Take `session`'s action out (it can't be taken twice); None if there is none, it is
    /// too old, or it was another person's.
    pub(crate) fn take(&self, session: &str, person: &str, now: u64) -> Option<Action> {
        let h = self.lock().remove(session)?;
        (h.person == person && now.saturating_sub(h.at) < PENDING_SECS).then_some(h.action)
    }

    /// Run `a` through the app again as `token`'s session (see [`Action::into_request`]).
    /// The answer is dropped: the page that loads next shows the result.
    pub(crate) async fn replay(&self, a: Action, via: &HeaderMap, token: &str, csrf: &str) {
        let Some(app) = self.app.get().and_then(Weak::upgrade) else {
            return;
        };
        let uri = a.uri.clone();
        let Some(req) = a.into_request(via, token, csrf) else {
            return;
        };
        let Ok(res) = Router::clone(&app).oneshot(req).await;
        if !(res.status().is_success() || res.status().is_redirection()) {
            tracing::info!(%uri, status = %res.status(), "held action not applied");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(uri: &str) -> Action {
        let req = Request::post(uri).body(Body::empty()).unwrap();
        Action::of(&req, Bytes::from_static(b"enabled=on")).unwrap()
    }

    #[test]
    fn an_action_is_taken_once_by_its_own_session_while_fresh() {
        let p = PendingActions::default();
        p.hold("s1", "ana", 100, action("/ui/apps/x/network/internet"));
        assert!(p.take("s2", "ana", 101).is_none(), "another session");
        assert!(p.take("s1", "bob", 101).is_none(), "another person");
        // A wrong person took it out: gone for good.
        assert!(p.take("s1", "ana", 101).is_none());

        p.hold("s1", "ana", 100, action("/a"));
        p.hold("s1", "ana", 110, action("/b"));
        assert_eq!(p.take("s1", "ana", 120).unwrap().uri, "/b", "the latest");
        assert!(p.take("s1", "ana", 121).is_none(), "once");

        p.hold("s1", "ana", 100, action("/a"));
        assert!(p.take("s1", "ana", 100 + PENDING_SECS).is_none(), "too old");
    }

    #[test]
    fn only_this_sites_pages_are_kept() {
        let req = |uri: &str| Request::post(uri).body(Body::empty()).unwrap();
        assert!(Action::of(&req("/api/apps/x/network"), Bytes::new()).is_none());
        assert!(Action::of(&req("http://evil.example/ui/x"), Bytes::new())
            .is_some_and(|a| a.uri == "/ui/x"));
        let p = PendingActions::default();
        for i in 0..MAX_PENDING + 5 {
            p.hold(&format!("s{i}"), "ana", i as u64, action("/a"));
        }
        assert!(p.lock().len() <= MAX_PENDING);
    }
}
