//! Host allowlist rules and the proxy's request-line parsing.

use std::fmt;

/// Port allowed when a rule names none.
pub const DEFAULT_PORT: u16 = 443;

/// One allowlist entry: `host`, `*.domain` (any subdomain, not the domain itself), each
/// optionally with `:port` (default 443).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRule {
    host: HostPattern,
    port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HostPattern {
    Exact(String),
    /// Stored with the leading dot (`.example.com`).
    Subdomains(String),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("bad egress allowlist entry `{0}`: use `host`, `*.domain`, optionally `:port`")]
pub struct BadRule(pub String);

impl HostRule {
    pub fn parse(rule: &str) -> Result<Self, BadRule> {
        let bad = || BadRule(rule.to_string());
        let (host, port) = match rule.rsplit_once(':') {
            Some((h, p)) => (
                h,
                p.parse::<u16>().ok().filter(|p| *p != 0).ok_or_else(bad)?,
            ),
            None => (rule, DEFAULT_PORT),
        };
        let (wild, name) = match host.strip_prefix("*.") {
            Some(rest) => (true, rest),
            None => (false, host),
        };
        let name = normalize_host(name).ok_or_else(bad)?;
        // A rule names DNS names, not addresses, and a wildcard needs a real domain under it.
        if name.parse::<std::net::IpAddr>().is_ok() || (wild && !name.contains('.')) {
            return Err(bad());
        }
        let host = if wild {
            HostPattern::Subdomains(format!(".{name}"))
        } else {
            HostPattern::Exact(name)
        };
        Ok(Self { host, port })
    }

    /// `host` must already be normalized ([`normalize_host`]).
    pub fn matches(&self, host: &str, port: u16) -> bool {
        port == self.port
            && match &self.host {
                HostPattern::Exact(h) => h == host,
                HostPattern::Subdomains(suffix) => host.ends_with(suffix.as_str()),
            }
    }
}

impl fmt::Display for HostRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.host {
            HostPattern::Exact(h) => write!(f, "{h}:{}", self.port),
            HostPattern::Subdomains(s) => write!(f, "*{s}:{}", self.port),
        }
    }
}

/// Lowercase, without a trailing dot; `None` unless it is a plausible DNS name (letters,
/// digits, `-`, `_`, dots between non-empty labels) or an IP address literal.
pub fn normalize_host(host: &str) -> Option<String> {
    let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    if host.parse::<std::net::IpAddr>().is_ok() {
        return Some(host);
    }
    let ok = !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && label
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        });
    ok.then_some(host)
}

/// True if some rule allows `host:port` (`host` normalized).
pub fn allowed(rules: &[HostRule], host: &str, port: u16) -> bool {
    rules.iter().any(|r| r.matches(host, port))
}

/// The first line of a proxy request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyRequest {
    /// `CONNECT host:port HTTP/1.x`: a tunnel (TLS inside, never inspected).
    Connect { host: String, port: u16 },
    /// Anything else (e.g. a plain `GET http://...` forward request), refused.
    Other { method: String, target: String },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("malformed proxy request")]
pub struct BadRequest;

/// Parse the request head (everything up to and including the blank line). Headers are
/// ignored: the proxy needs no authentication (the socket identifies the agent).
pub fn parse_request(head: &[u8]) -> Result<ProxyRequest, BadRequest> {
    let text = std::str::from_utf8(head).map_err(|_| BadRequest)?;
    let line = text.split("\r\n").next().ok_or(BadRequest)?;
    let mut parts = line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(BadRequest);
    };
    if !matches!(version, "HTTP/1.1" | "HTTP/1.0") || method.is_empty() || target.is_empty() {
        return Err(BadRequest);
    }
    if method != "CONNECT" {
        return Ok(ProxyRequest::Other {
            method: method.into(),
            target: target.into(),
        });
    }
    let (host, port) = if let Some(rest) = target.strip_prefix('[') {
        // `[v6]:port`
        let (h, p) = rest.split_once("]:").ok_or(BadRequest)?;
        h.parse::<std::net::Ipv6Addr>().map_err(|_| BadRequest)?;
        (h, p)
    } else {
        target.rsplit_once(':').ok_or(BadRequest)?
    };
    let port = port
        .parse::<u16>()
        .ok()
        .filter(|p| *p != 0)
        .ok_or(BadRequest)?;
    let host = normalize_host(host).ok_or(BadRequest)?;
    Ok(ProxyRequest::Connect { host, port })
}

/// Index just past the `\r\n\r\n` that ends a request head, if present.
pub fn head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(list: &[&str]) -> Vec<HostRule> {
        list.iter().map(|r| HostRule::parse(r).unwrap()).collect()
    }

    #[test]
    fn exact_and_subdomain_rules() {
        let r = rules(&[
            "api.example.com",
            "*.cdn.example.net",
            "alt.example.org:8443",
        ]);
        assert!(allowed(&r, "api.example.com", 443));
        assert!(!allowed(&r, "api.example.com", 80));
        assert!(!allowed(&r, "example.com", 443));
        assert!(!allowed(&r, "evil-api.example.com", 443));
        assert!(!allowed(&r, "api.example.com.evil.test", 443));
        assert!(allowed(&r, "a.cdn.example.net", 443));
        assert!(allowed(&r, "a.b.cdn.example.net", 443));
        assert!(
            !allowed(&r, "cdn.example.net", 443),
            "wildcard excludes the apex"
        );
        assert!(!allowed(&r, "xcdn.example.net", 443));
        assert!(allowed(&r, "alt.example.org", 8443));
        assert!(!allowed(&r, "alt.example.org", 443));
        assert_eq!(r[1].to_string(), "*.cdn.example.net:443");
    }

    #[test]
    fn rules_are_normalized_and_validated() {
        assert_eq!(
            HostRule::parse("API.Example.COM.").unwrap(),
            HostRule::parse("api.example.com:443").unwrap()
        );
        for bad in [
            "",
            "*",
            "*.com",
            "**.example.com",
            "example.com:0",
            "example.com:99999",
            "example.com:",
            "exa mple.com",
            "-x.example.com",
            "a..b",
            "1.2.3.4",
            "http://example.com",
            "example.com/path",
        ] {
            assert!(HostRule::parse(bad).is_err(), "{bad:?} accepted");
        }
    }

    #[test]
    fn parses_connect_requests() {
        let req = |s: &str| parse_request(s.as_bytes());
        assert_eq!(
            req("CONNECT API.example.com:443 HTTP/1.1\r\nHost: x\r\n\r\n"),
            Ok(ProxyRequest::Connect {
                host: "api.example.com".into(),
                port: 443
            })
        );
        assert_eq!(
            req("CONNECT [2001:db8::1]:8443 HTTP/1.0\r\n\r\n"),
            Ok(ProxyRequest::Connect {
                host: "2001:db8::1".into(),
                port: 8443
            })
        );
        assert_eq!(
            req("GET http://example.com/ HTTP/1.1\r\n\r\n"),
            Ok(ProxyRequest::Other {
                method: "GET".into(),
                target: "http://example.com/".into()
            })
        );
        for bad in [
            "CONNECT example.com HTTP/1.1\r\n\r\n",
            "CONNECT example.com:0 HTTP/1.1\r\n\r\n",
            "CONNECT example.com:443 HTTP/2\r\n\r\n",
            "CONNECT  example.com:443 HTTP/1.1\r\n\r\n",
            "CONNECT example.com:443\r\n\r\n",
            "CONNECT ex ample.com:443 HTTP/1.1\r\n\r\n",
            "CONNECT [not-v6]:443 HTTP/1.1\r\n\r\n",
            "CONNECT a/b:443 HTTP/1.1\r\n\r\n",
            "\r\n\r\n",
        ] {
            assert_eq!(req(bad), Err(BadRequest), "{bad:?}");
        }
        assert_eq!(parse_request(&[0xff, 0xfe]), Err(BadRequest));
        assert_eq!(head_end(b"CONNECT a:1 HTTP/1.1\r\n\r\nrest"), Some(24));
        assert_eq!(head_end(b"CONNECT a:1 HTTP/1.1\r\n"), None);
    }
}
