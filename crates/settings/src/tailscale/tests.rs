use std::sync::Mutex;

use melissa_agent::tools::{TaskContext, Tool};

use super::*;
use crate::test_support::scratch;

const RUNNING: &str = include_str!("fixtures/running.json");
const NEEDS_LOGIN: &str = include_str!("fixtures/needs_login.json");
const NEEDS_LOGIN_URL: &str = include_str!("fixtures/needs_login_url.json");
const URL: &str = "https://login.tailscale.com/a/0123456789abcdef";
const TARGET: &str = "http://127.0.0.1:8082";
const ALLOW_HTTPS: &str = "https://login.tailscale.com/f/serve?node=n0000000000001";
const OURS: &str = r#"{"TCP":{"443":{"HTTPS":true}},"Web":{"melissa-box.tail0000.ts.net:443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:8082"}}}}}"#;

// --- detection and names -----------------------------------------------------------------

#[test]
fn host_flag_and_management() {
    assert_eq!(parse_host_flag(Some("yes")), Some(true));
    assert_eq!(parse_host_flag(Some(" 1 ")), Some(true));
    assert_eq!(parse_host_flag(Some("no")), Some(false));
    assert_eq!(parse_host_flag(Some("")), None);
    assert_eq!(parse_host_flag(Some("maybe")), None);
    assert_eq!(parse_host_flag(None), None);
    // The host's Tailscale wins, whatever the image ships.
    assert_eq!(management(true, Some("yes")), Management::Host);
    assert_eq!(management(false, Some("yes")), Management::Host);
    assert_eq!(management(true, Some("no")), Management::Melissa);
    // No built-in binaries (Nix/host install), or install.sh didn't check: not Melissa's.
    assert_eq!(management(false, Some("no")), Management::Unavailable);
    assert_eq!(management(true, None), Management::Unavailable);
    assert_eq!(management(true, Some("")), Management::Unavailable);
}

#[test]
fn node_names_are_dns_labels() {
    assert_eq!(node_name(Some("melissa-box")), "melissa-box");
    assert_eq!(
        node_name(Some("Melissa-My_Box.local")),
        "melissa-my-box-local"
    );
    assert_eq!(node_name(Some("--x--")), "x");
    assert_eq!(node_name(Some("")), "melissa");
    assert_eq!(node_name(None), "melissa");
    assert_eq!(node_name(Some("ÄÖ")), "melissa");
    assert_eq!(node_name(Some(&"a".repeat(80))).len(), 63);
}

#[test]
fn setting_file_round_trip_and_the_unit_condition() {
    let dir = scratch("setting");
    let path = dir.join(FILE_NAME);
    assert_eq!(read_setting(&path), Ok(false), "missing: off");
    write_setting(&path, true).unwrap();
    assert_eq!(read_setting(&path), Ok(true));
    assert_eq!(should_run(&path, Some("no")), Ok(true));
    assert_eq!(should_run(&path, None), Ok(true));
    assert_eq!(should_run(&path, Some("yes")), Ok(false), "the host's wins");
    write_setting(&path, false).unwrap();
    assert_eq!(should_run(&path, Some("no")), Ok(false));
    std::fs::write(&path, "enabled = \"yes\"\n").unwrap();
    assert!(read_setting(&path).is_err());
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(dir.join("elsewhere"), &path).unwrap();
    assert!(read_setting(&path)
        .unwrap_err()
        .contains("not a regular file"));
}

// --- status parsing ------------------------------------------------------------------------

#[test]
fn parses_a_running_node() {
    let d = parse_status(RUNNING).unwrap();
    assert_eq!(d.backend, Backend::Running);
    assert_eq!(d.login_url, None);
    let n = d.node.clone().unwrap();
    assert_eq!(n.name, "melissa-box");
    assert_eq!(n.dns_name.as_deref(), Some("melissa-box.tail0000.ts.net"));
    assert_eq!(n.ipv4, Some(Ipv4Addr::new(100, 64, 0, 7)));
    assert_eq!(n.ipv6, "fd7a:115c:a1e0::7".parse().ok());
    let state = state_from(Ok(d));
    assert_eq!(state.as_str(), "connected");
    let sentence = state.sentence();
    assert!(
        sentence.contains("melissa-box") && sentence.contains("100.64.0.7"),
        "{sentence}"
    );
    // MagicDNS off: no DNS name, the address stays.
    let off = RUNNING.replace("\"MagicDNSEnabled\": true", "\"MagicDNSEnabled\": false");
    let n = parse_status(&off).unwrap().node.unwrap();
    assert_eq!(n.dns_name, None);
    assert!(n.ipv4.is_some());
}

#[test]
fn parses_sign_in_states() {
    let d = parse_status(NEEDS_LOGIN).unwrap();
    assert_eq!(d.backend, Backend::NeedsLogin);
    assert_eq!(d.login_url, None);
    assert_eq!(d.node, None, "no node until running");
    assert_eq!(d.health, ["Tailscale is stopped."]);
    assert_eq!(state_from(Ok(d)), State::SignInNeeded);
    let d = parse_status(NEEDS_LOGIN_URL).unwrap();
    assert_eq!(d.login_url.as_ref().map(LoginUrl::expose), Some(URL));
    assert_eq!(state_from(Ok(d)), State::SignInNeeded);
    for (backend, want) in [
        ("NoState", "starting"),
        ("Starting", "starting"),
        ("Stopped", "sign_in_needed"),
        ("NeedsMachineAuth", "needs_approval"),
        ("SomethingNew", "error"),
    ] {
        let json = NEEDS_LOGIN.replace("\"NeedsLogin\"", &format!("\"{backend}\""));
        assert_eq!(state_from(parse_status(&json)).as_str(), want, "{backend}");
    }
    assert!(matches!(
        state_from(Err("not running".into())),
        State::Error(_)
    ));
}

#[test]
fn bad_output_and_foreign_urls_are_refused_without_quoting_them() {
    let err = parse_status("nope https://login.tailscale.com/a/secret").unwrap_err();
    assert!(!err.contains("secret"), "{err}");
    let evil = NEEDS_LOGIN_URL.replace(URL, "https://login.example.com/a/secret");
    let err = parse_status(&evil).unwrap_err();
    assert!(!err.contains("secret"), "{err}");
    for bad in [
        "http://login.tailscale.com/a/x",
        "https://login.tailscale.com.example.com/a/x",
        "https://evil.example/login.tailscale.com/a/x",
        "https://login.tailscale.com",
        "https://login.tailscale.com/a/x y",
        "javascript:alert(1)",
    ] {
        assert_eq!(LoginUrl::parse(bad), None, "{bad}");
    }
    assert!(LoginUrl::parse(URL).is_some());
    // Never printed.
    let url = LoginUrl::parse(URL).unwrap();
    assert_eq!(format!("{url:?}"), "LoginUrl(..)");
    let d = parse_status(NEEDS_LOGIN_URL).unwrap();
    assert!(!format!("{d:?}").contains("0123456789abcdef"));
}

// --- the control -----------------------------------------------------------------------------

/// A fake tailscaled + unit: `start` makes it answer (signed out); `start_login` gives a URL;
/// `login_completes` makes a sign-in finish; `logout` signs out.
#[derive(Default)]
struct Fake {
    running: Mutex<bool>,
    status: Mutex<&'static str>,
    calls: Mutex<Vec<String>>,
    /// A sign-in finishes as soon as it starts (no URL to open).
    login_completes: bool,
    /// The unit fails to start.
    start_fails: bool,
    /// `tailscale serve status --json`.
    serve_config: Mutex<String>,
    /// How `tailscale serve --bg` goes.
    serve_mode: Mutex<ServeMode>,
    /// A waiting `tailscale serve`: its end, and whether it was stopped.
    waiting: Mutex<Option<WaitingCli>>,
}

/// A waiting CLI's end (to send) and its stop signal (to check).
type WaitingCli = (
    tokio::sync::oneshot::Sender<Result<(), String>>,
    tokio::sync::oneshot::Receiver<()>,
);

#[derive(Default, Clone, Copy, PartialEq)]
enum ServeMode {
    #[default]
    Done,
    /// The tailnet has HTTPS off: the CLI prints the page and waits.
    NeedsHttps,
    Fails,
}

impl Fake {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn call(&self, c: &str) {
        self.calls.lock().unwrap().push(c.into());
    }
}

impl Cli for Fake {
    fn status(&self) -> BoxFuture<'_, Result<String, String>> {
        Box::pin(async move {
            if !*self.running.lock().unwrap() {
                return Err("cannot connect".into());
            }
            Ok(self.status.lock().unwrap().to_string())
        })
    }
    fn start_login<'a>(&'a self, node_name: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.call(&format!("up {node_name}"));
            *self.status.lock().unwrap() = if self.login_completes {
                RUNNING
            } else {
                NEEDS_LOGIN_URL
            };
            Ok(())
        })
    }
    fn logout(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.call("logout");
            *self.status.lock().unwrap() = NEEDS_LOGIN;
            Ok(())
        })
    }
    fn set_hostname<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.call(&format!("set --hostname={name}"));
            let leaked: &'static str = Box::leak(
                RUNNING
                    .replace("\"melissa-box", &format!("\"{name}"))
                    .into_boxed_str(),
            );
            *self.status.lock().unwrap() = leaked;
            Ok(())
        })
    }
    fn serve_status(&self) -> BoxFuture<'_, Result<String, String>> {
        Box::pin(async move { Ok(self.serve_config.lock().unwrap().clone()) })
    }
    fn serve_reset(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.call("serve reset");
            *self.serve_config.lock().unwrap() = "{}".into();
            Ok(())
        })
    }
    fn serve_start<'a>(&'a self, target: &'a str) -> BoxFuture<'a, Result<ServeStart, String>> {
        Box::pin(async move {
            self.call(&format!("serve {target}"));
            match *self.serve_mode.lock().unwrap() {
                ServeMode::Done => {
                    *self.serve_config.lock().unwrap() = OURS.into();
                    Ok(ServeStart::Done)
                }
                ServeMode::Fails => Err("`tailscale serve` failed (exit status: 1)".into()),
                ServeMode::NeedsHttps => {
                    let (done_tx, done) = tokio::sync::oneshot::channel();
                    let (cancel, cancelled) = tokio::sync::oneshot::channel();
                    *self.waiting.lock().unwrap() = Some((done_tx, cancelled));
                    Ok(ServeStart::Waiting {
                        url: LoginUrl::parse(ALLOW_HTTPS),
                        done,
                        cancel,
                    })
                }
            }
        })
    }
}

impl Fake {
    /// The owner allowed HTTPS: the waiting CLI finishes.
    fn https_allowed(&self) {
        *self.serve_config.lock().unwrap() = OURS.into();
        let (done, _) = self.waiting.lock().unwrap().take().expect("a waiting CLI");
        done.send(Ok(())).unwrap();
    }

    /// The waiting CLI was stopped.
    fn waiting_stopped(&self) -> bool {
        let mut w = self.waiting.lock().unwrap();
        let (_, cancelled) = w.as_mut().expect("a waiting CLI");
        matches!(
            cancelled.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        )
    }
}

impl Service for Fake {
    fn start(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.call("start");
            if self.start_fails {
                return Err("systemctl start melissa-tailscaled.service failed".into());
            }
            let mut s = self.status.lock().unwrap();
            if s.is_empty() {
                *s = NEEDS_LOGIN;
            }
            *self.running.lock().unwrap() = true;
            Ok(())
        })
    }
    fn stop(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.call("stop");
            *self.running.lock().unwrap() = false;
            Ok(())
        })
    }
}

fn control(dir: &Path, management: Management, fake: Arc<Fake>) -> TailscaleControl {
    TailscaleControl::new(Parts {
        management,
        setting: dir.join(FILE_NAME),
        node_name: "melissa-box".into(),
        serve_target: Some(TARGET.into()),
        ui_port: 8080,
        cli: fake.clone(),
        service: fake,
        audit: Some(AuditLog::new(dir.join("audit.jsonl"))),
        wait: Duration::from_millis(400),
    })
}

fn audit(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("audit.jsonl")).unwrap_or_default()
}

#[tokio::test]
async fn not_managed_when_the_host_runs_tailscale() {
    let dir = scratch("host");
    let fake = Arc::new(Fake::default());
    for m in [Management::Host, Management::Unavailable] {
        let c = control(&dir, m, fake.clone());
        assert_eq!(c.state().await, State::NotManaged);
        assert!(matches!(
            c.enable("ui").await,
            Err(TailscaleError::NotManaged)
        ));
        assert!(matches!(
            c.start_login("ui").await,
            Err(TailscaleError::NotManaged)
        ));
        assert!(c.own_addresses().await.is_empty());
    }
    assert!(fake.calls().is_empty(), "nothing ran: {:?}", fake.calls());
    assert!(!dir.join(FILE_NAME).exists(), "nothing saved");
}

#[tokio::test]
async fn off_by_default_then_enable_sign_in_and_disable() {
    let dir = scratch("flow");
    let fake = Arc::new(Fake::default());
    let c = control(&dir, Management::Melissa, fake.clone());
    assert_eq!(c.state().await, State::Off);
    assert!(matches!(
        c.start_login("ui").await,
        Err(TailscaleError::Not("off"))
    ));

    assert_eq!(c.enable("ui").await.unwrap(), State::SignInNeeded);
    assert!(c.enabled());
    assert!(c.own_addresses().await.is_empty());

    let Login::Url(url) = c.start_login("ui").await.unwrap() else {
        panic!("a URL")
    };
    assert_eq!(url.expose(), URL);
    // A second click reuses the open sign-in.
    assert!(matches!(c.start_login("ui").await, Ok(Login::Url(_))));
    assert_eq!(fake.calls(), ["start", "up melissa-box"]);

    // The owner signs in.
    *fake.status.lock().unwrap() = RUNNING;
    let state = c.state().await;
    assert_eq!(state.as_str(), "connected");
    assert_eq!(
        c.own_addresses().await,
        ["melissa-box.tail0000.ts.net", "100.64.0.7"]
    );
    assert_eq!(c.start_login("ui").await.unwrap(), Login::Connected);

    assert_eq!(c.disable("ui").await.unwrap(), State::Off);
    assert!(!c.enabled());
    assert_eq!(c.state().await, State::Off);
    assert!(c.own_addresses().await.is_empty());
    assert_eq!(
        fake.calls(),
        ["start", "up melissa-box", "logout", "stop"],
        "disable logs the node out first"
    );

    let log = audit(&dir);
    assert!(log.contains("\"action\":\"settings.tailscale\""), "{log}");
    assert!(
        log.contains("enabled (node melissa-box); sign_in_needed"),
        "{log}"
    );
    assert!(log.contains("disabled; logged out; stopped"), "{log}");
    // One sign-in audited per URL handed out, and never the URL itself.
    assert_eq!(log.matches("tailscale.login_started").count(), 2, "{log}");
    assert!(!log.contains("0123456789abcdef"), "{log}");
    assert!(!log.contains("login.tailscale.com"), "{log}");
}

#[tokio::test]
async fn a_sign_in_that_completes_at_once_gives_no_url() {
    let dir = scratch("quick");
    let fake = Arc::new(Fake {
        login_completes: true,
        ..Fake::default()
    });
    let c = control(&dir, Management::Melissa, fake.clone());
    c.enable("ui").await.unwrap();
    assert_eq!(c.start_login("ui").await.unwrap(), Login::Connected);
    assert!(!audit(&dir).contains("tailscale.login_started"));
}

#[tokio::test]
async fn a_unit_that_does_not_start_is_an_error_and_audited() {
    let dir = scratch("fail");
    let fake = Arc::new(Fake {
        start_fails: true,
        ..Fake::default()
    });
    let c = control(&dir, Management::Melissa, fake.clone());
    assert!(matches!(
        c.enable("agent").await,
        Err(TailscaleError::Failed(_))
    ));
    // Enabled but not running: the page shows the error line (and Disable).
    assert_eq!(
        c.state().await,
        State::Error("Tailscale isn't running.".into())
    );
    let log = audit(&dir);
    assert!(
        log.contains("\"actor\":\"agent\"") && log.contains("not started"),
        "{log}"
    );
    // Disable still works when it isn't running.
    assert_eq!(c.disable("ui").await.unwrap(), State::Off);
    assert!(audit(&dir).contains("disabled; not running; stopped"));
}

// --- the tools -----------------------------------------------------------------------------

#[tokio::test]
async fn tools_never_return_the_sign_in_url() {
    let dir = scratch("tools");
    let fake = Arc::new(Fake::default());
    let c = control(&dir, Management::Melissa, fake.clone());
    let status = tool::TailscaleStatus(c.clone());
    let enable = tool::EnableTailscale(c.clone());
    let ctx = TaskContext::default();

    let out = status.call(json_empty()).await.unwrap();
    assert_eq!(out["state"], "off");
    assert_eq!(out["managed_by"], "melissa");

    // Enabling always asks the owner.
    assert!(enable
        .owner_approval(&ctx, &json_empty())
        .unwrap()
        .is_some());
    assert!(enable.check_args(&serde_json::json!({"x": 1})).is_err());
    let out = enable.call(json_empty()).await.unwrap();
    assert_eq!(out["state"], "sign_in_needed");

    // The owner started a sign-in: the URL is in tailscaled's status, never in a tool result.
    c.start_login("ui").await.unwrap();
    let out = status.call(json_empty()).await.unwrap().to_string();
    assert!(!out.contains("0123456789abcdef"), "{out}");
    assert!(!out.contains("login.tailscale.com"), "{out}");
    assert!(out.contains("Settings → Tailscale"), "{out}");

    *fake.status.lock().unwrap() = RUNNING;
    let out = status.call(json_empty()).await.unwrap();
    assert_eq!(out["state"], "connected");
    assert_eq!(out["node"]["ipv4"], "100.64.0.7");
    assert_eq!(out["node"]["dns_name"], "melissa-box.tail0000.ts.net");

    let host = control(&dir, Management::Host, fake);
    let out = tool::TailscaleStatus(host.clone())
        .call(json_empty())
        .await
        .unwrap();
    assert_eq!(out["state"], "not_managed");
    assert_eq!(out["managed_by"], "host");
    let out = tool::EnableTailscale(host)
        .call(json_empty())
        .await
        .unwrap();
    assert_eq!(out["state"], "not_managed");
}

// --- open from other devices (tailscale serve) -------------------------------------------------

/// Enabled and signed in.
async fn connected(tag: &str, fake: Arc<Fake>) -> (PathBuf, TailscaleControl) {
    let dir = scratch(tag);
    let c = control(&dir, Management::Melissa, fake.clone());
    c.enable("ui").await.unwrap();
    *fake.status.lock().unwrap() = RUNNING;
    *fake.serve_config.lock().unwrap() = "{}".into();
    (dir, c)
}

#[tokio::test]
async fn serve_is_offered_only_while_connected_and_managed() {
    let dir = scratch("serve-offered");
    let fake = Arc::new(Fake::default());
    let c = control(&dir, Management::Melissa, fake.clone());
    assert_eq!(c.serve_state().await, Serve::Unavailable, "off");
    assert!(matches!(
        c.serve_on("ui", None).await,
        Err(TailscaleError::Not("not connected"))
    ));
    c.enable("ui").await.unwrap();
    assert_eq!(c.serve_state().await, Serve::Unavailable, "signed out");
    let host = control(&dir, Management::Host, fake.clone());
    assert_eq!(host.serve_state().await, Serve::Unavailable);
    assert!(matches!(
        host.serve_on("ui", None).await,
        Err(TailscaleError::NotManaged)
    ));
    assert_eq!(host.ui_port(), 8080);
}

#[tokio::test]
async fn serve_turns_on_and_off() {
    let fake = Arc::new(Fake::default());
    let (dir, c) = connected("serve-on", fake.clone()).await;
    assert_eq!(c.serve_state().await, Serve::Off);
    assert_eq!(
        c.own_addresses().await,
        ["melissa-box.tail0000.ts.net", "100.64.0.7"]
    );

    let on = c.serve_on("ui", Some("melissa-box")).await.unwrap();
    assert_eq!(
        on,
        Serve::On {
            url: "https://melissa-box.tail0000.ts.net".into()
        }
    );
    assert!(c.serve_setting());
    // Same name: no rename. Melissa starts from an empty configuration.
    assert_eq!(
        fake.calls(),
        ["start", "serve reset", "serve http://127.0.0.1:8082"]
    );
    // App links leave the HTTPS name out (HSTS would turn their http:// links into https).
    c.forget();
    assert_eq!(c.own_addresses().await, ["100.64.0.7"]);

    // Changed behind Melissa's back (another target, a Funnel): set up again.
    *fake.serve_config.lock().unwrap() = OURS.replace("8082", "9999");
    assert_eq!(c.serve_state().await, Serve::TurningOn);
    for _ in 0..50 {
        if matches!(c.serve_state().await, Serve::On { .. }) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(matches!(c.serve_state().await, Serve::On { .. }));

    assert_eq!(c.serve_off("ui").await.unwrap(), Serve::Off);
    assert!(!c.serve_setting());
    assert_eq!(*fake.serve_config.lock().unwrap(), "{}");
    let log = audit(&dir);
    assert!(log.contains("\"action\":\"tailscale.serve_on\""), "{log}");
    assert!(log.contains("\"action\":\"tailscale.serve_off\""), "{log}");
    assert!(log.contains("node melissa-box"), "{log}");
}

#[tokio::test]
async fn serve_renames_the_node_first() {
    let fake = Arc::new(Fake::default());
    let (dir, c) = connected("serve-name", fake.clone()).await;
    for bad in ["Living Room", "-box", "box-", "", "b\u{e4}r"] {
        assert!(
            matches!(
                c.serve_on("ui", Some(bad)).await,
                Err(TailscaleError::BadName)
            ),
            "{bad:?}"
        );
    }
    assert!(!c.serve_setting(), "nothing happened");
    let on = c.serve_on("ui", Some("Box-7")).await.unwrap();
    assert_eq!(
        on,
        Serve::On {
            url: "https://box-7.tail0000.ts.net".into()
        }
    );
    assert!(fake.calls().contains(&"set --hostname=box-7".to_string()));
    assert_eq!(c.node_name(), "box-7", "kept for the next sign-in");
    assert!(audit(&dir).contains("renamed from melissa-box to box-7"));
}

#[tokio::test]
async fn serve_waits_for_https_to_be_allowed() {
    let fake = Arc::new(Fake::default());
    *fake.serve_mode.lock().unwrap() = ServeMode::NeedsHttps;
    let (_dir, c) = connected("serve-https", fake.clone()).await;
    let state = c.serve_on("ui", None).await.unwrap();
    let Serve::NeedsHttps(Some(url)) = &state else {
        panic!("{state:?}")
    };
    assert_eq!(url.expose(), ALLOW_HTTPS);
    assert!(!c.serve_setting(), "not on yet");
    // Agents see the state, never the page.
    let out = tool::TailscaleStatus(c.clone())
        .call(json_empty())
        .await
        .unwrap();
    assert_eq!(out["web_ui_https"]["state"], "needs_https");
    assert!(!out.to_string().contains("login.tailscale.com"), "{out}");

    // The owner allows HTTPS: the CLI finishes, and it is on.
    fake.https_allowed();
    for _ in 0..50 {
        if c.serve_setting() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(matches!(c.serve_state().await, Serve::On { .. }));
    let out = tool::TailscaleStatus(c.clone())
        .call(json_empty())
        .await
        .unwrap();
    assert_eq!(out["web_ui_https"]["state"], "on");
    assert_eq!(
        out["web_ui_https"]["url"],
        "https://melissa-box.tail0000.ts.net"
    );
}

#[tokio::test]
async fn cancel_stops_the_waiting_cli() {
    let fake = Arc::new(Fake::default());
    *fake.serve_mode.lock().unwrap() = ServeMode::NeedsHttps;
    let (_dir, c) = connected("serve-cancel", fake.clone()).await;
    c.serve_on("ui", None).await.unwrap();
    assert!(!fake.waiting_stopped());
    c.serve_cancel();
    assert!(fake.waiting_stopped());
    assert_eq!(c.serve_state().await, Serve::Off);
}

#[tokio::test]
async fn a_failed_try_is_reported_once_and_the_row_is_off() {
    let fake = Arc::new(Fake::default());
    *fake.serve_mode.lock().unwrap() = ServeMode::Fails;
    let (dir, c) = connected("serve-fail", fake.clone()).await;
    c.serve_on("ui", None).await.unwrap();
    let failure = c.take_serve_failure().expect("a failure");
    assert!(failure.detail.contains("failed"), "{failure:?}");
    assert_eq!(c.take_serve_failure(), None, "once");
    assert_eq!(c.serve_state().await, Serve::Off);
    assert!(audit(&dir).contains("didn't turn on"));
}

#[tokio::test]
async fn disable_removes_the_https_address_first() {
    let fake = Arc::new(Fake::default());
    let (dir, c) = connected("serve-disable", fake.clone()).await;
    c.serve_on("ui", None).await.unwrap();
    c.disable("ui").await.unwrap();
    let calls = fake.calls();
    let reset = calls.iter().rposition(|c| c == "serve reset").unwrap();
    let logout = calls.iter().position(|c| c == "logout").unwrap();
    assert!(reset < logout, "{calls:?}");
    assert!(!c.serve_setting());
    assert!(audit(&dir).contains("tailscale.serve_off"));
}

#[tokio::test]
async fn each_start_sets_it_up_again_or_removes_a_leftover() {
    let fake = Arc::new(Fake::default());
    let (_dir, c) = connected("serve-resume", fake.clone()).await;
    c.serve_on("ui", None).await.unwrap();
    // A restart: tailscaled lost it.
    *fake.serve_config.lock().unwrap() = "{}".into();
    c.resume_serve().await;
    for _ in 0..50 {
        if *fake.serve_config.lock().unwrap() == OURS {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(*fake.serve_config.lock().unwrap(), OURS);

    // Off, but something is configured: removed.
    c.serve_off("ui").await.unwrap();
    *fake.serve_config.lock().unwrap() = OURS.into();
    c.resume_serve().await;
    assert_eq!(*fake.serve_config.lock().unwrap(), "{}");
}

#[test]
fn the_setting_file_keeps_the_other_fields() {
    let dir = scratch("serve-file");
    let path = dir.join(FILE_NAME);
    update_file(&path, |f| {
        f.enabled = true;
        f.serve = true;
        f.name = Some("box-7".into());
    })
    .unwrap();
    write_setting(&path, false).unwrap();
    let f = read_file(&path).unwrap();
    assert!(!f.enabled && f.serve);
    assert_eq!(f.name.as_deref(), Some("box-7"));
    // Root's check reads it too.
    assert_eq!(should_run(&path, Some("no")), Ok(false));
}

fn json_empty() -> serde_json::Value {
    serde_json::json!({})
}
