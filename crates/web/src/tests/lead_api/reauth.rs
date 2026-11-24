//! The action that waited for the password (`auth::pending`): applied once after the
//! password, only for the session that asked, only while fresh.

use std::sync::atomic::Ordering;

use melissa_auth::NewSession;

use super::{
    auth::{as_, claimed, store},
    *,
};
use crate::auth::{pending::PENDING_SECS, CSRF_HEADER};

const HOST: &str = "melissa.example.com";

/// Settings → People's "Invite" from the People page (owner, sensitive).
fn invite(s: &NewSession) -> axum::http::Request<Body> {
    as_(s, "POST", "/ui/people/invites")
        .header(CSRF_HEADER, &s.view.csrf)
        .header("hx-request", "true")
        .header("hx-current-url", format!("http://{HOST}/settings/people"))
        .header(header::ORIGIN, format!("http://{HOST}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::empty())
        .unwrap()
}

/// The password form's post.
fn password(s: &NewSession) -> axum::http::Request<Body> {
    let body = format!(
        "_csrf={}&password=owner-password&next=%2Fsettings%2Fpeople",
        s.view.csrf
    );
    as_(s, "POST", "/reauth")
        .header(header::ORIGIN, format!("http://{HOST}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn the_held_action_runs_once_for_its_own_session_while_fresh() {
    let (auth, clock) = store();
    let owner = claimed(&auth);
    let other = auth
        .login("ana", "owner-password", "test", "Other browser")
        .unwrap();
    let (app, _) = app_signed_out(auth.clone());
    clock.fetch_add(melissa_auth::REAUTH_SECS, Ordering::SeqCst);

    // Held: nothing made yet.
    let (status, headers, _) = send(&app, invite(&owner)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(headers.contains_key("hx-redirect"));
    assert!(auth.invites().is_empty());

    // Another session of the same person: its password doesn't run this one's action.
    let (status, _, _) = send(&app, password(&other)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(auth.invites().is_empty(), "not another session's action");

    // The password in: applied, and back to the page it came from (never the action's path).
    let (status, headers, _) = send(&app, password(&owner)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/settings/people");
    assert_eq!(auth.invites().len(), 1, "applied");

    // Once: the password again does nothing more.
    let (status, _, _) = send(&app, password(&owner)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(auth.invites().len(), 1, "used once");

    // Too old: held, then the password after the action expired.
    clock.fetch_add(melissa_auth::REAUTH_SECS, Ordering::SeqCst);
    let (status, _, _) = send(&app, invite(&owner)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    clock.fetch_add(PENDING_SECS, Ordering::SeqCst);
    send(&app, password(&owner)).await;
    assert_eq!(auth.invites().len(), 1, "expired");
}

#[tokio::test]
async fn nothing_is_held_from_the_api_or_another_site() {
    let (auth, clock) = store();
    let owner = claimed(&auth);
    let (app, _) = app_signed_out(auth.clone());
    clock.fetch_add(melissa_auth::REAUTH_SECS, Ordering::SeqCst);

    // Another site's post is refused before anything is held.
    let mut cross = invite(&owner);
    cross
        .headers_mut()
        .insert(header::ORIGIN, "http://evil.example".parse().unwrap());
    let (status, _, _) = send(&app, cross).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // The JSON API answers with where to go; it holds nothing.
    let (status, _, _) = send(
        &app,
        as_(&owner, "POST", "/api/people/invites")
            .header(CSRF_HEADER, &owner.view.csrf)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    send(&app, password(&owner)).await;
    assert!(auth.invites().is_empty());
}
