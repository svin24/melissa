//! The details panel beside each chat (2026-11-20): the agent, its apps, schedules and
//! files; renaming and deleting a helper from it; adding, referencing and sending files.

use melissa_agent::{
    files::AddedBy,
    lead::schedules::{Creator, ScheduleInput, Schedules, TimeZone},
    tools::permissions::Principal,
};

use super::*;

fn schedule(name: &str, agent: Principal) -> ScheduleInput {
    ScheduleInput {
        name: name.into(),
        agent,
        instruction: "Check the backups".into(),
        cron: "0 3 * * *".into(),
        enabled: true,
        max_minutes: None,
    }
}

/// The `<aside>` of a page (the details panel), to look only inside it.
fn panel_of(page: &str) -> &str {
    let start = page
        .find("<aside class=\"details\"")
        .expect("a details panel");
    let end = page[start..].find("</aside>").expect("its end") + start;
    &page[start..end]
}

#[tokio::test]
async fn each_chat_shows_its_own_agents_details() {
    let store = Arc::new(OwnedBy::default());
    let (app, lead) = app_full(None, Some(store.clone()), false);
    let sched = lead.start_schedules(Schedules::in_memory(TimeZone::UTC));
    let reg = lead.agents().registry().clone();
    let ada = reg
        .create("Ada", "stuck", None, "the lead agent (task 1)")
        .unwrap()
        .id;
    let bo = reg.create("Bo", "stuck", None, "the user").unwrap().id;
    *store.0.lock().unwrap() = Some(ada.clone());
    let s = sched.schedules();
    s.create(
        &schedule("Nightly check", Principal::Agent(ada.clone())),
        &Creator::Owner,
    )
    .unwrap();
    s.create(
        &schedule("Morning summary", Principal::Lead),
        &Creator::Owner,
    )
    .unwrap();
    lead.files()
        .save_named(&Principal::Agent(ada.clone()), "ada-notes.txt", b"hers")
        .unwrap();
    lead.files()
        .save_named(&Principal::Lead, "melissa-notes.txt", b"mine")
        .unwrap();

    // Melissa's chat: the lead agent, every app with who looks after it, her schedule, her
    // files. No rename, no delete.
    let (status, _, page) = get_html(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    let p = panel_of(&page);
    assert!(p.contains("<p class=\"agent-name\">Melissa</p>"), "{p}");
    assert!(p.contains("Lead agent"));
    assert!(!p.contains("/rename"), "{p}");
    assert!(!p.contains("/delete\" hx-post=\"/ui/agents"), "{p}");
    assert!(p.contains("hx-get=\"/ui/apps/nextcloud\""), "{p}");
    assert!(
        p.contains(" · Ada"),
        "every app names who looks after it: {p}"
    );
    assert!(
        p.contains("Morning summary") && !p.contains("Nightly check"),
        "{p}"
    );
    assert!(p.contains("melissa-notes.txt") && !p.contains("ada-notes.txt"));
    // The model picker opens from the panel; the app dialog is on the page.
    assert!(p.contains("popovertarget=\"model-picker\""), "{p}");
    assert!(page.contains("id=\"app-dialog\""));

    // Ada's chat: hers only. Made by Melissa; she looks after an app, so no Delete yet.
    let (_, _, page) = get_html(&app, &format!("/agents/{ada}")).await;
    let p = panel_of(&page);
    assert!(p.contains("<p class=\"agent-name\">Ada</p>"), "{p}");
    assert!(p.contains("Helper") && p.contains("By Melissa, "), "{p}");
    assert!(
        p.contains(&format!("action=\"/ui/agents/{ada}/rename\"")),
        "{p}"
    );
    assert!(
        p.contains("Looks after Nextcloud. Reassign or remove its apps"),
        "{p}"
    );
    assert!(!p.contains(&format!("/ui/agents/{ada}/delete")), "{p}");
    assert!(
        p.contains("Nightly check") && !p.contains("Morning summary"),
        "{p}"
    );
    assert!(p.contains("ada-notes.txt") && !p.contains("melissa-notes.txt"));
    // Files go to Melissa or Bo, not to herself.
    assert!(p.contains("<option value=\"lead\">Melissa</option>"), "{p}");
    assert!(p.contains(&format!("<option value=\"{bo}\">Bo</option>")));
    assert!(!p.contains(&format!("<option value=\"{ada}\">")));

    // Bo's chat: made by the user, no apps, deletable (in-page confirmation, no dialog).
    let (_, _, page) = get_html(&app, &format!("/agents/{bo}")).await;
    let p = panel_of(&page);
    assert!(p.contains("By You, "), "{p}");
    assert!(p.contains("None yet."), "{p}");
    assert!(
        p.contains(&format!("action=\"/ui/agents/{bo}/delete\"")),
        "{p}"
    );
    assert!(p.contains("Delete Bo? Its chat, file box and schedules go with it."));
    assert!(p.contains("No schedules."));
    assert!(!page.contains("hx-confirm"), "no browser dialogs");

    // The apps section polls; the JSON API says the same.
    let (status, _, html) = get_html(&app, &format!("/ui/details/{ada}/apps")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        html.contains("Nextcloud") && html.contains("every 15s"),
        "{html}"
    );
    let (status, body) = get_json(&app, "/api/details/Ada").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["agent"]["name"], "Ada");
    assert_eq!(body["agent"]["can_delete"], false);
    assert_eq!(body["apps"][0]["app"], "nextcloud");
    assert_eq!(body["schedules"][0]["name"], "Nightly check");
    assert_eq!(body["files"]["files"][0]["name"], "ada-notes.txt");
    let (_, body) = get_json(&app, "/api/details/lead").await;
    assert_eq!(body["agent"]["role"], "lead");
    assert_eq!(body["schedules"].as_array().unwrap().len(), 1);
    assert_eq!(
        get_json(&app, "/api/details/nobody").await.0,
        StatusCode::NOT_FOUND
    );

    // A schedule is switched off from the panel: only this agent's.
    let id = s
        .list()
        .into_iter()
        .find(|x| x.name == "Nightly check")
        .unwrap()
        .id;
    let (status, _, html) = send(
        &app,
        htmx_post(
            &format!("/ui/details/{ada}/schedules/{id}/enabled"),
            "enabled=off",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Turn on"), "{html}");
    assert!(!s.list().into_iter().find(|x| x.id == id).unwrap().enabled);
    let lead_id = s
        .list()
        .into_iter()
        .find(|x| x.name == "Morning summary")
        .unwrap()
        .id;
    let (_, _, html) = send(
        &app,
        htmx_post(
            &format!("/ui/details/{ada}/schedules/{lead_id}/enabled"),
            "enabled=off",
        ),
    )
    .await;
    assert!(html.contains("That schedule is gone."), "{html}");
    assert!(
        s.list()
            .into_iter()
            .find(|x| x.id == lead_id)
            .unwrap()
            .enabled
    );
}

#[tokio::test]
async fn helpers_are_renamed_and_deleted_from_the_panel() {
    let store = Arc::new(OwnedBy::default());
    let (app, lead) = app_full(None, Some(store.clone()), false);
    let reg = lead.agents().registry().clone();
    let ada = reg.create("Ada", "stuck", None, "the user").unwrap().id;
    reg.create("Bo", "stuck", None, "the user").unwrap();

    // Rename: the page reloads to show the name everywhere.
    let (status, headers, _) = send(
        &app,
        htmx_post(&format!("/ui/agents/{ada}/rename"), "name=Ada+Lovelace"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["hx-refresh"], "true");
    assert_eq!(lead.agents().get(&ada).unwrap().name, "Ada Lovelace");
    // A taken name: the agent section says why.
    let (_, _, html) = send(
        &app,
        htmx_post(&format!("/ui/agents/{ada}/rename"), "name=bo"),
    )
    .await;
    assert!(html.contains("id=\"details-agent\""), "{html}");
    assert!(
        html.contains("Couldn&#39;t rename it") || html.contains("Couldn't rename it"),
        "{html}"
    );
    // Without script: back to the chat.
    let req = post(&format!("/ui/agents/{ada}/rename"))
        .body(Body::from("name=Ada"))
        .unwrap();
    let (status, headers, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], format!("/agents/{ada}"));
    assert_eq!(lead.agents().get(&ada).unwrap().name, "Ada");

    // Delete while she looks after an app: refused in the panel, nothing removed.
    *store.0.lock().unwrap() = Some(ada.clone());
    let del = format!("/ui/agents/{ada}/delete");
    let (_, headers, html) = send(&app, htmx_post(&del, "")).await;
    assert!(headers.get("hx-redirect").is_none());
    assert!(
        html.contains("Ada still looks after nextcloud. Reassign or remove its apps first."),
        "{html}"
    );
    assert!(lead.agents().get(&ada).is_some());
    // Free of apps: gone, and the page goes to Melissa's chat.
    *store.0.lock().unwrap() = None;
    let (status, headers, _) = send(&app, htmx_post(&del, "")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["hx-redirect"], "/");
    assert!(lead.agents().get(&ada).is_none());
    // Without script: a redirect too.
    let req = post("/ui/agents/Bo/delete").body(Body::empty()).unwrap();
    let (status, headers, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/");
    assert!(lead.agents().get("Bo").is_none());
}

#[tokio::test]
async fn files_are_added_referenced_and_sent_from_the_panel() {
    let (app, lead) = app();
    let reg = lead.agents().registry().clone();
    let ada = reg.create("Ada", "stuck", None, "the user").unwrap().id;
    let bo = reg.create("Bo", "stuck", None, "the user").unwrap().id;
    let (ada_box, bo_box) = (Principal::Agent(ada.clone()), Principal::Agent(bo.clone()));

    // Upload straight into Ada's box (no message): the body lists it, by You.
    let (status, _, html) = send(
        &app,
        multipart(
            &format!("/ui/files/{ada}/upload"),
            &[("files", Some("plan.txt"), b"step one")],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Added plan.txt."), "{html}");
    assert!(
        html.contains("plan.txt") && html.contains(" · You<"),
        "{html}"
    );
    assert!(html.contains("hx-swap-oob=\"true\">1<"), "{html}");
    let file = lead.files().list(&ada_box).remove(0);
    assert_eq!(file.by, AddedBy::Owner);
    // Nothing chosen: says so.
    let (_, _, html) = send(&app, multipart(&format!("/ui/files/{ada}/upload"), &[])).await;
    assert!(html.contains("Choose a file first."), "{html}");

    // Reference in chat: the box name, inserted by script or as a draft without it.
    let (_, _, page) = get_html(&app, &format!("/agents/{ada}")).await;
    assert!(
        page.contains("data-insert=\"[file box: plan.txt]\""),
        "{page}"
    );
    let draft = format!("/agents/{ada}?draft=%5Bfile%20box%3A%20plan.txt%5D");
    assert!(page.contains(&format!("href=\"{draft}\"")), "{page}");
    let (_, _, page) = get_html(&app, &draft).await;
    assert!(
        page.contains(">[file box: plan.txt]</textarea>"),
        "the draft is in the message box: {page}"
    );

    // Send to Bo: a copy by default; both have it, Bo's says where it came from.
    let send_url = format!("/ui/files/{ada}/{}/send", file.id);
    let (_, _, html) = send(&app, htmx_post(&send_url, &format!("to={bo}"))).await;
    assert!(html.contains("Copied plan.txt to Bo."), "{html}");
    assert_eq!(lead.files().list(&ada_box).len(), 1);
    let bos = lead.files().list(&bo_box);
    assert_eq!(bos[0].by, AddedBy::Sent);
    let (_, _, page) = get_html(&app, &format!("/agents/{bo}")).await;
    assert!(panel_of(&page).contains(" · From Ada<"), "{page}");
    // Move to Melissa: Ada's is gone.
    let (_, _, html) = send(&app, htmx_post(&send_url, "to=lead&mode=move")).await;
    assert!(html.contains("Moved plan.txt to Melissa."), "{html}");
    assert!(lead.files().list(&ada_box).is_empty());
    assert_eq!(lead.files().list(&Principal::Lead)[0].name, "plan.txt");
    // Gone now; nobody to send to.
    let (_, _, html) = send(&app, htmx_post(&send_url, "to=lead")).await;
    assert!(html.contains("That file is gone."), "{html}");
    let (_, _, html) = send(&app, htmx_post(&send_url, "to=nobody")).await;
    assert!(html.contains("Choose who gets it."), "{html}");
    // Without script: back to the chat.
    let req = post(&format!("/ui/files/{bo}/{}/send", bos[0].id))
        .body(Body::from("to=lead"))
        .unwrap();
    let (status, headers, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], format!("/agents/{bo}"));
    assert_eq!(lead.files().list(&Principal::Lead).len(), 2);

    // The JSON API: upload and transfer (copy unless `move`), by name or key.
    let mut req = multipart(
        "/api/files/Bo/upload",
        &[("files", Some("data.csv"), b"a,b\n")],
    );
    req.headers_mut().remove("hx-request");
    let (status, body) = send_json(&app, req).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["added"][0]["name"], "data.csv");
    assert_eq!(body["added"][0]["by"], "owner");
    let id = body["added"][0]["id"].as_str().unwrap().to_string();
    let (status, body) = post_json(
        &app,
        &format!("/api/files/{bo}/{id}/transfer"),
        serde_json::json!({"to": "Ada", "move": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["moved"], true);
    assert_eq!(body["to"], ada.as_str());
    assert_eq!(body["file"]["by"], "sent");
    assert_eq!(body["file"]["from"], bo.as_str());
    assert_eq!(body["file"]["reference"], "[file box: data.csv]");
    let new_id = body["file"]["id"].as_str().unwrap().to_string();
    for (args, status) in [
        (
            serde_json::json!({"to": "Ada"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (serde_json::json!({"to": "nobody"}), StatusCode::NOT_FOUND),
    ] {
        let (got, body) =
            post_json(&app, &format!("/api/files/{ada}/{new_id}/transfer"), args).await;
        assert_eq!(got, status, "{body}");
    }
    assert_eq!(
        post_json(
            &app,
            &format!("/api/files/{bo}/{id}/transfer"),
            serde_json::json!({"to": "lead"}),
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

/// A full receiving box refuses a transfer; the sender keeps the file.
#[tokio::test]
async fn transfers_stop_at_the_receiving_boxs_cap() {
    let (app, lead) = app();
    let ada = lead
        .agents()
        .registry()
        .create("Ada", "stuck", None, "the user")
        .unwrap()
        .id;
    let ada_box = Principal::Agent(ada.clone());
    lead.files().save_named(&ada_box, "x.txt", b"abc").unwrap();
    let file = lead.files().list(&ada_box).remove(0);
    let dir = lead.files().ensure_dir(&Principal::Lead).unwrap();
    std::fs::File::create(dir.join("huge.bin"))
        .unwrap()
        .set_len(melissa_agent::files::MAX_BOX_BYTES - 1)
        .unwrap();
    let (_, _, html) = send(
        &app,
        htmx_post(&format!("/ui/files/{ada}/{}/send", file.id), "to=lead"),
    )
    .await;
    assert!(
        html.contains("Melissa&#39;s file box is full.")
            || html.contains("Melissa's file box is full."),
        "{html}"
    );
    let (status, _) = post_json(
        &app,
        &format!("/api/files/{ada}/{}/transfer", file.id),
        serde_json::json!({"to": "lead"}),
    )
    .await;
    assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(lead.files().list(&ada_box).len(), 1);
}

/// A file sent to Melissa shows in her open chat at once, and so does her status.
#[tokio::test]
async fn sent_files_and_status_update_the_open_panel() {
    let (app, lead) = app();
    let ada = lead
        .agents()
        .registry()
        .create("Ada", "stuck", None, "the user")
        .unwrap()
        .id;
    let ada_box = Principal::Agent(ada.clone());
    lead.files().save_named(&ada_box, "x.txt", b"abc").unwrap();
    let file = lead.files().list(&ada_box).remove(0);
    let since = lead.events().last_seq();
    let epoch = lead.events().epoch();
    let mut body = app
        .clone()
        .oneshot(get(&format!("/ui/stream?since={since}&epoch={epoch}")))
        .await
        .unwrap()
        .into_body();
    send(
        &app,
        htmx_post(&format!("/ui/files/{ada}/{}/send", file.id), "to=lead"),
    )
    .await;
    let frame = next_frame(&mut body).await;
    assert!(frame.contains("innerHTML:#file-box-body"), "{frame}");
    assert!(frame.contains(" · From Ada<"), "{frame}");

    // A turn starts: the panel's status is swapped too.
    lead.events().push(LeadEvent::UserMessage {
        turn: 1,
        text: "hello".into(),
        attachments: Vec::new(),
        scheduled: None,
    });
    let frame = next_frame(&mut body).await;
    assert!(frame.contains("innerHTML:#details-status"), "{frame}");
}
