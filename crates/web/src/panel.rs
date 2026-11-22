//! Panels as a modal (2026-11-20): Settings, Apps and Usage and resources are pages of their
//! own (`/settings/…`, `/tasks/{id}`, `/cluster`, `/usage`), and melissa.js shows them in a
//! modal over the current page. It asks for one with htmx into `#overlay-body`, so the request
//! carries `HX-Target: overlay-body`; then the page answers with the panel alone (no rail, no
//! `<html>`), see `pages::panel_shell`. Moving to another Settings section while Settings is
//! open asks with `HX-Target: settings-main` and gets the same (melissa.js swaps only that
//! part). Every other request gets the full page, which is also what works without script.
//!
//! The flag travels as a task-local set by [`layer`] around each request, so the many page
//! handlers don't each need the header.

use axum::{
    extract::Request,
    http::{header::VARY, HeaderValue},
    middleware::Next,
    response::Response,
};

use crate::components::{OVERLAY_BODY_ID, SETTINGS_MAIN_ID};

tokio::task_local! {
    static PANEL_ONLY: bool;
}

/// This request asked for the panel alone (it is loaded into the modal).
pub(crate) fn requested() -> bool {
    PANEL_ONLY.try_with(|p| *p).unwrap_or(false)
}

/// Middleware: note whether the request asks for a panel alone, and tell caches the answer
/// depends on it.
pub(crate) async fn layer(req: Request, next: Next) -> Response {
    let panel = req.headers().get("hx-target").is_some_and(|v| {
        v.as_bytes() == OVERLAY_BODY_ID.as_bytes() || v.as_bytes() == SETTINGS_MAIN_ID.as_bytes()
    });
    let mut res = PANEL_ONLY.scope(panel, next.run(req)).await;
    res.headers_mut()
        .append(VARY, HeaderValue::from_static("HX-Target"));
    res
}
