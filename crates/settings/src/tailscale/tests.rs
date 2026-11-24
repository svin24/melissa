use std::sync::Mutex;

use melissa_agent::tools::{TaskContext, Tool};

use super::*;
use crate::test_support::scratch;

const RUNNING: &str = include_str!("fixtures/running.json");
const NEEDS_LOGIN: &str = include_str!("fixtures/needs_login.json");
const NEEDS_LOGIN_URL: &str = include_str!("fixtures/needs_login_url.json");
const URL: &str = "https://login.tailscale.com/a/0123456789abcdef";

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

fn json_empty() -> serde_json::Value {
    serde_json::json!({})
}
