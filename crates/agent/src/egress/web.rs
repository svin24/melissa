//! The "web read" class: any public web page, read-only, for every proxied sandbox.
//!
//! Next to each backend's API allowlist, the proxy lets an agent read the open web:
//!
//! - HTTPS: `CONNECT host:443` to any host that resolves to a public address. The tunnel is
//!   TLS, so methods and bodies are invisible; read-only is approximated by a small upload cap
//!   per tunnel ([`WebRead::max_upload`]: TLS handshake plus request headers fit, an upload
//!   does not), a download cap and a lifetime.
//! - HTTP: a plain forward request (`GET http://host/path`) to port 80. Here the proxy sees
//!   the request: only `GET` and `HEAD` without a body pass, one request per connection
//!   (`Connection: close`), and nothing the client sends after the request head is relayed.
//!
//! Every web tunnel is audited and taints the task (its later changes need the owner's OK).

use std::time::Duration;

/// Settings of the web read class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebRead {
    /// Off: only the backends' allowlists (the old behaviour).
    pub enabled: bool,
    /// Ports open to `CONNECT` (HTTPS).
    pub https_ports: Vec<u16>,
    /// Ports open to plain forward requests (HTTP).
    pub http_ports: Vec<u16>,
    /// Bytes the agent may send through one web tunnel (TLS handshake, request headers).
    pub max_upload: u64,
    /// Bytes one web tunnel may bring back.
    pub max_download: u64,
    /// How long one web tunnel may stay open in total.
    pub max_duration: Duration,
    /// A web tunnel with no bytes either way for this long is closed.
    pub idle_timeout: Duration,
}

impl WebRead {
    /// The web read class switched on with the default limits.
    pub fn on() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }
}

impl Default for WebRead {
    /// Off (a library default; the server turns it on unless `[egress] web_read = false`).
    fn default() -> Self {
        Self {
            enabled: false,
            https_ports: vec![443],
            http_ports: vec![80],
            // A TLS 1.3 handshake is ~2 KiB, request headers a few KiB, HTTP/2 window updates
            // ~40 B each: a 32 MiB download sends well under 16 KiB.
            max_upload: 64 * 1024,
            max_download: 32 * 1024 * 1024,
            max_duration: Duration::from_secs(120),
            idle_timeout: Duration::from_secs(30),
        }
    }
}

/// A plain HTTP request the proxy forwards: target, and the head rewritten for the origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forward {
    pub method: String,
    /// Normalized host name or IP literal (no brackets).
    pub host: String,
    pub port: u16,
    /// `http://host[:port]/path` for the audit (query cut at 200 characters).
    pub url: String,
    /// The request head to send upstream (origin form, `Connection: close`).
    pub head: Vec<u8>,
}

/// Why a plain HTTP request is refused. The text completes "blocked by Melissa's network
/// policy: <url>: ...".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ForwardRefusal {
    #[error("only GET and HEAD are allowed (web reading is read-only), not {0}")]
    Method(String),
    #[error("requests with a body are not allowed (web reading is read-only)")]
    Body,
    #[error("only http:// URLs can be forwarded; HTTPS goes through CONNECT")]
    Scheme,
    #[error("malformed request")]
    Malformed,
}

/// Parse a forward request head (`GET http://host/path HTTP/1.1` + headers) and rewrite it
/// for the origin server. `method`/`target` come from [`super::parse_request`].
pub fn forward_request(method: &str, target: &str, head: &[u8]) -> Result<Forward, ForwardRefusal> {
    let rest = target
        .strip_prefix("http://")
        .or_else(|| {
            // Scheme names are case-insensitive.
            target
                .get(..7)
                .filter(|s| s.eq_ignore_ascii_case("http://"))
                .map(|_| &target[7..])
        })
        .ok_or(ForwardRefusal::Scheme)?;
    if !matches!(method, "GET" | "HEAD") {
        return Err(ForwardRefusal::Method(method.chars().take(16).collect()));
    }
    let (authority, path) = match rest.find(['/', '?']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let path = path.split('#').next().unwrap_or("/");
    let path = if path.starts_with('?') {
        format!("/{path}")
    } else {
        path.to_string()
    };
    // `user:pass@host` is refused: credentials in URLs, and an easy way to confuse parsers.
    if authority.is_empty() || authority.contains('@') {
        return Err(ForwardRefusal::Malformed);
    }
    let (host, port) = split_authority(authority).ok_or(ForwardRefusal::Malformed)?;
    let host = super::normalize_host(host).ok_or(ForwardRefusal::Malformed)?;

    let text = std::str::from_utf8(head).map_err(|_| ForwardRefusal::Malformed)?;
    let mut out = format!("{method} {path} HTTP/1.1\r\n");
    let mut lines = text.split("\r\n").skip(1);
    for line in lines.by_ref() {
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').ok_or(ForwardRefusal::Malformed)?;
        let lname = name.trim().to_ascii_lowercase();
        let value = value.trim();
        match lname.as_str() {
            "content-length" if value != "0" => return Err(ForwardRefusal::Body),
            "transfer-encoding" | "expect" => return Err(ForwardRefusal::Body),
            // Hop-by-hop and proxy headers stay here; Host is set from the URL.
            "host"
            | "connection"
            | "keep-alive"
            | "upgrade"
            | "te"
            | "trailer"
            | "proxy-connection"
            | "proxy-authorization"
            | "proxy-authenticate" => {}
            _ => {
                out.push_str(line);
                out.push_str("\r\n");
            }
        }
    }
    let host_header = match (host.contains(':'), port) {
        (true, 80) => format!("[{host}]"),
        (true, p) => format!("[{host}]:{p}"),
        (false, 80) => host.clone(),
        (false, p) => format!("{host}:{p}"),
    };
    out = out.replacen(
        "\r\n",
        &format!("\r\nHost: {host_header}\r\nConnection: close\r\n"),
        1,
    );
    out.push_str("\r\n");
    let url: String = format!("http://{host_header}{path}")
        .chars()
        .take(200)
        .collect();
    Ok(Forward {
        method: method.to_string(),
        host,
        port,
        url,
        head: out.into_bytes(),
    })
}

/// `host`, `host:port`, `[v6]` or `[v6]:port` (default port 80).
fn split_authority(authority: &str) -> Option<(&str, u16)> {
    let port = |p: &str| p.parse::<u16>().ok().filter(|p| *p != 0);
    if let Some(rest) = authority.strip_prefix('[') {
        let (h, after) = rest.split_once(']')?;
        h.parse::<std::net::Ipv6Addr>().ok()?;
        return match after {
            "" => Some((h, 80)),
            _ => Some((h, port(after.strip_prefix(':')?)?)),
        };
    }
    match authority.rsplit_once(':') {
        Some((h, p)) => Some((h, port(p)?)),
        None => Some((authority, 80)),
    }
}

/// Scheme (lowercase), host (normalized, no brackets) and port of an absolute URL, if it
/// has the `scheme://authority` form. Default ports for http and https.
pub fn url_target(url: &str) -> Option<(String, String, u16)> {
    let (scheme, rest) = url.trim().split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let authority = rest.split(['/', '?', '#']).next()?;
    // Credentials before the host: the host is after the last `@`.
    let authority = authority.rsplit('@').next()?;
    let (host, port) = split_authority(authority)?;
    let port = if authority.ends_with(']') || !authority.contains(':') {
        match scheme.as_str() {
            "https" => 443,
            "http" => 80,
            _ => port,
        }
    } else {
        port
    };
    Some((scheme, super::normalize_host(host)?, port))
}

/// Local-only names an agent may try (`localhost`, mDNS, single labels): refused before any
/// lookup, with a clear reason. Public DNS never serves them.
pub fn local_name(host: &str) -> bool {
    if host.parse::<std::net::IpAddr>().is_ok() {
        return false;
    }
    !host.contains('.')
        || [
            ".localhost",
            ".local",
            ".internal",
            ".home.arpa",
            ".lan",
            ".localdomain",
            ".ts.net",
        ]
        .iter()
        .any(|s| host.ends_with(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fwd(head: &str) -> Result<Forward, ForwardRefusal> {
        let line = head.split("\r\n").next().unwrap();
        let mut parts = line.split(' ');
        let (m, t) = (parts.next().unwrap(), parts.next().unwrap());
        forward_request(m, t, head.as_bytes())
    }

    #[test]
    fn get_is_rewritten_to_origin_form_with_connection_close() {
        let f = fwd("GET http://Example.com/a/b?q=1#frag HTTP/1.1\r\nHost: example.com\r\nUser-Agent: t\r\nProxy-Connection: keep-alive\r\nConnection: keep-alive\r\n\r\n").unwrap();
        assert_eq!(f.host, "example.com");
        assert_eq!(f.port, 80);
        assert_eq!(f.url, "http://example.com/a/b?q=1");
        assert_eq!(
            String::from_utf8(f.head).unwrap(),
            "GET /a/b?q=1 HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\nUser-Agent: t\r\n\r\n"
        );
        let f = fwd("HEAD http://[2001:db8::1]:8080 HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!((f.host.as_str(), f.port), ("2001:db8::1", 8080));
        assert!(String::from_utf8(f.head)
            .unwrap()
            .starts_with("HEAD / HTTP/1.1\r\nHost: [2001:db8::1]:8080\r\n"));
        let f = fwd("GET http://example.com?x HTTP/1.1\r\n\r\n").unwrap();
        assert!(f.head.starts_with(b"GET /?x HTTP/1.1\r\n"));
    }

    #[test]
    fn writes_bodies_and_odd_targets_are_refused() {
        for (head, want) in [
            (
                "POST http://example.com/ HTTP/1.1\r\nContent-Length: 3\r\n\r\n",
                ForwardRefusal::Method("POST".into()),
            ),
            (
                "PUT http://example.com/ HTTP/1.1\r\n\r\n",
                ForwardRefusal::Method("PUT".into()),
            ),
            (
                "GET http://example.com/ HTTP/1.1\r\nContent-Length: 10\r\n\r\n",
                ForwardRefusal::Body,
            ),
            (
                "GET http://example.com/ HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n",
                ForwardRefusal::Body,
            ),
            (
                "GET https://example.com/ HTTP/1.1\r\n\r\n",
                ForwardRefusal::Scheme,
            ),
            ("GET /local HTTP/1.1\r\n\r\n", ForwardRefusal::Scheme),
            (
                "GET http://user:pw@example.com/ HTTP/1.1\r\n\r\n",
                ForwardRefusal::Malformed,
            ),
            (
                "GET http://example.com:0/ HTTP/1.1\r\n\r\n",
                ForwardRefusal::Malformed,
            ),
            (
                "GET http://exa mple.com/ HTTP/1.1\r\n\r\n",
                ForwardRefusal::Malformed,
            ),
        ] {
            // Request lines with spaces in the target split differently; feed them raw.
            let line = head.split("\r\n").next().unwrap();
            let mut parts = line.splitn(2, ' ');
            let m = parts.next().unwrap();
            let t = parts.next().unwrap().rsplit_once(' ').unwrap().0;
            assert_eq!(
                forward_request(m, t, head.as_bytes()),
                Err(want.clone()),
                "{head:?}"
            );
        }
        // A zero Content-Length is no body.
        assert!(fwd("GET http://example.com/ HTTP/1.1\r\nContent-Length: 0\r\n\r\n").is_ok());
    }

    #[test]
    fn url_targets() {
        let t = |u: &str| url_target(u).map(|(s, h, p)| format!("{s} {h} {p}"));
        assert_eq!(
            t("https://Example.com/a").as_deref(),
            Some("https example.com 443")
        );
        assert_eq!(
            t("http://example.com").as_deref(),
            Some("http example.com 80")
        );
        assert_eq!(
            t("HTTP://example.com:8080/x").as_deref(),
            Some("http example.com 8080")
        );
        assert_eq!(t("https://[::1]/").as_deref(), Some("https ::1 443"));
        assert_eq!(
            t("https://[fd00::1]:8443/").as_deref(),
            Some("https fd00::1 8443")
        );
        assert_eq!(
            t("https://u:p@10.0.0.1/").as_deref(),
            Some("https 10.0.0.1 443")
        );
        assert_eq!(
            t("https://example.com?q").as_deref(),
            Some("https example.com 443")
        );
        assert_eq!(t("example.com"), None);
        assert_eq!(t("https://bad host/"), None);
    }

    #[test]
    fn local_names_are_recognised() {
        for h in [
            "localhost",
            "printer",
            "nas.local",
            "x.localhost",
            "router.lan",
            "box.home.arpa",
            "db.internal",
            "laptop.tail1234.ts.net",
        ] {
            assert!(local_name(h), "{h}");
        }
        for h in ["example.com", "203.0.113.7", "::1", "docs.rs"] {
            assert!(!local_name(h), "{h}");
        }
    }
}
