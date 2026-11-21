//! The details panel beside each chat (2026-11-20, owner design): services, files,
//! permissions, recent activity and schedules; the chat header and the agent menu; a helper's
//! page in Settings → Agents (make, rename, icon, pin, fresh start, delete); adding,
//! referencing and sending files.

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

    // Melissa's chat: "Agent details" with every app grouped by who looks after it, this
    // device, her files, permissions, recent activity and her schedule.
    let (status, _, page) = get_html(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    let p = panel_of(&page);
    assert!(p.contains("<h2 id=\"details-h\">Agent details</h2>"), "{p}");
    assert!(p.contains("On-device services"));
    assert!(p.contains("hx-get=\"/ui/apps/nextcloud\""), "{p}");
    assert!(
        p.contains("<p class=\"service-group\">Ada</p>"),
        "every app grouped by who looks after it: {p}"
    );
    assert!(p.contains("<p class=\"service-name\">device-1</p>"), "{p}");
    assert!(
        p.contains("Morning summary") && !p.contains("Nightly check"),
        "{p}"
    );
    assert!(p.contains("melissa-notes.txt") && !p.contains("ada-notes.txt"));
    assert!(p.contains("Permissions") && p.contains("Nothing asked or allowed ahead."));
    assert!(p.contains("Recent activity"));
    // The header of the chat: the agent's icon, "device / name", the menu (rename too).
    assert!(page.contains("<h1>Melissa</h1>"), "{page}");
    assert!(page.contains("<span class=\"chat-device\">device-1</span>"));
    assert!(page.contains("popovertarget=\"menu-head\""));
    assert!(page.contains("action=\"/ui/fresh-start\""));
    assert!(page.contains("href=\"/settings/agents/lead#agent-name\""));
    assert!(page.contains("id=\"app-dialog\""));

    // Ada's chat: hers only; her app with its key facts, her menu (rename, icon, pin, delete).
    let (_, _, page) = get_html(&app, &format!("/agents/{ada}")).await;
    let p = panel_of(&page);
    assert!(p.contains("<p class=\"service-name\">Nextcloud</p>"), "{p}");
    assert!(!p.contains("service-group"), "only her own apps: {p}");
    assert!(
        p.contains("Nightly check") && !p.contains("Morning summary"),
        "{p}"
    );
    assert!(p.contains("ada-notes.txt") && !p.contains("melissa-notes.txt"));
    // Files go to Melissa or Bo, not to herself.
    assert!(p.contains("<option value=\"lead\">Melissa</option>"), "{p}");
    assert!(p.contains(&format!("<option value=\"{bo}\">Bo</option>")));
    assert!(!p.contains(&format!("<option value=\"{ada}\">")));
    let head = &page[page.find("<header class=\"chat-head\">").unwrap()..];
    let head = &head[..head.find("</header>").unwrap()];
    assert!(head.contains("<h1>Ada</h1>"), "{head}");
    for item in [
        format!("href=\"/settings/agents/{ada}#agent-name\""),
        format!("href=\"/settings/agents/{ada}#agent-icon\""),
        format!("href=\"/settings/agents/{ada}\""),
        format!("action=\"/ui/agents/{ada}/pin\""),
        format!("action=\"/ui/agents/{ada}/fresh-start\""),
        format!("href=\"/settings/agents/{ada}#agent-delete\""),
    ] {
        assert!(head.contains(&item), "{item}: {head}");
    }
    assert!(head.contains("<span>Pin</span>"));
    assert!(!page.contains("hx-confirm"), "no browser dialogs");

    // Her page in Settings → Agents: made by Melissa; she looks after an app, so no Delete.
    let (status, _, page) = get_html(&app, &format!("/settings/agents/{ada}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("By Melissa, "), "{page}");
    assert!(page.contains(&format!("action=\"/ui/settings/agents/{ada}/rename\"")));
    assert!(page.contains("id=\"agent-icon\""));
    assert!(
        page.contains("Looks after Nextcloud. Reassign or remove its apps"),
        "{page}"
    );
    assert!(!page.contains(&format!("/ui/settings/agents/{ada}/delete")));
    // Bo's: made by the user, no apps, deletable (in-page confirmation).
    let (_, _, page) = get_html(&app, &format!("/settings/agents/{bo}")).await;
    assert!(page.contains("By You, "), "{page}");
    assert!(page.contains(&format!("action=\"/ui/settings/agents/{bo}/delete\"")));
    assert!(page.contains("Delete Bo? Its chat, file box and schedules go with it."));
    let (_, _, page) = get_html(&app, &format!("/agents/{bo}")).await;
    assert!(panel_of(&page).contains("No apps yet."));

    // The services section polls; the JSON API says the same.
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
    assert_eq!(body["agent"]["pinned"], false);
    assert!(body["agent"]["icon"]["shape"].is_string());
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

/// The Permissions section: what an agent waits for (with a link to the card) and its
/// "Always allow" rules, removable in place; Recent activity says what happened.
#[tokio::test]
async fn permissions_and_recent_activity_of_an_agent() {
    use melissa_agent::tools::{ActionKey, RuleOffer};
    let (app, lead) = app();
    let ada = lead
        .agents()
        .registry()
        .create("Ada", "stuck", None, "the user")
        .unwrap()
        .id;
    // Her conversation exists (as when she works); a request from her task waits.
    let log = lead.agents().events_of(&ada).unwrap();
    let task = lead.agents().conversation_task_of(&ada).unwrap();
    let offer = RuleOffer {
        key: ActionKey {
            principal: Principal::Agent(ada.clone()),
            tool: "update_app".into(),
            target: Some("minecraft".into()),
        },
        verb: "update".into(),
    };
    let spec = serde_json::json!({
        "app": "minecraft",
        "spec": {"containers": [{"image": "x", "ports": [{"name": "game", "port": 25565, "protocol": "tcp", "public": true}]}]},
    });
    let (c, _rx) =
        lead.confirmations()
            .request_with(task, "deploy_app", &spec, "runs software", Some(offer));
    log.push(LeadEvent::ToolCall {
        turn: None,
        id: "x".into(),
        title: "mcp__melissa__my_apps".into(),
        status: "done".into(),
        ms: None,
    });
    let (_, _, page) = get_html(&app, &format!("/agents/{ada}")).await;
    let p = panel_of(&page);
    assert!(
        p.contains("<span class=\"name\">Network access</span>"),
        "{p}"
    );
    assert!(p.contains("TCP 25565 · waiting for you"), "{p}");
    assert!(
        p.contains(&format!("href=\"/agents/{ada}#f-c{}\"", c.id)),
        "{p}"
    );
    assert!(p.contains(">Review<"), "{p}");
    assert!(p.contains("Checked whose apps are whose"), "{p}");
    // She needs the user: first group of the sidebar, and her header says so.
    assert!(page.contains("<span class=\"pill attention\">Needs permission</span>"));
    let side = &page[page.find("group-attention").expect("Needs attention")..];
    assert!(side.contains("<span class=\"name\">Ada</span>"));
    assert!(side.contains("Permission needed"));

    // Always allow saves a rule; it shows in her panel and goes from there.
    send(
        &app,
        htmx_post(&format!("/ui/confirmations/{}/always", c.id), ""),
    )
    .await;
    let rules = lead.server().registry().permissions().rules();
    assert_eq!(rules.len(), 1);
    let (_, _, page) = get_html(&app, &format!("/agents/{ada}")).await;
    let p = panel_of(&page);
    assert!(
        p.contains("<span class=\"name\">Update Minecraft</span>"),
        "{p}"
    );
    assert!(p.contains("You allowed a request"), "{p}");
    // Melissa's panel doesn't list her rule; another agent's key can't remove it.
    let (_, _, home) = get_html(&app, "/").await;
    assert!(!panel_of(&home).contains("Update Minecraft"));
    let (_, _, html) = send(
        &app,
        htmx_post(
            &format!("/ui/details/lead/rules/{}/remove", rules[0].id),
            "",
        ),
    )
    .await;
    assert!(html.contains("That rule is already gone."), "{html}");
    assert_eq!(lead.server().registry().permissions().rules().len(), 1);
    let (status, _, html) = send(
        &app,
        htmx_post(
            &format!("/ui/details/{ada}/rules/{}/remove", rules[0].id),
            "",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("id=\"details-permissions\""), "{html}");
    assert!(
        html.contains("Removed. It asks you again next time."),
        "{html}"
    );
    assert!(lead.server().registry().permissions().rules().is_empty());
}

#[tokio::test]
async fn helpers_are_made_renamed_pinned_and_deleted_in_their_settings() {
    let store = Arc::new(OwnedBy::default());
    let (app, lead) = app_full(None, Some(store.clone()), false);
    let reg = lead.agents().registry().clone();
    reg.create("Bo", "stuck", None, "the user").unwrap();

    // "+": a name and an icon from our own shapes and colours; then its chat opens.
    let (status, _, page) = get_html(&app, "/settings/agents/new").await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("name=\"shape\" value=\"cube\""), "{page}");
    assert!(page.contains("name=\"color\" value=\"teal\""), "{page}");
    let (status, headers, _) = send(
        &app,
        htmx_post("/ui/settings/agents/new", "name=Ada&shape=bolt&color=rose"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ada = lead.agents().get("Ada").expect("made");
    assert_eq!(
        headers["hx-redirect"],
        format!("/agents/{}", ada.id).as_str()
    );
    assert_eq!(
        (ada.icon.shape.as_str(), ada.icon.color.as_str()),
        ("bolt", "rose")
    );
    let ada = ada.id;
    // Its chat draws its icon.
    let (_, _, page) = get_html(&app, &format!("/agents/{ada}")).await;
    assert!(page.contains("<svg class=\"avatar ic-rose av\""), "{page}");
    // A taken name, or an icon we don't have: the form again, with what was typed.
    let (_, _, html) = send(
        &app,
        htmx_post("/ui/settings/agents/new", "name=bo&shape=bolt&color=rose"),
    )
    .await;
    assert!(
        html.contains("id=\"new-agent\"") && html.contains("value=\"bo\""),
        "{html}"
    );
    let (_, _, html) = send(
        &app,
        htmx_post(
            "/ui/settings/agents/new",
            "name=Cy&shape=hexagon&color=honey",
        ),
    )
    .await;
    assert!(html.contains("Pick a shape and a colour."), "{html}");
    assert!(lead.agents().get("Cy").is_none());

    // Rename: the page reloads to show the name everywhere.
    let base = format!("/ui/settings/agents/{ada}");
    let (status, headers, _) = send(
        &app,
        htmx_post(&format!("{base}/rename"), "name=Ada+Lovelace"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["hx-refresh"], "true");
    assert_eq!(lead.agents().get(&ada).unwrap().name, "Ada Lovelace");
    // A taken name: the section says why.
    let (_, _, html) = send(&app, htmx_post(&format!("{base}/rename"), "name=bo")).await;
    assert!(html.contains("id=\"agent-settings\""), "{html}");
    assert!(
        html.contains("Couldn&#39;t rename it") || html.contains("Couldn't rename it"),
        "{html}"
    );
    // Without script: back to its page.
    let req = post(&format!("{base}/rename"))
        .body(Body::from("name=Ada"))
        .unwrap();
    let (status, headers, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], format!("/settings/agents/{ada}"));
    // Change icon.
    let (_, headers, _) = send(
        &app,
        htmx_post(&format!("{base}/icon"), "shape=leaf&color=green"),
    )
    .await;
    assert_eq!(headers["hx-refresh"], "true");
    assert_eq!(lead.agents().get(&ada).unwrap().icon.shape, "leaf");

    // Pin from its page or its menu (a plain form that goes back where it came from).
    let (_, _, html) = send(&app, htmx_post(&format!("{base}/pin"), "pinned=on")).await;
    assert!(html.contains("Pinned."), "{html}");
    assert!(lead.agents().get(&ada).unwrap().pinned);
    let req = post(&format!("/ui/agents/{ada}/pin"))
        .header(header::REFERER, "http://127.0.0.1:8080/settings/agents")
        .header(header::HOST, "127.0.0.1:8080")
        .body(Body::from("pinned=off"))
        .unwrap();
    let (status, headers, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/settings/agents");
    assert!(!lead.agents().get(&ada).unwrap().pinned);
    // Another site's page is not where it goes back to.
    let req = post(&format!("/ui/agents/{ada}/pin"))
        .header(header::REFERER, "http://example.com/x")
        .header(header::HOST, "127.0.0.1:8080")
        .body(Body::from("pinned=on"))
        .unwrap();
    let (_, headers, _) = send(&app, req).await;
    assert_eq!(headers[header::LOCATION], format!("/agents/{ada}"));
    // The JSON API.
    let (status, body) = post_json(
        &app,
        &format!("/api/agents/{ada}/pin"),
        serde_json::json!({"pinned": false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["pinned"], false);
    let (status, _) = post_json(
        &app,
        &format!("/api/agents/{ada}/icon"),
        serde_json::json!({"shape": "nope", "color": "teal"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Fresh start: the chat goes on with a divider; its memory is gone.
    let (_, _, html) = send(&app, htmx_post(&format!("{base}/fresh-start"), "")).await;
    assert!(html.contains("memory is cleared"), "{html}");
    let (_, _, page) = get_html(&app, &format!("/agents/{ada}")).await;
    assert!(page.contains("<span>Fresh start</span>"), "{page}");

    // Delete while it looks after an app: refused on its page, nothing removed.
    *store.0.lock().unwrap() = Some(ada.clone());
    let del = format!("{base}/delete");
    let mut req = htmx_post(&del, "");
    req.headers_mut()
        .insert("hx-target", "agent-settings".parse().unwrap());
    let (_, headers, html) = send(&app, req).await;
    assert!(headers.get("hx-redirect").is_none());
    assert!(
        html.contains("Reassign or remove its apps first."),
        "{html}"
    );
    assert!(lead.agents().get(&ada).is_some());
    // Free of apps: gone, and the page goes to Melissa's chat.
    *store.0.lock().unwrap() = None;
    let mut req = htmx_post(&del, "");
    req.headers_mut()
        .insert("hx-target", "agent-settings".parse().unwrap());
    let (status, headers, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["hx-redirect"], "/");
    assert!(lead.agents().get(&ada).is_none());
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
        html.contains("plan.txt") && html.contains("Uploaded in this conversation · "),
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
    assert!(panel_of(&page).contains("From Ada · "), "{page}");
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
    assert!(frame.contains("From Ada · "), "{frame}");

    // A turn starts: the header's status is swapped too.
    lead.send("hello").unwrap();
    let mut frames = String::new();
    while !frames.contains("innerHTML:#chat-status") {
        frames.push_str(&next_frame(&mut body).await);
    }
    assert!(frames.contains("<span class=\"pill working\">"), "{frames}");
}

#[tokio::test]
async fn the_lead_agent_is_renamed_like_a_helper() {
    let (app, lead) = app_full(None, None, false);
    lead.agents()
        .registry()
        .create("Bo", "stuck", None, "the lead agent (task 1)")
        .unwrap();

    // Its menu's Rename agent leads to its page: its name only (no icon, pin or delete).
    let (status, _, page) = get_html(&app, "/settings/agents/lead").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        page.contains("action=\"/ui/settings/agents/lead/rename\""),
        "{page}"
    );
    assert!(page.contains("id=\"agent-name\""));
    assert!(page.contains("value=\"Melissa\""));
    for absent in [
        "/ui/settings/agents/lead/delete",
        "/ui/settings/agents/lead/icon",
        "/ui/settings/agents/lead/pin",
    ] {
        assert!(!page.contains(absent), "{absent}");
    }

    // Names stay unique among the lead agent and its helpers: the section says why.
    let (_, _, html) = send(
        &app,
        htmx_post("/ui/settings/agents/lead/rename", "name=bo"),
    )
    .await;
    assert!(html.contains("id=\"agent-settings\""), "{html}");
    assert!(html.contains("already an agent called"), "{html}");
    assert_eq!(lead.agents().lead_name(), "Melissa");

    // Renamed: the page reloads to show the name everywhere.
    let (status, headers, _) = send(
        &app,
        htmx_post("/ui/settings/agents/lead/rename", "name=Athena"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["hx-refresh"], "true");
    assert_eq!(lead.agents().lead_name(), "Athena");
    let (_, _, page) = get_html(&app, "/chat").await;
    assert!(page.contains("<h1>Athena</h1>"), "chat header: {page}");
    assert!(
        page.contains("<span class=\"name\">Athena</span>"),
        "sidebar: {page}"
    );
    assert!(page.contains("Hi, I&#39;m Athena.") || page.contains("Hi, I'm Athena."));
    // The product keeps its name: the brand and the page title.
    assert!(
        page.contains("<span class=\"brand-word\">Melissa</span>"),
        "brand: {page}"
    );
    assert!(page.contains(" · Melissa</title>"));
    // A helper's chat and settings name it too.
    let bo = lead.agents().get("Bo").unwrap().id;
    let (_, _, page) = get_html(&app, &format!("/settings/agents/{bo}")).await;
    assert!(page.contains("By Athena, "), "{page}");
    let (_, _, page) = get_html(&app, "/settings/agents").await;
    assert!(page.contains("<a href=\"/\">Athena</a>"), "{page}");
    // A helper can't take its name.
    let (_, _, html) = send(
        &app,
        htmx_post(&format!("/ui/settings/agents/{bo}/rename"), "name=athena"),
    )
    .await;
    assert!(html.contains("already an agent called"), "{html}");

    // Without script: back to its page.
    let req = post("/ui/settings/agents/lead/rename")
        .body(Body::from("name=Iris"))
        .unwrap();
    let (status, headers, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/settings/agents/lead");
    assert_eq!(lead.agents().lead_name(), "Iris");

    // The API: by `lead` or by its name, the same rules.
    let (status, body) = send_json(
        &app,
        json_post("/api/agents/lead/rename", r#"{"name":"Ada"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "Ada");
    assert_eq!(body["key"], "lead");
    let (status, _) = send_json(
        &app,
        json_post("/api/agents/Ada/rename", r#"{"name":"BO"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, body) = send_json(
        &app,
        json_post("/api/agents/ada/rename", r#"{"name":"Melissa"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(lead.agents().lead_name(), "Melissa");
}
