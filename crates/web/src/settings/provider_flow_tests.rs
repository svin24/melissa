// Router tests for provider logos, Remove provider and Add provider (2026-11-20), included
// into `lib.rs`'s `tests::lead_api::provider_flow`, so they use that module's fixture and
// helpers. No network: the registry and downloads come from a fake fetcher; the ACP agent is
// a small shell script.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use axum::body::Bytes;
use futures::future::BoxFuture;
use melissa_settings::registry;
use melissa_voice::{
    fetch::{Body as FetchBody, Fetch},
    VoiceError,
};

use super::*;

/// Serves fixed bytes per URL and counts every request.
struct FakeNet {
    files: Vec<(String, Vec<u8>)>,
    calls: Arc<AtomicUsize>,
}

struct Once(Option<Bytes>);

impl FetchBody for Once {
    fn len_hint(&self) -> Option<u64> {
        None
    }
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Bytes>, VoiceError>> {
        let piece = self.0.take();
        Box::pin(async move { Ok(piece) })
    }
}

impl Fetch for FakeNet {
    fn get<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Box<dyn FetchBody>, VoiceError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let found = self
            .files
            .iter()
            .find(|(u, _)| u == url)
            .map(|(_, b)| b.clone());
        Box::pin(async move {
            match found {
                Some(b) => Ok(Box::new(Once(Some(Bytes::from(b)))) as Box<dyn FetchBody>),
                None => Err(VoiceError::Download("404".into())),
            }
        })
    }
}

const ARCHIVE_URL: &str = "https://example.com/fake-agent-linux.tar.gz";

/// A fake ACP agent: answers `initialize` (the request id is sent back as it came), then waits.
fn fake_agent_script(mcp_http: bool) -> String {
    let reply = format!(
        r#"{{"jsonrpc":"2.0","id":ID,"result":{{"protocolVersion":1,"agentCapabilities":{{"mcpCapabilities":{{"http":{mcp_http}}}}},"agentInfo":{{"name":"fake","version":"0.1.0"}},"authMethods":[]}}}}"#
    );
    format!(
        "#!/bin/sh\nread -r line\nid=$(printf '%s' \"$line\" | sed -n 's/^{{\"jsonrpc\":\"2.0\",\"id\":\\(\"[^\"]*\"\\|[0-9]*\\),.*/\\1/p')\n\
         printf '%s\\n' '{reply}' | sed \"s/ID/$id/\"\ncat >/dev/null\n"
    )
}

fn write_exec(path: &std::path::Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A tarball holding `fake-agent`, and a registry listing it (with the tarball's real SHA-256)
/// next to an npx-only agent.
fn fake_registry(dir: &std::path::Path) -> (Vec<u8>, Vec<u8>) {
    let src = dir.join("pkg-src");
    write_exec(&src.join("fake-agent"), &fake_agent_script(true));
    let tgz = dir.join("fake.tar.gz");
    let ok = std::process::Command::new("tar")
        .arg("-czf")
        .arg(&tgz)
        .arg("-C")
        .arg(&src)
        .arg("fake-agent")
        .status()
        .unwrap()
        .success();
    assert!(ok);
    let archive = std::fs::read(&tgz).unwrap();
    let sha = melissa_settings::sha256_hex(&archive);
    let reg = serde_json::json!({
        "version": "1.0.0",
        "agents": [
            {
                "id": "fake-agent", "name": "Fake Agent", "version": "0.1.0",
                "description": "A test agent", "website": "https://example.com/fake",
                "distribution": {"binary": {registry::platform(): {
                    "archive": ARCHIVE_URL, "cmd": "./fake-agent", "args": ["acp"], "sha256": sha,
                }}},
            },
            {
                "id": "node-agent", "name": "Node Agent", "version": "2.0.0",
                "description": "Runs with npx", "distribution": {"npx": {"package": "x@2"}},
            },
        ],
    });
    (serde_json::to_vec(&reg).unwrap(), archive)
}

impl SettingsFixture {
    fn with_catalog(mut self, catalog: Catalog) -> Self {
        self.catalog = catalog;
        self
    }

    fn home(&self) -> std::path::PathBuf {
        self.dir.join("home")
    }

    /// A fake network with the registry and the archive; returns the request counter.
    fn with_fake_net(self) -> (Self, Arc<AtomicUsize>) {
        let (reg, archive) = fake_registry(&self.dir);
        let calls = Arc::new(AtomicUsize::new(0));
        let net = FakeNet {
            files: vec![
                (registry::REGISTRY_URL.to_string(), reg),
                (ARCHIVE_URL.to_string(), archive),
            ],
            calls: calls.clone(),
        };
        let catalog = Catalog::with_parts(
            Some(Arc::new(net)),
            Some(self.dir.join("state").join(registry::CACHE_FILE)),
            registry::platform(),
            None,
        );
        (self.with_catalog(catalog), calls)
    }

    fn providers_toml(&self) -> String {
        std::fs::read_to_string(self.dir.join("config/providers.toml")).unwrap_or_default()
    }
}

fn delete(uri: &str) -> axum::http::Request<Body> {
    axum::http::Request::delete(uri)
        .header(header::HOST, "127.0.0.1:8080")
        .body(Body::empty())
        .unwrap()
}

// --- Logos ---------------------------------------------------------------------------------

#[tokio::test]
async fn providers_show_logos_where_allowed_and_letter_marks_otherwise() {
    let fx = SettingsFixture::new("logos");
    let (app, _) = app_with(Some(fx.settings(HostKind::Other, false)));
    let (status, _, html) = send(&app, get("/settings/providers?p=grok")).await;
    assert_eq!(status, StatusCode::OK);
    // Grok: its logo (drawn paths, no hexagon). Claude: the letter mark.
    assert!(html.contains("class=\"pmark plogo\""), "{html}");
    assert!(html.contains("fill-rule=\"evenodd\""));
    assert!(html.contains("<span>C</span>"));
    // Never a remote image or an <img> for a logo.
    assert!(!html.contains("<img"));
    assert!(!html.contains("cdn.agentclientprotocol.com"));
}

// --- Remove provider -----------------------------------------------------------------------

#[tokio::test]
async fn remove_is_refused_while_melissa_uses_the_provider() {
    let fx = SettingsFixture::new("remove-in-use");
    let (app, _) = app_with(Some(fx.settings_with(
        HostKind::Container,
        true,
        InstallWay::Button,
    )));
    // Melissa runs on Claude (the fixture's "stuck" backend).
    let (status, _, html) =
        send(&app, form("/ui/settings/providers/claude/remove", "", true)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        html.contains("Melissa uses Claude Code. Switch Melissa to another provider first."),
        "{html}"
    );
    let (status, body) = send_json(&app, delete("/api/settings/providers/claude")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("Melissa uses Claude Code"));
    // Unknown providers and the hidden echo partner: 404.
    for id in ["nope", "echo"] {
        let (status, _) = send_json(&app, delete(&format!("/api/settings/providers/{id}"))).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{id}");
    }
    assert!(fx.audit().contains("settings.provider.remove"));
    assert!(fx.audit().contains("claude: refused"));
    assert!(!fx.providers_toml().contains("removed"));
}

#[tokio::test]
async fn remove_uninstalls_deletes_keys_hides_it_and_add_brings_it_back() {
    let fx = SettingsFixture::new("remove-ok");
    let home = fx.home();
    // What melissa-setup leaves for Grok, a symlink out of home, and someone else's file.
    write_exec(&home.join(".grok/bin/grok"), "#!/bin/sh\n");
    std::fs::write(home.join(".grok/auth.json"), "{\"token\":\"t\"}").unwrap();
    std::fs::create_dir_all(fx.dir.join("outside")).unwrap();
    std::fs::write(fx.dir.join("outside/keep"), "x").unwrap();
    std::os::unix::fs::symlink(fx.dir.join("outside"), home.join(".grok/link")).unwrap();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let (app, _) = app_with(Some(fx.settings_with(
        HostKind::Container,
        true,
        InstallWay::Button,
    )));
    let (status, _) = send_json(
        &app,
        json_post(
            "/api/settings/providers/grok",
            r#"{"set_env": {"XAI_API_KEY": "secret-xai-value"}}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // The detail pane offers it, confirmed in the page (no browser dialog).
    let (_, _, html) = send(&app, get("/settings/providers?p=grok")).await;
    assert!(html.contains("/ui/settings/providers/grok/remove"));
    assert!(html.contains("Uninstall Grok and delete its sign-in and API key?"));
    assert!(!html.contains("hx-confirm") && !html.contains("confirm("));

    // No JS: a plain form post redirects back.
    let (status, headers, _) =
        send(&app, form("/ui/settings/providers/grok/remove", "", false)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/settings/providers");
    assert!(!home.join(".grok").exists());
    assert!(fx.dir.join("outside/keep").exists());
    // Claude's files are not touched.
    assert!(home.join(".claude").exists());
    let toml = fx.providers_toml();
    assert!(toml.contains("removed = true"), "{toml}");
    assert!(!toml.contains("secret-xai-value") && !toml.contains("XAI_API_KEY"));
    let audit = fx.audit();
    assert!(audit.contains("grok: removed (1 paths deleted)"), "{audit}");
    assert!(!audit.contains("secret-xai-value"));

    // Gone from the list; a second remove is a 404.
    let (_, _, html) = send(&app, get("/settings/providers")).await;
    assert!(!html.contains("/settings/providers?p=grok"), "{html}");
    let (status, _) = send_json(&app, delete("/api/settings/providers/grok")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Add provider offers it again; choosing it continues to the usual Install.
    let (_, _, html) = send(&app, get("/settings/providers/add")).await;
    assert!(html.contains("/ui/settings/providers/add/grok"), "{html}");
    // Claude is added already: no form for it.
    assert!(!html.contains("/ui/settings/providers/add/claude"));
    let (status, headers, _) = send(&app, form("/ui/settings/providers/add/grok", "", true)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["hx-redirect"], "/settings/providers?p=grok");
    assert!(!fx.providers_toml().contains("removed"));
    let (_, _, html) = send(&app, get("/settings/providers?p=grok")).await;
    assert!(html.contains("/settings/providers?p=grok"));
}

#[tokio::test]
async fn removing_from_source_keeps_the_program() {
    let fx = SettingsFixture::new("remove-source");
    write_exec(&fx.home().join(".grok/bin/grok"), "#!/bin/sh\n");
    let (app, _) = app_with(Some(fx.settings(HostKind::Other, false)));
    let (_, _, html) = send(&app, get("/settings/providers?p=grok")).await;
    assert!(html.contains("Its program stays on this computer."));
    let (status, body) = send_json(&app, delete("/api/settings/providers/grok")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["deleted_paths"], 0);
    assert!(fx.home().join(".grok/bin/grok").exists());
}

// --- Add provider --------------------------------------------------------------------------

#[tokio::test]
async fn the_registry_is_fetched_only_when_asked() {
    let (fx, calls) = SettingsFixture::new("registry-explicit").with_fake_net();
    let (app, _) = app_with(Some(fx.settings(HostKind::Other, false)));
    // Page views, the panel, search and the JSON list never fetch.
    for uri in [
        "/settings/providers",
        "/settings/providers/add",
        "/settings/providers/add?q=fake",
        "/ui/settings/providers/registry?q=fake",
        "/api/settings/providers/registry",
    ] {
        let (status, _, _) = send(&app, get(uri)).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let (_, _, html) = send(&app, get("/settings/providers/add")).await;
    assert!(html.contains("Load the list"));

    // The owner presses Load the list: one fetch, then the list (from the cache).
    let (status, _, html) = send(&app, form("/ui/settings/providers/registry", "", true)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        html.contains("Fake Agent") && html.contains("Node Agent"),
        "{html}"
    );
    assert!(html.contains("/ui/settings/providers/registry/fake-agent/add"));
    assert!(html.contains("Needs Node.js") && html.contains("Enter manually"));
    assert!(html.contains("rel=\"noopener noreferrer\""));
    // Search filters the cached copy, with or without script.
    let (_, _, html) = send(&app, get("/ui/settings/providers/registry?q=node")).await;
    assert!(html.contains("Node Agent") && !html.contains("Fake Agent"));
    let (_, _, html) = send(&app, get("/settings/providers/add?q=fake")).await;
    assert!(html.contains("Fake Agent") && !html.contains("Node Agent"));
    // That is a plain GET form, so search works without script too.
    assert!(html.contains(r#"method="get" action="/settings/providers/add""#));
    let (_, body) = send_json(&app, get("/api/settings/providers/registry")).await;
    assert_eq!(body["agents"].as_array().unwrap().len(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(fx.audit().contains("settings.provider.registry"));
}

#[tokio::test]
async fn a_local_acp_command_is_checked_before_it_is_saved() {
    let fx = SettingsFixture::new("manual");
    let agent = fx.dir.join("agents/my-agent");
    write_exec(&agent, &fake_agent_script(true));
    let not_acp = fx.dir.join("agents/not-acp");
    write_exec(&not_acp, "#!/bin/sh\necho hello\n");
    let no_tools = fx.dir.join("agents/no-tools");
    write_exec(&no_tools, &fake_agent_script(false));
    let (app, _) = app_with(Some(fx.settings(HostKind::Other, false)));
    let (_, _, html) = send(&app, get("/settings/providers/add?step=manual")).await;
    assert!(html.contains("/ui/settings/providers/manual"));

    let body = |name: &str, cmd: &std::path::Path, extra: &str| {
        format!(
            "name={name}&command={}{extra}",
            cmd.display().to_string().replace('/', "%2F")
        )
    };
    for (cmd, want) in [
        (&not_acp, "didn&#39;t answer as an ACP agent"),
        (&no_tools, "can&#39;t use Melissa&#39;s tools"),
    ] {
        let (status, _, html) = send(
            &app,
            form("/ui/settings/providers/manual", &body("X", cmd, ""), true),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{html}");
        assert!(
            html.contains(want) || html.contains(&want.replace("&#39;", "'")),
            "{html}"
        );
    }
    // Relative paths and built-in names are refused before anything runs.
    let (status, _, html) = send(
        &app,
        form(
            "/ui/settings/providers/manual",
            "name=X&command=.%2Fagent",
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(html.contains("full path"), "{html}");
    assert!(fx.providers_toml().is_empty());

    // A real (fake) ACP agent: saved, then step 2.
    let (status, headers, _) = send(
        &app,
        form(
            "/ui/settings/providers/manual",
            &body("My+agent", &agent, "+acp"),
            false,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        headers[header::LOCATION],
        "/settings/providers/add?step=identity&id=my-agent"
    );
    let toml = fx.providers_toml();
    assert!(
        toml.contains("[added.my-agent]") && toml.contains("label = \"My agent\""),
        "{toml}"
    );
    assert!(toml.contains("args = [\"acp\"]"));
    assert!(fx.audit().contains("my-agent: added by hand (fake 0.1.0)"));
    // The same again: already added.
    let (status, _, _) = send(
        &app,
        form(
            "/ui/settings/providers/manual",
            &body("Y", &agent, ""),
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn adding_a_registry_agent_end_to_end() {
    let (fx, calls) = SettingsFixture::new("registry-add").with_fake_net();
    std::fs::create_dir_all(fx.home()).unwrap();
    let (app, _) = app_with(Some(fx.settings(HostKind::Other, false)));
    let (status, _, _) = send(&app, form("/ui/settings/providers/registry", "", true)).await;
    assert_eq!(status, StatusCode::OK);

    // Step 1: Add. The install runs in the background; the list polls while it does.
    let (status, _, html) = send(
        &app,
        form("/ui/settings/providers/registry/fake-agent/add", "", true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        html.contains("hx-trigger=\"every 2s\"") || html.contains("Added"),
        "{html}"
    );
    let mut done = String::new();
    for _ in 0..200 {
        let (_, _, html) = send(&app, get("/ui/settings/providers/registry")).await;
        if html.contains("step=identity&amp;id=fake-agent") {
            done = html;
            break;
        }
        assert!(!html.contains("reg-error"), "{html}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!done.is_empty(), "the install never finished");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "registry + archive");
    let dir = fx.home().join(".local/share/melissa-agents/fake-agent");
    assert!(dir.join("0.1.0/fake-agent").is_file());
    assert!(!dir.join("0.1.0.download").exists());
    let toml = fx.providers_toml();
    assert!(
        toml.contains("[added.fake-agent]") && toml.contains("registry = \"fake-agent\""),
        "{toml}"
    );
    assert!(fx
        .audit()
        .contains("fake-agent: installed fake-agent 0.1.0 from the ACP registry"));

    // Step 2: the API key, write-only.
    let (_, _, html) = send(
        &app,
        get("/settings/providers/add?step=identity&id=fake-agent"),
    )
    .await;
    assert!(
        html.contains("FAKE_AGENT_API_KEY") && html.contains("type=\"password\""),
        "{html}"
    );
    let (status, _, html) = send(
        &app,
        form(
            "/ui/settings/providers/new/fake-agent/key",
            "var=PLAIN_NAME&value=v",
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(html.contains("KEY or TOKEN"));
    let (status, _, html) = send(
        &app,
        form(
            "/ui/settings/providers/new/fake-agent/key",
            "var=FAKE_AGENT_API_KEY&value=sk-fake-secret",
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("API key saved.") && !html.contains("sk-fake-secret"));
    assert!(!fx.audit().contains("sk-fake-secret"));

    // Step 3: hosts (checked), then Finish goes back to Providers.
    let (status, _, _) = send(
        &app,
        form(
            "/ui/settings/providers/new/fake-agent/finish",
            "hosts=*",
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, headers, _) = send(
        &app,
        form(
            "/ui/settings/providers/new/fake-agent/finish",
            "hosts=api.example.com%0Aauth.example.com",
            false,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        headers[header::LOCATION],
        "/settings/providers?p=fake-agent"
    );
    let toml = fx.providers_toml();
    assert!(
        toml.contains("\"api.example.com\"") && toml.contains("FAKE_AGENT_API_KEY"),
        "{toml}"
    );
    // It runs after the restart: the saved change waits.
    let (_, _, html) = send(&app, get("/settings/providers")).await;
    assert!(
        html.contains("Restart Melissa to apply.") || html.contains("Applying"),
        "{html}"
    );

    // Removing it deletes what Melissa unpacked, and its key.
    let (status, body) = send_json(&app, delete("/api/settings/providers/fake-agent")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!dir.exists());
    let toml = fx.providers_toml();
    assert!(
        !toml.contains("fake-agent") && !toml.contains("FAKE_AGENT_API_KEY"),
        "{toml}"
    );
}

#[tokio::test]
async fn a_download_that_does_not_match_its_checksum_is_refused() {
    let fx = SettingsFixture::new("registry-bad-sum");
    let (reg, _) = fake_registry(&fx.dir);
    let calls = Arc::new(AtomicUsize::new(0));
    let net = FakeNet {
        files: vec![
            (registry::REGISTRY_URL.to_string(), reg),
            (ARCHIVE_URL.to_string(), b"not the archive".to_vec()),
        ],
        calls,
    };
    let cache = fx.dir.join("state").join(registry::CACHE_FILE);
    let fx = fx.with_catalog(Catalog::with_parts(
        Some(Arc::new(net)),
        Some(cache),
        registry::platform(),
        None,
    ));
    std::fs::create_dir_all(fx.home()).unwrap();
    let (app, _) = app_with(Some(fx.settings(HostKind::Other, false)));
    send(&app, form("/ui/settings/providers/registry", "", true)).await;
    send(
        &app,
        form("/ui/settings/providers/registry/fake-agent/add", "", true),
    )
    .await;
    let mut failed = String::new();
    for _ in 0..200 {
        let (_, _, html) = send(&app, get("/ui/settings/providers/registry")).await;
        if html.contains("reg-error") {
            failed = html;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(failed.contains("checksum"), "{failed}");
    assert!(failed.contains("Try again"));
    assert!(!fx.providers_toml().contains("fake-agent"));
    assert!(!fx
        .home()
        .join(".local/share/melissa-agents/fake-agent/0.1.0")
        .exists());
}
