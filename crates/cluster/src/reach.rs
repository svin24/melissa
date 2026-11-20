//! Is an app really reachable at the addresses its links name?
//!
//! App links are built from `[apps] addresses`, which is configuration: it can disagree with
//! where the host actually publishes the app ports (container install with `BIND=127.0.0.1`
//! but a Tailscale address in `[apps] addresses`: every link is "connection refused").
//!
//! Two ways to tell, depending on what Melissa knows:
//!
//! - **Publish bind known** (container install: `MELISSA_PUBLISH_BIND`, the `BIND` the host
//!   publishes the app ports on). The bind decides: an advertised address it provably does
//!   not cover (loopback bind, non-loopback address; one address bind, another address) is
//!   dead, dropped from the links, with a warning the agent passes on to the owner. Addresses
//!   are *not* probed: from inside the container a connect to the host's published address
//!   fails (no hairpin through podman's DNAT) although it works from the host and from other
//!   machines, so such a probe would strip working links.
//! - **No publish bind** (Nix/host install: k3s on the host serves the NodePorts on every
//!   host address; or a container installed before the bind was passed in). Each advertised
//!   `address:port` gets a TCP connect from the device. A failure while the app answers is
//!   only *unverified*: noted, link kept. A probe is never proof enough to drop a link.
//!
//! Either way one control connect goes straight to the NodePort on the device's own address:
//! when that doesn't answer, the app itself isn't answering yet (starting, or listening on
//! another port), which is not an address problem. UDP can't be probed (no handshake);
//! loopback addresses only work on the device itself. Both are said so.
//!
//! Limits: what the device sees is not what other machines see (a firewall that only blocks
//! other machines, or a Tailscale ACL, is not seen).

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use melissa_agent::BoxFuture;
use melissa_appspec::Protocol;
use serde_json::{json, Value};

use crate::device;

/// The result of one TCP connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    Open,
    Refused,
    TimedOut,
    /// Anything else (no route, name not found, ...): the message.
    Failed(String),
}

impl Probe {
    fn describe(&self) -> String {
        match self {
            Self::Open => "open".into(),
            Self::Refused => "refused".into(),
            Self::TimedOut => "timed out".into(),
            Self::Failed(e) => e.clone(),
        }
    }
}

/// Tries a TCP connect to `host:port`. Injected so tests run without a network.
pub trait PortProbe: Send + Sync {
    fn probe<'a>(&'a self, host: &'a str, port: u16) -> BoxFuture<'a, Probe>;
}

/// The real probe: `TcpStream::connect` with a timeout. Open and timed-out results are kept
/// for [`TcpProbe::ttl`] (an agent polling `app_status` doesn't wait on a timeout every
/// time); refusals are instant and not kept, so a starting app shows up as soon as it answers.
pub struct TcpProbe {
    timeout: Duration,
    ttl: Duration,
    cache: Mutex<HashMap<(String, u16), (Instant, Probe)>>,
}

impl Default for TcpProbe {
    fn default() -> Self {
        Self {
            timeout: Duration::from_millis(800),
            ttl: Duration::from_secs(15),
            cache: Mutex::new(HashMap::new()),
        }
    }
}

impl TcpProbe {
    fn cached(&self, key: &(String, u16)) -> Option<Probe> {
        let cache = self.cache.lock().ok()?;
        let (at, probe) = cache.get(key)?;
        (at.elapsed() < self.ttl).then(|| probe.clone())
    }

    fn keep(&self, key: (String, u16), probe: &Probe) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.retain(|_, (at, _)| at.elapsed() < self.ttl);
            if matches!(probe, Probe::Open | Probe::TimedOut) {
                cache.insert(key, (Instant::now(), probe.clone()));
            }
        }
    }
}

impl PortProbe for TcpProbe {
    fn probe<'a>(&'a self, host: &'a str, port: u16) -> BoxFuture<'a, Probe> {
        Box::pin(async move {
            let key = (host.to_string(), port);
            if let Some(p) = self.cached(&key) {
                return p;
            }
            let target = device::host_port(host, port);
            let probe =
                match tokio::time::timeout(self.timeout, tokio::net::TcpStream::connect(&target))
                    .await
                {
                    Ok(Ok(_)) => Probe::Open,
                    Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                        Probe::Refused
                    }
                    Ok(Err(e)) => Probe::Failed(e.kind().to_string()),
                    Err(_) => Probe::TimedOut,
                };
            self.keep(key, &probe);
            probe
        })
    }
}

/// The checker the app tools use: a probe, the control address (the device's own address
/// where the NodePorts are served directly, without the host's publishing) and, when known,
/// the address the host publishes the app ports on.
#[derive(Clone)]
pub struct ReachCheck {
    pub probe: Arc<dyn PortProbe>,
    /// `None`: no control connect (an unanswered address then counts as "app not answering").
    pub control: Option<String>,
    /// Where the host publishes the app ports (container install: its `BIND`; `0.0.0.0`:
    /// every address). `Some`: addresses are judged by it and not probed. `None`: probed,
    /// and a failed probe is only a note.
    pub publish_bind: Option<IpAddr>,
}

impl ReachCheck {
    /// TCP connects from this device; control: the address of the default route (in the
    /// container that is the container's own address, which is k3s's node address).
    pub fn tcp(publish_bind: Option<IpAddr>) -> Self {
        Self {
            probe: Arc::new(TcpProbe::default()),
            control: device::default_route_address().map(|ip| ip.to_string()),
            publish_bind,
        }
    }
}

/// The publish bind as the container install passes it (`MELISSA_PUBLISH_BIND`): an IP
/// address (IPv6 with or without brackets) or `localhost`. Empty or anything else: `None`
/// (unknown: addresses are probed instead).
pub fn parse_publish_bind(value: &str) -> Option<IpAddr> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    as_ip(value)
}

/// An address as an IP (`localhost` is 127.0.0.1); `None` for a host name.
fn as_ip(address: &str) -> Option<IpAddr> {
    if address.eq_ignore_ascii_case("localhost") {
        return Some(Ipv4Addr::LOCALHOST.into());
    }
    address
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .ok()
        .map(|ip| ip.to_canonical())
}

/// Does a publish bind cover an address?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Coverage {
    Covered,
    NotCovered,
    /// A host name: where it leads isn't known from here (DNS can differ per client).
    Unknown,
}

/// Are app ports published on `bind` reachable at `address`? `0.0.0.0` and `::` cover every
/// address; a loopback bind covers loopback addresses only; one address covers itself.
pub(crate) fn covers(bind: IpAddr, address: &str) -> Coverage {
    let bind = bind.to_canonical();
    if bind.is_unspecified() {
        return Coverage::Covered;
    }
    let Some(ip) = as_ip(address) else {
        return Coverage::Unknown;
    };
    let covered = if bind.is_loopback() {
        ip.is_loopback()
    } else {
        ip == bind
    };
    if covered {
        Coverage::Covered
    } else {
        Coverage::NotCovered
    }
}

/// One port of an app as the check sees it: the NodePort and the port in links.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CheckedPort {
    pub protocol: Protocol,
    pub node_port: u16,
    pub published: u16,
}

/// What the check found: a JSON report for the tool result and the addresses to leave out
/// of links.
pub(crate) struct Reachability {
    pub report: Value,
    pub dead: Vec<String>,
}

/// Loopback addresses (and `localhost`) only work on the device itself.
fn is_loopback(address: &str) -> bool {
    as_ip(address).is_some_and(|ip| ip.is_loopback())
}

const LOOPBACK: &str = "loopback: only works on this device itself";
const NOT_ANSWERING: &str = "the app does not answer on its port yet (starting, or it listens on \
                             another port than its spec says): not an address problem";

/// Check the app's `ports` at each of `addresses`. `None` when there is nothing to report
/// (no ports).
pub(crate) async fn check(
    reach: &ReachCheck,
    addresses: &[String],
    ports: &[CheckedPort],
) -> Option<Reachability> {
    if ports.is_empty() {
        return None;
    }
    let tcp: Vec<CheckedPort> = ports
        .iter()
        .copied()
        .filter(|p| p.protocol == Protocol::Tcp)
        .collect();
    let mut out = match reach.publish_bind {
        Some(bind) => by_bind(reach, bind, addresses, &tcp).await,
        None => by_probe(reach, addresses, &tcp).await,
    };
    if ports.iter().any(|p| p.protocol == Protocol::Udp) {
        out.report["udp"] = json!("UDP ports can't be checked (no handshake): only TCP is tested");
    }
    Some(out)
}

/// The control connects, one per TCP port: does the app answer on its NodePort on the device?
async fn control(reach: &ReachCheck, tcp: &[CheckedPort]) -> Vec<bool> {
    futures::future::join_all(tcp.iter().map(|p| async move {
        match &reach.control {
            Some(c) => reach.probe.probe(c, p.node_port).await == Probe::Open,
            None => false,
        }
    }))
    .await
}

/// Container install: the publish bind says which addresses reach the app; only the app
/// itself is probed (control).
async fn by_bind(
    reach: &ReachCheck,
    bind: IpAddr,
    addresses: &[String],
    tcp: &[CheckedPort],
) -> Reachability {
    // Whether the app answers: `None` when that can't be tested (UDP only, no control).
    let answers = if tcp.is_empty() || reach.control.is_none() {
        None
    } else {
        Some(control(reach, tcp).await.into_iter().all(|open| open))
    };
    let mut rows = Vec::new();
    let mut dead = Vec::new();
    for a in addresses {
        let coverage = covers(bind, a);
        let row = match (coverage, answers) {
            (Coverage::NotCovered, _) => {
                dead.push(a.clone());
                json!({"address": a, "status": "unreachable", "detail": format!(
                    "the host publishes the app ports on {bind} only, not on this address")})
            }
            (_, Some(false)) => {
                json!({"address": a, "status": "app_not_answering", "detail": NOT_ANSWERING})
            }
            (Coverage::Unknown, _) => json!({"address": a, "status": "unverified",
                "detail": format!("a host name: not checked whether it leads to {bind} (where \
                                   the app ports are published); kept in the links")}),
            (Coverage::Covered, Some(true)) => {
                let mut row = json!({"address": a, "status": "ok"});
                if is_loopback(a) {
                    row["detail"] = json!(LOOPBACK);
                }
                row
            }
            (Coverage::Covered, None) => json!({"address": a, "status": "not_checked",
                "detail": "app ports are published here; whether the app answers can't be \
                           checked (no TCP port, or no device address to check from)"}),
        };
        rows.push(row);
    }
    let mut report = json!({
        "checked_by": format!("the host's publish bind ({bind}) for the addresses; the app \
                               itself on this device"),
        "addresses": rows,
    });
    if !dead.is_empty() {
        let usable = addresses.len() - dead.len();
        report["warning"] = json!(format!(
            "not reachable: {} (the host publishes the app ports on {bind} only). Left out of \
             the links: don't give these addresses out. Tell the owner: rerun install.sh with \
             a BIND that covers them (that address, or 0.0.0.0), or set [apps] addresses to \
             match. Restarting the app won't fix this.{}",
            dead.join(", "),
            if usable == 0 {
                " No working address is left."
            } else {
                ""
            }
        ));
    }
    Reachability { report, dead }
}

/// No publish bind known (Nix/host install): each address is probed from the device. A
/// failed probe is a note, never a reason to drop a link.
async fn by_probe(reach: &ReachCheck, addresses: &[String], tcp: &[CheckedPort]) -> Reachability {
    let checked: Vec<&String> = addresses.iter().filter(|a| !is_loopback(a)).collect();

    // Every connect at once: the slowest (a timeout) bounds the whole check.
    let advertised = futures::future::join_all(checked.iter().flat_map(|a| {
        tcp.iter()
            .map(move |p| async move { (a.as_str(), *p, reach.probe.probe(a, p.published).await) })
    }));
    let (advertised, control) = tokio::join!(advertised, control(reach, tcp));

    let mut rows = Vec::new();
    let mut unverified = Vec::new();
    for a in addresses {
        if is_loopback(a) {
            rows.push(json!({"address": a, "status": "not_checked", "detail": LOOPBACK}));
            continue;
        }
        if tcp.is_empty() {
            rows.push(json!({"address": a, "status": "not_checked",
                "detail": "no TCP port to check"}));
            continue;
        }
        // A port the app answers on: the control connect, or this port open at any address.
        let answers = |p: &CheckedPort| {
            tcp.iter()
                .position(|t| t.node_port == p.node_port)
                .is_some_and(|i| control[i])
                || advertised
                    .iter()
                    .any(|(_, q, probe)| q.node_port == p.node_port && *probe == Probe::Open)
        };
        let failed: Vec<(CheckedPort, &Probe)> = advertised
            .iter()
            .filter(|(addr, _, probe)| addr == a && *probe != Probe::Open)
            .map(|(_, p, probe)| (*p, probe))
            .collect();
        if failed.is_empty() {
            rows.push(json!({"address": a, "status": "ok"}));
        } else if let Some((p, probe)) = failed.iter().find(|(p, _)| answers(p)) {
            let at = device::host_port(a, p.published);
            unverified.push(format!("{at} {}", probe.describe()));
            rows.push(
                json!({"address": a, "status": "unverified", "detail": format!(
                "{at} {} from this device although the app answers here; kept in the links \
                 (a connect from the device is not proof: it may work from other machines)",
                probe.describe())}),
            );
        } else {
            rows.push(json!({"address": a, "status": "app_not_answering",
                "detail": NOT_ANSWERING}));
        }
    }

    let mut report = json!({
        "checked_by": "connects from this device (a firewall that only blocks other machines \
                       is not seen)",
        "addresses": rows,
    });
    if !unverified.is_empty() {
        report["note"] = json!(format!(
            "not verified from this device: {}. The links are kept. If people can't open \
             them, tell the owner the app ports may not be served on that address, or a \
             firewall blocks them.",
            unverified.join(", ")
        ));
    }
    Reachability {
        report,
        dead: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, net::TcpListener};

    use super::*;

    /// Answers from a table (anything else is refused) and records every connect.
    #[derive(Default)]
    pub(crate) struct FakeProbe {
        pub open: BTreeMap<(String, u16), Probe>,
        pub calls: Mutex<Vec<String>>,
    }

    impl FakeProbe {
        pub fn with(entries: &[(&str, u16, Probe)]) -> Self {
            Self {
                open: entries
                    .iter()
                    .map(|(h, p, r)| (((*h).to_string(), *p), r.clone()))
                    .collect(),
                calls: Mutex::default(),
            }
        }

        fn calls(&self) -> Vec<String> {
            let mut calls = self.calls.lock().unwrap().clone();
            calls.sort();
            calls
        }
    }

    impl PortProbe for FakeProbe {
        fn probe<'a>(&'a self, host: &'a str, port: u16) -> BoxFuture<'a, Probe> {
            self.calls
                .lock()
                .unwrap()
                .push(device::host_port(host, port));
            let r = self
                .open
                .get(&(host.to_string(), port))
                .cloned()
                .unwrap_or(Probe::Refused);
            Box::pin(async move { r })
        }
    }

    /// Control at the container's address; `bind`: the publish bind (`None`: probe mode).
    fn reach(probe: FakeProbe, bind: Option<&str>) -> (ReachCheck, Arc<FakeProbe>) {
        let probe = Arc::new(probe);
        (
            ReachCheck {
                probe: probe.clone(),
                control: Some("10.88.0.2".into()),
                publish_bind: bind.map(|b| parse_publish_bind(b).unwrap()),
            },
            probe,
        )
    }

    fn tcp(node_port: u16, published: u16) -> CheckedPort {
        CheckedPort {
            protocol: Protocol::Tcp,
            node_port,
            published,
        }
    }

    fn udp(node_port: u16, published: u16) -> CheckedPort {
        CheckedPort {
            protocol: Protocol::Udp,
            node_port,
            published,
        }
    }

    fn addrs(a: &[&str]) -> Vec<String> {
        a.iter().map(ToString::to_string).collect()
    }

    fn statuses(out: &Reachability) -> Vec<String> {
        out.report["addresses"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["status"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn coverage_matrix() {
        use Coverage::{Covered, NotCovered, Unknown};
        let cases = [
            // Every address.
            ("0.0.0.0", "100.64.0.10", Covered),
            ("0.0.0.0", "127.0.0.1", Covered),
            ("0.0.0.0", "box.example.com", Covered),
            ("::", "203.0.113.5", Covered),
            ("::", "2001:db8::1", Covered),
            // Loopback covers loopback only.
            ("127.0.0.1", "127.0.0.1", Covered),
            ("127.0.0.1", "localhost", Covered),
            ("127.0.0.1", "::1", Covered),
            ("localhost", "127.0.0.1", Covered),
            ("::1", "[::1]", Covered),
            ("127.0.0.1", "100.64.0.10", NotCovered),
            ("127.0.0.1", "203.0.113.5", NotCovered),
            ("127.0.0.1", "box.example.com", Unknown),
            // One address covers itself.
            ("100.64.0.10", "100.64.0.10", Covered),
            ("100.64.0.10", "203.0.113.5", NotCovered),
            ("100.64.0.10", "127.0.0.1", NotCovered),
            ("100.64.0.10", "localhost", NotCovered),
            ("2001:db8::1", "[2001:db8::1]", Covered),
            ("[2001:db8::1]", "2001:db8::2", NotCovered),
            ("::ffff:203.0.113.5", "203.0.113.5", Covered),
            ("100.64.0.10", "box.example.com", Unknown),
        ];
        for (bind, address, want) in cases {
            let ip = parse_publish_bind(bind).unwrap();
            assert_eq!(covers(ip, address), want, "bind {bind}, address {address}");
        }
    }

    #[test]
    fn publish_bind_values() {
        assert_eq!(parse_publish_bind(""), None);
        assert_eq!(parse_publish_bind("  "), None);
        assert_eq!(parse_publish_bind("box.example.com"), None);
        assert_eq!(
            parse_publish_bind(" 203.0.113.5 "),
            Some("203.0.113.5".parse().unwrap())
        );
        assert_eq!(parse_publish_bind("[::]"), Some("::".parse().unwrap()));
        assert_eq!(
            parse_publish_bind("localhost"),
            Some("127.0.0.1".parse().unwrap())
        );
    }

    /// The incident: ports published on loopback only, links naming the Tailscale address.
    #[tokio::test]
    async fn loopback_bind_drops_a_tailscale_address_with_a_warning() {
        let (r, probe) = reach(
            FakeProbe::with(&[("10.88.0.2", 30000, Probe::Open)]),
            Some("127.0.0.1"),
        );
        let out = check(&r, &addrs(&["100.64.0.10"]), &[tcp(30000, 30000)])
            .await
            .unwrap();
        assert_eq!(out.dead, ["100.64.0.10"]);
        assert_eq!(statuses(&out), ["unreachable"]);
        let warning = out.report["warning"].as_str().unwrap();
        assert!(warning.contains("100.64.0.10"), "{warning}");
        assert!(warning.contains("on 127.0.0.1 only"), "{warning}");
        assert!(warning.contains("No working address"), "{warning}");
        assert!(
            warning.contains("Restarting the app won't fix this"),
            "{warning}"
        );
        // Decided by the bind: only the app itself is probed.
        assert_eq!(probe.calls(), ["10.88.0.2:30000"]);
    }

    /// The live problem: in the container a connect to the host's published address fails
    /// (no hairpin), although it works from everywhere else. The bind covers it: kept.
    #[tokio::test]
    async fn container_without_hairpin_keeps_a_covered_address() {
        let (r, probe) = reach(
            FakeProbe::with(&[
                ("10.88.0.2", 30000, Probe::Open),
                ("100.64.0.10", 31000, Probe::Refused),
            ]),
            Some("100.64.0.10"),
        );
        let out = check(
            &r,
            &addrs(&["100.64.0.10", "203.0.113.5"]),
            &[tcp(30000, 31000)],
        )
        .await
        .unwrap();
        assert_eq!(out.dead, ["203.0.113.5"]);
        assert_eq!(statuses(&out), ["ok", "unreachable"]);
        let warning = out.report["warning"].as_str().unwrap();
        assert!(warning.contains("203.0.113.5"), "{warning}");
        assert!(
            warning.starts_with(
                "not reachable: 203.0.113.5 (the host publishes the app ports on 100.64.0.10 only)"
            ),
            "{warning}"
        );
        assert!(!warning.contains("No working address"), "{warning}");
        assert_eq!(probe.calls(), ["10.88.0.2:30000"]);
    }

    #[tokio::test]
    async fn any_bind_covers_everything_and_reports_an_app_that_does_not_answer() {
        let (r, _) = reach(FakeProbe::default(), Some("0.0.0.0"));
        let out = check(
            &r,
            &addrs(&["203.0.113.5", "100.64.0.10", "box.example.com"]),
            &[tcp(30000, 30000)],
        )
        .await
        .unwrap();
        assert!(out.dead.is_empty());
        assert_eq!(
            statuses(&out),
            [
                "app_not_answering",
                "app_not_answering",
                "app_not_answering"
            ]
        );
        assert_eq!(out.report["warning"], Value::Null);
    }

    #[tokio::test]
    async fn bind_mode_host_names_are_kept_unverified_and_udp_only_is_judged_too() {
        let (r, probe) = reach(
            FakeProbe::with(&[("10.88.0.2", 30000, Probe::Open)]),
            Some("127.0.0.1"),
        );
        let out = check(
            &r,
            &addrs(&["127.0.0.1", "box.example.com"]),
            &[tcp(30000, 30000)],
        )
        .await
        .unwrap();
        assert!(out.dead.is_empty());
        assert_eq!(statuses(&out), ["ok", "unverified"]);
        assert_eq!(out.report["addresses"][0]["detail"], LOOPBACK);

        // UDP only: no probe at all, but an address the bind doesn't cover is still dead.
        probe.calls.lock().unwrap().clear();
        let out = check(
            &r,
            &addrs(&["127.0.0.1", "100.64.0.10"]),
            &[udp(30001, 30001)],
        )
        .await
        .unwrap();
        assert_eq!(out.dead, ["100.64.0.10"]);
        assert_eq!(statuses(&out), ["not_checked", "unreachable"]);
        assert!(out.report["udp"].is_string());
        assert!(probe.calls().is_empty());
    }

    /// Nix/host install (no bind): the address is probed; a failure while the app answers
    /// is a note, never a dropped link.
    #[tokio::test]
    async fn probe_mode_never_drops_an_address() {
        let (r, _) = reach(
            FakeProbe::with(&[
                ("10.88.0.2", 30000, Probe::Open),
                ("203.0.113.10", 30000, Probe::Open),
                ("100.64.0.10", 30000, Probe::TimedOut),
            ]),
            None,
        );
        let out = check(
            &r,
            &addrs(&["203.0.113.10", "100.64.0.10"]),
            &[tcp(30000, 30000)],
        )
        .await
        .unwrap();
        assert!(out.dead.is_empty());
        assert_eq!(statuses(&out), ["ok", "unverified"]);
        assert_eq!(out.report["warning"], Value::Null);
        let note = out.report["note"].as_str().unwrap();
        assert!(note.contains("100.64.0.10:30000 timed out"), "{note}");
        assert!(note.contains("links are kept"), "{note}");
    }

    #[tokio::test]
    async fn probe_mode_an_app_that_does_not_answer_is_not_an_address_problem() {
        let (r, _) = reach(FakeProbe::default(), None);
        let out = check(&r, &addrs(&["203.0.113.10"]), &[tcp(30000, 30000)])
            .await
            .unwrap();
        assert!(out.dead.is_empty());
        assert_eq!(statuses(&out), ["app_not_answering"]);
        assert_eq!(out.report["note"], Value::Null);
    }

    #[tokio::test]
    async fn probe_mode_loopback_and_udp_are_not_checked_and_the_control_uses_the_node_port() {
        let (r, probe) = reach(
            FakeProbe::with(&[
                ("203.0.113.10", 31000, Probe::Open),
                ("10.88.0.2", 30000, Probe::Open),
            ]),
            None,
        );
        let out = check(
            &r,
            &addrs(&["203.0.113.10", "127.0.0.1", "localhost"]),
            &[tcp(30000, 31000), udp(30001, 31001)],
        )
        .await
        .unwrap();
        assert!(out.dead.is_empty());
        assert_eq!(statuses(&out), ["ok", "not_checked", "not_checked"]);
        assert!(out.report["udp"]
            .as_str()
            .unwrap()
            .contains("can't be checked"));
        assert_eq!(probe.calls(), ["10.88.0.2:30000", "203.0.113.10:31000"]);
    }

    #[tokio::test]
    async fn only_udp_ports_say_so() {
        let (r, probe) = reach(FakeProbe::default(), None);
        let out = check(&r, &addrs(&["203.0.113.10"]), &[udp(30001, 30001)])
            .await
            .unwrap();
        assert_eq!(statuses(&out), ["not_checked"]);
        assert!(out.report["udp"].is_string());
        assert!(probe.calls.lock().unwrap().is_empty());
        // No ports at all: nothing to report.
        assert!(check(&r, &addrs(&["203.0.113.10"]), &[]).await.is_none());
    }

    #[test]
    fn loopback_addresses() {
        for a in ["127.0.0.1", "127.1.2.3", "::1", "[::1]", "LOCALHOST"] {
            assert!(is_loopback(a), "{a}");
        }
        for a in ["203.0.113.10", "100.64.0.10", "box.example.com", "fd7a::1"] {
            assert!(!is_loopback(a), "{a}");
        }
    }

    /// The real probe against this process's own loopback listener (no network beyond
    /// loopback): open is kept for the TTL, a refusal is not.
    #[tokio::test]
    async fn tcp_probe_keeps_open_results_briefly() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let probe = TcpProbe::default();
        assert_eq!(probe.probe("127.0.0.1", port).await, Probe::Open);
        drop(listener);
        // Cached: still open within the TTL.
        assert_eq!(probe.probe("127.0.0.1", port).await, Probe::Open);
        let fresh = TcpProbe {
            ttl: Duration::ZERO,
            ..TcpProbe::default()
        };
        assert_eq!(fresh.probe("127.0.0.1", port).await, Probe::Refused);
        assert!(fresh.cached(&("127.0.0.1".into(), port)).is_none());
    }
}
