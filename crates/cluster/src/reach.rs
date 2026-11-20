//! Is an app really reachable at the addresses its links name?
//!
//! App links are built from `[apps] addresses`, which is configuration: it can disagree with
//! where the host actually publishes the app ports (container install with `BIND=127.0.0.1`
//! but a Tailscale address in `[apps] addresses`: every link is "connection refused"). Before
//! the app tools hand links to agents they try a TCP connect to each advertised
//! `address:port` from where Melissa runs (short timeout, results cached briefly), plus one
//! control connect straight to the NodePort on the device's own address. When the control
//! answers but an advertised address doesn't, the address is dead (not published there) and
//! is dropped from the links, with a warning the agent passes on to the owner. When nothing
//! answers, the app itself isn't answering yet (starting, or listening on another port): not
//! an address problem. UDP can't be checked this way (no handshake), loopback addresses only
//! work on the device itself and aren't checked either; both are said so.
//!
//! Limits: the check runs from the device, so a firewall that only blocks other machines
//! (or a Tailscale ACL) is not seen.

use std::{
    collections::HashMap,
    net::IpAddr,
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

/// The checker the app tools use: a probe and the control address (the device's own
/// address where the NodePorts are served directly, without the host's publishing).
#[derive(Clone)]
pub struct ReachCheck {
    pub probe: Arc<dyn PortProbe>,
    /// `None`: no control connect (an unanswered address then counts as "app not answering").
    pub control: Option<String>,
}

impl ReachCheck {
    /// TCP connects from this device; control: the address of the default route (in the
    /// container that is the container's own address, which is k3s's node address).
    pub fn tcp() -> Self {
        Self {
            probe: Arc::new(TcpProbe::default()),
            control: device::default_route_address().map(|ip| ip.to_string()),
        }
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
    address.eq_ignore_ascii_case("localhost")
        || address
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Check the app's TCP `ports` at each of `addresses`. `None` when there is nothing to
/// report (no ports).
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
    let checked: Vec<&String> = addresses.iter().filter(|a| !is_loopback(a)).collect();

    // Every connect at once: the slowest (a timeout) bounds the whole check.
    let advertised = futures::future::join_all(checked.iter().flat_map(|a| {
        tcp.iter()
            .map(move |p| async move { (a.as_str(), *p, reach.probe.probe(a, p.published).await) })
    }));
    let control = futures::future::join_all(tcp.iter().map(|p| async move {
        match &reach.control {
            Some(c) => reach.probe.probe(c, p.node_port).await == Probe::Open,
            None => false,
        }
    }));
    let (advertised, control) = tokio::join!(advertised, control);

    let mut rows = Vec::new();
    let mut dead = Vec::new();
    let mut problems = Vec::new();
    for a in addresses {
        if is_loopback(a) {
            rows.push(json!({"address": a, "status": "not_checked",
                "detail": "loopback: only works on this device itself"}));
            continue;
        }
        if tcp.is_empty() {
            rows.push(json!({"address": a, "status": "not_checked",
                "detail": "no TCP port to check"}));
            continue;
        }
        let mine: Vec<(CheckedPort, &Probe)> = advertised
            .iter()
            .filter(|(addr, ..)| addr == a)
            .map(|(_, p, probe)| (*p, probe))
            .collect();
        // A port the app answers on: the control connect, or this port open at any address.
        let answers = |p: &CheckedPort| {
            tcp.iter()
                .position(|t| t.node_port == p.node_port)
                .is_some_and(|i| control[i])
                || advertised
                    .iter()
                    .any(|(_, q, probe)| q.node_port == p.node_port && *probe == Probe::Open)
        };
        let failed: Vec<&(CheckedPort, &Probe)> = mine
            .iter()
            .filter(|(_, probe)| **probe != Probe::Open)
            .collect();
        if failed.is_empty() {
            rows.push(json!({"address": a, "status": "ok"}));
        } else if let Some((p, probe)) = failed.iter().find(|(p, _)| answers(p)) {
            let at = device::host_port(a, p.published);
            let detail = format!(
                "{at} {} although the app answers on the device: the host does not publish \
                 app ports on this address (or a firewall blocks it)",
                probe.describe()
            );
            problems.push(format!("{at} {}", probe.describe()));
            rows.push(json!({"address": a, "status": "unreachable", "detail": detail}));
            dead.push(a.clone());
        } else {
            rows.push(json!({"address": a, "status": "app_not_answering",
                "detail": "the app does not answer on its port yet (starting, or it listens \
                           on another port than its spec says): not an address problem"}));
        }
    }

    let mut report = json!({
        "checked_from": "this device (a firewall that only blocks other machines is not seen)",
        "addresses": rows,
    });
    if ports.iter().any(|p| p.protocol == Protocol::Udp) {
        report["udp"] = json!("UDP ports can't be checked (no handshake): only TCP is tested");
    }
    if !problems.is_empty() {
        let usable = addresses.len() - dead.len();
        report["warning"] = json!(format!(
            "not reachable: {}. Left out of the links: don't give these addresses out. Tell \
             the owner the app ports are not published on that address (container install: \
             BIND doesn't cover it; rerun install.sh with a BIND that matches [apps] \
             addresses). Restarting the app won't fix this.{}",
            problems.join(", "),
            if usable == 0 {
                " No working address is left."
            } else {
                ""
            }
        ));
    }
    Some(Reachability { report, dead })
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

    fn reach(probe: FakeProbe) -> (ReachCheck, Arc<FakeProbe>) {
        let probe = Arc::new(probe);
        (
            ReachCheck {
                probe: probe.clone(),
                control: Some("10.88.0.2".into()),
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

    fn addrs(a: &[&str]) -> Vec<String> {
        a.iter().map(ToString::to_string).collect()
    }

    /// The incident: ports published on loopback only, links naming the Tailscale address.
    #[tokio::test]
    async fn an_address_the_host_does_not_publish_on_is_dead() {
        let (r, _) = reach(FakeProbe::with(&[("10.88.0.2", 30000, Probe::Open)]));
        let out = check(&r, &addrs(&["100.64.0.10"]), &[tcp(30000, 30000)])
            .await
            .unwrap();
        assert_eq!(out.dead, ["100.64.0.10"]);
        assert_eq!(out.report["addresses"][0]["status"], "unreachable");
        let warning = out.report["warning"].as_str().unwrap();
        assert!(warning.contains("100.64.0.10:30000 refused"), "{warning}");
        assert!(warning.contains("No working address"), "{warning}");
        assert!(
            warning.contains("Restarting the app won't fix this"),
            "{warning}"
        );
    }

    #[tokio::test]
    async fn one_dead_address_among_working_ones() {
        // No control address: the port open at another address is the evidence.
        let probe = FakeProbe::with(&[
            ("203.0.113.10", 31000, Probe::Open),
            ("100.64.0.10", 31000, Probe::TimedOut),
        ]);
        let r = ReachCheck {
            probe: Arc::new(probe),
            control: None,
        };
        let out = check(
            &r,
            &addrs(&["203.0.113.10", "100.64.0.10"]),
            &[tcp(30000, 31000)],
        )
        .await
        .unwrap();
        assert_eq!(out.dead, ["100.64.0.10"]);
        assert_eq!(out.report["addresses"][0]["status"], "ok");
        let warning = out.report["warning"].as_str().unwrap();
        assert!(warning.contains("100.64.0.10:31000 timed out"), "{warning}");
        assert!(!warning.contains("No working address"), "{warning}");
    }

    #[tokio::test]
    async fn an_app_that_does_not_answer_is_not_an_address_problem() {
        let (r, _) = reach(FakeProbe::default());
        let out = check(&r, &addrs(&["203.0.113.10"]), &[tcp(30000, 30000)])
            .await
            .unwrap();
        assert!(out.dead.is_empty());
        assert_eq!(out.report["addresses"][0]["status"], "app_not_answering");
        assert_eq!(out.report["warning"], Value::Null);
    }

    #[tokio::test]
    async fn loopback_and_udp_are_not_checked_and_the_control_uses_the_node_port() {
        let (r, probe) = reach(FakeProbe::with(&[
            ("203.0.113.10", 31000, Probe::Open),
            ("10.88.0.2", 30000, Probe::Open),
        ]));
        let udp = CheckedPort {
            protocol: Protocol::Udp,
            node_port: 30001,
            published: 31001,
        };
        let out = check(
            &r,
            &addrs(&["203.0.113.10", "127.0.0.1", "localhost"]),
            &[tcp(30000, 31000), udp],
        )
        .await
        .unwrap();
        assert!(out.dead.is_empty());
        let statuses: Vec<&str> = out.report["addresses"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["status"].as_str().unwrap())
            .collect();
        assert_eq!(statuses, ["ok", "not_checked", "not_checked"]);
        assert!(out.report["udp"]
            .as_str()
            .unwrap()
            .contains("can't be checked"));
        let mut calls = probe.calls.lock().unwrap().clone();
        calls.sort();
        assert_eq!(calls, ["10.88.0.2:30000", "203.0.113.10:31000"]);
    }

    #[tokio::test]
    async fn only_udp_ports_say_so() {
        let (r, probe) = reach(FakeProbe::default());
        let udp = CheckedPort {
            protocol: Protocol::Udp,
            node_port: 30001,
            published: 30001,
        };
        let out = check(&r, &addrs(&["203.0.113.10"]), &[udp]).await.unwrap();
        assert_eq!(out.report["addresses"][0]["status"], "not_checked");
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
