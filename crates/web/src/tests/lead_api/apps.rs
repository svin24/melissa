//! The Apps list by person (2026-11-22): the owner sees every app and can show one person's;
//! a member sees only their own, in the list, its JSON and every app route. Checked on the
//! server with real sessions through the sign-in gate.

use melissa_auth::{Auth, Cost, NewSession};

use super::*;
use crate::auth::{COOKIE_PLAIN, CSRF_HEADER};

const HOST: &str = "melissa.example.com";

struct Box3 {
    app: Router,
    owner: NewSession,
    member: NewSession,
    lead: Lead,
}

/// A box with an owner (`ana`) and a member (`bob`), and three apps: Jellyfin installed for
/// the owner, Minecraft for the member, Forgejo from before anyone was recorded.
fn box3() -> Box3 {
    let auth = Auth::in_memory().with_cost(Cost::Test);
    let (code, _) = auth.setup_code().unwrap().unwrap();
    let owner = auth
        .claim(&code, "ana", "owner-password", "test", "Test")
        .unwrap();
    let inv = auth.create_invite(&owner.view.person.id, "test").unwrap();
    let member = auth
        .accept_invite(&inv.token, "bob", "member-password", "test", "Test")
        .unwrap();
    let list = serde_json::json!({"apps": [
        {"app": "jellyfin", "phase": "ready", "kind": "media", "port": 30001,
         "url": "http://203.0.113.10:30001", "installed_for": owner.view.person.id},
        {"app": "minecraft", "phase": "ready", "kind": "games",
         "ports": [{"name": "game", "port": 30002, "protocol": "tcp", "web": false}],
         "installed_for": member.view.person.id},
        {"app": "forgejo", "phase": "stopped", "kind": "developer"},
    ]});
    let nodes = serde_json::json!({"nodes": [{"name": "device-1", "ready": true}]});
    let (board, mcp, lead) = lead_parts_with(
        None,
        false,
        vec![
            Arc::new(Fixed("list_apps", list)) as Arc<dyn Tool>,
            Arc::new(Fixed("cluster_health", nodes)),
        ],
    );
    let app = router_with_settings(board, mcp, lead.clone(), Settings::detached(), auth);
    Box3 {
        app,
        owner,
        member,
        lead,
    }
}

fn as_(s: &NewSession, method: &str, uri: &str) -> axum::http::Request<Body> {
    axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, HOST)
        .header(header::COOKIE, format!("{COOKIE_PLAIN}={}", s.token))
        .header(CSRF_HEADER, s.view.csrf.clone())
        .header("hx-request", "true")
        .header("sec-fetch-site", "same-origin")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::empty())
        .unwrap()
}

async fn page(app: &Router, s: &NewSession, uri: &str) -> (StatusCode, String) {
    let (status, _, body) = send(app, as_(s, "GET", uri)).await;
    (status, body)
}

async fn api(app: &Router, s: &NewSession, uri: &str) -> (StatusCode, Value) {
    let (status, _, body) = send(app, as_(s, "GET", uri)).await;
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}

fn names(v: &Value) -> Vec<String> {
    v["apps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["app"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn the_owner_sees_every_app_with_whose_it_is_and_a_filter() {
    let b = box3();
    let (status, body) = page(&b.app, &b.owner, "/cluster").await;
    assert_eq!(status, StatusCode::OK);
    for app in ["Jellyfin", "Minecraft", "Forgejo"] {
        assert!(
            body.contains(&format!("<span class=\"app-row-name\">{app}</span>")),
            "{app}"
        );
    }
    // Grouped by kind, in order.
    let at = |s: &str| body.find(s).unwrap_or_else(|| panic!("no {s}"));
    assert!(at(">Games<") < at(">Media<") && at(">Media<") < at(">Developer<"));
    // Whose each is: a member's name, the owner's own, nobody known.
    assert!(body.contains("Installed by </span>bob"));
    assert!(body.contains("Installed by </span>ana"));
    assert!(body.contains("Installed by </span>Unknown"));
    // The filter: Everyone (current), Me, bob, Unknown; labelled, swapping the list in place.
    assert!(body.contains("<nav class=\"show-filter\" aria-labelledby=\"apps-show-label\">"));
    assert!(body.contains("id=\"apps-by-everyone\" href=\"/cluster?by=everyone\""));
    assert!(body.contains("hx-get=\"/ui/cluster/apps?by=me\""));
    let bob = &b.member.view.person.id;
    assert!(body.contains(&format!("hx-get=\"/ui/cluster/apps?by={bob}\"")));
    assert!(body.contains("hx-get=\"/ui/cluster/apps?by=unknown\""));
    assert!(body.contains(
        "hx-target=\"#cluster-apps\" hx-swap=\"outerHTML\" aria-current=\"true\">Everyone"
    ));
    assert!(body.contains("3 apps"));

    // One person's apps, in the fragment the filter swaps in.
    let (_, frag) = page(&b.app, &b.owner, &format!("/ui/cluster/apps?by={bob}")).await;
    assert!(frag.starts_with("<div id=\"cluster-apps\""));
    assert!(frag.contains("Minecraft") && !frag.contains("Jellyfin") && !frag.contains("Forgejo"));
    assert!(frag.contains(&format!(
        "hx-get=\"/ui/cluster/apps?by={bob}\" hx-trigger=\"every"
    )));
    assert!(frag.contains("1 app<"));
    let (_, frag) = page(&b.app, &b.owner, "/ui/cluster/apps?by=unknown").await;
    assert!(frag.contains("Forgejo") && !frag.contains("Minecraft"));
    let (_, frag) = page(&b.app, &b.owner, "/cluster?by=me").await;
    assert!(frag.contains("Jellyfin") && !frag.contains("Minecraft"));

    // The JSON says the same.
    let (status, v) = api(&b.app, &b.owner, "/api/apps").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["k3s"], "ready");
    assert_eq!(names(&v), ["jellyfin", "minecraft", "forgejo"]);
    let mc = &v["apps"][1];
    assert_eq!(mc["installed_for"]["name"], "bob");
    assert_eq!(mc["kind"], "games");
    assert!(v["apps"][2]["installed_for"].is_null());
    let (_, v) = api(&b.app, &b.owner, &format!("/api/apps?by={bob}")).await;
    assert_eq!(names(&v), ["minecraft"]);

    // Every app's details are the owner's to open, force delete included.
    for app in ["jellyfin", "minecraft", "forgejo"] {
        let (status, body) = page(&b.app, &b.owner, &format!("/cluster/apps/{app}")).await;
        assert_eq!(status, StatusCode::OK, "{app}");
        assert!(body.contains("Force delete"), "{app}");
    }
}

#[tokio::test]
async fn a_member_sees_and_reaches_only_their_own_apps() {
    let b = box3();
    let (status, body) = page(&b.app, &b.member, "/cluster").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Minecraft"));
    assert!(
        !body.contains("Jellyfin") && !body.contains("Forgejo"),
        "{body}"
    );
    // No filter and no names: they are all theirs.
    assert!(body.contains("Your apps"));
    assert!(!body.contains("show-filter") && !body.contains("Installed by"));
    assert!(body.contains("1 app<"));
    // Asking for someone else's changes nothing.
    let owner = &b.owner.view.person.id;
    for uri in [
        "/cluster?by=everyone".to_string(),
        format!("/ui/cluster/apps?by={owner}"),
        "/ui/cluster/apps?by=unknown".to_string(),
    ] {
        let (_, body) = page(&b.app, &b.member, &uri).await;
        assert!(body.contains("Minecraft"), "{uri}");
        assert!(
            !body.contains("Jellyfin") && !body.contains("Forgejo"),
            "{uri}"
        );
    }
    let (status, v) = api(&b.app, &b.member, "/api/apps?by=everyone").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&v), ["minecraft"]);

    // Their own app's details, without force delete (the owner's).
    let (status, body) = page(&b.app, &b.member, "/cluster/apps/minecraft").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<h2>Minecraft</h2>"));
    assert!(!body.contains("Force delete"));
    let (status, _) = page(&b.app, &b.member, "/ui/apps/minecraft/logs").await;
    assert_eq!(status, StatusCode::OK);

    // Anyone else's app (or one nobody is recorded for) doesn't exist for them.
    for app in ["jellyfin", "forgejo", "nosuchapp"] {
        for uri in [
            format!("/cluster/apps/{app}"),
            format!("/ui/apps/{app}"),
            format!("/ui/apps/{app}/logs"),
            format!("/api/apps/{app}"),
            format!("/api/apps/{app}/logs"),
        ] {
            let (status, _) = page(&b.app, &b.member, &uri).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        }
        for uri in [
            format!("/ui/apps/{app}/stop"),
            format!("/ui/apps/{app}/start"),
            format!("/ui/apps/{app}/icon"),
        ] {
            let (status, _, _) = send(&b.app, as_(&b.member, "POST", &uri)).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        }
    }
    // Force delete stays the owner's, even for their own app.
    let (status, _, _) = send(
        &b.app,
        as_(&b.member, "POST", "/ui/apps/minecraft/force-delete"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn without_k3s_the_list_says_it_isnt_set_up() {
    // Replace the answer: no app platform at all.
    let (board, mcp, lead) = lead_parts_with(
        None,
        false,
        vec![Arc::new(Fixed(
            "list_apps",
            serde_json::json!({"k3s_set_up": false, "k3s_reachable": false}),
        )) as Arc<dyn Tool>],
    );
    let auth = Auth::in_memory().with_cost(Cost::Test);
    let (code, _) = auth.setup_code().unwrap().unwrap();
    let owner = auth
        .claim(&code, "ana", "owner-password", "test", "Test")
        .unwrap();
    let app = router_with_settings(board, mcp, lead, Settings::detached(), auth);
    let (status, body) = page(&app, &owner, "/cluster").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("No apps yet"));
    assert!(
        body.contains("The app platform isn't set up on this device."),
        "{body}"
    );
    assert!(!body.contains("Couldn't") && !body.contains("tone-caution"));
    let (_, v) = api(&app, &owner, "/api/apps").await;
    assert_eq!(v["k3s"], "not_set_up");
}

/// Melissa's chat stream for `s`, from where its page ends.
async fn stream_as(b: &Box3, s: &NewSession) -> Body {
    let (since, epoch) = (b.lead.events().last_seq(), b.lead.events().epoch());
    let uri = format!("/ui/stream?since={since}&epoch={epoch}");
    let res = b.app.clone().oneshot(as_(s, "GET", &uri)).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    res.into_body()
}

/// SSE frames from `body` until one matches `found` (each within 5 s), joined.
async fn frames_until(body: &mut Body, found: impl Fn(&str) -> bool) -> String {
    let mut all = String::new();
    while !found(&all) {
        let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
            .await
            .expect("an SSE frame within 5 s")
            .expect("stream still open")
            .unwrap();
        all.push_str(std::str::from_utf8(&frame.into_data().unwrap()).unwrap());
    }
    all
}

/// The live details panel (merge of the Apps list with live settings, 2026-11-22): the chat's
/// stream runs outside the request's sign-in scope, yet a member's updates still hold only
/// their own apps. A device rename reaches both open panels and headers without a reload.
#[tokio::test]
async fn live_details_updates_keep_each_persons_apps() {
    let b = box3();
    let mut owner = stream_as(&b, &b.owner).await;
    let mut member = stream_as(&b, &b.member).await;

    let mut rename = as_(&b.owner, "POST", "/ui/settings/device-name");
    *rename.body_mut() = Body::from("name=Den+box");
    let (status, _, _) = send(&b.app, rename).await;
    assert_eq!(status, StatusCode::OK);

    let services = |f: &str| {
        f.find("innerHTML:#details-apps")
            .is_some_and(|i| f[i..].contains("Den box"))
    };
    let frames = frames_until(&mut owner, services).await;
    assert!(
        frames.contains("Jellyfin") && frames.contains("Minecraft") && frames.contains("Forgejo"),
        "{frames}"
    );
    let frames = frames_until(&mut member, services).await;
    assert!(frames.contains("Minecraft"), "{frames}");
    assert!(
        !frames.contains("Jellyfin") && !frames.contains("Forgejo"),
        "{frames}"
    );

    // Melissa hands both people's apps to a helper: the panel's recent activity names only
    // the apps the person looking may see.
    let ada = b
        .lead
        .agents()
        .registry()
        .create("Ada", "stuck", None, "the user")
        .unwrap()
        .id;
    for app in ["jellyfin", "minecraft"] {
        b.lead.events().push(LeadEvent::WorkspaceReassigned {
            workspace: app.into(),
            from: None,
            to: ada.clone(),
            by: "lead".into(),
        });
    }
    let activity = |f: &str| -> Option<String> {
        let i = f.rfind("innerHTML:#details-activity")?;
        let rest = &f[i..];
        Some(rest[..rest.find("</ul>").unwrap_or(rest.len())].to_string())
    };
    let done = |f: &str| activity(f).is_some_and(|a| a.contains("Ada looks after Minecraft now"));
    let frames = frames_until(&mut owner, done).await;
    assert!(
        activity(&frames)
            .unwrap()
            .contains("Ada looks after Jellyfin now"),
        "{frames}"
    );
    let frames = frames_until(&mut member, done).await;
    assert!(!activity(&frames).unwrap().contains("Jellyfin"), "{frames}");
}
