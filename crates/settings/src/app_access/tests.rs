use std::{
    cell::RefCell,
    collections::VecDeque,
    sync::atomic::{AtomicU32, Ordering},
};

use melissa_agent::tools::{TaskContext, Tool};

use super::*;

const GW: Ipv4Addr = Ipv4Addr::new(10, 88, 0, 1);

fn temp_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "melissa-app-access-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// --- levels and addresses ----------------------------------------------------------------

#[test]
fn levels_are_cumulative_and_round_trip() {
    assert!(AppAccess::ThisDevice < AppAccess::Tailnet);
    assert!(AppAccess::Tailnet < AppAccess::HomeNetwork);
    assert_eq!(AppAccess::default(), AppAccess::ThisDevice);
    for a in AppAccess::ALL {
        assert_eq!(AppAccess::parse(a.as_str()), Some(a));
    }
    assert_eq!(AppAccess::parse("everyone"), None);
}

#[test]
fn addresses_need_the_right_level() {
    use AppAccess::*;
    for (address, want) in [
        ("127.0.0.1", Some(ThisDevice)),
        ("localhost", Some(ThisDevice)),
        ("::1", Some(ThisDevice)),
        ("[::1]", Some(ThisDevice)),
        ("100.64.0.10", Some(Tailnet)),
        ("100.127.255.1", Some(Tailnet)),
        ("fd7a:115c:a1e0::5", Some(Tailnet)),
        ("box.tail1234.ts.net", Some(Tailnet)),
        ("192.168.1.20", Some(HomeNetwork)),
        ("10.0.0.5", Some(HomeNetwork)),
        ("172.16.3.4", Some(HomeNetwork)),
        ("169.254.1.1", Some(HomeNetwork)),
        ("fd00::1", Some(HomeNetwork)),
        ("fe80::1", Some(HomeNetwork)),
        ("box.local", Some(HomeNetwork)),
        ("100.128.0.1", None),
        ("203.0.113.5", None),
        ("2001:db8::1", None),
    ] {
        assert_eq!(needed_for(address), want, "{address}");
    }
    assert!(ThisDevice.allows("127.0.0.1"));
    assert!(!ThisDevice.allows("100.64.0.10"));
    assert!(Tailnet.allows("100.64.0.10"));
    assert!(!Tailnet.allows("192.168.1.20"));
    assert!(HomeNetwork.allows("192.168.1.20"));
    assert!(HomeNetwork.allows("127.0.0.1"));
    assert!(!HomeNetwork.allows("203.0.113.5"));
}

// --- the setting file ------------------------------------------------------------------------

#[test]
fn setting_file_round_trip_and_defaults() {
    let dir = temp_dir("file");
    let path = dir.join(FILE_NAME);
    assert_eq!(
        read_setting(&path),
        Ok(AppAccess::ThisDevice),
        "missing: default"
    );
    write_setting(&path, AppAccess::Tailnet).unwrap();
    assert_eq!(read_setting(&path), Ok(AppAccess::Tailnet));
    std::fs::write(&path, "apps = \"everyone\"\n").unwrap();
    let err = read_setting(&path).unwrap_err();
    assert!(!err.contains("everyone"), "never quotes the file: {err}");
    std::fs::write(&path, "x".repeat(10_000)).unwrap();
    assert!(read_setting(&path).unwrap_err().contains("too big"));
}

#[test]
fn setting_file_refuses_a_symlink() {
    let dir = temp_dir("link");
    let target = dir.join("secret");
    std::fs::write(&target, "apps = \"tailnet\"\n").unwrap();
    let path = dir.join(FILE_NAME);
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(read_setting(&path)
        .unwrap_err()
        .contains("not a regular file"));
}

// --- the gateway -------------------------------------------------------------------------------

#[test]
fn default_gateway_from_proc_net_route() {
    let route =
        "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                 eth0\t00000000\t0100580A\t0003\t0\t0\t0\t00000000\t0\t0\t0\n\
                 eth0\t0000580A\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0\n";
    // The test assumes a little-endian host (x86_64, aarch64), like the kernel's output above.
    if cfg!(target_endian = "little") {
        assert_eq!(default_gateway(route), Some(GW));
    }
    assert_eq!(
        default_gateway("Iface\tDestination\n"),
        None,
        "no default route"
    );
    let no_gateway = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\n\
                      eth0\t0000580A\t00000000\t0001\t0\t0\t0\t0000FFFF\n";
    assert_eq!(default_gateway(no_gateway), None);
}

// --- rules -------------------------------------------------------------------------------------

#[test]
fn ruleset_per_level() {
    let this = ruleset(Some((AppAccess::ThisDevice, GW)));
    let tail = ruleset(Some((AppAccess::Tailnet, GW)));
    let home = ruleset(Some((AppAccess::HomeNetwork, GW)));
    for r in [&this, &tail, &home] {
        assert!(r.contains("table inet melissa_app_access {"), "{r}");
        assert!(
            r.contains("delete table inet melissa_app_access"),
            "atomic replace"
        );
        assert!(r.contains("type filter hook prerouting priority -150; policy accept;"));
        assert!(r.contains("iifname != { \"lo\", \"cni0\" } tcp dport 30000-30079 jump app_ports"));
        assert!(r.contains("iifname != { \"lo\", \"cni0\" } udp dport 30000-30079 jump app_ports"));
        assert!(r.contains("ip saddr 10.88.0.1 accept comment \"melissa:this_device\""));
        assert!(
            r.trim_end()
                .ends_with("counter drop comment \"melissa:drop\"\n\t}\n}"),
            "drop last: {r}"
        );
    }
    assert!(!this.contains("100.64.0.0/10"));
    assert!(!this.contains("192.168.0.0/16"));
    assert!(tail.contains("ip saddr 100.64.0.0/10 accept comment \"melissa:tailnet\""));
    assert!(tail.contains("ip6 saddr fd7a:115c:a1e0::/48 accept comment \"melissa:tailnet\""));
    assert!(!tail.contains("192.168.0.0/16"));
    assert!(home.contains("100.64.0.0/10"));
    assert!(home.contains(
        "ip saddr { 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 169.254.0.0/16 } accept comment \"melissa:home_network\""
    ));
    assert!(home.contains("ip6 saddr { fc00::/7, fe80::/10 } accept"));
}

#[test]
fn closed_ruleset_lets_nothing_in() {
    let closed = ruleset(None);
    assert!(!closed.contains("accept comment"), "{closed}");
    assert!(closed.contains("counter drop comment \"melissa:drop\""));
    assert!(closed.contains("tcp dport 30000-30079 jump app_ports"));
}

/// What `nft list table` prints back for a script (close enough: our lines, re-indented).
fn listed(script: &str) -> String {
    script
        .lines()
        .filter(|l| {
            !l.starts_with('#') && !l.starts_with("delete") && *l != "table inet melissa_app_access"
        })
        .map(|l| l.replace("counter drop", "counter packets 0 bytes 0 drop"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn verify_accepts_what_was_loaded_and_nothing_wider() {
    for level in AppAccess::ALL {
        let open = Some((level, GW));
        assert_eq!(verify(&listed(&ruleset(open)), open), Ok(()), "{level}");
    }
    assert_eq!(verify(&listed(&ruleset(None)), None), Ok(()));
    // A wider table than wanted (a stale one) fails.
    let wide = listed(&ruleset(Some((AppAccess::HomeNetwork, GW))));
    assert!(verify(&wide, Some((AppAccess::Tailnet, GW))).is_err());
    assert!(verify(&wide, None).is_err());
    // A table without the drop, or with another gateway, fails.
    let no_drop = listed(&ruleset(Some((AppAccess::ThisDevice, GW)))).replace("drop", "accept");
    assert!(verify(&no_drop, Some((AppAccess::ThisDevice, GW))).is_err());
    let other_gw = Some((AppAccess::ThisDevice, Ipv4Addr::new(10, 89, 0, 1)));
    assert!(verify(
        &listed(&ruleset(Some((AppAccess::ThisDevice, GW)))),
        other_gw
    )
    .is_err());
    assert!(verify("", None).is_err());
}

// --- fail closed -----------------------------------------------------------------------------

/// A fake nft: each load takes the next outcome (default: works); list returns what the last
/// successful load put in place.
#[derive(Default)]
struct FakeNft {
    loads: RefCell<VecDeque<Result<(), String>>>,
    table: RefCell<Option<String>>,
    /// `list` prints this instead (a table that didn't come out as loaded).
    listing: Option<String>,
    loaded: RefCell<Vec<String>>,
}

impl FakeNft {
    fn failing(n: usize) -> Self {
        let f = Self::default();
        for _ in 0..n {
            f.loads
                .borrow_mut()
                .push_back(Err("nft failed: Error: syntax error".into()));
        }
        f
    }
}

impl Nft for FakeNft {
    fn load(&self, script: &str) -> Result<(), String> {
        self.loaded.borrow_mut().push(script.to_string());
        let r = self.loads.borrow_mut().pop_front().unwrap_or(Ok(()));
        if r.is_ok() {
            *self.table.borrow_mut() = Some(listed(script));
        }
        r
    }

    fn list(&self) -> Result<String, String> {
        if let Some(l) = &self.listing {
            return Ok(l.clone());
        }
        self.table
            .borrow()
            .clone()
            .ok_or_else(|| "Error: No such file or directory".into())
    }
}

#[test]
fn apply_puts_the_setting_in_force() {
    let nft = FakeNft::default();
    let s = apply(&nft, Ok(AppAccess::Tailnet), Some(GW));
    assert!(s.ok(), "{s:?}");
    assert_eq!(s.open_to, Some(AppAccess::Tailnet));
    assert_eq!(nft.loaded.borrow().len(), 1);
}

#[test]
fn apply_falls_back_to_this_device_then_closed() {
    // The wanted table fails to load: this device.
    let nft = FakeNft::failing(1);
    let s = apply(&nft, Ok(AppAccess::HomeNetwork), Some(GW));
    assert_eq!((s.open_to, s.verified), (Some(AppAccess::ThisDevice), true));
    assert!(!s.ok());
    assert_eq!(s.errors.len(), 1);

    // Both fail: closed.
    let nft = FakeNft::failing(2);
    let s = apply(&nft, Ok(AppAccess::Tailnet), Some(GW));
    assert_eq!((s.open_to, s.verified), (None, true), "{s:?}");
    assert!(nft.loaded.borrow()[2].contains("(closed)"));

    // This device wanted and failing: straight to closed (no second try of the same).
    let nft = FakeNft::failing(1);
    let s = apply(&nft, Ok(AppAccess::ThisDevice), Some(GW));
    assert_eq!((s.open_to, s.verified), (None, true));
    assert_eq!(nft.loaded.borrow().len(), 2);
}

#[test]
fn apply_closed_when_verification_fails() {
    // nft says it loaded, but the table that comes back lets everything in.
    let nft = FakeNft {
        listing: Some("table inet melissa_app_access { }".into()),
        ..FakeNft::default()
    };
    let s = apply(&nft, Ok(AppAccess::Tailnet), Some(GW));
    assert_eq!((s.open_to, s.verified), (None, false), "{s:?}");
    assert!(
        s.errors.iter().all(|e| e.contains("lacks")),
        "{:?}",
        s.errors
    );
    assert_eq!(nft.loaded.borrow().len(), 3, "wanted, this device, closed");
}

#[test]
fn apply_closed_without_a_gateway_or_a_readable_setting() {
    let nft = FakeNft::default();
    let s = apply(&nft, Ok(AppAccess::HomeNetwork), None);
    assert_eq!((s.open_to, s.verified), (None, true));
    assert!(s.errors[0].contains("default route"));

    let nft = FakeNft::default();
    let s = apply(
        &nft,
        Err("app-access.toml is not a valid setting".into()),
        Some(GW),
    );
    assert_eq!(s.wanted, AppAccess::ThisDevice);
    assert_eq!(s.open_to, Some(AppAccess::ThisDevice));
    assert!(!s.ok(), "the read error is reported");
}

#[test]
fn nothing_works_is_unverified() {
    let nft = FakeNft::failing(3);
    let s = apply(&nft, Ok(AppAccess::Tailnet), Some(GW));
    assert_eq!((s.open_to, s.verified), (None, false));
    assert_eq!(s.errors.len(), 3);
}

// --- views and the control -------------------------------------------------------------------

fn status(wanted: AppAccess, open_to: Option<AppAccess>, verified: bool) -> Status {
    Status {
        wanted,
        open_to,
        verified,
        errors: if open_to == Some(wanted) && verified {
            vec![]
        } else {
            vec!["nft failed".into()]
        },
        at: 1,
    }
}

#[test]
fn links_follow_what_is_in_force() {
    let v = |setting, s| View { setting, status: s };
    let ok = v(
        AppAccess::Tailnet,
        Some(status(AppAccess::Tailnet, Some(AppAccess::Tailnet), true)),
    );
    assert_eq!(ok.for_links(), AppAccess::Tailnet);
    assert_eq!(ok.problem(), None);

    let fell_back = v(
        AppAccess::HomeNetwork,
        Some(status(
            AppAccess::HomeNetwork,
            Some(AppAccess::ThisDevice),
            true,
        )),
    );
    assert_eq!(fell_back.for_links(), AppAccess::ThisDevice);
    assert_eq!(
        fell_back.problem().as_deref(),
        Some("Couldn't apply this. Apps are open to this device only.")
    );

    let closed = v(
        AppAccess::Tailnet,
        Some(status(AppAccess::Tailnet, None, true)),
    );
    assert_eq!(closed.for_links(), AppAccess::ThisDevice);
    assert!(closed.problem().unwrap().contains("No device"));

    let never = v(AppAccess::Tailnet, None);
    assert_eq!(never.for_links(), AppAccess::ThisDevice);
    assert!(never.problem().is_some());

    // Narrowed but not applied yet: links already narrow (min of both).
    let pending = v(
        AppAccess::ThisDevice,
        Some(status(
            AppAccess::HomeNetwork,
            Some(AppAccess::HomeNetwork),
            true,
        )),
    );
    assert_eq!(pending.for_links(), AppAccess::ThisDevice);
    assert!(pending.problem().unwrap().starts_with("Not applied yet."));
}

/// Runs the root helper's logic in-process, like the unit would.
struct FakeHelper {
    setting: PathBuf,
    status: PathBuf,
    fail_loads: usize,
}

impl Applier for FakeHelper {
    fn apply(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            let nft = FakeNft::failing(self.fail_loads);
            let s = apply(&nft, read_setting(&self.setting), Some(GW));
            write_status(&self.status, &s).map_err(|e| e.to_string())?;
            if s.ok() {
                Ok(())
            } else {
                Err(format!("{UNIT} failed"))
            }
        })
    }
}

fn control(dir: &Path, fail_loads: usize) -> (AppAccessControl, PathBuf) {
    let setting = dir.join(FILE_NAME);
    let status = dir.join("status.json");
    let audit = dir.join("audit.jsonl");
    let helper = FakeHelper {
        setting: setting.clone(),
        status: status.clone(),
        fail_loads,
    };
    (
        AppAccessControl::new(
            setting,
            status,
            Arc::new(helper),
            Some(AuditLog::new(&audit)),
        ),
        audit,
    )
}

#[tokio::test]
async fn set_applies_and_audits() {
    let dir = temp_dir("set");
    let (c, audit) = control(&dir, 0);
    assert_eq!(c.setting(), AppAccess::ThisDevice);
    let view = c.set(AppAccess::Tailnet, "ui").await.unwrap();
    assert_eq!(view.open_to(), Some(AppAccess::Tailnet));
    assert_eq!(c.view().for_links(), AppAccess::Tailnet);
    let log = std::fs::read_to_string(audit).unwrap();
    assert!(log.contains("\"action\":\"settings.app_access\""), "{log}");
    assert!(log.contains("\"actor\":\"ui\""));
    assert!(
        log.contains("apps open to: tailnet (was this_device); applied"),
        "{log}"
    );
}

#[tokio::test]
async fn set_that_fails_reports_and_links_stay_closed() {
    let dir = temp_dir("setfail");
    let (c, audit) = control(&dir, 1);
    let err = c.set(AppAccess::HomeNetwork, "agent").await.unwrap_err();
    assert!(matches!(err, AppAccessError::Apply(_)), "{err}");
    let view = c.view();
    assert_eq!(view.setting, AppAccess::HomeNetwork, "the setting is kept");
    assert_eq!(view.for_links(), AppAccess::ThisDevice, "fail closed");
    let log = std::fs::read_to_string(audit).unwrap();
    assert!(
        log.contains("\"actor\":\"agent\"") && log.contains("not applied"),
        "{log}"
    );
}

// --- the tool --------------------------------------------------------------------------------

#[tokio::test]
async fn tool_asks_the_owner_only_to_widen() {
    let dir = temp_dir("tool");
    let (c, _) = control(&dir, 0);
    let t = tool::SetAppAccess(c.clone());
    let ctx = TaskContext::default();
    let args = |v: &str| serde_json::json!({ "open_to": v });
    assert!(t.owner_approval(&ctx, &args("tailnet")).unwrap().is_some());
    assert!(t
        .owner_approval(&ctx, &args("this_device"))
        .unwrap()
        .is_none());
    assert!(t.check_args(&args("everyone")).is_err());
    assert!(t
        .check_args(&serde_json::json!({ "open_to": "tailnet", "x": 1 }))
        .is_err());
    assert!(t
        .describe_call(&args("tailnet"))
        .unwrap()
        .contains("Tailnet"));

    let out = t.call(args("home_network")).await.unwrap();
    assert_eq!(out["open_to"], "home_network");
    assert!(out.get("problem").is_none(), "{out}");
    // Now narrowing is free, widening further is not possible, the same level is free.
    assert!(t.owner_approval(&ctx, &args("tailnet")).unwrap().is_none());
    assert!(t
        .owner_approval(&ctx, &args("home_network"))
        .unwrap()
        .is_none());
}

/// What nft 1.1 printed (`nft list table inet melissa_app_access`) after loading
/// `ruleset(Some((HomeNetwork, 10.88.0.1)))` and `ruleset(None)`: it reorders sets, names the
/// priority and adds counters.
#[test]
fn verify_reads_real_nft_listings() {
    let home = include_str!("fixtures/home_network.nft-list");
    assert_eq!(verify(home, Some((AppAccess::HomeNetwork, GW))), Ok(()));
    assert!(verify(home, Some((AppAccess::Tailnet, GW))).is_err());
    assert!(verify(home, None).is_err());
    let closed = include_str!("fixtures/closed.nft-list");
    assert_eq!(verify(closed, None), Ok(()));
    assert!(verify(closed, Some((AppAccess::ThisDevice, GW))).is_err());
}
