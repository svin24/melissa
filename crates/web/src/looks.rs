//! Per-browser looks the page has from its first paint (2026-11-22, owner: "the UI jumps").
//! melissa.js remembers a few choices (the pre-alpha banner closed, the sidebar collapsed,
//! the agent details hidden on wide screens) and mirrors each into a plain cookie, so the
//! server renders the page that way instead of the script fixing it up after the first paint.
//! The cookies hold no data and are not secret (the script sets them); a missing or unknown
//! value means the default look. Without script nothing sets them and the page is as before.
//!
//! The light/dark choice works the same way (`melissa_theme`, rendered as `data-theme` on
//! `<html>`); nothing sets it yet, and without it the page follows the system.
//!
//! The values travel as a task-local set by [`layer`], like `panel::requested`. Not to be
//! confused with the Settings preferences (`melissa_settings::prefs`), kept on the server.

use axum::{
    extract::Request,
    http::{header::COOKIE, HeaderMap},
    middleware::Next,
    response::Response,
};

/// Cookie: the pre-alpha banner was closed (`dismissed`).
pub(crate) const BANNER_COOKIE: &str = "melissa_banner";
/// Cookie: the sidebar is collapsed on wide screens (`collapsed`).
pub(crate) const SIDEBAR_COOKIE: &str = "melissa_sidebar";
/// Cookie: the agent details panel is hidden on wide screens (`closed`).
pub(crate) const DETAILS_COOKIE: &str = "melissa_details";
/// Cookie: light or dark chosen for this browser (`light`, `dark`); absent: follow the system.
pub(crate) const THEME_COOKIE: &str = "melissa_theme";

/// A light/dark choice that overrides the system's (`data-theme` on `<html>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Theme {
    Light,
    Dark,
}

impl Theme {
    /// The `data-theme` value, also the `color-scheme` it sets.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }
}

/// The remembered looks of one request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Looks {
    pub banner_dismissed: bool,
    pub sidebar_collapsed: bool,
    pub details_closed: bool,
    /// None: follow the system's light/dark.
    pub theme: Option<Theme>,
}

impl Looks {
    /// Read them from the request's `Cookie` headers.
    pub(crate) fn from_headers(headers: &HeaderMap) -> Self {
        let mut p = Looks::default();
        for value in headers.get_all(COOKIE) {
            let Ok(text) = value.to_str() else { continue };
            for pair in text.split(';') {
                let Some((name, v)) = pair.trim().split_once('=') else {
                    continue;
                };
                match (name, v) {
                    (BANNER_COOKIE, "dismissed") => p.banner_dismissed = true,
                    (SIDEBAR_COOKIE, "collapsed") => p.sidebar_collapsed = true,
                    (DETAILS_COOKIE, "closed") => p.details_closed = true,
                    (THEME_COOKIE, "light") => p.theme = Some(Theme::Light),
                    (THEME_COOKIE, "dark") => p.theme = Some(Theme::Dark),
                    _ => {}
                }
            }
        }
        p
    }
}

tokio::task_local! {
    static LOOKS: Looks;
}

/// The remembered looks of the request being rendered (defaults outside a request).
pub(crate) fn current() -> Looks {
    LOOKS.try_with(|p| *p).unwrap_or_default()
}

/// Middleware: read the looks for the rest of the request.
pub(crate) async fn layer(req: Request, next: Next) -> Response {
    let looks = Looks::from_headers(req.headers());
    LOOKS.scope(looks, next.run(req)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(cookies: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for c in cookies {
            h.append(COOKIE, HeaderValue::from_str(c).unwrap());
        }
        h
    }

    #[test]
    fn reads_known_values_only() {
        assert_eq!(Looks::from_headers(&headers(&[])), Looks::default());
        let p = Looks::from_headers(&headers(&[
            "melissa_session=abc; melissa_banner=dismissed",
            "melissa_sidebar=collapsed;melissa_details=closed; melissa_theme=dark",
        ]));
        assert_eq!(
            p,
            Looks {
                banner_dismissed: true,
                sidebar_collapsed: true,
                details_closed: true,
                theme: Some(Theme::Dark),
            }
        );
        let p = Looks::from_headers(&headers(&["melissa_theme=light"]));
        assert_eq!(p.theme, Some(Theme::Light));
        let p = Looks::from_headers(&headers(&[
            "melissa_banner=yes; melissa_sidebar=open; melissa_details=; melissa_theme=system",
        ]));
        assert_eq!(p, Looks::default());
    }
}
