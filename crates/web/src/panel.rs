//! Partial pages for in-page navigation.
//!
//! Panels as a modal (2026-11-20): Settings, Apps and Usage and resources are pages of their
//! own (`/settings/…`, `/tasks/{id}`, `/cluster`, `/usage`), and melissa.js shows them in a
//! modal over the current page. It asks for one with htmx into `#overlay-body`, so the request
//! carries `HX-Target: overlay-body`; then the page answers with the panel alone (no rail, no
//! `<html>`), see `pages::panel_shell`. Moving to another Settings section while Settings is
//! open asks with `HX-Target: settings-main` and gets the same (melissa.js swaps only that
//! part).
//!
//! Pages without full loads (2026-11-22): moving between the chats and the other pages with
//! the sidebar (`/`, `/chat`, `/agents/{id}`, `/chats`, `/approvals`, …) swaps only the main
//! column. melissa.js asks with htmx into `#main` (`HX-Target: main`); then the page answers
//! with its title, the main column's content and the sidebar's agent groups as an
//! out-of-band update (the selected agent), see `pages::shell`. The sidebar itself stays.
//!
//! Every other request gets the full page, which is also what works without script.
//!
//! The flags travel as task-locals set by [`layer`] around each request, so the many page
//! handlers don't each need the header.

use axum::{
    extract::Request,
    http::{header::VARY, HeaderValue},
    middleware::Next,
    response::Response,
};

use crate::components::{MAIN_ID, OVERLAY_BODY_ID, SETTINGS_MAIN_ID};

/// What part of a page a request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    /// The whole page (a load, a reload, no script).
    Page,
    /// The panel alone, for the modal.
    Panel,
    /// The main column alone, for in-page navigation.
    Main,
}

tokio::task_local! {
    static PART: Part;
}

fn part() -> Part {
    PART.try_with(|p| *p).unwrap_or(Part::Page)
}

/// This request asked for the panel alone (it is loaded into the modal).
pub(crate) fn requested() -> bool {
    part() == Part::Panel
}

/// This request asked for the main column alone (in-page navigation).
pub(crate) fn main_only() -> bool {
    part() == Part::Main
}

/// Which part the request's `HX-Target` asks for (only htmx sets it).
fn part_of(req: &Request) -> Part {
    match req.headers().get("hx-target").map(HeaderValue::as_bytes) {
        Some(t) if t == OVERLAY_BODY_ID.as_bytes() || t == SETTINGS_MAIN_ID.as_bytes() => {
            Part::Panel
        }
        Some(t) if t == MAIN_ID.as_bytes() => Part::Main,
        _ => Part::Page,
    }
}

/// Middleware: note which part of the page the request asks for, and tell caches the answer
/// depends on it.
pub(crate) async fn layer(req: Request, next: Next) -> Response {
    let part = part_of(&req);
    let mut res = PART.scope(part, next.run(req)).await;
    res.headers_mut()
        .append(VARY, HeaderValue::from_static("HX-Target"));
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;

    fn with_target(target: Option<&str>) -> Request {
        let mut b = Request::get("/chat");
        if let Some(t) = target {
            b = b.header("hx-target", t);
        }
        b.body(Body::empty()).unwrap()
    }

    #[test]
    fn the_htmx_target_names_the_part() {
        assert_eq!(part_of(&with_target(None)), Part::Page);
        assert_eq!(part_of(&with_target(Some("overlay-body"))), Part::Panel);
        assert_eq!(part_of(&with_target(Some("settings-main"))), Part::Panel);
        assert_eq!(part_of(&with_target(Some("main"))), Part::Main);
        // Any other target (a form's section, the composer) gets the page as before.
        assert_eq!(part_of(&with_target(Some("composer"))), Part::Page);
        assert_eq!(part_of(&with_target(Some("mainx"))), Part::Page);
        // Outside a request: the whole page.
        assert!(!requested() && !main_only());
    }
}
