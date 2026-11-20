//! This device's addresses, for app links (`http://<address>:<port>`).
//!
//! No DNS and no traffic: the LAN address is the source address the kernel would pick for a
//! public destination (a UDP `connect` only looks up the route; nothing is sent), and the
//! Tailscale address is a local address in 100.64.0.0/10, read from `/proc/net/fib_trie`.
//! k3s's own networks (pods 10.42.0.0/16, services 10.43.0.0/16), loopback and link-local
//! addresses are never offered.

use std::net::{IpAddr, Ipv4Addr, UdpSocket};

/// A documentation address (TEST-NET-3): only used to ask the kernel for a route.
const ROUTE_PROBE: (Ipv4Addr, u16) = (Ipv4Addr::new(203, 0, 113, 1), 9);

/// The addresses people can reach this device at, most useful first: the LAN address of
/// the default route, then Tailscale addresses. Empty if none can be found.
pub fn addresses() -> Vec<IpAddr> {
    let mut out = Vec::new();
    if let Some(lan) = default_route_address() {
        out.push(IpAddr::V4(lan));
    }
    if let Ok(trie) = std::fs::read_to_string("/proc/net/fib_trie") {
        for ip in local_ipv4(&trie) {
            if is_tailscale(ip) && !out.contains(&IpAddr::V4(ip)) {
                out.push(IpAddr::V4(ip));
            }
        }
    }
    out
}

/// The device's host name (from the kernel), if it is a usable single DNS label or name;
/// `localhost` and empty names don't count.
pub fn host_name() -> Option<String> {
    let raw = std::fs::read_to_string("/proc/sys/kernel/hostname").ok()?;
    valid_host_name(raw.trim()).then(|| raw.trim().to_ascii_lowercase())
}

pub(crate) fn valid_host_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && !name.eq_ignore_ascii_case("localhost")
        && !name.eq_ignore_ascii_case("localhost.localdomain")
        && name.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// The source address of the default route (inside the container: its own address, which is
/// also k3s's node address).
pub(crate) fn default_route_address() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    // `connect` on UDP sends nothing; it only fixes the route and so the source address.
    socket.connect(ROUTE_PROBE).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if usable(ip) => Some(ip),
        _ => None,
    }
}

/// Tailscale (and other CGNAT) addresses: 100.64.0.0/10.
fn is_tailscale(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    a == 100 && (b & 0xc0) == 64
}

/// Not loopback, link-local, unspecified or one of k3s's networks.
fn usable(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || (a == 10 && (b == 42 || b == 43)))
}

/// The device's own IPv4 addresses from `/proc/net/fib_trie` (entries `/32 host LOCAL`),
/// usable ones only, without duplicates.
pub(crate) fn local_ipv4(trie: &str) -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    let mut last: Option<Ipv4Addr> = None;
    for line in trie.lines() {
        let t = line.trim();
        if let Some(ip) = t.strip_prefix("|-- ") {
            last = ip.trim().parse().ok();
        } else if t.starts_with("/32 host LOCAL") {
            if let Some(ip) = last.take() {
                if usable(ip) && !out.contains(&ip) {
                    out.push(ip);
                }
            }
        }
    }
    out
}

/// `http://<address>:<port>` (IPv6 in brackets).
/// The port people use for an app whose NodePort is `node_port`, when the host publishes
/// NodePort 30000+n as 30000+n+`offset` (container mode with another `APP_PORT_BASE`).
/// `None` if that falls outside 1-65535.
pub fn published_port(node_port: u16, offset: i32) -> Option<u16> {
    u16::try_from(i32::from(node_port) + offset)
        .ok()
        .filter(|p| *p > 0)
}

pub fn url(address: &str, port: u16) -> String {
    format!("http://{}", host_port(address, port))
}

/// `address:port`, with IPv6 addresses in brackets (`[fd7a::1]:30001`).
pub fn host_port(address: &str, port: u16) -> String {
    if address.contains(':') && !address.starts_with('[') {
        format!("[{address}]:{port}")
    } else {
        format!("{address}:{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_ports_apply_the_offset() {
        assert_eq!(published_port(30000, 0), Some(30000));
        assert_eq!(published_port(30005, 1000), Some(31005));
        assert_eq!(published_port(30079, -10000), Some(20079));
        assert_eq!(published_port(30000, 40000), None);
        assert_eq!(published_port(30000, -30000), None);
        assert_eq!(url("203.0.113.10", 31005), "http://203.0.113.10:31005");
        assert_eq!(url("2001:db8::1", 31005), "http://[2001:db8::1]:31005");
    }

    const TRIE: &str = "Main:
  +-- 0.0.0.0/0 3 0 5
     |-- 0.0.0.0
        /0 universe UNICAST
     +-- 10.42.0.0/24 2 0 2
        |-- 10.42.0.1
           /32 host LOCAL
     |-- 100.79.0.7
        /32 host LOCAL
     +-- 127.0.0.0/8 2 0 2
        |-- 127.0.0.1
           /32 host LOCAL
     +-- 192.0.2.0/24 2 0 2
        |-- 192.0.2.0
           /32 link BROADCAST
           /24 link UNICAST
        |-- 192.0.2.15
           /32 host LOCAL
Local:
  +-- 0.0.0.0/0 3 0 5
     |-- 192.0.2.15
        /32 host LOCAL
";

    #[test]
    fn local_addresses_skip_k3s_and_loopback() {
        let ips = local_ipv4(TRIE);
        assert_eq!(
            ips,
            [Ipv4Addr::new(100, 79, 0, 7), Ipv4Addr::new(192, 0, 2, 15)]
        );
        assert!(is_tailscale(ips[0]));
        assert!(!is_tailscale(ips[1]));
        assert!(!is_tailscale(Ipv4Addr::new(100, 128, 0, 1)));
    }

    #[test]
    fn host_names_are_plain_dns_names() {
        assert!(valid_host_name("melissa"));
        assert!(valid_host_name("box-1.lan"));
        assert!(!valid_host_name("localhost"));
        assert!(!valid_host_name("a b"));
        assert!(!valid_host_name("-x"));
        assert!(!valid_host_name(""));
    }

    #[test]
    fn urls_bracket_ipv6() {
        assert_eq!(url("192.0.2.15", 30000), "http://192.0.2.15:30000");
        assert_eq!(
            url("melissa.example.com", 30001),
            "http://melissa.example.com:30001"
        );
        assert_eq!(url("2001:db8::1", 30000), "http://[2001:db8::1]:30000");
    }
}
