//! Which resolved addresses the egress proxy may connect to: public unicast only.
//!
//! Refused after DNS resolution, so a name that resolves (or rebinds) to the LAN, the
//! bee itself, the tailnet (CGNAT `100.64.0.0/10`, ULA `fd7a:115c:a1e0::/48`) or a cloud
//! metadata service (`169.254.169.254`) cannot be used to reach it.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// True if `ip` is a globally routable unicast address.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_public(v4),
        IpAddr::V6(v6) => v6_public(v6),
    }
}

fn v4_public(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    let special = ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || a == 0 // "this network"
        || (a == 100 && (b & 0xc0) == 64) // CGNAT 100.64.0.0/10 (Tailscale, carrier NAT)
        || (a == 192 && b == 0 && c == 0) // IETF protocol assignments 192.0.0.0/24
        || (a == 198 && (b & 0xfe) == 18) // benchmarking 198.18.0.0/15
        || a >= 240; // reserved 240.0.0.0/4
    !special
}

fn v6_public(ip: Ipv6Addr) -> bool {
    // `::ffff:a.b.c.d`: an IPv4 address in disguise; judge the IPv4 address.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return v4_public(v4);
    }
    let s = ip.segments();
    let embedded_v4 =
        |hi: u16, lo: u16| Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8);
    // NAT64 well-known prefix 64:ff9b::/96 reaches the embedded IPv4 address.
    if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
        return v4_public(embedded_v4(s[6], s[7]));
    }
    // 6to4 2002::/16 tunnels to the embedded IPv4 address.
    if s[0] == 0x2002 {
        return v4_public(embedded_v4(s[1], s[2]));
    }
    // Only global unicast 2000::/3 is public; within it, refuse the special blocks.
    let global = (s[0] & 0xe000) == 0x2000;
    let special = (s[0] == 0x2001 && s[1] < 0x0200) // IETF assignments incl. Teredo 2001::/32
        || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
        || (s[0] == 0x3fff && s[1] < 0x1000); // documentation 3fff::/20
    global && !special
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public(s: &str) -> bool {
        is_public(s.parse().unwrap())
    }

    #[test]
    fn private_and_special_v4_are_refused() {
        for ip in [
            "0.0.0.0",
            "0.1.2.3",
            "127.0.0.1",
            "127.255.0.9",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "100.127.255.254",
            "192.0.0.8",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.7",
            "203.0.113.9",
            "224.0.0.1",
            "239.255.255.250",
            "240.0.0.1",
            "255.255.255.255",
        ] {
            assert!(!public(ip), "{ip} accepted");
        }
    }

    #[test]
    fn public_v4_is_allowed() {
        for ip in [
            "1.1.1.1",
            "8.8.8.8",
            "100.63.255.255",
            "100.128.0.1",
            "172.32.0.1",
            "192.169.0.1",
        ] {
            assert!(public(ip), "{ip} refused");
        }
    }

    #[test]
    fn private_and_special_v6_are_refused() {
        for ip in [
            "::",
            "::1",
            "::127.0.0.1",      // IPv4-compatible (deprecated)
            "::ffff:127.0.0.1", // mapped loopback
            "::ffff:10.1.2.3",  // mapped private
            "::ffff:169.254.169.254",
            "64:ff9b::a00:1",   // NAT64 of 10.0.0.1
            "64:ff9b:1::1",     // local-use NAT64
            "2002:c0a8:101::1", // 6to4 of 192.168.1.1
            "fc00::1",
            "fd7a:115c:a1e0::1", // tailnet ULA
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "2001::1", // Teredo
            "2001:db8::1",
            "3fff::1",
            "100::1", // discard-only
        ] {
            assert!(!public(ip), "{ip} accepted");
        }
    }

    #[test]
    fn public_v6_is_allowed() {
        for ip in [
            "2606:4700:4700::1111",
            "2a00:1450:4001::1",
            "::ffff:1.1.1.1",
            "64:ff9b::101:101", // NAT64 of 1.1.1.1
            "2002:101:101::1",  // 6to4 of 1.1.1.1
        ] {
            assert!(public(ip), "{ip} refused");
        }
    }
}
